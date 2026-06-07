//! Workflow status projection from authoritative event history.

use serde::{Deserialize, Serialize};

use crate::Event;

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
}

/// Projects workflow status from an event history.
///
/// The last terminal workflow lifecycle event determines the projected status.
/// Histories without a terminal workflow event are considered running.
#[must_use]
pub fn status_from_events(events: &[Event]) -> WorkflowStatus {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::WorkflowCompleted { .. } => Some(WorkflowStatus::Completed),
            Event::WorkflowFailed { .. } => Some(WorkflowStatus::Failed),
            Event::WorkflowCancelled { .. } => Some(WorkflowStatus::Cancelled),
            Event::WorkflowTimedOut { .. } => Some(WorkflowStatus::TimedOut),
            Event::WorkflowStarted { .. }
            | Event::SearchAttributesUpdated { .. }
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
            | Event::ChildWorkflowCancelled { .. } => None,
        })
        .unwrap_or(WorkflowStatus::Running)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{WorkflowStatus, status_from_events};
    use crate::{
        ActivityId, Event, EventEnvelope, Payload, SearchAttributeValue, WorkflowError, WorkflowId,
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
}
