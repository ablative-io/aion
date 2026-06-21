//! Tests for the time-travel inspection lens (WA-004).

use aion_core::{
    ActivityError, ActivityErrorKind, ActivityId, Event, EventEnvelope, Payload, RunId, TimerId,
    WorkflowId,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{
    DivergentCommand, MockOutcome, StepProjection, WhatIfOutcome, inspect_run, what_if_from,
};
use crate::durability::{ReplayTerminal, Resolution};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(1))
}

fn run_id() -> RunId {
    RunId::new(Uuid::from_u128(2))
}

fn timestamp(seconds: i64) -> TestResult<DateTime<Utc>> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| format!("invalid timestamp {seconds}").into())
}

fn envelope(seq: u64, seconds: i64) -> TestResult<EventEnvelope> {
    Ok(EventEnvelope {
        seq,
        recorded_at: timestamp(seconds)?,
        workflow_id: workflow_id(),
    })
}

fn payload(label: &str) -> TestResult<Payload> {
    Ok(Payload::from_json(&json!({ "label": label }))?)
}

/// A recorded run: start, one activity (scheduled + completed), one timer
/// (started + fired), then completion. Six events at 10/20/30/40/50/60.
fn history() -> TestResult<Vec<Event>> {
    let activity_id = ActivityId::from_sequence_position(0);
    let timer_id = TimerId::anonymous(3);
    Ok(vec![
        Event::WorkflowStarted {
            envelope: envelope(1, 10)?,
            workflow_type: "workflow".to_owned(),
            input: payload("input")?,
            run_id: run_id(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::ActivityScheduled {
            envelope: envelope(2, 20)?,
            activity_id: activity_id.clone(),
            activity_type: "activity".to_owned(),
            input: payload("activity-input")?,
        },
        Event::ActivityCompleted {
            envelope: envelope(3, 30)?,
            activity_id,
            result: payload("activity-result")?,
        },
        Event::TimerStarted {
            envelope: envelope(4, 40)?,
            timer_id: timer_id.clone(),
            fire_at: timestamp(100)?,
        },
        Event::TimerFired {
            envelope: envelope(5, 50)?,
            timer_id,
        },
        Event::WorkflowCompleted {
            envelope: envelope(6, 60)?,
            result: payload("workflow-result")?,
        },
    ])
}

#[test]
fn inspect_run_projects_one_step_per_event_with_resolutions() -> TestResult {
    // R1/C20: the projection is computed from history and replay with no second
    // store — the signature takes only Vec<Event>, structurally proving it.
    let inspection = inspect_run(history()?, &run_id())?;

    assert_eq!(inspection.workflow_id, workflow_id());
    assert_eq!(inspection.run_id, run_id());
    assert_eq!(inspection.steps.len(), 6, "one step per recorded event");
    assert!(inspection.divergence.is_none());

    // The start event projects the run's type and input.
    assert!(matches!(
        inspection.steps[0].projection,
        StepProjection::Started { .. }
    ));
    // The activity schedule anchor resolves to the recorded completion.
    assert_eq!(
        inspection.steps[1].projection,
        StepProjection::Resolved(Resolution::ActivityCompleted(payload("activity-result")?))
    );
    // The timer-start anchor resolves to the recorded firing.
    assert_eq!(
        inspection.steps[3].projection,
        StepProjection::Resolved(Resolution::TimerFired)
    );
    // The terminal event projects the recorded completion.
    assert_eq!(
        inspection.steps[5].projection,
        StepProjection::Terminal(ReplayTerminal::Completed(payload("workflow-result")?))
    );
    Ok(())
}

#[test]
fn inspect_run_now_equals_each_event_recorded_at() -> TestResult {
    // R1/C17 (now): per-step now equals each event's recorded timestamp, proving
    // now() is the recorded clock and never wall-clock time.
    let inspection = inspect_run(history()?, &run_id())?;

    let nows: Vec<i64> = inspection
        .steps
        .iter()
        .map(|step| step.now.timestamp())
        .collect();
    assert_eq!(nows, vec![10, 20, 30, 40, 50, 60]);
    Ok(())
}

#[test]
fn inspect_run_random_is_deterministic_per_run() -> TestResult {
    // R1/C17 (random): two inspections of the same (WorkflowId, RunId) produce
    // identical random streams.
    let first = inspect_run(history()?, &run_id())?;
    let second = inspect_run(history()?, &run_id())?;

    let first_random: Vec<u64> = first.steps.iter().map(|step| step.random_u64).collect();
    let second_random: Vec<u64> = second.steps.iter().map(|step| step.random_u64).collect();
    assert_eq!(first_random, second_random);
    assert_eq!(first_random.len(), 6);
    Ok(())
}

#[test]
fn inspect_run_random_differs_for_a_different_run() -> TestResult {
    // R1/C17 (random): a different RunId yields a different random stream.
    let other_run = RunId::new(Uuid::from_u128(999));
    let mut other_history = history()?;
    if let Some(Event::WorkflowStarted { run_id, .. }) = other_history.first_mut() {
        *run_id = other_run.clone();
    } else {
        return Err("expected WorkflowStarted first".into());
    }

    let baseline = inspect_run(history()?, &run_id())?;
    let other = inspect_run(other_history, &other_run)?;

    let baseline_random: Vec<u64> = baseline.steps.iter().map(|step| step.random_u64).collect();
    let other_random: Vec<u64> = other.steps.iter().map(|step| step.random_u64).collect();
    assert_ne!(baseline_random, other_random);
    Ok(())
}

/// A recorded run that faulted on a determinism violation: the engine recorded a
/// terminal `WorkflowFailed` whose message is `fail_on_violation`'s formatting of
/// the resolver's [`NonDeterminismError`]. This is exactly what an injected
/// determinism fault leaves in history.
fn faulted_history() -> TestResult<(Vec<Event>, crate::durability::NonDeterminismError)> {
    let violation = crate::durability::NonDeterminismError {
        workflow_id: workflow_id(),
        seq: 3,
        expected: "Activity Activity(1)".to_owned(),
        found: "ActivityCompleted family Some(Activity) key Some(Activity(0))".to_owned(),
    };
    let message = format!(
        "{}: {violation}",
        crate::durability::NON_DETERMINISM_WORKFLOW_ERROR_PREFIX
    );

    let history = vec![
        Event::WorkflowStarted {
            envelope: envelope(1, 10)?,
            workflow_type: "workflow".to_owned(),
            input: payload("input")?,
            run_id: run_id(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::ActivityScheduled {
            envelope: envelope(2, 20)?,
            activity_id: ActivityId::from_sequence_position(0),
            activity_type: "activity".to_owned(),
            input: payload("activity-input")?,
        },
        Event::WorkflowFailed {
            envelope: envelope(3, 30)?,
            error: aion_core::WorkflowError {
                message,
                details: None,
            },
        },
    ];
    Ok((history, violation))
}

#[test]
fn inspect_run_surfaces_the_divergent_command() -> TestResult {
    // R1/C18: an injected determinism fault surfaces the exact divergent command
    // (expected vs found at the sequence) the resolver computes, read back from
    // the recorded non-determinism terminal rather than recomputed.
    let (history, violation) = faulted_history()?;
    let inspection = inspect_run(history, &run_id())?;

    let divergence = inspection
        .divergence
        .ok_or("a faulted run must surface its recorded divergence")?;

    // The parsed divergence equals the resolver's own expected/found at the seq.
    assert_eq!(divergence.seq, violation.seq);
    assert_eq!(divergence.expected, violation.expected);
    assert_eq!(divergence.found, violation.found);
    Ok(())
}

#[test]
fn recorded_divergence_round_trips_through_the_real_failure_format() -> TestResult {
    // C18: the parser reads back the resolver's own format, not a guessed shape.
    // Build the message exactly as fail_on_violation would and assert the parse
    // recovers the original expected/found/seq.
    let violation = crate::durability::NonDeterminismError {
        workflow_id: workflow_id(),
        seq: 42,
        expected: "Activity Activity(7)".to_owned(),
        found: "TimerFired family Some(Timer) key Some(Timer(TimerId(3)))".to_owned(),
    };
    let message = format!(
        "{}: {violation}",
        crate::durability::NON_DETERMINISM_WORKFLOW_ERROR_PREFIX
    );

    let history = vec![
        Event::WorkflowStarted {
            envelope: envelope(1, 10)?,
            workflow_type: "workflow".to_owned(),
            input: payload("input")?,
            run_id: run_id(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::WorkflowFailed {
            envelope: envelope(2, 20)?,
            error: aion_core::WorkflowError {
                message,
                details: None,
            },
        },
    ];

    let inspection = inspect_run(history, &run_id())?;
    let divergence = inspection
        .divergence
        .ok_or("expected a recorded divergence")?;
    assert_eq!(divergence.seq, 42);
    assert_eq!(divergence.expected, violation.expected);
    assert_eq!(divergence.found, violation.found);
    Ok(())
}

#[test]
fn divergent_command_is_built_from_the_resolver_error() {
    // C18: DivergentCommand mirrors NonDeterminismError exactly, never recomputed.
    let error = crate::durability::NonDeterminismError {
        workflow_id: workflow_id(),
        seq: 7,
        expected: "Activity Activity(1)".to_owned(),
        found: "ActivityCompleted family Activity key Activity(0)".to_owned(),
    };

    let divergence = DivergentCommand::from(&error);

    assert_eq!(divergence.seq, 7);
    assert_eq!(divergence.expected, error.expected);
    assert_eq!(divergence.found, error.found);
}

#[test]
fn what_if_activity_failure_diverges_from_the_recorded_completion() -> TestResult {
    // R2/C19: forking at the activity schedule with a mocked terminal failure
    // produces a path that differs from the recorded completion, driven through
    // the real Replay over a forked in-memory history (no live store touched).
    let failure = ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: "mocked failure".to_owned(),
        details: None,
    };

    let outcome = what_if_from(
        history()?,
        &run_id(),
        2, // the ActivityScheduled anchor
        &MockOutcome::ActivityFailed(failure.clone()),
    )?;

    match outcome {
        WhatIfOutcome::Resolved {
            from_seq,
            resolution,
        } => {
            assert_eq!(from_seq, 2);
            assert_eq!(resolution, Resolution::ActivityFailedTerminal(failure));
        }
        other => return Err(format!("expected a resolved fork, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn what_if_activity_completion_reproduces_the_recorded_path() -> TestResult {
    // R2/C19: forking with the same outcome the recorded run had resolves to the
    // same recorded resolution, proving the fork uses the production replay path.
    let outcome = what_if_from(
        history()?,
        &run_id(),
        2,
        &MockOutcome::ActivityCompleted(payload("activity-result")?),
    )?;

    assert_eq!(
        outcome,
        WhatIfOutcome::Resolved {
            from_seq: 2,
            resolution: Resolution::ActivityCompleted(payload("activity-result")?),
        }
    );
    Ok(())
}

#[test]
fn what_if_rejects_a_mismatched_mock_family() -> TestResult {
    // R2: a mocked outcome that does not match the anchor family is a hard error,
    // never a silently fabricated path (no silent failures).
    let error = what_if_from(history()?, &run_id(), 2, &MockOutcome::TimerFired).err();

    assert!(matches!(
        error,
        Some(crate::durability::DurabilityError::HistoryShape { .. })
    ));
    Ok(())
}

#[test]
fn what_if_rejects_an_absent_fork_sequence() -> TestResult {
    let error = what_if_from(history()?, &run_id(), 9999, &MockOutcome::TimerFired).err();

    assert!(matches!(
        error,
        Some(crate::durability::DurabilityError::HistoryShape { .. })
    ));
    Ok(())
}

#[test]
fn inspect_run_errors_when_run_segment_is_absent() -> TestResult {
    let absent = RunId::new(Uuid::from_u128(404));
    let error = inspect_run(history()?, &absent).err();

    assert!(matches!(
        error,
        Some(crate::durability::DurabilityError::HistoryShape { .. })
    ));
    Ok(())
}

#[test]
fn inspect_run_scopes_to_the_current_run_segment() -> TestResult {
    // Reopen/continue-as-new aware: a prior run's events must not appear in the
    // inspected segment.
    let first_run = RunId::new(Uuid::from_u128(100));
    let second_run = RunId::new(Uuid::from_u128(200));
    let history = vec![
        Event::WorkflowStarted {
            envelope: envelope(1, 10)?,
            workflow_type: "workflow".to_owned(),
            input: payload("first")?,
            run_id: first_run.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::WorkflowContinuedAsNew {
            envelope: envelope(2, 20)?,
            input: payload("again")?,
            workflow_type: None,
            parent_run_id: first_run.clone(),
        },
        Event::WorkflowStarted {
            envelope: envelope(3, 30)?,
            workflow_type: "workflow".to_owned(),
            input: payload("second")?,
            run_id: second_run.clone(),
            parent_run_id: Some(first_run),
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        },
        Event::WorkflowCompleted {
            envelope: envelope(4, 40)?,
            result: payload("second-result")?,
        },
    ];

    let inspection = inspect_run(history, &second_run)?;

    assert_eq!(inspection.steps.len(), 2, "only the second run's segment");
    assert_eq!(inspection.steps[0].seq, 3);
    assert_eq!(inspection.steps[1].seq, 4);
    Ok(())
}
