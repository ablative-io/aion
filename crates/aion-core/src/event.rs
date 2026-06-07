//! Workflow history events and their deterministic recording envelope.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{ActivityError, ActivityId, Payload, RunId, TimerId, WorkflowError, WorkflowId};

/// Metadata recorded with every workflow history event.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct EventEnvelope {
    /// Monotonic sequence number within the owning workflow history.
    pub seq: u64,
    /// Recorded UTC timestamp for this event.
    ///
    /// This timestamp is the determinism source for `workflow.now`; replay must use the recorded
    /// value rather than consulting wall-clock time.
    pub recorded_at: DateTime<Utc>,
    /// Workflow history that owns this event.
    pub workflow_id: WorkflowId,
}

/// A recorded workflow history event.
///
/// User data is carried as opaque [`Payload`] values, while failures use the closed workflow and
/// activity error types from this crate.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum Event {
    /// A workflow execution started with a type name and input payload.
    WorkflowStarted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Workflow type selected by the caller.
        workflow_type: String,
        /// Opaque workflow input payload.
        input: Payload,
    },
    /// A workflow execution completed successfully; this terminal event projects to Completed.
    WorkflowCompleted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Opaque workflow result payload.
        result: Payload,
    },
    /// A workflow execution failed terminally; this terminal event projects to Failed.
    WorkflowFailed {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Terminal workflow failure.
        error: WorkflowError,
    },
    /// A workflow execution was cancelled; this terminal event projects to Cancelled.
    WorkflowCancelled {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Human-readable cancellation reason.
        reason: String,
    },
    /// A workflow execution timed out; this terminal event projects to `TimedOut`.
    WorkflowTimedOut {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Descriptor identifying the timeout that elapsed.
        ///
        /// Intentionally stringly-typed: the closed set of timeout kinds is defined by cluster AT
        /// (timers and signals), not by the core event model.
        timeout: String,
    },
    /// A workflow execution continued as a new run; this terminal event projects to
    /// `ContinuedAsNew`.
    WorkflowContinuedAsNew {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Opaque workflow input payload carried into the new run.
        input: Payload,
        /// Workflow type override for the new run, when migration changes the workflow type.
        ///
        /// When absent, the new run uses the current workflow type.
        workflow_type: Option<String>,
        /// Run identifier for the current run that is being continued.
        parent_run_id: RunId,
    },
    /// An activity was scheduled by workflow code.
    ActivityScheduled {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Deterministic activity identifier derived from the scheduling sequence position.
        activity_id: ActivityId,
        /// Activity type selected by workflow code.
        activity_type: String,
        /// Opaque activity input payload.
        input: Payload,
    },
    /// An activity worker started executing an activity attempt.
    ActivityStarted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Activity being executed.
        activity_id: ActivityId,
    },
    /// An activity completed successfully.
    ActivityCompleted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Activity that produced the result.
        activity_id: ActivityId,
        /// Opaque activity result payload.
        result: Payload,
    },
    /// An activity attempt failed.
    ///
    /// The `attempt` field together with [`ActivityError`]'s retryable or terminal classification
    /// lets replay distinguish a retryable interim failure from a terminal one for the same
    /// [`ActivityId`].
    ActivityFailed {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Activity whose attempt failed.
        activity_id: ActivityId,
        /// Classified activity failure.
        error: ActivityError,
        /// One-based activity attempt number that produced this failure.
        attempt: u32,
    },
    /// An activity was cancelled as an explicit cancellation outcome.
    ActivityCancelled {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Activity that was cancelled.
        activity_id: ActivityId,
    },
    /// A timer was scheduled to fire at a deterministic timestamp.
    TimerStarted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Timer selected by workflow code or assigned by the engine.
        timer_id: TimerId,
        /// UTC timestamp at which the timer becomes eligible to fire.
        fire_at: DateTime<Utc>,
    },
    /// A timer fired.
    TimerFired {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Timer that fired.
        timer_id: TimerId,
    },
    /// A timer was cancelled as an explicit cancellation outcome.
    TimerCancelled {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Timer that was cancelled.
        timer_id: TimerId,
    },
    /// A signal was delivered to the workflow.
    SignalReceived {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Signal name selected by the sender.
        name: String,
        /// Opaque signal payload.
        payload: Payload,
    },
    /// A child workflow was started.
    ChildWorkflowStarted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Child workflow identifier.
        child_workflow_id: WorkflowId,
        /// Child workflow type selected by the parent.
        workflow_type: String,
        /// Opaque child workflow input payload.
        input: Payload,
    },
    /// A child workflow completed successfully.
    ChildWorkflowCompleted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Child workflow that produced the result.
        child_workflow_id: WorkflowId,
        /// Opaque child workflow result payload.
        result: Payload,
    },
    /// A child workflow failed terminally.
    ChildWorkflowFailed {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Child workflow that failed.
        child_workflow_id: WorkflowId,
        /// Terminal child workflow failure.
        error: WorkflowError,
    },
    /// A child workflow was cancelled as an explicit cancellation outcome.
    ChildWorkflowCancelled {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Child workflow that was cancelled.
        child_workflow_id: WorkflowId,
    },
}

impl Event {
    /// Returns the envelope recorded with this event.
    #[must_use]
    pub const fn envelope(&self) -> &EventEnvelope {
        match self {
            Self::WorkflowStarted { envelope, .. }
            | Self::WorkflowCompleted { envelope, .. }
            | Self::WorkflowFailed { envelope, .. }
            | Self::WorkflowCancelled { envelope, .. }
            | Self::WorkflowTimedOut { envelope, .. }
            | Self::WorkflowContinuedAsNew { envelope, .. }
            | Self::ActivityScheduled { envelope, .. }
            | Self::ActivityStarted { envelope, .. }
            | Self::ActivityCompleted { envelope, .. }
            | Self::ActivityFailed { envelope, .. }
            | Self::ActivityCancelled { envelope, .. }
            | Self::TimerStarted { envelope, .. }
            | Self::TimerFired { envelope, .. }
            | Self::TimerCancelled { envelope, .. }
            | Self::SignalReceived { envelope, .. }
            | Self::ChildWorkflowStarted { envelope, .. }
            | Self::ChildWorkflowCompleted { envelope, .. }
            | Self::ChildWorkflowFailed { envelope, .. }
            | Self::ChildWorkflowCancelled { envelope, .. } => envelope,
        }
    }

    /// Returns the monotonic sequence number recorded for this event.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.envelope().seq
    }

    /// Returns the deterministic recorded timestamp for this event.
    #[must_use]
    pub const fn recorded_at(&self) -> &DateTime<Utc> {
        &self.envelope().recorded_at
    }

    /// Returns the workflow history that owns this event.
    #[must_use]
    pub const fn workflow_id(&self) -> &WorkflowId {
        &self.envelope().workflow_id
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{Event, EventEnvelope};
    use crate::{
        ActivityError, ActivityErrorKind, ActivityId, Payload, RunId, TimerId, WorkflowError,
        WorkflowId,
    };

    fn recorded_at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 123_000_000).unwrap_or_default()
    }

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: recorded_at(),
            workflow_id: WorkflowId::new(uuid::Uuid::nil()),
        }
    }

    fn payload(label: &str) -> Result<Payload, crate::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn workflow_error(message: &str) -> WorkflowError {
        WorkflowError {
            message: String::from(message),
            details: None,
        }
    }

    fn activity_error(kind: ActivityErrorKind, message: &str) -> ActivityError {
        ActivityError {
            kind,
            message: String::from(message),
            details: None,
        }
    }

    fn round_trip(event: &Event) -> Result<(), serde_json::Error> {
        let json = serde_json::to_string(event)?;
        let decoded = serde_json::from_str::<Event>(&json)?;
        assert_eq!(*event, decoded);
        Ok(())
    }

    #[test]
    fn event_accessors_return_envelope_fields() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let recorded_at = recorded_at();
        let envelope = EventEnvelope {
            seq: 17,
            recorded_at,
            workflow_id: workflow_id.clone(),
        };
        let event = Event::WorkflowStarted {
            envelope,
            workflow_type: String::from("checkout"),
            input: payload("input")?,
        };

        assert_eq!(event.seq(), 17);
        assert_eq!(event.recorded_at(), &recorded_at);
        assert_eq!(event.workflow_id(), &workflow_id);
        Ok(())
    }

    #[test]
    fn events_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let child_workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let fire_at = DateTime::from_timestamp(1_700_000_100, 0).unwrap_or_default();
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1),
                workflow_type: String::from("checkout"),
                input: payload("workflow-input")?,
            },
            Event::WorkflowCompleted {
                envelope: envelope(2),
                result: payload("workflow-result")?,
            },
            Event::WorkflowFailed {
                envelope: envelope(3),
                error: workflow_error("workflow failed"),
            },
            Event::WorkflowCancelled {
                envelope: envelope(4),
                reason: String::from("caller requested cancellation"),
            },
            Event::WorkflowTimedOut {
                envelope: envelope(5),
                timeout: String::from("execution"),
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(6),
                input: payload("continued-input")?,
                workflow_type: Some(String::from("checkout-v2")),
                parent_run_id: RunId::new(uuid::Uuid::from_u128(2)),
            },
            Event::ActivityScheduled {
                envelope: envelope(7),
                activity_id: ActivityId::from_sequence_position(7),
                activity_type: String::from("charge-card"),
                input: payload("activity-input")?,
            },
            Event::ActivityStarted {
                envelope: envelope(8),
                activity_id: ActivityId::from_sequence_position(7),
            },
            Event::ActivityCompleted {
                envelope: envelope(9),
                activity_id: ActivityId::from_sequence_position(7),
                result: payload("activity-result")?,
            },
            Event::ActivityFailed {
                envelope: envelope(10),
                activity_id: ActivityId::from_sequence_position(7),
                error: activity_error(ActivityErrorKind::Retryable, "temporary outage"),
                attempt: 1,
            },
            Event::ActivityCancelled {
                envelope: envelope(11),
                activity_id: ActivityId::from_sequence_position(7),
            },
            Event::TimerStarted {
                envelope: envelope(12),
                timer_id: TimerId::anonymous(12),
                fire_at,
            },
            Event::TimerFired {
                envelope: envelope(13),
                timer_id: TimerId::anonymous(12),
            },
            Event::TimerCancelled {
                envelope: envelope(14),
                timer_id: TimerId::named("reminder")?,
            },
            Event::SignalReceived {
                envelope: envelope(15),
                name: String::from("approve"),
                payload: payload("signal")?,
            },
            Event::ChildWorkflowStarted {
                envelope: envelope(16),
                child_workflow_id: child_workflow_id.clone(),
                workflow_type: String::from("fulfillment"),
                input: payload("child-input")?,
            },
            Event::ChildWorkflowCompleted {
                envelope: envelope(17),
                child_workflow_id: child_workflow_id.clone(),
                result: payload("child-result")?,
            },
            Event::ChildWorkflowFailed {
                envelope: envelope(18),
                child_workflow_id: child_workflow_id.clone(),
                error: workflow_error("child failed"),
            },
            Event::ChildWorkflowCancelled {
                envelope: envelope(19),
                child_workflow_id,
            },
        ];

        for event in events {
            round_trip(&event)?;
        }
        Ok(())
    }
}
