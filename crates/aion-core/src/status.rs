//! Workflow status projection from authoritative event history.

use serde::{Deserialize, Serialize};

use crate::{Event, RunId};

/// Projected lifecycle status for a workflow execution.
///
/// Status must be obtained only by projecting from event history with
/// [`status_from_events`], never assigned directly or stored as an independent
/// mutable field. Event history remains authoritative for every workflow state.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// The workflow has not recorded a terminal lifecycle event.
    Running,
    /// The workflow recorded a [`Event::WorkflowCompleted`] terminal event.
    Completed,
    /// The workflow recorded a [`Event::WorkflowFailed`] terminal event.
    Failed,
    /// The workflow recorded a [`Event::WorkflowCancelled`] terminal event.
    Cancelled,
    /// The workflow recorded a [`Event::WorkflowTimedOut`] terminal event.
    TimedOut,
    /// The workflow recorded a [`Event::WorkflowContinuedAsNew`] terminal event.
    ContinuedAsNew,
    /// The workflow recorded a [`Event::WorkflowPaused`] marker with no later
    /// [`Event::WorkflowResumed`]. NON-terminal: the run has recorded no terminal
    /// event and can still complete/fail/cancel or be resumed.
    Paused,
}

impl WorkflowStatus {
    /// Returns whether this status represents a terminal workflow execution state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        match self {
            // Paused is non-terminal: complete/fail/cancel stay reachable, and a
            // paused run is excluded from the active set without being terminal.
            Self::Running | Self::Paused => false,
            Self::Completed
            | Self::Failed
            | Self::Cancelled
            | Self::TimedOut
            | Self::ContinuedAsNew => true,
        }
    }
}

/// Projects workflow status from an event history.
///
/// The last terminal workflow lifecycle event determines the projected status.
/// Histories without a terminal workflow event are considered running.
/// When a history contains multiple runs for continue-as-new, a later
/// [`Event::WorkflowStarted`] begins the current run and supersedes earlier
/// terminal events from the previous run.
#[must_use]
pub fn status_from_events(events: &[Event]) -> WorkflowStatus {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            // A run start and a reopen both put the run in Running. A reopen
            // supersedes the run's prior terminal event under this same
            // last-lifecycle-event-wins scan.
            // A run start, a reopen, and a resume all put the run in Running. A
            // resume supersedes the run's prior WorkflowPaused under this same
            // last-lifecycle-event-wins scan.
            Event::WorkflowStarted { .. }
            | Event::WorkflowReopened { .. }
            | Event::WorkflowResumed { .. } => Some(WorkflowStatus::Running),
            Event::WorkflowCompleted { .. } => Some(WorkflowStatus::Completed),
            Event::WorkflowFailed { .. } => Some(WorkflowStatus::Failed),
            Event::WorkflowCancelled { .. } => Some(WorkflowStatus::Cancelled),
            Event::WorkflowTimedOut { .. } => Some(WorkflowStatus::TimedOut),
            Event::WorkflowContinuedAsNew { .. } => Some(WorkflowStatus::ContinuedAsNew),
            // Paused is the one non-terminal lifecycle event that projects a
            // distinct status; a later WorkflowResumed supersedes it above.
            Event::WorkflowPaused { .. } => Some(WorkflowStatus::Paused),
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
            | Event::ScheduleTriggered { .. } => None,
        })
        .unwrap_or(WorkflowStatus::Running)
}

/// Returns the terminal lifecycle event of the run's current lease, or `None`
/// when the run is not currently terminal — either it never recorded a terminal
/// event, or a later [`Event::WorkflowReopened`] reopened it.
///
/// This is the single reset-aware terminal predicate every site derives from:
/// close-time is its `recorded_at`, the terminal outcome is a match on the
/// returned event, and "is the run terminal now" is `is_some()`. Scanning back
/// from the end it stops at the first reset point (a run start or a reopen), so
/// terminality is scoped to "since the last reopen point" — a run holds exactly
/// one terminal event per lease.
#[must_use]
pub fn current_lease_terminal(events: &[Event]) -> Option<&Event> {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. } => Some(Some(event)),
            // Reset points: the current lease has no terminal before them.
            Event::WorkflowStarted { .. } | Event::WorkflowReopened { .. } => Some(None),
            // Pause/resume are neither a terminal nor a run-start reset — they
            // fall through like SearchAttributesUpdated, so a paused (or resumed)
            // run keeps whatever current-lease terminal state it otherwise has
            // (None while live), leaving complete/fail/cancel reachable.
            Event::WorkflowPaused { .. }
            | Event::WorkflowResumed { .. }
            | Event::SearchAttributesUpdated { .. }
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
            | Event::ScheduleTriggered { .. } => None,
        })
        .flatten()
}

/// Returns the slice of `events` belonging to the run identified by `run_id`:
/// from that run's [`Event::WorkflowStarted`] up to (but excluding) the next
/// run's start, or an empty slice when the run is absent.
///
/// Scoping [`current_lease_terminal`] to a `run_segment` answers "is this
/// particular run currently terminal" without a separate bespoke scan per call
/// site.
#[must_use]
pub fn run_segment<'a>(events: &'a [Event], run_id: &RunId) -> &'a [Event] {
    let Some(start) = events.iter().position(
        |event| matches!(event, Event::WorkflowStarted { run_id: id, .. } if id == run_id),
    ) else {
        return &[];
    };
    let end = events[start + 1..]
        .iter()
        .position(|event| matches!(event, Event::WorkflowStarted { .. }))
        .map_or(events.len(), |offset| start + 1 + offset);
    &events[start..end]
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{WorkflowStatus, current_lease_terminal, run_segment, status_from_events};
    use crate::{
        ActivityId, Event, EventEnvelope, Payload, RunId, ScheduleId, SearchAttributeValue,
        WorkflowError, WorkflowId,
    };

    fn recorded_at(offset: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + offset, 0).unwrap_or_default()
    }

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: recorded_at(i64::try_from(seq).unwrap_or(0)),
            workflow_id: WorkflowId::new(uuid::Uuid::nil()),
        }
    }

    fn payload(label: &str) -> Result<Payload, crate::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn workflow_started(seq: u64) -> Result<Event, crate::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(seq),
            workflow_type: String::from("checkout"),
            input: payload("input")?,
            run_id: RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: crate::PackageVersion::new("a".repeat(64)),
        })
    }

    fn workflow_error(message: &str) -> WorkflowError {
        WorkflowError {
            message: String::from(message),
            details: None,
        }
    }

    #[test]
    fn empty_history_projects_to_running() {
        assert_eq!(status_from_events(&[]), WorkflowStatus::Running);
    }

    #[test]
    fn replacement_start_projects_continue_as_new_chain_running() -> Result<(), crate::PayloadError>
    {
        let parent_run_id = RunId::new(uuid::Uuid::from_u128(7));
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: payload("replacement")?,
                workflow_type: None,
                parent_run_id: parent_run_id.clone(),
            },
            Event::WorkflowStarted {
                envelope: envelope(3),
                workflow_type: String::from("checkout"),
                input: payload("replacement")?,
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: Some(parent_run_id),
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Running);
        Ok(())
    }

    #[test]
    fn completed_terminal_event_projects_to_completed() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowCompleted {
                envelope: envelope(2),
                result: payload("result")?,
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Completed);
        Ok(())
    }

    #[test]
    fn failed_terminal_event_projects_to_failed() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowFailed {
                envelope: envelope(2),
                error: workflow_error("failed"),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Failed);
        Ok(())
    }

    #[test]
    fn cancelled_terminal_event_projects_to_cancelled() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowCancelled {
                envelope: envelope(2),
                reason: String::from("caller requested cancellation"),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Cancelled);
        Ok(())
    }

    #[test]
    fn timed_out_terminal_event_projects_to_timed_out() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowTimedOut {
                envelope: envelope(2),
                timeout: String::from("execution"),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::TimedOut);
        Ok(())
    }

    #[test]
    fn continued_as_new_projects_status() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: payload("continued-input")?,
                workflow_type: Some(String::from("checkout-v2")),
                parent_run_id: RunId::new(uuid::Uuid::from_u128(2)),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::ContinuedAsNew);
        Ok(())
    }

    #[test]
    fn workflow_status_terminality_classifies_running_and_terminal_statuses() {
        assert!(!WorkflowStatus::Running.is_terminal());
        assert!(WorkflowStatus::Completed.is_terminal());
        assert!(WorkflowStatus::Failed.is_terminal());
        assert!(WorkflowStatus::Cancelled.is_terminal());
        assert!(WorkflowStatus::TimedOut.is_terminal());
        assert!(WorkflowStatus::ContinuedAsNew.is_terminal());
    }

    #[test]
    fn started_then_continued_as_new_projects_status() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: payload("continued-input")?,
                workflow_type: None,
                parent_run_id: RunId::new(uuid::Uuid::from_u128(3)),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::ContinuedAsNew);
        Ok(())
    }

    #[test]
    fn non_terminal_history_projects_to_running() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::SearchAttributesUpdated {
                envelope: envelope(2),
                workflow_id: WorkflowId::new(uuid::Uuid::nil()),
                attributes: HashMap::from([(
                    String::from("customer_id"),
                    SearchAttributeValue::String(String::from("customer-123")),
                )]),
            },
            Event::ActivityScheduled {
                envelope: envelope(3),
                activity_id: ActivityId::from_sequence_position(3),
                activity_type: String::from("charge-card"),
                input: payload("activity-input")?,
                task_queue: String::from("default"),
                node: None,
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Running);
        Ok(())
    }

    #[test]
    fn schedule_events_do_not_change_workflow_status() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::SchedulePaused {
                envelope: envelope(2),
                schedule_id: ScheduleId::new(uuid::Uuid::from_u128(2)),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Running);
        Ok(())
    }

    #[test]
    fn projection_is_deterministic() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowCompleted {
                envelope: envelope(2),
                result: payload("result")?,
            },
        ];

        let first = status_from_events(&events);
        let second = status_from_events(&events);

        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn last_terminal_lifecycle_event_determines_status() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowCompleted {
                envelope: envelope(2),
                result: payload("result")?,
            },
            Event::WorkflowTimedOut {
                envelope: envelope(3),
                timeout: String::from("execution"),
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::TimedOut);
        Ok(())
    }

    fn run_id() -> RunId {
        RunId::new(uuid::Uuid::from_u128(1))
    }

    fn workflow_reopened(seq: u64, reopened: Vec<ActivityId>) -> Event {
        Event::WorkflowReopened {
            envelope: envelope(seq),
            run_id: run_id(),
            reopened,
        }
    }

    #[test]
    fn reopen_after_failure_projects_running() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowFailed {
                envelope: envelope(2),
                error: workflow_error("transient"),
            },
            workflow_reopened(3, vec![ActivityId::from_sequence_position(2)]),
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Running);
        Ok(())
    }

    #[test]
    fn reopen_then_new_terminal_projects_that_terminal() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowFailed {
                envelope: envelope(2),
                error: workflow_error("transient"),
            },
            workflow_reopened(3, vec![ActivityId::from_sequence_position(2)]),
            Event::WorkflowCompleted {
                envelope: envelope(4),
                result: payload("result")?,
            },
        ];

        assert_eq!(status_from_events(&events), WorkflowStatus::Completed);
        Ok(())
    }

    #[test]
    fn current_lease_terminal_is_none_for_running_and_reopened()
    -> Result<(), Box<dyn std::error::Error>> {
        let running = vec![workflow_started(1)?];
        assert!(current_lease_terminal(&running).is_none());

        let failed = vec![
            workflow_started(1)?,
            Event::WorkflowFailed {
                envelope: envelope(2),
                error: workflow_error("boom"),
            },
        ];
        assert!(matches!(
            current_lease_terminal(&failed),
            Some(Event::WorkflowFailed { .. })
        ));

        let mut reopened = failed.clone();
        reopened.push(workflow_reopened(
            3,
            vec![ActivityId::from_sequence_position(2)],
        ));
        assert!(
            current_lease_terminal(&reopened).is_none(),
            "a reopened run has no current-lease terminal"
        );
        Ok(())
    }

    #[test]
    fn current_lease_terminal_returns_terminal_after_reopen_and_retermination()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            Event::WorkflowFailed {
                envelope: envelope(2),
                error: workflow_error("boom"),
            },
            workflow_reopened(3, vec![ActivityId::from_sequence_position(2)]),
            Event::WorkflowCompleted {
                envelope: envelope(4),
                result: payload("result")?,
            },
        ];

        assert!(matches!(
            current_lease_terminal(&events),
            Some(Event::WorkflowCompleted { .. })
        ));
        Ok(())
    }

    fn workflow_paused(seq: u64) -> Event {
        Event::WorkflowPaused {
            envelope: envelope(seq),
            run_id: run_id(),
            reason: None,
            operator: None,
        }
    }

    fn workflow_resumed(seq: u64) -> Event {
        Event::WorkflowResumed {
            envelope: envelope(seq),
            run_id: run_id(),
            operator: None,
        }
    }

    #[test]
    fn pause_projects_paused_and_is_non_terminal() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![workflow_started(1)?, workflow_paused(2)];
        assert_eq!(status_from_events(&events), WorkflowStatus::Paused);
        assert!(!WorkflowStatus::Paused.is_terminal());
        // Paused is non-terminal, so the run has no current-lease terminal and can
        // still complete/fail/cancel.
        assert!(current_lease_terminal(&events).is_none());
        Ok(())
    }

    #[test]
    fn resume_after_pause_projects_running() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            workflow_paused(2),
            workflow_resumed(3),
        ];
        assert_eq!(status_from_events(&events), WorkflowStatus::Running);
        assert!(current_lease_terminal(&events).is_none());
        Ok(())
    }

    #[test]
    fn complete_after_resume_projects_completed() -> Result<(), Box<dyn std::error::Error>> {
        let events = vec![
            workflow_started(1)?,
            workflow_paused(2),
            workflow_resumed(3),
            Event::WorkflowCompleted {
                envelope: envelope(4),
                result: payload("result")?,
            },
        ];
        assert_eq!(status_from_events(&events), WorkflowStatus::Completed);
        assert!(matches!(
            current_lease_terminal(&events),
            Some(Event::WorkflowCompleted { .. })
        ));
        Ok(())
    }

    #[test]
    fn complete_while_paused_still_projects_completed() -> Result<(), Box<dyn std::error::Error>> {
        // The drain case: a paused run reaches a terminal via drained work.
        // Last-lifecycle-event-wins projects the terminal, not Paused.
        let events = vec![
            workflow_started(1)?,
            workflow_paused(2),
            Event::WorkflowCompleted {
                envelope: envelope(3),
                result: payload("result")?,
            },
        ];
        assert_eq!(status_from_events(&events), WorkflowStatus::Completed);
        Ok(())
    }

    #[test]
    fn run_segment_scopes_to_the_named_run() -> Result<(), Box<dyn std::error::Error>> {
        let first_run = RunId::new(uuid::Uuid::from_u128(10));
        let second_run = RunId::new(uuid::Uuid::from_u128(20));
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
                run_id: first_run.clone(),
                parent_run_id: None,
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2),
                input: payload("again")?,
                workflow_type: None,
                parent_run_id: first_run.clone(),
            },
            Event::WorkflowStarted {
                envelope: envelope(3),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
                run_id: second_run.clone(),
                parent_run_id: Some(first_run.clone()),
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowCompleted {
                envelope: envelope(4),
                result: payload("result")?,
            },
        ];

        let first = run_segment(&events, &first_run);
        assert_eq!(first.len(), 2, "first run spans its start through its CAN");
        assert!(matches!(
            current_lease_terminal(first),
            Some(Event::WorkflowContinuedAsNew { .. })
        ));

        let second = run_segment(&events, &second_run);
        assert_eq!(
            second.len(),
            2,
            "second run spans its start through completion"
        );
        assert!(matches!(
            current_lease_terminal(second),
            Some(Event::WorkflowCompleted { .. })
        ));

        assert!(
            run_segment(&events, &RunId::new(uuid::Uuid::from_u128(99))).is_empty(),
            "absent run yields an empty segment"
        );
        Ok(())
    }
}
