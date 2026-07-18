//! Durable operator pause/resume (#204).
//!
//! Pause holds NEW activity dispatch for a live, non-terminal run while keeping
//! every durable record path alive (timer fires, signal receipts, drained
//! completions). It is the Option-B "dispatch-hold" ruling of
//! `docs/PAUSE-RESUME-DESIGN.md` §4: the resident process is NOT torn down; the
//! hold lives at outbox claim time (see [`crate::lifecycle::pause::PausedRuns`]
//! threaded into the outbox dispatcher), so a held row sits `Pending` — never
//! `Claimed` — for the whole paused window, and release is purely resume plus the
//! existing sweep.
//!
//! # Single-writer discipline
//!
//! Pause and resume of a RESIDENT run append through the live handle's own
//! recorder (the cancel precedent, `terminate.rs`), never a side-constructed
//! `Recorder::resume_at`: a resident run's handle already owns the single-writer
//! recorder, and a second writer at the same head would desync the live
//! recorder's sequence tracker and hard-fail its next append (the exact hazard
//! the surviving refutation identified). Only the crashed-while-paused resume
//! path — where the run is NOT resident — builds one continuous
//! `Recorder::resume_at` and hands it to the reopen respawn machinery.
//!
//! # Naming
//!
//! Named `pause` (not `suspend`/`resume`) to avoid colliding with
//! `transition.rs`'s non-durable residency flip and the signal-router `resume`
//! vocabulary, exactly as reopen avoided `resume`.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use aion_core::{
    Event, RunId, SearchAttributeSchema, WorkflowId, WorkflowStatus, run_segment,
    status_from_events,
};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

use crate::EngineError;
use crate::durability::Recorder;
use crate::loader::WorkflowCatalog;
use crate::registry::{Registry, WorkflowHandle};
use crate::runtime::RuntimeHandle;
use crate::supervision::SupervisionTree;

use super::reopen::{self, ReopenWorkflowContext};

/// Shared, in-memory snapshot of the workflow ids whose outbox dispatch is held
/// because the run is durably `Paused` (#204).
///
/// Cloneable handle over a shared set: the engine's pause/resume ops mutate it
/// and the outbox dispatcher reads a snapshot at claim time to exclude held rows.
/// It is rebuilt from durable state ([`EventStore::list_paused`]) at startup and
/// at shard adoption, so a run paused before a `kill -9` keeps its rows held
/// after restart — the in-memory set is a cache of the durable projection, never
/// the source of truth.
#[derive(Clone, Debug, Default)]
pub struct PausedRuns {
    inner: Arc<RwLock<HashSet<WorkflowId>>>,
}

impl PausedRuns {
    /// Marks `workflow_id` held (paused).
    pub fn insert(&self, workflow_id: WorkflowId) {
        if let Ok(mut set) = self.inner.write() {
            set.insert(workflow_id);
        }
    }

    /// Releases `workflow_id` from the hold (resumed or cancelled).
    pub fn remove(&self, workflow_id: &WorkflowId) {
        if let Ok(mut set) = self.inner.write() {
            set.remove(workflow_id);
        }
    }

    /// Returns a point-in-time snapshot of the held set for a claim exclusion.
    #[must_use]
    pub fn snapshot(&self) -> HashSet<WorkflowId> {
        self.inner.read().map(|set| set.clone()).unwrap_or_default()
    }

    /// Replaces the entire held set from the durable projection — the
    /// startup/adoption rebuild from [`EventStore::list_paused`].
    pub fn replace_all(&self, workflow_ids: impl IntoIterator<Item = WorkflowId>) {
        if let Ok(mut set) = self.inner.write() {
            *set = workflow_ids.into_iter().collect();
        }
    }

    /// Merges additional paused ids into the held set (shard adoption widens the
    /// set without dropping already-held runs from other shards).
    pub fn extend(&self, workflow_ids: impl IntoIterator<Item = WorkflowId>) {
        if let Ok(mut set) = self.inner.write() {
            set.extend(workflow_ids);
        }
    }
}

/// Dependencies required to pause or resume a workflow run.
pub struct PauseWorkflowContext<'a> {
    /// Durable event store used to read history and construct recorders.
    pub store: Arc<dyn EventStore>,
    /// Visibility store the recorder projects the run into.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Workflow catalog resolving the pinned package version to respawn.
    pub catalog: Arc<WorkflowCatalog>,
    /// Runtime boundary used to respawn a crashed-while-paused run on resume.
    pub runtime: &'a Arc<RuntimeHandle>,
    /// Structural supervision tree recording per-type supervisor placement.
    pub supervision: Arc<SupervisionTree>,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: &'a Arc<Registry>,
    /// Schema shared with startup recovery's resident registration.
    pub search_attribute_schema: Arc<SearchAttributeSchema>,
    /// The shared dispatch-hold set to update.
    pub paused_runs: PausedRuns,
}

impl<'a> PauseWorkflowContext<'a> {
    fn reopen_context(&self) -> ReopenWorkflowContext<'a> {
        ReopenWorkflowContext {
            store: Arc::clone(&self.store),
            visibility_store: Arc::clone(&self.visibility_store),
            catalog: Arc::clone(&self.catalog),
            runtime: self.runtime,
            supervision: Arc::clone(&self.supervision),
            registry: self.registry,
            search_attribute_schema: Arc::clone(&self.search_attribute_schema),
        }
    }
}

fn status_name(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Running => "Running",
        WorkflowStatus::Completed => "Completed",
        WorkflowStatus::Failed => "Failed",
        WorkflowStatus::Cancelled => "Cancelled",
        WorkflowStatus::TimedOut => "TimedOut",
        WorkflowStatus::ContinuedAsNew => "ContinuedAsNew",
        WorkflowStatus::Paused => "Paused",
    }
}

/// Pauses a live, `Running` run: appends `WorkflowPaused` through the resident
/// handle's own recorder (single-writer, cancel precedent) and inserts the run
/// into the shared dispatch-hold set. The resident process is left alive.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when no history / no resident handle
/// exists for `(id, run)`, and [`EngineError::InvalidState`] — naming the actual
/// status — when the run is not `Running` (e.g. Completed, already Paused). A
/// rejection appends nothing to history.
pub async fn pause(
    context: &PauseWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    reason: Option<String>,
    operator: Option<String>,
) -> Result<WorkflowHandle, EngineError> {
    // Validate against HISTORY first so the rejection names the run's true state
    // (mirroring reopen): only a Running run can be paused.
    let history = context.store.read_history(id).await?;
    if history.is_empty() {
        return Err(crate::engine::api::workflow_not_found(id, run));
    }
    let segment = run_segment(&history, run);
    if segment.is_empty() {
        return Err(crate::engine::api::workflow_not_found(id, run));
    }
    let status = status_from_events(segment);
    if status != WorkflowStatus::Running {
        return Err(EngineError::InvalidState {
            reason: format!(
                "workflow {id} run {run} is {}, not Running; only a Running run can be paused",
                status_name(status)
            ),
        });
    }

    // A Running run is resident; append through its live recorder.
    let handle = context
        .registry
        .get(id, run)?
        .ok_or_else(|| crate::engine::api::workflow_not_found(id, run))?;
    {
        let recorder = handle.recorder();
        let mut recorder = recorder.lock().await;
        // Re-validate under the recorder lock: the exit monitor / a racing
        // transition records through this same recorder, so only the lock makes
        // the check-then-append atomic.
        let history = context.store.read_history(id).await?;
        let segment = run_segment(&history, run);
        let status = status_from_events(segment);
        if status != WorkflowStatus::Running {
            return Err(EngineError::InvalidState {
                reason: format!(
                    "workflow {id} run {run} is {}, not Running; only a Running run can be paused",
                    status_name(status)
                ),
            });
        }
        recorder
            .record_workflow_paused(Utc::now(), run.clone(), reason, operator)
            .await?;
    }
    context.paused_runs.insert(id.clone());
    Ok(handle)
}

/// Resumes a `Paused` run: appends `WorkflowResumed`, releases the dispatch hold,
/// and — when the run crashed while paused and is no longer resident — respawns
/// it through the reopen recovery machinery, re-arming unfired timers.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when no history exists for
/// `(id, run)`, and [`EngineError::InvalidState`] — naming the actual status —
/// when the run is not `Paused`. A rejection appends nothing to history.
pub async fn resume(
    context: &PauseWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    operator: Option<String>,
) -> Result<WorkflowHandle, EngineError> {
    let history = context.store.read_history(id).await?;
    if history.is_empty() {
        return Err(crate::engine::api::workflow_not_found(id, run));
    }
    let segment = run_segment(&history, run);
    if segment.is_empty() {
        return Err(crate::engine::api::workflow_not_found(id, run));
    }
    let status = status_from_events(segment);
    if status != WorkflowStatus::Paused {
        return Err(EngineError::InvalidState {
            reason: format!(
                "workflow {id} run {run} is {}, not Paused; only a Paused run can be resumed",
                status_name(status)
            ),
        });
    }

    // Resident (paused-but-alive): append through the live recorder and release
    // the hold — the ordinary sweep then claims the held rows.
    if let Some(handle) = context.registry.get(id, run)? {
        {
            let recorder = handle.recorder();
            let mut recorder = recorder.lock().await;
            let history = context.store.read_history(id).await?;
            let segment = run_segment(&history, run);
            let status = status_from_events(segment);
            if status != WorkflowStatus::Paused {
                return Err(EngineError::InvalidState {
                    reason: format!(
                        "workflow {id} run {run} is {}, not Paused; only a Paused run can be resumed",
                        status_name(status)
                    ),
                });
            }
            recorder
                .record_workflow_resumed(Utc::now(), run.clone(), operator)
                .await?;
        }
        context.paused_runs.remove(id);
        return Ok(handle);
    }

    // Not resident (crashed while paused): respawn via the reopen machinery,
    // replicating reopen's reconcile-after-append ordering (re-read the history
    // so it INCLUDES WorkflowResumed before registering) and re-arming timers.
    let rearm = reopen::rearmable_timers(segment);
    let history_head = history.last().map(Event::seq).unwrap_or_default();
    let mut recorder = Recorder::resume_at(id.clone(), Arc::clone(&context.store), history_head)
        .with_visibility(run.clone(), Arc::clone(&context.visibility_store));
    recorder
        .record_workflow_resumed(Utc::now(), run.clone(), operator)
        .await?;
    for timer in rearm.iter().filter(|timer| timer.needs_restart_marker) {
        recorder
            .record_timer_started(Utc::now(), timer.timer_id.clone(), timer.fire_at)
            .await?;
    }
    // The run is now durably Running; release the hold before respawn so a
    // respawn failure still leaves the run recoverable via list_active.
    context.paused_runs.remove(id);

    let reopen_context = context.reopen_context();
    let history = context.store.read_history(id).await?;
    let handle = reopen::respawn_and_register(&reopen_context, id, run, &history, recorder).await?;
    reopen::rearm_reopened_timers(&reopen_context, id, handle.pid(), &rearm).await?;
    Ok(handle)
}
