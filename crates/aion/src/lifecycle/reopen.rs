//! Reopen lifecycle operation: re-drive a terminal-Failed or terminal-Cancelled
//! workflow from where it left off.
//!
//! Reopen turns a run whose current-lease terminal is [`WorkflowFailed`] or
//! [`WorkflowCancelled`] back into a running one. It appends a single
//! [`Event::WorkflowReopened`] that supersedes the terminal in the status
//! projection (AD-011), then re-drives the SAME run through the existing recovery
//! respawn-and-register path (AD-012) so replay returns every recorded result and
//! only the reopened / in-flight step re-dispatches live — in the workflow's own
//! namespace, re-derived from history.
//!
//! Invariant #3 (single writer): the recorder that appends `WorkflowReopened` is
//! the very recorder handed to resident registration; no second writer is ever
//! constructed for the reopened run.
//!
//! Durable timers (#222): a reopened run's clock must come back with it. The
//! run segment is scanned last-event-wins per timer id — a timer whose last
//! event is `TimerCancelled { cause: CancelTeardown }` was retired by the
//! cancel teardown (engine bookkeeping, not workflow intent) and is re-armed at
//! its ORIGINAL `fire_at`; a timer still outstanding (`TimerStarted` with no
//! terminal — the failed-run case) is re-armed without touching history. A
//! deadline already in the past fires immediately after respawn, so a run
//! reopened after its deadline takes the timeout branch instead of parking
//! forever. Workflow-intent cancellations are never resurrected.
//!
//! [`WorkflowFailed`]: aion_core::Event::WorkflowFailed
//! [`WorkflowCancelled`]: aion_core::Event::WorkflowCancelled

use std::sync::Arc;

use aion_core::{
    ActivityId, Event, RunId, SearchAttributeSchema, TimerCancelCause, TimerId, WorkflowId,
    current_lease_terminal, run_segment, status_from_events,
};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::{DateTime, Utc};

use crate::EngineError;
use crate::durability::{
    ActiveWorkflowRecovery, ActiveWorkflowRecoverySeam, ActiveWorkflowRecoverySeamImpl, Recorder,
};
use crate::engine::startup::{
    RecoveredResident, StartupRecoveryContext, recover_active_workflow, register_recovered_resident,
};
use crate::loader::WorkflowCatalog;
use crate::registry::{Registry, WorkflowHandle};
use crate::runtime::RuntimeHandle;
use crate::supervision::SupervisionTree;

/// Dependencies required to reopen a terminal workflow.
pub struct ReopenWorkflowContext<'a> {
    /// Durable event store used to read history and construct the recorder.
    pub store: Arc<dyn EventStore>,
    /// Visibility store the resumed recorder projects the reopened run into.
    pub visibility_store: Arc<dyn VisibilityStore>,
    /// Workflow catalog resolving the pinned package version to spawn.
    pub catalog: Arc<WorkflowCatalog>,
    /// Runtime boundary used to spawn the reopened workflow process.
    pub runtime: &'a Arc<RuntimeHandle>,
    /// Structural supervision tree recording the per-type supervisor placement.
    pub supervision: Arc<SupervisionTree>,
    /// Active execution registry keyed by workflow/run identifiers.
    pub registry: &'a Arc<Registry>,
    /// Schema shared with startup recovery's resident registration.
    pub search_attribute_schema: Arc<SearchAttributeSchema>,
}

/// Reopens a terminal-`Failed` or terminal-`Cancelled` run and re-drives it.
///
/// Validates the precondition, computes the reopened activity set, appends
/// `WorkflowReopened` through one continuous recorder, then respawns and
/// registers the run as Resident via the existing recovery path.
///
/// # Errors
///
/// Returns [`EngineError::WorkflowNotFound`] when no history exists for
/// `(id, run)`, and [`EngineError::InvalidState`] when the run is not currently
/// terminal, terminal for a non-reopenable reason (Completed/`TimedOut`), or
/// already Running. A double-write race surfaces as a hard durability error via
/// the recorder's expected-sequence discipline. Recorder, runtime, supervision,
/// and registry failures surface as their typed [`EngineError`] variants.
pub async fn reopen(
    context: ReopenWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
) -> Result<WorkflowHandle, EngineError> {
    let history = context.store.read_history(id).await?;
    if history.is_empty() {
        return Err(crate::engine::api::workflow_not_found(id, run));
    }

    // Validate against HISTORY first so the rejection names the run's true
    // state: a terminal run can still hold a lingering registry handle (a
    // Completed run's handle lives until reconciliation), and the handle check
    // alone used to misreport every such rejection as "already Running".
    let segment = run_segment(&history, run);
    let reopened = validate_and_compute_reopened(id, run, segment)?;

    // History says reopenable-terminal, but a live handle can mean two things.
    // A run that fails while resident keeps its suspended handle registered —
    // `reconcile_terminal_registry` reconciles the cached status to the
    // terminal and suspends residency without removing the entry — so a
    // terminal-cached handle is that leftover, not a live run: clear it and
    // proceed. Only a non-terminal cached status means a concurrent reopen
    // already won the race and is driving the run. Two reopens racing past
    // this check are still serialized by the recorder's expected-sequence
    // discipline (the loser's `WorkflowReopened` append fails).
    if let Some(existing) = context.registry.get(id, run)? {
        if existing.cached_status().is_terminal() {
            context.registry.remove(id, run)?;
        } else {
            return Err(EngineError::InvalidState {
                reason: format!(
                    "workflow {id} run {run} was already reopened and is Running (concurrent reopen)"
                ),
            });
        }
    }

    let rearm = rearmable_timers(segment);

    // ONE continuous recorder from the WorkflowReopened append through the
    // respawn (invariant #3): built at the history head, it appends the marker
    // and is then handed — the same instance — to resident registration.
    let history_head = history.last().map(Event::seq).unwrap_or_default();
    let mut recorder = Recorder::resume_at(id.clone(), Arc::clone(&context.store), history_head)
        .with_visibility(run.clone(), Arc::clone(&context.visibility_store));
    recorder
        .record_workflow_reopened(Utc::now(), run.clone(), reopened)
        .await?;
    // A teardown-cancelled timer's last event is TimerCancelled, so liveness
    // (last-event-wins) reads it as dead: record a fresh TimerStarted — same
    // id, ORIGINAL fire_at — through the same recorder so the fire/replay
    // machinery sees it live again. Still-outstanding timers (failed-run case)
    // stay untouched: re-recording one would put a second unconsumed
    // TimerStarted resolution in front of replay.
    for timer in rearm.iter().filter(|timer| timer.needs_restart_marker) {
        recorder
            .record_timer_started(Utc::now(), timer.timer_id.clone(), timer.fire_at)
            .await?;
    }

    // Re-read so registration reconciles against the history that now INCLUDES
    // the WorkflowReopened marker: the projection (and the recovered resident's
    // reconciled cached status) is Running, not the superseded terminal.
    let history = context.store.read_history(id).await?;
    let handle = respawn_and_register(&context, id, run, &history, recorder).await?;
    rearm_reopened_timers(&context, id, handle.pid(), &rearm).await?;
    Ok(handle)
}

/// A durable timer a reopened or resumed run must get back.
pub(crate) struct RearmTimer {
    pub(crate) timer_id: TimerId,
    /// The ORIGINAL deadline. Reopen never extends a business deadline; one
    /// already in the past fires immediately after respawn.
    pub(crate) fire_at: DateTime<Utc>,
    /// True when the cancel teardown recorded `TimerCancelled` for this timer,
    /// so a fresh `TimerStarted` must be appended to make it live again. False
    /// for a timer still outstanding in history (failed-run case) — only the
    /// wheel/durable row needs re-arming.
    pub(crate) needs_restart_marker: bool,
}

/// The run segment's timers that reopen must re-arm, decided last-event-wins
/// per timer id:
///
/// * last event `TimerStarted` — outstanding at the terminal (a failure tears
///   nothing down): re-arm the wheel/row only.
/// * last event `TimerCancelled { cause: CancelTeardown }` — retired by the
///   cancel teardown: append a fresh `TimerStarted` and re-arm.
/// * last event `TimerFired` or `TimerCancelled { cause: WorkflowIntent }` —
///   a settled business fact: never resurrected.
pub(crate) fn rearmable_timers(segment: &[Event]) -> Vec<RearmTimer> {
    use std::collections::HashMap;

    struct TimerTrace {
        fire_at: DateTime<Utc>,
        last_was_teardown_cancel: Option<bool>,
    }

    let mut traces: HashMap<TimerId, TimerTrace> = HashMap::new();
    let mut order: Vec<TimerId> = Vec::new();
    for event in segment {
        match event {
            Event::TimerStarted {
                timer_id, fire_at, ..
            } => {
                if !traces.contains_key(timer_id) {
                    order.push(timer_id.clone());
                }
                traces.insert(
                    timer_id.clone(),
                    TimerTrace {
                        fire_at: *fire_at,
                        last_was_teardown_cancel: None,
                    },
                );
            }
            Event::TimerFired { timer_id, .. } => {
                traces.remove(timer_id);
            }
            Event::TimerCancelled {
                timer_id, cause, ..
            } => {
                if let Some(trace) = traces.get_mut(timer_id) {
                    match cause {
                        TimerCancelCause::CancelTeardown => {
                            trace.last_was_teardown_cancel = Some(true);
                        }
                        TimerCancelCause::WorkflowIntent => {
                            traces.remove(timer_id);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    order
        .into_iter()
        .filter_map(|timer_id| {
            traces.remove(&timer_id).map(|trace| RearmTimer {
                timer_id,
                fire_at: trace.fire_at,
                needs_restart_marker: trace.last_was_teardown_cancel == Some(true),
            })
        })
        .collect()
}

/// Arms (or immediately fires) the reopened run's re-armed timers through the
/// production [`crate::time::TimerService`], AFTER resident registration so
/// residency resolution and mailbox delivery hit the live process.
///
/// A future deadline gets its durable row and wheel entry back via
/// `TimerService::schedule`. A past deadline writes its durable row FIRST and
/// then fires through `TimerService::fire_timer` — if the fire fails, the row
/// survives so the next startup's `recover_due` sweep completes it instead of
/// losing the deadline silently.
///
/// # Errors
///
/// Surfaces timer-service and store failures as [`EngineError::Runtime`]: the
/// reopen itself is already durable, and the recorded `TimerStarted` plus
/// durable row make the failure recoverable at the next startup sweep, but the
/// operator must know the re-arm did not complete live.
pub(crate) async fn rearm_reopened_timers(
    context: &ReopenWorkflowContext<'_>,
    id: &WorkflowId,
    pid: crate::Pid,
    rearm: &[RearmTimer],
) -> Result<(), EngineError> {
    if rearm.is_empty() {
        return Ok(());
    }
    // A record-before-deliver fire can otherwise overtake the recovered
    // process's first await: the process reads `TimerFired`, completes, and
    // disappears before the redundant wake is enqueued. A pending-await entry
    // proves the process committed its park decision; beamr then admits the
    // wake either to that parked slot or its in-flight store-back.
    context.runtime.wait_for_pending_await(pid).await?;
    let timer_service =
        crate::runtime::nif_timer_bridge::installed_timer_service(context.runtime.nif_state())
            .map_err(|error| EngineError::Runtime {
                reason: format!("timer service unavailable while reopening {id}: {error}"),
            })?;
    let now = Utc::now();
    for timer in rearm {
        if timer.fire_at > now {
            timer_service
                .schedule(id.clone(), timer.timer_id.clone(), timer.fire_at)
                .await
                .map_err(|error| EngineError::Runtime {
                    reason: format!(
                        "failed to re-arm timer {} for reopened workflow {id}: {error}",
                        timer.timer_id
                    ),
                })?;
        } else {
            context
                .store
                .schedule_timer(id, &timer.timer_id, timer.fire_at)
                .await?;
            timer_service
                .fire_timer(id.clone(), timer.timer_id.clone(), timer.fire_at)
                .await
                .map_err(|error| EngineError::Runtime {
                    reason: format!(
                        "failed to fire past-due timer {} for reopened workflow {id}: {error}",
                        timer.timer_id
                    ),
                })?;
        }
    }
    Ok(())
}

/// Validates the reopen precondition and computes the reopened activity set.
///
/// Accepts a current-lease terminal of `WorkflowFailed` (AD-012) or
/// `WorkflowCancelled` (AD-013); rejects every other state with a typed
/// [`EngineError::InvalidState`] naming the actual status.
fn validate_and_compute_reopened(
    id: &WorkflowId,
    run: &RunId,
    segment: &[Event],
) -> Result<Vec<ActivityId>, EngineError> {
    match current_lease_terminal(segment) {
        Some(Event::WorkflowFailed { .. }) => Ok(reopened_failed_activities(segment)),
        // A cancel records no terminal activity failure and re-drives nothing
        // already recorded: the reopened set is empty, and the in-flight-at-cancel
        // step re-dispatches via the same ResumeLive path crash recovery uses.
        Some(Event::WorkflowCancelled { .. }) => Ok(Vec::new()),
        Some(other) => Err(EngineError::InvalidState {
            reason: format!(
                "workflow {id} run {run} is terminal for a non-reopenable reason ({}); only Failed and Cancelled are reopenable",
                terminal_status_name(other)
            ),
        }),
        None => Err(EngineError::InvalidState {
            reason: format!(
                "workflow {id} run {run} is {:?}, not a reopenable terminal (Failed or Cancelled)",
                status_from_events(segment)
            ),
        }),
    }
}

/// The activities that ended in a terminal failure in the CURRENT lease with no
/// later successful attempt — exactly the steps the cursor reset rule (AD-011)
/// must re-dispatch. A merely in-flight (scheduled, no terminal) activity is
/// never listed: it already re-dispatches through the existing recovery path.
///
/// Scoped to the current lease (events after the last run start or reopen) so a
/// failure superseded by an earlier `WorkflowReopened` — already re-driven in a
/// prior lease — is never re-listed.
fn reopened_failed_activities(segment: &[Event]) -> Vec<ActivityId> {
    use std::collections::HashSet;

    let lease_start = segment
        .iter()
        .rposition(|event| {
            matches!(
                event,
                Event::WorkflowStarted { .. } | Event::WorkflowReopened { .. }
            )
        })
        .map_or(0, |index| index + 1);
    let lease = &segment[lease_start..];

    let mut failed: HashSet<ActivityId> = HashSet::new();
    let mut succeeded: HashSet<ActivityId> = HashSet::new();
    for event in lease {
        match event {
            Event::ActivityFailed { activity_id, .. } => {
                failed.insert(activity_id.clone());
            }
            Event::ActivityCompleted { activity_id, .. }
            | Event::ActivityCancelled { activity_id, .. } => {
                succeeded.insert(activity_id.clone());
            }
            _ => {}
        }
    }
    let mut reopened: Vec<ActivityId> = failed
        .into_iter()
        .filter(|activity_id| !succeeded.contains(activity_id))
        .collect();
    // Deterministic order for a stable recorded event.
    reopened.sort_by_key(ActivityId::sequence_position);
    reopened
}

fn terminal_status_name(event: &Event) -> &'static str {
    match event {
        Event::WorkflowCompleted { .. } => "Completed",
        Event::WorkflowTimedOut { .. } => "TimedOut",
        Event::WorkflowContinuedAsNew { .. } => "ContinuedAsNew",
        Event::WorkflowFailed { .. } => "Failed",
        Event::WorkflowCancelled { .. } => "Cancelled",
        _ => "non-terminal",
    }
}

/// Respawns the reopened run through the recovery seam and registers it Resident,
/// reusing `recorder` (the one that appended `WorkflowReopened`) so no second
/// writer is ever constructed.
pub(crate) async fn respawn_and_register(
    context: &ReopenWorkflowContext<'_>,
    id: &WorkflowId,
    run: &RunId,
    history: &[Event],
    recorder: Recorder,
) -> Result<WorkflowHandle, EngineError> {
    let workflow_type = started_workflow_type(id, history)?;
    context
        .supervision
        .ensure_type_supervisor(workflow_type.clone())?;

    let seam: Arc<dyn ActiveWorkflowRecoverySeam> = Arc::new(ActiveWorkflowRecoverySeamImpl::new(
        Arc::clone(context.runtime),
    ));
    let recovered =
        recover_active_workflow(seam.as_ref(), id, &workflow_type, history, &context.catalog)?;
    let (run_id, loaded_version, pid) = match recovered {
        ActiveWorkflowRecovery::Resident {
            run_id,
            loaded_version,
            pid,
        } => (run_id, loaded_version, pid),
        ActiveWorkflowRecovery::ScheduleCoordinator { .. } => {
            return Err(EngineError::InvalidState {
                reason: format!(
                    "workflow {id} run {run} is the schedule coordinator, not reopenable"
                ),
            });
        }
    };

    let startup_context = StartupRecoveryContext {
        store: Arc::clone(&context.store),
        visibility_store: Arc::clone(&context.visibility_store),
        runtime: Arc::clone(context.runtime),
        catalog: Arc::clone(&context.catalog),
        registry: Arc::clone(context.registry),
        supervision: Arc::clone(&context.supervision),
        recovery: Some(seam),
        search_attribute_schema: Arc::clone(&context.search_attribute_schema),
        bootstrap_schedule_coordinator: false,
    };
    let history_head = history.last().map(Event::seq).unwrap_or_default();
    register_recovered_resident(
        &startup_context,
        RecoveredResident {
            workflow_id: id,
            workflow_type: &workflow_type,
            history,
            history_head,
            projected_status: aion_core::WorkflowStatus::Running,
            run_id: run_id.clone(),
            loaded_version,
            pid,
            recorder: Some(recorder),
        },
    )
    .await?;

    context
        .registry
        .get(id, &run_id)?
        .ok_or_else(|| EngineError::Runtime {
            reason: format!("reopened workflow {id} run {run_id} was not registered"),
        })
}

fn started_workflow_type(id: &WorkflowId, history: &[Event]) -> Result<String, EngineError> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted { workflow_type, .. } => Some(workflow_type.clone()),
            _ => None,
        })
        .ok_or_else(|| EngineError::Load {
            reason: format!("workflow {id} has no WorkflowStarted event to reopen from"),
        })
}

#[cfg(test)]
#[path = "reopen_tests.rs"]
mod tests;
