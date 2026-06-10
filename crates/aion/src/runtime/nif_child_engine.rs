//! Runtime-local child NIF adapters for AT child services.

use std::sync::Arc;

use aion_core::{Event, WorkflowError, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use tokio::runtime::Handle;

use crate::child::{ChildWorkflowError, ChildWorkflowMailbox};
use crate::durability::DurabilityError;
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::lifecycle::start::{StartWorkflowContext, start_workflow};
use crate::loader::LoadedWorkflows;
use crate::registry::{HandleResidency, Registry, TerminalOutcome, WorkflowHandle};
use crate::runtime::RuntimeHandle;
use crate::signal::SignalResumeHandoff;
use crate::supervision::SupervisionTree;

/// Engine-owned context for child workflow NIF calls.
pub(crate) struct ChildNifBridge {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    runtime: Arc<RuntimeHandle>,
    loaded_workflows: LoadedWorkflows,
    registry: Arc<Registry>,
    supervision: Arc<SupervisionTree>,
    signal_handoff: Arc<SignalResumeHandoff>,
    tokio_handle: Handle,
}

/// Constructor dependencies for [`ChildNifBridge`].
pub(crate) struct ChildNifBridgeParts {
    pub(crate) store: Arc<dyn EventStore>,
    pub(crate) visibility_store: Arc<dyn VisibilityStore>,
    pub(crate) runtime: Arc<RuntimeHandle>,
    pub(crate) loaded_workflows: LoadedWorkflows,
    pub(crate) registry: Arc<Registry>,
    pub(crate) supervision: Arc<SupervisionTree>,
    pub(crate) signal_handoff: Arc<SignalResumeHandoff>,
    pub(crate) tokio_handle: Handle,
}

impl ChildNifBridge {
    /// Creates a bridge from engine components.
    #[must_use]
    pub(crate) fn new(parts: ChildNifBridgeParts) -> Self {
        let ChildNifBridgeParts {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            signal_handoff,
            tokio_handle,
        } = parts;
        Self {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            signal_handoff,
            tokio_handle,
        }
    }

    pub(crate) fn registry(&self) -> &Registry {
        self.registry.as_ref()
    }

    pub(crate) fn store(&self) -> Arc<dyn EventStore> {
        Arc::clone(&self.store)
    }

    pub(crate) fn tokio_handle(&self) -> Handle {
        self.tokio_handle.clone()
    }
}

pub(crate) struct CompletionMailbox {
    message: Option<WorkflowMailboxMessage>,
}

impl CompletionMailbox {
    pub(crate) fn new(
        bridge: &ChildNifBridge,
        child_workflow_id: &WorkflowId,
    ) -> Result<Self, String> {
        let child = bridge
            .registry
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|handle| handle.workflow_id() == child_workflow_id)
            .ok_or_else(|| format!("unknown_child_workflow:{child_workflow_id}"))?;
        let mut receiver = child.completion().subscribe();
        let outcome = bridge.tokio_handle.block_on(async {
            loop {
                if let Some(outcome) = receiver.borrow().clone() {
                    break Ok(outcome);
                }
                if receiver.changed().await.is_err() {
                    break Err("child_completion_channel_closed".to_owned());
                }
            }
        })?;
        Ok(Self {
            message: Some(outcome_to_message(child_workflow_id.clone(), outcome)?),
        })
    }
}

impl ChildWorkflowMailbox for CompletionMailbox {
    fn receive_child_workflow_message(
        &mut self,
        child_workflow_id: &WorkflowId,
    ) -> Result<WorkflowMailboxMessage, ChildWorkflowError> {
        self.message
            .take()
            .ok_or_else(|| ChildWorkflowError::MailboxClosed {
                child_workflow_id: child_workflow_id.clone(),
            })
    }
}

fn outcome_to_message(
    child_workflow_id: WorkflowId,
    outcome: TerminalOutcome,
) -> Result<WorkflowMailboxMessage, String> {
    match outcome {
        TerminalOutcome::Completed(result) => Ok(WorkflowMailboxMessage::ChildWorkflowCompleted {
            child_workflow_id,
            correlation: 0,
            result,
        }),
        TerminalOutcome::Failed(error) => Ok(WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation: 0,
            error,
        }),
        TerminalOutcome::Cancelled(reason) => Ok(WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation: 0,
            error: WorkflowError {
                message: format!("cancelled:{reason}"),
                details: None,
            },
        }),
        TerminalOutcome::TimedOut(timeout) => Ok(WorkflowMailboxMessage::ChildWorkflowFailed {
            child_workflow_id,
            correlation: 0,
            error: WorkflowError {
                message: format!("timed_out:{timeout}"),
                details: None,
            },
        }),
        TerminalOutcome::ContinuedAsNew { .. } => {
            Err("child_continued_as_new_without_terminal_result".to_owned())
        }
    }
}

pub(crate) struct NifChildEngine {
    bridge: Arc<ChildNifBridge>,
    parent: WorkflowHandle,
}

impl NifChildEngine {
    #[must_use]
    pub(crate) fn new(bridge: Arc<ChildNifBridge>, parent: WorkflowHandle) -> Self {
        Self { bridge, parent }
    }
}

impl EngineHandle for NifChildEngine {
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError> {
        let handle = self
            .bridge
            .registry
            .list()
            .map_err(|error| EngineSeamError::Delivery {
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id);
        match handle {
            Some(handle) if handle.residency() == HandleResidency::Resident => Ok(
                WorkflowResidency::Resident(WorkflowProcessHandle::new(handle.pid())),
            ),
            Some(_) => Ok(WorkflowResidency::NonResident),
            None => Ok(WorkflowResidency::Unknown),
        }
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        match message {
            WorkflowMailboxMessage::SignalReceived { .. } => self
                .bridge
                .runtime
                .deliver_signal_received(process.pid())
                .map_err(|error| EngineSeamError::Delivery {
                    reason: error.to_string(),
                }),
            other => Err(EngineSeamError::Delivery {
                reason: format!("unsupported child NIF message: {other:?}"),
            }),
        }
    }

    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
        let child = self
            .bridge
            .tokio_handle
            .block_on(start_workflow(
                StartWorkflowContext {
                    store: Arc::clone(&self.bridge.store),
                    visibility_store: Arc::clone(&self.bridge.visibility_store),
                    loaded_workflows: &self.bridge.loaded_workflows,
                    runtime: Arc::clone(&self.bridge.runtime),
                    supervision: Arc::clone(&self.bridge.supervision),
                    registry: Arc::clone(&self.bridge.registry),
                    signal_handoff: Some(Arc::clone(&self.bridge.signal_handoff)),
                },
                &request.workflow_type,
                request.input,
            ))
            .map_err(|error| EngineSeamError::ChildSpawn {
                reason: error.to_string(),
            })?;
        Ok(ChildWorkflowSpawnResult {
            child_workflow_id: child.workflow_id().clone(),
            child_process: WorkflowProcessHandle::new(child.pid()),
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        _parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        _correlation: u64,
    ) -> Result<(), EngineSeamError> {
        self.bridge
            .runtime
            .cancel_pid(child_process.pid())
            .map_err(|error| EngineSeamError::ChildTermination {
                reason: error.to_string(),
            })
    }

    fn terminate_linked_activity(
        &self,
        _parent_workflow_id: &WorkflowId,
        activity_process: crate::Pid,
        _correlation: u64,
    ) -> Result<(), EngineSeamError> {
        self.bridge
            .runtime
            .cancel_pid(activity_process)
            .map_err(|error| EngineSeamError::ChildTermination {
                reason: error.to_string(),
            })
    }

    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
        let _ = entry;
        Err(EngineSeamError::TimerWheel {
            reason: "child NIF engine cannot arm timers".to_owned(),
        })
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &aion_core::TimerId,
    ) -> Result<(), EngineSeamError> {
        let _ = (process, timer_id);
        Err(EngineSeamError::TimerWheel {
            reason: "child NIF engine cannot disarm timers".to_owned(),
        })
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError> {
        if workflow_id != self.parent.workflow_id() {
            return Err(EngineSeamError::Recorder {
                reason: format!("cannot record child event for unrelated workflow {workflow_id}"),
            });
        }
        record_child_event(&self.bridge.tokio_handle, &self.parent, event)
    }
}

fn record_child_event(
    tokio_handle: &Handle,
    parent: &WorkflowHandle,
    event: Event,
) -> Result<(), EngineSeamError> {
    let recorder = parent.recorder();
    tokio_handle
        .block_on(async {
            let mut recorder = recorder.lock().await;
            match event {
                Event::ChildWorkflowStarted {
                    child_workflow_id,
                    workflow_type,
                    input,
                    envelope,
                } => {
                    recorder
                        .record_child_workflow_started(
                            envelope.recorded_at,
                            child_workflow_id,
                            workflow_type,
                            input,
                        )
                        .await
                }
                Event::ChildWorkflowCompleted {
                    child_workflow_id,
                    result,
                    envelope,
                } => {
                    recorder
                        .record_child_workflow_completed(
                            envelope.recorded_at,
                            child_workflow_id,
                            result,
                        )
                        .await
                }
                Event::ChildWorkflowFailed {
                    child_workflow_id,
                    error,
                    envelope,
                } => {
                    recorder
                        .record_child_workflow_failed(
                            envelope.recorded_at,
                            child_workflow_id,
                            error,
                        )
                        .await
                }
                other => Err(DurabilityError::HistoryShape {
                    reason: format!("child NIF cannot record non-child event: {other:?}"),
                }),
            }
        })
        .map_err(|error| EngineSeamError::Recorder {
            reason: error.to_string(),
        })
}
