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
//! [`WorkflowFailed`]: aion_core::Event::WorkflowFailed
//! [`WorkflowCancelled`]: aion_core::Event::WorkflowCancelled

use std::sync::Arc;

use aion_core::{
    ActivityId, Event, RunId, SearchAttributeSchema, WorkflowId, current_lease_terminal,
    run_segment, status_from_events,
};
use aion_store::EventStore;
use aion_store::visibility::VisibilityStore;
use chrono::Utc;

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
    // Reject an already-live run before touching history: a terminal workflow has
    // no live handle, so a present handle means it is already Running (e.g. a
    // concurrent reopen already won).
    if context.registry.get(id, run)?.is_some() {
        return Err(EngineError::InvalidState {
            reason: format!("workflow {id} run {run} is already Running"),
        });
    }

    let segment = run_segment(&history, run);
    let reopened = validate_and_compute_reopened(id, run, segment)?;

    // ONE continuous recorder from the WorkflowReopened append through the
    // respawn (invariant #3): built at the history head, it appends the marker
    // and is then handed — the same instance — to resident registration.
    let history_head = history.last().map(Event::seq).unwrap_or_default();
    let mut recorder = Recorder::resume_at(id.clone(), Arc::clone(&context.store), history_head)
        .with_visibility(run.clone(), Arc::clone(&context.visibility_store));
    recorder
        .record_workflow_reopened(Utc::now(), run.clone(), reopened)
        .await?;

    // Re-read so registration reconciles against the history that now INCLUDES
    // the WorkflowReopened marker: the projection (and the recovered resident's
    // reconciled cached status) is Running, not the superseded terminal.
    let history = context.store.read_history(id).await?;
    respawn_and_register(&context, id, run, &history, recorder).await
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
async fn respawn_and_register(
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
