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
    EngineError, Registry, RuntimeHandle, SupervisionTree, WorkflowCatalog,
    signal::SignalResumeHandoff,
};

use super::api_schedule::{
    ScheduleRuntimeDeps, default_schedule_evaluator, schedule_coordinator_workflow_id,
};
use super::delegated::DelegatedSeams;
use super::shutdown_gate::ShutdownGate;
use crate::time::timer_service::live_timers_in_active_segment;

/// Live embedded workflow engine assembled by [`crate::EngineBuilder`].
pub struct Engine {
    store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    pub(super) schedule_recorder: Arc<AsyncMutex<Recorder>>,
    pub(super) schedule_evaluator: Arc<AsyncMutex<ScheduleEvaluator>>,
    pub(super) schedule_coordinator_workflow_id: WorkflowId,
    runtime: Arc<RuntimeHandle>,
    catalog: Arc<WorkflowCatalog>,
    registry: Arc<Registry>,
    supervision: Arc<SupervisionTree>,
    delegated: DelegatedSeams,
    signal_handoff: Arc<SignalResumeHandoff>,
    search_attribute_schema: Arc<SearchAttributeSchema>,
    pub(super) shutdown_gate: ShutdownGate,
    /// Serializes the deploy mutations (load / route / unload) end-to-end
    /// across BOTH the catalog commit and its store persistence write, so
    /// the persisted package set and route pointers can never disagree with
    /// the catalog through interleaving (for example a concurrent re-deploy
    /// re-persisting a version an unload just deleted). Workflow dispatch
    /// never takes this lock.
    pub(super) deploy_mutations: AsyncMutex<()>,
    visibility_reconciliation_task: Option<JoinHandle<()>>,
}

/// Components required to construct an [`Engine`].
pub(crate) struct EngineComponents {
    pub(crate) store: Arc<dyn EventStore>,
    pub(crate) visibility_store: Arc<dyn VisibilityStore>,
    pub(crate) runtime: Arc<RuntimeHandle>,
    pub(crate) catalog: Arc<WorkflowCatalog>,
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
            catalog,
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
                catalog: Arc::clone(&catalog),
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
            catalog,
            registry: registry_arc,
            supervision: supervision_arc,
            delegated,
            signal_handoff,
            search_attribute_schema,
            shutdown_gate: ShutdownGate::default(),
            deploy_mutations: AsyncMutex::new(()),
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

    /// Shared workflow package catalog: loaded versions and routing.
    #[must_use]
    pub fn workflow_catalog(&self) -> &Arc<WorkflowCatalog> {
        &self.catalog
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
        namespace: String,
    ) -> Result<WorkflowHandle, EngineError> {
        self.start_workflow_with_id(
            workflow_type,
            input,
            search_attributes,
            namespace,
            None,
            None,
        )
        .await
    }

    /// Start a loaded workflow type, optionally with a caller-chosen
    /// `workflow_id` and/or R-4 steered-start `routing_key`.
    ///
    /// The request-routing edge supplies `workflow_id` to *place* a new start on
    /// a shard this node owns: the R-1 unsteered-start remint (any locally-owned
    /// shard) or, for a steered start, an id the edge derived on the
    /// `routing_key`'s shard before deciding to run locally. So a `start` whose
    /// id would otherwise hash to a non-owned shard never fences. When
    /// `workflow_id` is `None` this is identical to [`Self::start_workflow`]: the
    /// lifecycle mints a fresh `WorkflowId`, so the default single-node path is
    /// unchanged.
    ///
    /// `routing_key` is the caller-chosen steered-start key recorded on the start
    /// options. Shard derivation for the cluster path is performed at the edge
    /// (which holds the concrete cluster store); here it is threaded through for
    /// API completeness and direct callers.
    ///
    /// # Errors
    ///
    /// Identical to [`Self::start_workflow`]. A supplied `workflow_id` is treated
    /// as a fresh execution; the caller is responsible for choosing an unused id.
    pub async fn start_workflow_with_id(
        &self,
        workflow_type: &str,
        input: Payload,
        search_attributes: HashMap<String, SearchAttributeValue>,
        namespace: String,
        workflow_id: Option<WorkflowId>,
        routing_key: Option<String>,
    ) -> Result<WorkflowHandle, EngineError> {
        let operation = self.shutdown_gate.begin_start()?;
        let result = start::start_workflow_with_options(
            StartWorkflowContext {
                store: self.store(),
                visibility_store: self.visibility_store(),
                catalog: Arc::clone(&self.catalog),
                runtime: Arc::clone(&self.runtime),
                supervision: Arc::clone(&self.supervision),
                registry: Arc::clone(&self.registry),
                signal_handoff: Some(self.signal_handoff()),
                search_attribute_schema: Arc::clone(&self.search_attribute_schema),
                monitor_tokio_handle: tokio::runtime::Handle::current(),
            },
            workflow_type,
            input,
            start::StartWorkflowOptions {
                namespace: Some(namespace),
                search_attributes,
                workflow_id,
                routing_key,
                ..start::StartWorkflowOptions::default()
            },
        )
        .await;
        drop(operation);
        result
    }

    /// Absorb a dead peer's distribution shards into this LIVE engine and resume
    /// their orphaned workflows — the SS-5 failover entry point.
    ///
    /// This is the production failover step a cluster supervisor invokes when it
    /// observes a peer gone (membership loss). It is the post-boot counterpart to
    /// the boot path's `EngineBuilder::owned_shards` election + recovery, run
    /// against an already-running engine:
    ///
    /// 1. **Elect + union-merge.** `acquire_owned_shards` wins the per-shard
    ///    election for each `shards` entry (fencing the dead owner) and
    ///    `become_live` union-merges that shard's committed history locally, so
    ///    every event the dead node had quorum-committed is now present on this
    ///    node. The election is blocking and runs off the tokio runtime inside the
    ///    store seam, honouring haematite's no-blocking-election-in-async
    ///    constraint, so this `async` method may call it directly.
    /// 2. **Widen the scope.** `extend_owned_shards` unions `shards` into this
    ///    node's owned-enumeration set so the adopted workflows, timers, and
    ///    outbox rows become visible to enumeration WITHOUT dropping this node's
    ///    own shards.
    /// 3. **Publish ownership.** `publish_shard_owner` records this node as each
    ///    adopted shard's current owner in the cluster's quorum-replicated
    ///    shard-owner directory (SS-3), so a request reaching a DIFFERENT survivor
    ///    routes to this adopter rather than mis-resolving to the dead declared
    ///    owner. The publish is fenced by the election just won, so only the true
    ///    adopter writes it; a non-distributed store no-ops it.
    /// 4. **Re-resident.** Re-run the idempotent active-workflow recovery and
    ///    timer recovery, which re-spawn every adopted workflow from the
    ///    union-merged history through the same production recovery seam the boot
    ///    path uses, skipping the workflows this node already owns.
    ///
    /// Detection of the peer's death is the CALLER's responsibility (a cluster
    /// supervisor / membership-loss trigger); this method performs the
    /// re-acquisition and resume once that decision is made. It is idempotent:
    /// adopting a shard this node already serves re-acquires (a no-op on the
    /// fence it already holds) and recovers nothing new.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins, store errors
    /// from the election / union-merge ([`EngineError::Durability`]), and any
    /// typed recovery error from re-residenting an adopted workflow.
    pub async fn adopt_shards(&self, shards: &[usize]) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_start()?;
        let result = self.adopt_shards_inner(shards).await;
        drop(operation);
        result
    }

    /// Body of [`Self::adopt_shards`]: acquire+publish each shard as a UNIT under
    /// the double-adoption fence (ADR-021 clean-partial), then widen scope and
    /// recover over EXACTLY the shards that survived BOTH steps.
    ///
    /// ## Ordering invariant (the fix)
    ///
    /// For each shard the publish-fence happens BEFORE the shard contributes to
    /// `extend_owned_shards` AND before it is recovered. The pre-fix order
    /// (extend → publish) let a survivor that won the election but was then
    /// deposed at publish-time still widen its scope and recover the shard, so two
    /// survivors could both execute its workflows. Here, a `NotOwner` from EITHER
    /// `acquire_owned_shard` OR `publish_shard_owner` DROPS that shard: it never
    /// reaches `extend_owned_shards`, is never recovered, and is NEVER a hard
    /// `Durability` error. A deposed survivor therefore leaves ZERO widened
    /// owned-shards scope and recovers nothing.
    async fn adopt_shards_inner(&self, shards: &[usize]) -> Result<(), EngineError> {
        // 1-3. Drive the double-adoption fence in the FIXED order (acquire →
        //      publish per shard as a UNIT, then re-assert ownership and widen the
        //      enumeration scope ONCE) and learn which shards survived it. A shard
        //      deposed at acquire OR publish (or in the residual window) is dropped
        //      cleanly — never extended, never recovered, never a hard error. The
        //      planner GUARANTEES each survivor's publish-fence precedes both the
        //      scope widening and (below) recovery. A single-node store no-ops
        //      every step, so this path stays byte-identical there.
        // The returned survivor set is already reflected in the store's widened
        // owned-shard scope (the planner's single `extend`), which is what recovery
        // enumerates over; the value is bound only to make that contract explicit.
        let _recoverable = super::fence::plan_adopted_shards(
            &super::fence::StoreFenceSeam {
                store: &*self.store,
            },
            shards,
        )?;
        // 4. Re-resident the adopted workflows through the production recovery
        //    seam (idempotent: this node's own workflows are skipped). Recovery
        //    enumerates over the owned scope, which now contains only shards that
        //    survived the fence.
        super::startup::recover_adopted_shards(super::startup::StartupRecoveryContext {
            store: Arc::clone(&self.store),
            visibility_store: Arc::clone(&self.visibility_store),
            runtime: Arc::clone(&self.runtime),
            catalog: Arc::clone(&self.catalog),
            registry: Arc::clone(&self.registry),
            supervision: Arc::clone(&self.supervision),
            recovery: None,
            search_attribute_schema: Arc::clone(&self.search_attribute_schema),
            bootstrap_schedule_coordinator: false,
        })
        .await?;
        super::startup::recover_timers_on_startup(self.runtime.nif_state(), Arc::clone(&self.store))
            .await
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
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins, and
    /// [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
    /// is not live. Other typed errors come from the cancel transition.
    pub async fn cancel(
        &self,
        id: &WorkflowId,
        run: &RunId,
        reason: impl Into<String>,
    ) -> Result<(), EngineError> {
        let operation = self.shutdown_gate.begin_operation()?;
        // Tear down the run's in-flight durable timers BEFORE the cancel
        // transition. Cancellation that leaves a live timer behind orphans it:
        // recovery later tries to fire it against a workflow that no longer
        // exists. See `cancel_inflight_timers` for the ordering constraints.
        self.cancel_inflight_timers(id).await;
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

    /// Cancel the workflow's in-flight durable timers, routed through the
    /// production [`crate::time::TimerService`] so each records a
    /// `TimerCancelled` (and disarms the resident wheel) under the service's
    /// terminal-update guard. Once `TimerCancelled` is in history the timer is
    /// dead everywhere — a later wheel or recovery fire no-ops on the liveness
    /// check — so a cancelled workflow no longer leaves orphaned timers that
    /// brick startup recovery.
    ///
    /// Ordering matters and is the reason this lives in `Engine::cancel` rather
    /// than inside `terminate::cancel`:
    /// * It runs **before** `terminate::cancel`, while the workflow is still in
    ///   the registry (before `terminate::cancel`'s final `registry.remove`), so
    ///   the timer bridge's registry lookup succeeds. `UnknownWorkflow` is raised
    ///   only when the workflow is absent from the registry entirely — a
    ///   suspended (non-resident-but-registered) workflow is fine: its wheel
    ///   disarm is skipped but `TimerCancelled` is still recorded.
    /// * It runs **outside** `terminate::cancel`'s recorder lock —
    ///   `TimerService::cancel` re-acquires that same per-handle lock to record,
    ///   and the tokio mutex is not reentrant.
    ///
    /// Best-effort by design: every failure path here is backstopped by
    /// `recover_due`'s orphaned-timer skip (see [`crate::time`]'s recovery
    /// module), so it is logged but never fails the cancel. The only residual
    /// orphan window — a timer armed in the instant between enumeration and the
    /// process kill — is absorbed by that same recovery skip.
    async fn cancel_inflight_timers(&self, id: &WorkflowId) {
        let timer_service = match crate::runtime::nif_timer_bridge::installed_timer_service(
            self.runtime.nif_state(),
        ) {
            Ok(service) => service,
            Err(error) => {
                tracing::warn!(
                    %error,
                    workflow_id = %id,
                    "timer service unavailable during cancel; any in-flight timers will be skipped by recovery"
                );
                return;
            }
        };
        let history = match self.store.read_history(id).await {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    %error,
                    workflow_id = %id,
                    "could not read history for timer cleanup during cancel; any in-flight timers will be skipped by recovery"
                );
                return;
            }
        };
        for timer_id in live_timers_in_active_segment(&history) {
            if let Err(error) = timer_service.cancel(id.clone(), timer_id.clone()).await {
                tracing::warn!(
                    %error,
                    workflow_id = %id,
                    %timer_id,
                    "failed to cancel in-flight timer during workflow cancel; recovery will skip it if orphaned"
                );
            }
        }
    }

    /// Continue a live workflow run as a new run under the same workflow id.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ShuttingDown`] after shutdown begins, and
    /// [`EngineError::WorkflowNotFound`] when the `(workflow, run)` pair
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
                catalog: Arc::clone(&self.catalog),
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
    // Reset-aware via the shared single-source predicate: the current lease's
    // terminal event, where a reopen (WorkflowReopened) supersedes any earlier
    // terminal.
    match aion_core::current_lease_terminal(events)? {
        Event::WorkflowCompleted { result, .. } => Some(TerminalOutcome::Completed(result.clone())),
        Event::WorkflowFailed { error, .. } => Some(TerminalOutcome::Failed(error.clone())),
        Event::WorkflowCancelled { reason, .. } => Some(TerminalOutcome::Cancelled(reason.clone())),
        Event::WorkflowTimedOut { timeout, .. } => Some(TerminalOutcome::TimedOut(timeout.clone())),
        Event::WorkflowContinuedAsNew {
            input,
            workflow_type,
            parent_run_id,
            ..
        } => Some(TerminalOutcome::ContinuedAsNew {
            input: input.clone(),
            workflow_type: workflow_type.clone(),
            parent_run_id: parent_run_id.clone(),
        }),
        _ => None,
    }
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
    use std::time::Duration;

    use aion_core::{
        Event, EventEnvelope, PackageVersion, Payload, RunId, SearchAttributeSchema, TimerId,
        WorkflowFilter, WorkflowId, WorkflowStatus,
    };
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore, ReadableEventStore};
    use serde_json::json;

    use super::{DelegatedSeams, Engine, EngineComponents, live_timers_in_active_segment};
    use crate::durability::Recorder;
    use crate::lifecycle::terminate::{self, TerminateWorkflowContext};
    use crate::registry::{CompletionNotifier, HandleResidency, WorkflowHandleParts};
    use crate::time::TimerRecovery;
    use crate::{
        EngineError, Registry, RuntimeConfig, RuntimeHandle, SupervisionTree, WorkflowCatalog,
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

    fn workflow_catalog(workflow_type: &str, deployed_module: &str) -> Arc<WorkflowCatalog> {
        let catalog = Arc::new(WorkflowCatalog::new());
        catalog.note_loaded_workflow_for_test(
            workflow_type,
            deployed_module,
            "run",
            ContentHash::from_bytes([5; 32]),
        );
        catalog
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
            catalog: workflow_catalog(workflow_type, deployed_module),
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
                crate::durability::WorkflowStartRecord {
                    workflow_type: workflow_type.to_owned(),
                    input: payload("input")?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        let pid = engine.runtime().spawn_test_process_with_trap_exit(true)?;
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: workflow_type.to_owned(),
            namespace: String::from("default"),
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
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
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

    fn test_envelope(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn started_event(workflow_id: &WorkflowId, seq: u64) -> Event {
        Event::WorkflowStarted {
            envelope: test_envelope(workflow_id, seq),
            workflow_type: String::from("checkout"),
            input: Payload::new(aion_core::ContentType::Json, b"{}".to_vec()),
            run_id: RunId::new_v4(),
            parent_run_id: None,
            package_version: PackageVersion::new("a".repeat(64)),
        }
    }

    fn timer_started_event(workflow_id: &WorkflowId, seq: u64, timer_id: &TimerId) -> Event {
        Event::TimerStarted {
            envelope: test_envelope(workflow_id, seq),
            timer_id: timer_id.clone(),
            fire_at: chrono::DateTime::from_timestamp(1_700_000_500, 0).unwrap_or_default(),
        }
    }

    fn timer_fired_event(workflow_id: &WorkflowId, seq: u64, timer_id: &TimerId) -> Event {
        Event::TimerFired {
            envelope: test_envelope(workflow_id, seq),
            timer_id: timer_id.clone(),
        }
    }

    fn timer_cancelled_event(workflow_id: &WorkflowId, seq: u64, timer_id: &TimerId) -> Event {
        Event::TimerCancelled {
            envelope: test_envelope(workflow_id, seq),
            timer_id: timer_id.clone(),
        }
    }

    #[test]
    fn live_timers_lists_started_and_unterminated() {
        let workflow_id = WorkflowId::new_v4();
        let first = TimerId::anonymous(0);
        let second = TimerId::anonymous(1);
        let history = vec![
            started_event(&workflow_id, 0),
            timer_started_event(&workflow_id, 1, &first),
            timer_started_event(&workflow_id, 2, &second),
        ];
        assert_eq!(
            live_timers_in_active_segment(&history),
            vec![first, second],
            "both started, unterminated timers should be live, in start order"
        );
    }

    #[test]
    fn live_timers_excludes_fired_and_cancelled() {
        let workflow_id = WorkflowId::new_v4();
        let fired = TimerId::anonymous(0);
        let cancelled = TimerId::anonymous(1);
        let live = TimerId::anonymous(2);
        let history = vec![
            started_event(&workflow_id, 0),
            timer_started_event(&workflow_id, 1, &fired),
            timer_started_event(&workflow_id, 2, &cancelled),
            timer_started_event(&workflow_id, 3, &live),
            timer_fired_event(&workflow_id, 4, &fired),
            timer_cancelled_event(&workflow_id, 5, &cancelled),
        ];
        assert_eq!(
            live_timers_in_active_segment(&history),
            vec![live],
            "only the timer with no terminal event remains live"
        );
    }

    #[test]
    fn live_timers_dedups_repeated_start() {
        let workflow_id = WorkflowId::new_v4();
        let timer = TimerId::anonymous(0);
        let history = vec![
            started_event(&workflow_id, 0),
            timer_started_event(&workflow_id, 1, &timer),
            timer_started_event(&workflow_id, 2, &timer),
        ];
        assert_eq!(live_timers_in_active_segment(&history), vec![timer]);
    }

    #[test]
    fn live_timers_scopes_to_active_run_segment() {
        // A timer started in a prior run (before a continue-as-new
        // `WorkflowStarted`) must not be surfaced for the replacement run.
        let workflow_id = WorkflowId::new_v4();
        let prior_run = TimerId::anonymous(0);
        let current_run = TimerId::anonymous(0);
        let history = vec![
            started_event(&workflow_id, 0),
            timer_started_event(&workflow_id, 1, &prior_run),
            started_event(&workflow_id, 2),
            timer_started_event(&workflow_id, 3, &current_run),
        ];
        assert_eq!(
            live_timers_in_active_segment(&history),
            vec![current_run],
            "only timers from the latest WorkflowStarted segment are live"
        );
    }

    #[test]
    fn live_timers_empty_history_is_empty() {
        assert!(live_timers_in_active_segment(&[]).is_empty());
    }

    /// Build an engine whose runtime has the production timer NIF bridge
    /// installed against the given store + registry, so `Engine::cancel`'s timer
    /// cleanup exercises the real `TimerService` path (not a fake). Must be
    /// called from within a tokio runtime (`Handle::current()`).
    fn engine_with_timer_bridge(
        store: Arc<dyn EventStore>,
        registry: Arc<Registry>,
    ) -> Result<Engine, EngineError> {
        let runtime = RuntimeHandle::new(RuntimeConfig::new(Some(1)))?;
        runtime.register_waiting_test_module("checkout_deployed", "run");
        crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
            runtime.nif_state(),
            Arc::clone(&registry),
            Arc::clone(&store),
            tokio::runtime::Handle::current(),
            crate::runtime::SignalDeliveryConfig::default(),
        );
        let visibility_store: Arc<dyn VisibilityStore> = Arc::new(InMemoryStore::default());
        Ok(Engine::new(EngineComponents {
            store,
            visibility_store,
            runtime: Arc::new(runtime),
            catalog: workflow_catalog("checkout", "checkout_deployed"),
            registry,
            supervision: Arc::new(SupervisionTree::new()),
            delegated: DelegatedSeams::default(),
            signal_handoff: Arc::new(crate::signal::SignalResumeHandoff::new()),
            search_attribute_schema: Arc::new(SearchAttributeSchema::new()),
            visibility_reconciliation_task: None,
        }))
    }

    /// Root-cause regression: cancelling a workflow with a live durable timer
    /// must record `TimerCancelled` (before the terminal `WorkflowCancelled`),
    /// so the timer is dead in history and recovery never fires it as an
    /// orphan. Drives the real `Engine::cancel` against a runtime with the
    /// production timer bridge installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_records_timer_cancelled_before_workflow_cancelled()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let engine = engine_with_timer_bridge(Arc::clone(&store), Arc::clone(&registry))?;

        let handle = engine
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
            .await?;

        // Arm a live durable timer for the resident run and record its
        // `TimerStarted`, exactly as the resume-live handoff would in production.
        let timer_id = TimerId::anonymous(0);
        let fire_at = chrono::Utc::now() + chrono::Duration::hours(1);
        handle
            .recorder()
            .lock()
            .await
            .record_timer_started(chrono::Utc::now(), timer_id.clone(), fire_at)
            .await?;
        let timer_service =
            crate::runtime::nif_timer_bridge::installed_timer_service(engine.runtime().nif_state())
                .map_err(|error| format!("timer service unavailable: {error}"))?;
        timer_service
            .schedule(handle.workflow_id().clone(), timer_id.clone(), fire_at)
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
                Event::TimerStarted {
                    timer_id: started, ..
                },
                Event::TimerCancelled {
                    timer_id: cancelled,
                    ..
                },
                Event::WorkflowCancelled { reason, .. },
            ] => {
                assert_eq!(started, &timer_id);
                assert_eq!(cancelled, &timer_id, "the live timer must be cancelled");
                assert_eq!(reason, "caller requested cancellation");
            }
            other => {
                return Err(format!(
                    "expected [started, timer-started, timer-cancelled, cancelled], found {other:?}"
                )
                .into());
            }
        }
        engine.shutdown()?;
        Ok(())
    }

    /// All live timers (not just one) are cancelled, in start order, before the
    /// terminal `WorkflowCancelled`.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_cancels_multiple_live_timers() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let registry = Arc::new(Registry::default());
        let engine = engine_with_timer_bridge(Arc::clone(&store), Arc::clone(&registry))?;
        let handle = engine
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
            .await?;

        let first = TimerId::anonymous(0);
        let second = TimerId::anonymous(1);
        let fire_at = chrono::Utc::now() + chrono::Duration::hours(1);
        {
            let recorder = handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_timer_started(chrono::Utc::now(), first.clone(), fire_at)
                .await?;
            recorder
                .record_timer_started(chrono::Utc::now(), second.clone(), fire_at)
                .await?;
        }

        engine
            .cancel(handle.workflow_id(), handle.run_id(), "stop")
            .await?;

        let history = store.read_history(handle.workflow_id()).await?;
        match history.as_slice() {
            [
                Event::WorkflowStarted { .. },
                Event::TimerStarted {
                    timer_id: started_first,
                    ..
                },
                Event::TimerStarted {
                    timer_id: started_second,
                    ..
                },
                Event::TimerCancelled {
                    timer_id: cancelled_first,
                    ..
                },
                Event::TimerCancelled {
                    timer_id: cancelled_second,
                    ..
                },
                Event::WorkflowCancelled { .. },
            ] => {
                assert_eq!(started_first, &first);
                assert_eq!(started_second, &second);
                assert_eq!(cancelled_first, &first, "first live timer cancelled first");
                assert_eq!(
                    cancelled_second, &second,
                    "second live timer cancelled second"
                );
            }
            other => {
                return Err(format!(
                    "expected two timer-cancels before workflow-cancel, found {other:?}"
                )
                .into());
            }
        }
        engine.shutdown()?;
        Ok(())
    }

    /// End-to-end source-of-bug proof: a cancelled workflow leaves no orphan for
    /// startup recovery. With a past-due durable timer row (the exact shape that
    /// bricked startup before the fix), recovery surfaces no `UnknownWorkflow`
    /// and fires nothing — because cancel recorded `TimerCancelled`, so the
    /// timer is dead in history. Complements the committed `recover_due` defense
    /// test by proving the orphan is gone *at the source*.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_workflow_leaves_no_orphan_for_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let concrete: Arc<InMemoryStore> = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = concrete.clone();
        let registry = Arc::new(Registry::default());
        let engine = engine_with_timer_bridge(Arc::clone(&store), Arc::clone(&registry))?;
        let handle = engine
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
            .await?;
        let workflow_id = handle.workflow_id().clone();

        // A live timer whose durable row is already past-due, inserted directly
        // (no wheel arm, so nothing races the cancel).
        let timer_id = TimerId::anonymous(0);
        let fire_at = chrono::Utc::now() - chrono::Duration::hours(1);
        handle
            .recorder()
            .lock()
            .await
            .record_timer_started(chrono::Utc::now(), timer_id.clone(), fire_at)
            .await?;
        concrete
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;

        let timer_service =
            crate::runtime::nif_timer_bridge::installed_timer_service(engine.runtime().nif_state())
                .map_err(|error| format!("timer service unavailable: {error}"))?;

        engine.cancel(&workflow_id, handle.run_id(), "stop").await?;

        // Cancel removed the workflow from the registry and the durable row is
        // now past-due — exactly the orphan scenario. Recovery must handle it
        // cleanly: the recorded `TimerCancelled` makes `fire_timer` a no-op, so
        // no `TimerFired` and (critically) no `UnknownWorkflow`.
        let readable: Arc<dyn ReadableEventStore> = concrete.clone();
        TimerRecovery::new(readable, timer_service, Duration::ZERO)
            .recover_on_startup(chrono::Utc::now())
            .await?;

        let history = concrete.read_history(&workflow_id).await?;
        assert!(
            !history
                .iter()
                .any(|event| matches!(event, Event::TimerFired { .. })),
            "no timer should fire for a cancelled workflow during recovery"
        );
        assert!(
            history
                .iter()
                .any(|event| matches!(event, Event::TimerCancelled { .. })),
            "cancel must have recorded TimerCancelled at the source"
        );
        engine.shutdown()?;
        Ok(())
    }

    #[tokio::test]
    async fn result_returns_completed_payload() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
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
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
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
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
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
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
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
            .start_workflow(
                "checkout",
                payload("after-shutdown")?,
                HashMap::new(),
                String::from("default"),
            )
            .await;

        assert!(matches!(result, Err(EngineError::ShuttingDown)));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
            .await?;
        terminate::complete(
            termination_context(&engine),
            handle.workflow_id(),
            handle.run_id(),
            payload("result")?,
        )
        .await?;

        engine.shutdown()?;
        let second = engine.shutdown();

        assert!(
            second.is_ok(),
            "double shutdown should succeed; got {second:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_rejects_schedule_creation() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine =
            engine_with_loaded_workflow(Arc::clone(&store), "checkout", "checkout_deployed")?;
        let handle = engine
            .start_workflow(
                "checkout",
                payload("input")?,
                HashMap::new(),
                String::from("default"),
            )
            .await?;
        terminate::complete(
            termination_context(&engine),
            handle.workflow_id(),
            handle.run_id(),
            payload("result")?,
        )
        .await?;
        engine.shutdown()?;

        let config = aion_core::ScheduleConfig {
            trigger: aion_core::TriggerSpec::Interval {
                period: Duration::from_secs(60),
            },
            overlap_policy: aion_core::OverlapPolicy::Skip,
            catch_up_policy: aion_core::CatchUpPolicy::Skip,
            workflow_type: String::from("checkout"),
            input: payload("scheduled")?,
            search_attributes: HashMap::new(),
        };
        let result = engine.create_schedule(config).await;

        assert!(
            matches!(result, Err(EngineError::ShuttingDown)),
            "create_schedule after shutdown should return ShuttingDown; got {result:?}"
        );
        Ok(())
    }
}
