//! `Engine` start, cancel, result, list, and shutdown support.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use aion_core::{
    Event, Payload, RunId, WorkflowError, WorkflowFilter, WorkflowId, WorkflowSummary,
};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;

use crate::lifecycle::continue_as_new::{self, ContinueAsNewContext, ContinueAsNewRequest};
use crate::lifecycle::start::{self, StartWorkflowContext};
use crate::lifecycle::terminate::{self, TerminateWorkflowContext};
use crate::registry::{TerminalOutcome, WorkflowHandle};
use crate::{EngineError, LoadedWorkflows, Registry, RuntimeHandle, SupervisionTree};

use super::delegated::DelegatedSeams;

/// Live embedded workflow engine assembled by [`crate::EngineBuilder`].
pub struct Engine {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    runtime: RuntimeHandle,
    loaded_workflows: LoadedWorkflows,
    registry: Registry,
    supervision: SupervisionTree,
    delegated: DelegatedSeams,
    shutdown_gate: ShutdownGate,
}

impl Engine {
    /// Construct an engine from already-assembled components.
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn EventStore>,
        visibility_store: Arc<dyn VisibilityStore>,
        runtime: RuntimeHandle,
        loaded_workflows: LoadedWorkflows,
        registry: Registry,
        supervision: SupervisionTree,
        delegated: DelegatedSeams,
    ) -> Self {
        Self {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            delegated,
            shutdown_gate: ShutdownGate::default(),
        }
    }

    /// Event store used by lifecycle and delegated AD/AT operations.
    #[must_use]
    pub fn store(&self) -> Arc<dyn EventStore> {
        Arc::clone(&self.store)
    }

    /// Visibility store used for workflow summary projections.
    #[must_use]
    pub fn visibility_store(&self) -> Arc<dyn VisibilityStore> {
        Arc::clone(&self.visibility_store)
    }

    /// Runtime boundary assembled for this engine.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeHandle {
        &self.runtime
    }

    /// Loaded workflow package registry.
    #[must_use]
    pub const fn loaded_workflows(&self) -> &LoadedWorkflows {
        &self.loaded_workflows
    }

    /// Active execution registry.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Supervision tree snapshot/model.
    #[must_use]
    pub const fn supervision(&self) -> &SupervisionTree {
        &self.supervision
    }

    /// Delegated signal/query/subscribe seams installed for AT/AD integration.
    #[must_use]
    pub const fn delegated(&self) -> &DelegatedSeams {
        &self.delegated
    }

    /// Start a loaded workflow type as a new BEAM process.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins. Otherwise
    /// delegates to the start lifecycle transition and returns its typed errors.
    pub async fn start_workflow(
        &self,
        workflow_type: &str,
        input: Payload,
    ) -> Result<WorkflowHandle, EngineError> {
        let operation = self.shutdown_gate.begin_start()?;
        let result = start::start_workflow(
            StartWorkflowContext {
                store: self.store(),
                visibility_store: self.visibility_store(),
                loaded_workflows: &self.loaded_workflows,
                runtime: &self.runtime,
                supervision: &self.supervision,
                registry: &self.registry,
            },
            workflow_type,
            input,
        )
        .await;
        drop(operation);
        result
    }

    /// Cancel a live workflow run by killing its runtime process.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
    /// is not live. Other typed errors come from the cancel transition.
    pub async fn cancel(
        &self,
        id: &WorkflowId,
        run: &RunId,
        reason: impl Into<String>,
    ) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = terminate::cancel(
            TerminateWorkflowContext {
                runtime: &self.runtime,
                store: self.store(),
                visibility_store: self.visibility_store(),
                registry: &self.registry,
            },
            id,
            run,
            reason,
        )
        .await;
        drop(operation);
        result
    }

    /// Continue a live workflow run as a new run under the same workflow id.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
    /// is not live. Other typed errors come from the continue-as-new transition.
    pub async fn continue_as_new(
        &self,
        id: &WorkflowId,
        run: &RunId,
        input: Payload,
        workflow_type: Option<String>,
    ) -> Result<WorkflowHandle, EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        let result = continue_as_new::continue_as_new(
            ContinueAsNewContext {
                store: self.store(),
                visibility_store: Arc::clone(&self.visibility_store),
                loaded_workflows: &self.loaded_workflows,
                runtime: &self.runtime,
                supervision: &self.supervision,
                registry: &self.registry,
            },
            id,
            run,
            ContinueAsNewRequest {
                input,
                workflow_type,
            },
        )
        .await;
        drop(operation);
        result
    }

    /// Await a workflow run's terminal result.
    ///
    /// Already-terminal histories return immediately. Live workflows await their
    /// completion notifier. Unknown workflow/run pairs return not found.
    ///
    /// # Errors
    ///
    /// Returns store, registry, or runtime channel errors as typed [`EngineError`]
    /// variants, or [`EngineError::WorkflowNotFound`] when no live handle or
    /// terminal history exists for the requested pair.
    pub async fn result(
        &self,
        id: &WorkflowId,
        run: &RunId,
    ) -> Result<Result<Payload, WorkflowError>, EngineError> {
        if let Some(outcome) = terminal_outcome_from_history(&self.store.read_history(id).await?) {
            return Ok(outcome_to_result(outcome));
        }

        let handle = self
            .registry
            .get(id, run)?
            .ok_or_else(|| workflow_not_found(id, run))?;
        let runtime_outcome = self.runtime.workflow_outcome(handle.pid())?;
        match runtime_outcome {
            Ok(payload) => {
                terminate::complete(
                    TerminateWorkflowContext {
                        runtime: &self.runtime,
                        store: self.store(),
                        visibility_store: self.visibility_store(),
                        registry: &self.registry,
                    },
                    id,
                    run,
                    payload,
                )
                .await?;
            }
            Err(error) => {
                terminate::fail(
                    TerminateWorkflowContext {
                        runtime: &self.runtime,
                        store: self.store(),
                        visibility_store: self.visibility_store(),
                        registry: &self.registry,
                    },
                    id,
                    run,
                    error,
                )
                .await?;
            }
        }
        if let Some(outcome) = terminal_outcome_from_history(&self.store.read_history(id).await?) {
            return Ok(outcome_to_result(outcome));
        }

        let handle = self
            .registry
            .get(id, run)?
            .ok_or_else(|| workflow_not_found(id, run))?;
        let mut receiver = handle.completion().subscribe();
        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return Ok(outcome_to_result(outcome));
            }
            if receiver.changed().await.is_err() {
                if let Some(outcome) =
                    terminal_outcome_from_history(&self.store.read_history(id).await?)
                {
                    return Ok(outcome_to_result(outcome));
                }
                return Err(EngineError::Runtime {
                    reason: format!(
                        "completion channel closed before workflow `{id}/{run}` finished"
                    ),
                });
            }
        }
    }

    /// List live and terminal workflow summaries matching `filter`.
    ///
    /// Store projections are authoritative; live registry entries are projected
    /// from durable history before being merged and deduplicated.
    ///
    /// # Errors
    ///
    /// Returns typed store or registry errors when visibility data cannot be read.
    pub async fn list_workflows(
        &self,
        filter: WorkflowFilter,
    ) -> Result<Vec<WorkflowSummary>, EngineError> {
        let mut summaries = self
            .store
            .query(&filter)
            .await?
            .into_iter()
            .map(|summary| (summary.workflow_id.clone(), summary))
            .collect::<HashMap<_, _>>();

        for handle in self.registry.list()? {
            let history = self.store.read_history(handle.workflow_id()).await?;
            self.registry
                .reconcile(handle.workflow_id(), handle.run_id(), &history)?;
            if let Some(summary) = WorkflowSummary::from_history(&history) {
                if filter.matches(&summary) {
                    summaries.insert(summary.workflow_id.clone(), summary);
                }
            }
        }

        let mut summaries = summaries.into_values().collect::<Vec<_>>();
        summaries.sort_by(|left, right| {
            left.started_at.cmp(&right.started_at).then_with(|| {
                left.workflow_id
                    .to_string()
                    .cmp(&right.workflow_id.to_string())
            })
        });
        Ok(summaries)
    }

    /// Gracefully stop accepting new starts and shut down the embedded runtime.
    ///
    /// # Errors
    ///
    /// Returns registry poison or runtime shutdown failures as typed errors.
    pub fn shutdown(&self) -> Result<(), EngineError> {
        self.shutdown_gate.close_and_wait()?;
        self.runtime.shutdown()
    }
}

#[derive(Clone, Default)]
struct ShutdownGate {
    inner: Arc<ShutdownGateInner>,
}

#[derive(Default)]
struct ShutdownGateInner {
    state: Mutex<ShutdownState>,
    idle: Condvar,
}

#[derive(Default)]
struct ShutdownState {
    shutting_down: bool,
    active_operations: usize,
}

impl ShutdownGate {
    fn begin_start(&self) -> Result<LifecycleOperation, EngineError> {
        let mut state = self.state()?;
        if state.shutting_down {
            return Err(EngineError::ShuttingDown);
        }
        state.active_operations += 1;
        Ok(LifecycleOperation {
            inner: Arc::clone(&self.inner),
        })
    }

    fn begin_operation(&self) -> Result<LifecycleOperation, EngineError> {
        let mut state = self.state()?;
        state.active_operations += 1;
        Ok(LifecycleOperation {
            inner: Arc::clone(&self.inner),
        })
    }

    fn close_and_wait(&self) -> Result<(), EngineError> {
        let mut state = self.state()?;
        state.shutting_down = true;
        while state.active_operations > 0 {
            state = self
                .inner
                .idle
                .wait(state)
                .map_err(|_| EngineError::RegistryPoisoned)?;
        }
        Ok(())
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, ShutdownState>, EngineError> {
        self.inner
            .state
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)
    }
}

struct LifecycleOperation {
    inner: Arc<ShutdownGateInner>,
}

impl Drop for LifecycleOperation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.active_operations = state.active_operations.saturating_sub(1);
            if state.active_operations == 0 {
                self.inner.idle.notify_all();
            }
        }
    }
}

fn terminal_outcome_from_history(events: &[Event]) -> Option<TerminalOutcome> {
    for event in events.iter().rev() {
        match event {
            Event::WorkflowStarted { .. } => return None,
            Event::WorkflowCompleted { result, .. } => {
                return Some(TerminalOutcome::Completed(result.clone()));
            }
            Event::WorkflowFailed { error, .. } => {
                return Some(TerminalOutcome::Failed(error.clone()));
            }
            Event::WorkflowCancelled { reason, .. } => {
                return Some(TerminalOutcome::Cancelled(reason.clone()));
            }
            Event::WorkflowTimedOut { timeout, .. } => {
                return Some(TerminalOutcome::TimedOut(timeout.clone()));
            }
            Event::WorkflowContinuedAsNew {
                input,
                workflow_type,
                parent_run_id,
                ..
            } => {
                return Some(TerminalOutcome::ContinuedAsNew {
                    input: input.clone(),
                    workflow_type: workflow_type.clone(),
                    parent_run_id: parent_run_id.clone(),
                });
            }
            Event::SearchAttributesUpdated { .. }
            | Event::ActivityScheduled { .. }
            | Event::ActivityStarted { .. }
            | Event::ActivityCompleted { .. }
            | Event::ActivityFailed { .. }
            | Event::ActivityCancelled { .. }
            | Event::TimerStarted { .. }
            | Event::TimerFired { .. }
            | Event::TimerCancelled { .. }
            | Event::SignalReceived { .. }
            | Event::ChildWorkflowStarted { .. }
            | Event::ChildWorkflowCompleted { .. }
            | Event::ChildWorkflowFailed { .. }
            | Event::ChildWorkflowCancelled { .. }
            | Event::ScheduleCreated { .. }
            | Event::ScheduleUpdated { .. }
            | Event::SchedulePaused { .. }
            | Event::ScheduleResumed { .. }
            | Event::ScheduleDeleted { .. }
            | Event::ScheduleTriggered { .. } => {}
        }
    }
    None
}

fn outcome_to_result(outcome: TerminalOutcome) -> Result<Payload, WorkflowError> {
    match outcome {
        TerminalOutcome::Completed(payload) => Ok(payload),
        TerminalOutcome::Failed(error) => Err(error),
        TerminalOutcome::Cancelled(reason) => Err(WorkflowError {
            message: format!("workflow cancelled: {reason}"),
            details: None,
        }),
        TerminalOutcome::TimedOut(timeout) => Err(WorkflowError {
            message: format!("workflow timed out: {timeout}"),
            details: None,
        }),
        TerminalOutcome::ContinuedAsNew { parent_run_id, .. } => Err(WorkflowError {
            message: format!("workflow continued as new from run {parent_run_id}"),
            details: None,
        }),
    }
}

pub(crate) fn workflow_not_found(id: &WorkflowId, run: &RunId) -> EngineError {
    EngineError::WorkflowNotFound {
        workflow_type: format!("{id}/{run}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, WorkflowFilter, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{DelegatedSeams, Engine};
    use crate::durability::Recorder;
    use crate::lifecycle::terminate::{self, TerminateWorkflowContext};
    use crate::registry::{CompletionNotifier, HandleResidency, WorkflowHandleParts};
    use crate::{
        EngineError, LoadedWorkflows, Registry, RuntimeConfig, RuntimeHandle, SupervisionTree,
        WorkflowHandle,
    };

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn workflow_error(message: &str) -> aion_core::WorkflowError {
        aion_core::WorkflowError {
            message: message.to_owned(),
            details: None,
        }
    }

    fn loaded_workflows(workflow_type: &str, deployed_module: &str) -> LoadedWorkflows {
        let mut loaded = LoadedWorkflows::new();
        loaded.note_loaded_workflow_for_test(
            workflow_type,
            deployed_module,
            "run",
            ContentHash::from_bytes([5; 32]),
        );
        loaded
    }

    fn engine_with_loaded_workflow(
        store: Arc<dyn EventStore>,
        workflow_type: &str,
        deployed_module: &str,
    ) -> Result<Engine, EngineError> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        runtime.register_waiting_test_module(deployed_module, "run");
        let visibility_store: Arc<dyn VisibilityStore> = Arc::new(InMemoryStore::default());
        Ok(Engine::new(
            store,
            visibility_store,
            runtime,
            loaded_workflows(workflow_type, deployed_module),
            Registry::default(),
            SupervisionTree::new(),
            DelegatedSeams::default(),
        ))
    }

    fn termination_context(engine: &Engine) -> TerminateWorkflowContext<'_> {
        TerminateWorkflowContext {
            runtime: engine.runtime(),
            store: engine.store(),
            visibility_store: engine.visibility_store(),
            registry: engine.registry(),
        }
    }

    async fn insert_active_handle(
        engine: &Engine,
        store: Arc<dyn EventStore>,
        workflow_type: &str,
    ) -> Result<WorkflowHandle, Box<dyn std::error::Error>> {
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();
        let mut recorder = Recorder::new(workflow_id.clone(), store);
        recorder
            .record_workflow_started(
                chrono::Utc::now(),
                workflow_type.to_owned(),
                payload("input")?,
            )
            .await?;
        let pid = engine.runtime().spawn_test_process_with_trap_exit(true)?;
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: workflow_type.to_owned(),
            loaded_version: ContentHash::from_bytes([9; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        engine
            .registry()
            .insert((workflow_id, run_id), handle.clone())?;
        Ok(handle)
    }

    #[tokio::test]
    async fn start_then_cancel_records_started_then_cancelled()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine.start_workflow("checkout", payload("input")?).await?;

        engine
            .cancel(
                handle.workflow_id(),
                handle.run_id(),
                "caller requested cancellation",
            )
            .await?;

        let history = store.read_history(handle.workflow_id()).await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::WorkflowCancelled { reason, .. },
            ] => {
                assert_eq!(reason, "caller requested cancellation");
            }
            other => return Err(format!("expected started then cancelled, found {other:?}").into()),
        }
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn result_returns_completed_payload() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine.start_workflow("checkout", payload("input")?).await?;
        let result_payload = payload("result")?;

        terminate::complete(
            termination_context(&engine),
            handle.workflow_id(),
            handle.run_id(),
            result_payload.clone(),
        )
        .await?;

        assert_eq!(
            engine.result(handle.workflow_id(), handle.run_id()).await?,
            Ok(result_payload)
        );
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn result_returns_failed_workflow_error() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine.start_workflow("checkout", payload("input")?).await?;
        let error = workflow_error("workflow failed");

        terminate::fail(
            termination_context(&engine),
            handle.workflow_id(),
            handle.run_id(),
            error.clone(),
        )
        .await?;

        assert_eq!(
            engine.result(handle.workflow_id(), handle.run_id()).await?,
            Err(error)
        );
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn result_unknown_workflow_returns_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = engine_with_loaded_workflow(store, "checkout", "checkout_deployed")?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();

        let result = engine.result(&workflow_id, &run_id).await;

        assert!(matches!(result, Err(EngineError::WorkflowNotFound { .. })));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn continue_as_new_unknown_workflow_returns_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = engine_with_loaded_workflow(store, "checkout", "checkout_deployed")?;
        let workflow_id = aion_core::WorkflowId::new_v4();
        let run_id = aion_core::RunId::new_v4();

        let result = engine
            .continue_as_new(&workflow_id, &run_id, payload("next")?, None)
            .await;

        assert!(matches!(result, Err(EngineError::WorkflowNotFound { .. })));
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn list_workflows_merges_live_and_terminal_without_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let running = insert_active_handle(&engine, Arc::clone(&store), "checkout").await?;
        let completed = engine.start_workflow("checkout", payload("input")?).await?;
        terminate::complete(
            termination_context(&engine),
            completed.workflow_id(),
            completed.run_id(),
            payload("result")?,
        )
        .await?;

        let summaries = engine.list_workflows(WorkflowFilter::default()).await?;
        assert_eq!(summaries.len(), 2);
        assert!(summaries.iter().any(|summary| {
            &summary.workflow_id == running.workflow_id()
                && summary.status == WorkflowStatus::Running
        }));
        assert!(summaries.iter().any(|summary| {
            &summary.workflow_id == completed.workflow_id()
                && summary.status == WorkflowStatus::Completed
        }));

        let completed_only = engine
            .list_workflows(WorkflowFilter {
                status: Some(WorkflowStatus::Completed),
                ..WorkflowFilter::default()
            })
            .await?;
        assert_eq!(completed_only.len(), 1);
        assert_eq!(&completed_only[0].workflow_id, completed.workflow_id());
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_rejects_subsequent_starts() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine.start_workflow("checkout", payload("input")?).await?;
        terminate::complete(
            termination_context(&engine),
            handle.workflow_id(),
            handle.run_id(),
            payload("result")?,
        )
        .await?;

        engine.shutdown()?;
        let result = engine
            .start_workflow("checkout", payload("after-shutdown")?)
            .await;

        assert!(matches!(result, Err(EngineError::ShuttingDown)));
        Ok(())
    }
}
