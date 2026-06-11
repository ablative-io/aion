//! `Engine` start, cancel, result, list, and shutdown support.

use std::collections::HashMap;
use std::sync::Arc;

use aion_core::{
    Event, Payload, RunId, SearchAttributeSchema, SearchAttributeValue, WorkflowError,
    WorkflowFilter, WorkflowId, WorkflowSummary,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::durability::Recorder;
use crate::schedule::ScheduleEvaluator;
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;

use crate::lifecycle::continue_as_new::{self, ContinueAsNewContext, ContinueAsNewRequest};
use crate::lifecycle::start::{self, StartWorkflowContext};
use crate::lifecycle::terminate::{self, TerminateWorkflowContext};
use crate::lifecycle::transition;
use crate::registry::{TerminalOutcome, WorkflowHandle};
use crate::{
    EngineError, LoadedWorkflows, Registry, RuntimeHandle, SupervisionTree,
    signal::SignalResumeHandoff,
};

use super::api_schedule::{
    ScheduleRuntimeDeps, default_schedule_evaluator, schedule_coordinator_workflow_id,
};
use super::delegated::DelegatedSeams;
use super::shutdown_gate::ShutdownGate;

/// Live embedded workflow engine assembled by [`crate::EngineBuilder`].
pub struct Engine {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    pub(super) schedule_recorder: Arc<AsyncMutex<Recorder>>,
    pub(super) schedule_evaluator: Arc<AsyncMutex<ScheduleEvaluator>>,
    pub(super) schedule_coordinator_workflow_id: WorkflowId,
    runtime: Arc<RuntimeHandle>,
    loaded_workflows: LoadedWorkflows,
    registry: Arc<Registry>,
    supervision: Arc<SupervisionTree>,
    delegated: DelegatedSeams,
    signal_handoff: Arc<SignalResumeHandoff>,
    search_attribute_schema: Arc<SearchAttributeSchema>,
    pub(super) shutdown_gate: ShutdownGate,
    visibility_reconciliation_task: Option<JoinHandle<()>>,
}

/// Components required to construct an [`Engine`].
pub(crate) struct EngineComponents {
    pub(crate) store: Arc<dyn EventStore>,
    pub(crate) visibility_store: Arc<dyn VisibilityStore>,
    pub(crate) runtime: Arc<RuntimeHandle>,
    pub(crate) loaded_workflows: LoadedWorkflows,
    pub(crate) registry: Arc<Registry>,
    pub(crate) supervision: Arc<SupervisionTree>,
    pub(crate) delegated: DelegatedSeams,
    pub(crate) signal_handoff: Arc<SignalResumeHandoff>,
    pub(crate) search_attribute_schema: Arc<SearchAttributeSchema>,
    pub(crate) visibility_reconciliation_task: Option<JoinHandle<()>>,
}

impl Engine {
    /// Construct an engine from already-assembled components.
    #[must_use]
    pub(crate) fn new(components: EngineComponents) -> Self {
        let EngineComponents {
            store,
            visibility_store,
            runtime,
            loaded_workflows,
            registry,
            supervision,
            delegated,
            signal_handoff,
            search_attribute_schema,
            visibility_reconciliation_task,
        } = components;
        let schedule_coordinator_workflow_id = schedule_coordinator_workflow_id();
        let schedule_recorder = Arc::new(AsyncMutex::new(Recorder::new(
            schedule_coordinator_workflow_id.clone(),
            Arc::clone(&store),
        )));
        let runtime_arc = runtime;
        let registry_arc = registry;
        let supervision_arc = supervision;
        let schedule_evaluator = Arc::new(AsyncMutex::new(default_schedule_evaluator(
            schedule_coordinator_workflow_id.clone(),
            Arc::clone(&schedule_recorder),
            ScheduleRuntimeDeps {
                store: Arc::clone(&store),
                visibility_store: Arc::clone(&visibility_store),
                runtime: Arc::clone(&runtime_arc),
                loaded_workflows: loaded_workflows.clone(),
                registry: Arc::clone(&registry_arc),
                supervision: Arc::clone(&supervision_arc),
                search_attribute_schema: Arc::clone(&search_attribute_schema),
            },
        )));
        Self {
            store,
            visibility_store,
            schedule_recorder,
            schedule_evaluator,
            schedule_coordinator_workflow_id,
            runtime: runtime_arc,
            loaded_workflows,
            registry: registry_arc,
            supervision: supervision_arc,
            delegated,
            signal_handoff,
            search_attribute_schema,
            shutdown_gate: ShutdownGate::default(),
            visibility_reconciliation_task,
        }
    }

    /// Advance the schedule coordinator's recorder head to match persisted
    /// events so that a rebuilt engine resumes appending at the correct
    /// sequence rather than conflicting at head 0.
    ///
    /// # Errors
    ///
    /// Returns store read errors.
    pub(crate) async fn catchup_schedule_coordinator(&self) -> Result<(), EngineError> {
        let history = self
            .store
            .read_history(&self.schedule_coordinator_workflow_id)
            .await?;
        let head = u64::try_from(history.len()).unwrap_or(u64::MAX);
        if head > 0 {
            let mut recorder = self.schedule_recorder.lock().await;
            *recorder = Recorder::resume_at(
                self.schedule_coordinator_workflow_id.clone(),
                Arc::clone(&self.store),
                head,
            );
        }
        Ok(())
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
    pub fn runtime(&self) -> &RuntimeHandle {
        &self.runtime
    }

    /// Loaded workflow package registry.
    #[must_use]
    pub const fn loaded_workflows(&self) -> &LoadedWorkflows {
        &self.loaded_workflows
    }

    /// Active execution registry.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Supervision tree snapshot/model.
    #[must_use]
    pub fn supervision(&self) -> &SupervisionTree {
        &self.supervision
    }

    /// Delegated signal/query/subscribe seams installed for AT/AD integration.
    #[must_use]
    pub const fn delegated(&self) -> &DelegatedSeams {
        &self.delegated
    }

    /// Shared in-memory handoff for already-recorded non-resident signals.
    #[must_use]
    pub fn signal_handoff(&self) -> Arc<SignalResumeHandoff> {
        Arc::clone(&self.signal_handoff)
    }

    /// Start a loaded workflow type as a new BEAM process.
    ///
    /// `search_attributes` are validated against the engine's configured
    /// [`SearchAttributeSchema`] and recorded atomically with the
    /// `WorkflowStarted` event, so visibility metadata can never be lost to a
    /// crash between start and a later attribute update.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins, and
    /// [`EngineError::Durability`] when a search attribute is unregistered or
    /// mistyped (nothing is appended and no process is spawned). Otherwise
    /// delegates to the start lifecycle transition and returns its typed errors.
    pub async fn start_workflow(
        &self,
        workflow_type: &str,
        input: Payload,
        search_attributes: HashMap<String, SearchAttributeValue>,
    ) -> Result<WorkflowHandle, EngineError> {
        let operation = self.shutdown_gate.begin_start()?;
        let result = start::start_workflow_with_options(
            StartWorkflowContext {
                store: self.store(),
                visibility_store: self.visibility_store(),
                loaded_workflows: &self.loaded_workflows,
                runtime: Arc::clone(&self.runtime),
                supervision: Arc::clone(&self.supervision),
                registry: Arc::clone(&self.registry),
                signal_handoff: Some(self.signal_handoff()),
                search_attribute_schema: Arc::clone(&self.search_attribute_schema),
            },
            workflow_type,
            input,
            start::StartWorkflowOptions {
                search_attributes,
                ..start::StartWorkflowOptions::default()
            },
        )
        .await;
        drop(operation);
        result
    }

    /// Resume a suspended workflow run and flush deferred signals through its mailbox.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
    /// is absent, or registry errors from the residency transition. Deferred
    /// delivery failures are logged and dropped because signals are already durable.
    pub fn resume_workflow(
        &self,
        id: &WorkflowId,
        run: &RunId,
    ) -> Result<WorkflowHandle, EngineError> {
        let handle = transition::resume(self.registry(), id, run)?;
        if let Err(error) = self.signal_handoff.deliver_deferred(self, id) {
            tracing::warn!(
                workflow_id = %id,
                run_id = %run,
                error = %error,
                "failed to flush deferred signals after workflow resume"
            );
        }
        Ok(handle)
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
                supervision: Arc::clone(&self.supervision),
                registry: &self.registry,
                search_attribute_schema: Arc::clone(&self.search_attribute_schema),
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
        let history = self.store.read_history(id).await?;
        if let Some(outcome) = terminal_outcome_from_history(&history) {
            return Ok(outcome_to_result(outcome));
        }

        let handle = match self.registry.get(id, run)? {
            Some(handle) => handle,
            // Registration birth window: the run is durably started but its
            // handle insert has not landed yet (see
            // `Engine::handle_after_birth_window`).
            None => self
                .handle_after_birth_window(id, run, &history)
                .await?
                .ok_or_else(|| workflow_not_found(id, run))?,
        };
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
        if let Some(task) = &self.visibility_reconciliation_task {
            task.abort();
        }
        self.shutdown_gate.close_and_wait()?;
        // Epoch close for engine-side child tasks (F4): the scheduler stops
        // first (so no NIF can arm a new watcher mid-shutdown), then every
        // watcher and spawn-recovery task is aborted AND awaited to
        // quiescence — a task still mid-record after shutdown could
        // double-write a parent history a successor engine over the same
        // store also records into. Arming is additionally gated inside the
        // task registry the moment shutdown begins.
        self.runtime.shutdown()?;
        self.runtime.nif_state().shutdown_child_tasks();
        Ok(())
    }
}

pub(crate) fn terminal_outcome_from_history(events: &[Event]) -> Option<TerminalOutcome> {
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
            | Event::WithTimeoutCompleted { .. }
            | Event::SignalReceived { .. }
            | Event::SignalSent { .. }
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use aion_core::{Event, Payload, SearchAttributeSchema, WorkflowFilter, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore};
    use serde_json::json;

    use super::{DelegatedSeams, Engine, EngineComponents};
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
        Ok(Engine::new(EngineComponents {
            store,
            visibility_store,
            runtime: Arc::new(runtime),
            loaded_workflows: loaded_workflows(workflow_type, deployed_module),
            registry: Arc::new(Registry::default()),
            supervision: Arc::new(SupervisionTree::new()),
            delegated: DelegatedSeams::default(),
            signal_handoff: Arc::new(crate::signal::SignalResumeHandoff::new()),
            search_attribute_schema: Arc::new(SearchAttributeSchema::new()),
            visibility_reconciliation_task: None,
        }))
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
                run_id.clone(),
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
        let handle = engine
            .start_workflow("checkout", payload("input")?, HashMap::new())
            .await?;

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
        let handle = engine
            .start_workflow("checkout", payload("input")?, HashMap::new())
            .await?;
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
        let handle = engine
            .start_workflow("checkout", payload("input")?, HashMap::new())
            .await?;
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
        let completed = engine
            .start_workflow("checkout", payload("input")?, HashMap::new())
            .await?;
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
        let handle = engine
            .start_workflow("checkout", payload("input")?, HashMap::new())
            .await?;
        terminate::complete(
            termination_context(&engine),
            handle.workflow_id(),
            handle.run_id(),
            payload("result")?,
        )
        .await?;

        engine.shutdown()?;
        let result = engine
            .start_workflow("checkout", payload("after-shutdown")?, HashMap::new())
            .await;

        assert!(matches!(result, Err(EngineError::ShuttingDown)));
        Ok(())
    }
}
