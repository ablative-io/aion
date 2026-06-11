//! Runtime-local child NIF adapters for AT child services.

use std::sync::Arc;

use aion_core::{Event, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use tokio::runtime::Handle;

use crate::durability::DurabilityError;
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::lifecycle::start::{
    StartWorkflowContext, StartWorkflowOptions, start_workflow_with_options,
};
use crate::loader::LoadedWorkflows;
use crate::registry::{HandleResidency, Registry, WorkflowHandle};
use crate::runtime::nif_child_watch::ChildTerminalWatches;
use crate::runtime::{RuntimeHandle, SignalDeliveryConfig};
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
    search_attribute_schema: Arc<aion_core::SearchAttributeSchema>,
    tokio_handle: Handle,
    /// Armed child-terminal watcher tasks keyed by `(parent pid, child id)`.
    child_terminal_watches: Arc<ChildTerminalWatches>,
    /// Builder-supplied backoff policy for watcher registry-miss windows.
    watch_backoff: SignalDeliveryConfig,
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
    pub(crate) search_attribute_schema: Arc<aion_core::SearchAttributeSchema>,
    pub(crate) tokio_handle: Handle,
    pub(crate) watch_backoff: SignalDeliveryConfig,
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
            search_attribute_schema,
            tokio_handle,
            watch_backoff,
        } = parts;
        Self {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            signal_handoff,
            search_attribute_schema,
            tokio_handle,
            child_terminal_watches: Arc::new(ChildTerminalWatches::default()),
            watch_backoff,
        }
    }

    pub(crate) fn registry(&self) -> &Registry {
        self.registry.as_ref()
    }

    pub(crate) fn registry_arc(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
    }

    pub(crate) fn store(&self) -> Arc<dyn EventStore> {
        Arc::clone(&self.store)
    }

    pub(crate) fn runtime(&self) -> Arc<RuntimeHandle> {
        Arc::clone(&self.runtime)
    }

    pub(crate) fn tokio_handle(&self) -> Handle {
        self.tokio_handle.clone()
    }

    pub(crate) fn child_terminal_watches(&self) -> Arc<ChildTerminalWatches> {
        Arc::clone(&self.child_terminal_watches)
    }

    pub(crate) fn watch_backoff(&self) -> SignalDeliveryConfig {
        self.watch_backoff
    }

    /// Abort every child-terminal watcher armed by an exited parent pid.
    pub(crate) fn abort_child_terminal_watches_for_parent(&self, parent_pid: u64) {
        self.child_terminal_watches.abort_for_parent(parent_pid);
    }

    /// Abort every armed child-terminal watcher (engine shutdown).
    pub(crate) fn abort_all_child_terminal_watches(&self) {
        self.child_terminal_watches.abort_all();
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
            .block_on(async {
                // Children inherit the parent's current search attributes so
                // visibility metadata (such as a server-assigned tenancy
                // attribute) follows the execution tree. The snapshot is taken
                // from the parent's recorded history at spawn time.
                let parent_history = self
                    .bridge
                    .store
                    .read_history(self.parent.workflow_id())
                    .await
                    .map_err(crate::EngineError::from)?;
                let inherited = aion_core::search_attributes_from_events(&parent_history);
                start_workflow_with_options(
                    StartWorkflowContext {
                        store: Arc::clone(&self.bridge.store),
                        visibility_store: Arc::clone(&self.bridge.visibility_store),
                        loaded_workflows: &self.bridge.loaded_workflows,
                        runtime: Arc::clone(&self.bridge.runtime),
                        supervision: Arc::clone(&self.bridge.supervision),
                        registry: Arc::clone(&self.bridge.registry),
                        signal_handoff: Some(Arc::clone(&self.bridge.signal_handoff)),
                        search_attribute_schema: Arc::clone(&self.bridge.search_attribute_schema),
                    },
                    &request.workflow_type,
                    request.input,
                    StartWorkflowOptions {
                        // Record-then-spawn (#56): the parent already recorded
                        // ChildWorkflowStarted under this pre-allocated id, so
                        // the child must start under exactly this identity.
                        workflow_id: Some(request.child_workflow_id),
                        search_attributes: inherited,
                        ..StartWorkflowOptions::default()
                    },
                )
                .await
            })
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
