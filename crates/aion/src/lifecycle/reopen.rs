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

    // History says reopenable-terminal, so a live handle now means exactly one
    // thing: a concurrent reopen already won the race.
    if context.registry.get(id, run)?.is_some() {
        return Err(EngineError::InvalidState {
            reason: format!(
                "workflow {id} run {run} was already reopened and is Running (concurrent reopen)"
            ),
        });
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
    rearm_reopened_timers(&context, id, &rearm).await?;
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
    rearm: &[RearmTimer],
) -> Result<(), EngineError> {
    if rearm.is_empty() {
        return Ok(());
    }
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
mod tests {
    use aion_core::{
        ActivityError, ActivityErrorKind, ActivityId, Event, EventEnvelope, Payload, RunId,
        WorkflowError, WorkflowId,
    };
    use chrono::Utc;

    use super::{reopened_failed_activities, validate_and_compute_reopened};
    use crate::EngineError;

    fn wf() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }

    fn run() -> RunId {
        RunId::new(uuid::Uuid::from_u128(1))
    }

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: wf(),
        }
    }

    fn payload() -> Payload {
        // Non-fallible: a fixed valid JSON byte string, so no expect/unwrap.
        Payload::new(aion_core::ContentType::Json, b"null".to_vec())
    }

    fn started() -> Event {
        Event::WorkflowStarted {
            envelope: envelope(1),
            workflow_type: String::from("stacked_dev"),
            input: payload(),
            run_id: run(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        }
    }

    fn scheduled(seq: u64, ordinal: u64) -> Event {
        Event::ActivityScheduled {
            envelope: envelope(seq),
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: String::from("dev_review"),
            input: payload(),
            task_queue: String::from("default"),
            node: None,
        }
    }

    fn activity_failed(seq: u64, ordinal: u64) -> Event {
        Event::ActivityFailed {
            envelope: envelope(seq),
            activity_id: ActivityId::from_sequence_position(ordinal),
            error: ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: String::from("boom"),
                details: None,
            },
            attempt: 1,
        }
    }

    fn workflow_failed(seq: u64) -> Event {
        Event::WorkflowFailed {
            envelope: envelope(seq),
            error: WorkflowError {
                message: String::from("failed"),
                details: None,
            },
        }
    }

    #[test]
    fn failed_run_computes_the_terminally_failed_step() -> Result<(), Box<dyn std::error::Error>> {
        let segment = vec![
            started(),
            scheduled(2, 0),
            activity_failed(3, 0),
            workflow_failed(4),
        ];
        let reopened = validate_and_compute_reopened(&wf(), &run(), &segment)?;
        assert_eq!(reopened, vec![ActivityId::from_sequence_position(0)]);
        Ok(())
    }

    #[test]
    fn in_flight_activity_is_never_reopened() -> Result<(), Box<dyn std::error::Error>> {
        // Activity 1 was scheduled but had no terminal at crash time: it is
        // handled by ordinary recovery, not listed in the reopened set.
        let segment = vec![
            started(),
            scheduled(2, 0),
            activity_failed(3, 0),
            scheduled(4, 1),
            workflow_failed(5),
        ];
        let reopened = validate_and_compute_reopened(&wf(), &run(), &segment)?;
        assert_eq!(
            reopened,
            vec![ActivityId::from_sequence_position(0)],
            "only the terminally-failed step is reopened; the in-flight sibling is not"
        );
        Ok(())
    }

    #[test]
    fn failed_then_succeeded_step_is_not_reopened() {
        let segment = vec![
            started(),
            scheduled(2, 0),
            activity_failed(3, 0),
            Event::ActivityCompleted {
                envelope: envelope(4),
                activity_id: ActivityId::from_sequence_position(0),
                result: payload(),
                attempt: 2,
            },
            workflow_failed(5),
        ];
        assert!(
            reopened_failed_activities(&segment).is_empty(),
            "a step that recovered before the failure is not re-driven"
        );
    }

    #[test]
    fn concurrent_fan_out_reopens_every_failed_key() {
        let segment = vec![
            started(),
            scheduled(2, 0),
            scheduled(3, 1),
            activity_failed(4, 0),
            activity_failed(5, 1),
            workflow_failed(6),
        ];
        assert_eq!(
            reopened_failed_activities(&segment),
            vec![
                ActivityId::from_sequence_position(0),
                ActivityId::from_sequence_position(1),
            ]
        );
    }

    #[test]
    fn cancelled_run_reopens_with_an_empty_set() -> Result<(), Box<dyn std::error::Error>> {
        let segment = vec![
            started(),
            scheduled(2, 0),
            Event::WorkflowCancelled {
                envelope: envelope(3),
                reason: String::from("operator stop"),
            },
        ];
        let reopened = validate_and_compute_reopened(&wf(), &run(), &segment)?;
        assert!(
            reopened.is_empty(),
            "a cancel records no terminal activity failure to re-drive"
        );
        Ok(())
    }

    #[test]
    fn completed_run_is_rejected_as_invalid_state() {
        let segment = vec![
            started(),
            Event::WorkflowCompleted {
                envelope: envelope(2),
                result: payload(),
            },
        ];
        assert!(matches!(
            validate_and_compute_reopened(&wf(), &run(), &segment),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[test]
    fn timed_out_run_is_rejected_as_invalid_state() {
        let segment = vec![
            started(),
            Event::WorkflowTimedOut {
                envelope: envelope(2),
                timeout: String::from("execution"),
            },
        ];
        assert!(matches!(
            validate_and_compute_reopened(&wf(), &run(), &segment),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[test]
    fn running_run_is_rejected_as_invalid_state() {
        let segment = vec![started(), scheduled(2, 0)];
        assert!(matches!(
            validate_and_compute_reopened(&wf(), &run(), &segment),
            Err(EngineError::InvalidState { .. })
        ));
    }

    fn timer_started(seq: u64, timer_id: &aion_core::TimerId, fire_at_offset: i64) -> Event {
        Event::TimerStarted {
            envelope: envelope(seq),
            timer_id: timer_id.clone(),
            fire_at: Utc::now() + chrono::Duration::seconds(fire_at_offset),
        }
    }

    fn timer_cancelled(
        seq: u64,
        timer_id: &aion_core::TimerId,
        cause: aion_core::TimerCancelCause,
    ) -> Event {
        Event::TimerCancelled {
            envelope: envelope(seq),
            timer_id: timer_id.clone(),
            cause,
        }
    }

    fn workflow_cancelled(seq: u64) -> Event {
        Event::WorkflowCancelled {
            envelope: envelope(seq),
            reason: String::from("operator stop"),
        }
    }

    #[test]
    fn teardown_cancelled_timer_is_rearmed_with_a_restart_marker() {
        use aion_core::{TimerCancelCause, TimerId};
        let named = TimerId::anonymous(1);
        let segment = vec![
            started(),
            timer_started(2, &named, 3600),
            timer_cancelled(3, &named, TimerCancelCause::CancelTeardown),
            workflow_cancelled(4),
        ];
        let rearm = super::rearmable_timers(&segment);
        assert_eq!(rearm.len(), 1);
        assert_eq!(rearm[0].timer_id, named);
        assert!(
            rearm[0].needs_restart_marker,
            "a teardown-cancelled timer needs a fresh TimerStarted to be live again"
        );
    }

    #[test]
    fn workflow_intent_cancellation_is_never_resurrected() {
        use aion_core::{TimerCancelCause, TimerId};
        let named = TimerId::anonymous(1);
        let segment = vec![
            started(),
            timer_started(2, &named, 3600),
            timer_cancelled(3, &named, TimerCancelCause::WorkflowIntent),
            workflow_cancelled(4),
        ];
        assert!(
            super::rearmable_timers(&segment).is_empty(),
            "a timer the workflow retired is a settled business fact"
        );
    }

    #[test]
    fn fired_timer_is_not_rearmed() {
        use aion_core::TimerId;
        let named = TimerId::anonymous(1);
        let segment = vec![
            started(),
            timer_started(2, &named, -5),
            Event::TimerFired {
                envelope: envelope(3),
                timer_id: named.clone(),
            },
            workflow_cancelled(4),
        ];
        assert!(super::rearmable_timers(&segment).is_empty());
    }

    #[test]
    fn outstanding_timer_rearms_without_touching_history() {
        // The failed-run case: a failure tears no timers down, so the timer is
        // still outstanding by last-event-wins — only the wheel/row re-arms.
        use aion_core::TimerId;
        let named = TimerId::anonymous(1);
        let segment = vec![
            started(),
            timer_started(2, &named, 3600),
            workflow_failed(3),
        ];
        let rearm = super::rearmable_timers(&segment);
        assert_eq!(rearm.len(), 1);
        assert!(
            !rearm[0].needs_restart_marker,
            "an outstanding timer must not gain a duplicate TimerStarted"
        );
    }

    #[test]
    fn rearm_keeps_the_original_fire_at_and_covers_multiple_timers() {
        use aion_core::{TimerCancelCause, TimerId};
        let deadline = TimerId::anonymous(1);
        let scope = TimerId::anonymous(2);
        let expected_deadline = Utc::now() + chrono::Duration::seconds(120);
        let segment = vec![
            started(),
            Event::TimerStarted {
                envelope: envelope(2),
                timer_id: deadline.clone(),
                fire_at: expected_deadline,
            },
            timer_started(3, &scope, 120),
            timer_cancelled(4, &deadline, TimerCancelCause::CancelTeardown),
            timer_cancelled(5, &scope, TimerCancelCause::CancelTeardown),
            workflow_cancelled(6),
        ];
        let rearm = super::rearmable_timers(&segment);
        assert_eq!(rearm.len(), 2, "both teardown-cancelled timers re-arm");
        let recovered = rearm
            .iter()
            .find(|timer| timer.timer_id == deadline)
            .map(|timer| timer.fire_at);
        assert_eq!(
            recovered,
            Some(expected_deadline),
            "reopen never moves a business deadline"
        );
    }

    #[test]
    fn a_reopened_run_that_reterminated_reopens_from_the_new_failure() {
        // Failed -> Reopened -> Failed: the current lease's failure drives the set.
        let segment = vec![
            started(),
            scheduled(2, 0),
            activity_failed(3, 0),
            workflow_failed(4),
            Event::WorkflowReopened {
                envelope: envelope(5),
                run_id: run(),
                reopened: vec![ActivityId::from_sequence_position(0)],
            },
            scheduled(6, 1),
            activity_failed(7, 1),
            workflow_failed(8),
        ];
        let reopened = validate_and_compute_reopened(&wf(), &run(), &segment);
        assert!(
            matches!(
                reopened.as_deref(),
                Ok([id]) if *id == ActivityId::from_sequence_position(1)
            ),
            "only the current lease's failed step is reopened, not the superseded one: {reopened:?}"
        );
    }
}
