//! Runtime-local child NIF adapters for AT child services.

use std::sync::Arc;

use aion_core::{Event, WorkflowId};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use tokio::runtime::Handle;

use crate::EngineError;
use crate::durability::DurabilityError;
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::lifecycle::start::{
    StartWorkflowContext, StartWorkflowOptions, start_workflow_with_options,
};
use crate::loader::WorkflowCatalog;
use crate::registry::{HandleResidency, Registry, WorkflowHandle};
use crate::runtime::nif_child_tasks::ChildTaskRuntime;
use crate::runtime::{RuntimeHandle, SignalDeliveryConfig};
use crate::signal::SignalResumeHandoff;
use crate::supervision::SupervisionTree;

/// Engine-owned context for child workflow NIF calls.
pub(crate) struct ChildNifBridge {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    runtime: Arc<RuntimeHandle>,
    catalog: Arc<WorkflowCatalog>,
    registry: Arc<Registry>,
    supervision: Arc<SupervisionTree>,
    signal_handoff: Arc<SignalResumeHandoff>,
    search_attribute_schema: Arc<aion_core::SearchAttributeSchema>,
    tokio_handle: Handle,
    /// Engine-owned executor and registry for child-terminal watchers and
    /// spawn-recovery tasks; gated and abort-awaited at epoch close (F4).
    child_tasks: Arc<ChildTaskRuntime>,
    /// Builder-supplied backoff policy for watcher registry-miss windows,
    /// transient record retries, and spawn-recovery retries.
    watch_backoff: SignalDeliveryConfig,
}

/// Constructor dependencies for [`ChildNifBridge`].
pub(crate) struct ChildNifBridgeParts {
    pub(crate) store: Arc<dyn EventStore>,
    pub(crate) visibility_store: Arc<dyn VisibilityStore>,
    pub(crate) runtime: Arc<RuntimeHandle>,
    pub(crate) catalog: Arc<WorkflowCatalog>,
    pub(crate) registry: Arc<Registry>,
    pub(crate) supervision: Arc<SupervisionTree>,
    pub(crate) signal_handoff: Arc<SignalResumeHandoff>,
    pub(crate) search_attribute_schema: Arc<aion_core::SearchAttributeSchema>,
    pub(crate) tokio_handle: Handle,
    pub(crate) watch_backoff: SignalDeliveryConfig,
}

impl ChildNifBridge {
    /// Creates a bridge from engine components.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the child-task runtime's worker
    /// thread cannot be started.
    pub(crate) fn new(parts: ChildNifBridgeParts) -> Result<Self, EngineError> {
        let ChildNifBridgeParts {
            store,
            visibility_store,
            runtime,
            catalog,
            registry,
            supervision,
            signal_handoff,
            search_attribute_schema,
            tokio_handle,
            watch_backoff,
        } = parts;
        Ok(Self {
            store,
            visibility_store,
            runtime,
            catalog,
            registry,
            supervision,
            signal_handoff,
            search_attribute_schema,
            tokio_handle,
            child_tasks: Arc::new(ChildTaskRuntime::new()?),
            watch_backoff,
        })
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

    pub(crate) fn child_tasks(&self) -> Arc<ChildTaskRuntime> {
        Arc::clone(&self.child_tasks)
    }

    pub(crate) fn watch_backoff(&self) -> SignalDeliveryConfig {
        self.watch_backoff
    }

    /// Resolves a child entry at the parent's exact package version.
    ///
    /// Same-archive entries share one content hash, so this exact lookup keeps
    /// child starts pinned across a redeploy of either route.
    pub(crate) fn package_version_for_child(
        &self,
        workflow_type: &str,
        parent_version: &aion_package::ContentHash,
    ) -> Result<Option<aion_core::PackageVersion>, EngineError> {
        Ok(self
            .catalog
            .get(workflow_type, parent_version)?
            .map(|workflow| crate::loader::package_version_of(workflow.version())))
    }

    /// Abort every child-terminal watcher armed by an exited parent pid.
    pub(crate) fn abort_child_terminal_watches_for_parent(&self, parent_pid: u64) {
        self.child_tasks.abort_watches_for_parent(parent_pid);
    }

    /// Close the epoch for engine-side child tasks: gate new arms, abort
    /// every task, and await each to quiescence (F4).
    pub(crate) fn shutdown_child_tasks(&self) {
        self.child_tasks.shutdown();
    }

    /// Start the child under its parent-recorded identity, inheriting the
    /// parent's current search attributes and namespace.
    ///
    /// Shared by the synchronous spawn path and the background
    /// spawn-recovery retry (F3): both must start exactly the recorded
    /// identity, and both inherit visibility metadata from the parent's
    /// recorded history at start time.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the parent history cannot be read or the
    /// start path fails.
    pub(super) async fn start_child_under_recorded_id(
        &self,
        parent_workflow_id: &WorkflowId,
        parent_namespace: &str,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<WorkflowHandle, EngineError> {
        // Children inherit the parent's current search attributes so
        // visibility metadata (such as a server-assigned tenancy attribute)
        // follows the execution tree.
        let parent_history = self.store.read_history(parent_workflow_id).await?;
        let inherited = aion_core::search_attributes_from_events(&parent_history);
        let loaded_version =
            crate::loader::parse_package_version(&request.workflow_type, &request.package_version)?;
        start_workflow_with_options(
            StartWorkflowContext {
                store: Arc::clone(&self.store),
                visibility_store: Arc::clone(&self.visibility_store),
                catalog: Arc::clone(&self.catalog),
                runtime: Arc::clone(&self.runtime),
                supervision: Arc::clone(&self.supervision),
                registry: Arc::clone(&self.registry),
                signal_handoff: Some(Arc::clone(&self.signal_handoff)),
                search_attribute_schema: Arc::clone(&self.search_attribute_schema),
                // Epoch-stable: the host runtime's handle, never the
                // child-task runtime polling a spawn-recovery attempt.
                monitor_tokio_handle: self.tokio_handle.clone(),
            },
            &request.workflow_type,
            request.input,
            StartWorkflowOptions {
                // Record-then-spawn (#56): the parent already recorded
                // ChildWorkflowStarted under this pre-allocated id, so the
                // child must start under exactly this identity — and on
                // exactly the recorded package version (D1).
                workflow_id: Some(request.child_workflow_id),
                loaded_version: Some(loaded_version),
                search_attributes: inherited,
                namespace: Some(parent_namespace.to_owned()),
                ..StartWorkflowOptions::default()
            },
        )
        .await
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
        let parent_workflow_id = self.parent.workflow_id().clone();
        let parent_namespace = self.parent.namespace().to_owned();
        let child = self
            .bridge
            .tokio_handle
            .block_on(self.bridge.start_child_under_recorded_id(
                &parent_workflow_id,
                &parent_namespace,
                request,
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
                    package_version,
                    envelope,
                } => {
                    recorder
                        .record_child_workflow_started(
                            envelope.recorded_at,
                            child_workflow_id,
                            workflow_type,
                            input,
                            package_version,
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
