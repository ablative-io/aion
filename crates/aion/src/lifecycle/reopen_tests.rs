//! Unit tests for reopen validation, activity selection, and timer rearming.

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
