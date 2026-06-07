//! Workflow history events and their deterministic recording envelope.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ActivityError, ActivityId, Payload, RunId, ScheduleConfig, ScheduleId, SearchAttributeValue,
    TimerId, WorkflowError, WorkflowId,
};

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
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
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
        /// Concrete run identifier started by this event.
        run_id: RunId,
        /// Parent run that continued as this run, when this start is part of a
        /// continue-as-new chain.
        parent_run_id: Option<RunId>,
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
    /// Workflow search attributes were updated for visibility and query projection.
    SearchAttributesUpdated {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Workflow whose search attributes changed.
        workflow_id: WorkflowId,
        /// Updated search attributes keyed by attribute name.
        attributes: HashMap<String, SearchAttributeValue>,
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
    /// A schedule resource was created.
    ScheduleCreated {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Schedule resource that was created.
        schedule_id: ScheduleId,
        /// Persisted schedule configuration.
        config: ScheduleConfig,
    },
    /// A schedule resource was updated.
    ScheduleUpdated {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Schedule resource that was updated.
        schedule_id: ScheduleId,
        /// Updated schedule configuration.
        config: ScheduleConfig,
    },
    /// A schedule resource was paused.
    SchedulePaused {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Schedule resource that was paused.
        schedule_id: ScheduleId,
    },
    /// A paused schedule resource was resumed.
    ScheduleResumed {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Schedule resource that was resumed.
        schedule_id: ScheduleId,
    },
    /// A schedule resource was deleted.
    ScheduleDeleted {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Schedule resource that was deleted.
        schedule_id: ScheduleId,
    },
    /// A schedule tick started a workflow execution.
    ScheduleTriggered {
        /// Recording metadata for this event.
        envelope: EventEnvelope,
        /// Schedule resource that fired.
        schedule_id: ScheduleId,
        /// Workflow execution started by the schedule tick.
        workflow_id: WorkflowId,
        /// Run started by the schedule tick.
        run_id: RunId,
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
            | Self::SearchAttributesUpdated { envelope, .. }
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
            | Self::ChildWorkflowCancelled { envelope, .. }
            | Self::ScheduleCreated { envelope, .. }
            | Self::ScheduleUpdated { envelope, .. }
            | Self::SchedulePaused { envelope, .. }
            | Self::ScheduleResumed { envelope, .. }
            | Self::ScheduleDeleted { envelope, .. }
            | Self::ScheduleTriggered { envelope, .. } => envelope,
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
    use std::collections::HashMap;

    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{Event, EventEnvelope};
    use crate::{
        ActivityError, ActivityErrorKind, ActivityId, CatchUpPolicy, OverlapPolicy, Payload, RunId,
        ScheduleConfig, ScheduleId, SearchAttributeValue, TimerId, TriggerSpec, WorkflowError,
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

    fn schedule_config(label: &str) -> Result<ScheduleConfig, crate::PayloadError> {
        Ok(ScheduleConfig {
            trigger: TriggerSpec::Cron {
                expression: String::from("0 0 * * *"),
            },
            overlap_policy: OverlapPolicy::Skip,
            catch_up_policy: CatchUpPolicy::One,
            workflow_type: String::from("checkout"),
            input: payload(label)?,
        })
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
            run_id: RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
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
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
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
            Event::ActivityScheduled {
                envelope: envelope(6),
                activity_id: ActivityId::from_sequence_position(6),
                activity_type: String::from("charge-card"),
                input: payload("activity-input")?,
            },
            Event::ActivityStarted {
                envelope: envelope(7),
                activity_id: ActivityId::from_sequence_position(6),
            },
            Event::ActivityCompleted {
                envelope: envelope(8),
                activity_id: ActivityId::from_sequence_position(6),
                result: payload("activity-result")?,
            },
            Event::ActivityFailed {
                envelope: envelope(9),
                activity_id: ActivityId::from_sequence_position(6),
                error: activity_error(ActivityErrorKind::Retryable, "temporary outage"),
                attempt: 1,
            },
            Event::ActivityCancelled {
                envelope: envelope(10),
                activity_id: ActivityId::from_sequence_position(6),
            },
            Event::TimerStarted {
                envelope: envelope(11),
                timer_id: TimerId::anonymous(11),
                fire_at,
            },
            Event::TimerFired {
                envelope: envelope(12),
                timer_id: TimerId::anonymous(11),
            },
            Event::TimerCancelled {
                envelope: envelope(13),
                timer_id: TimerId::named("reminder")?,
            },
            Event::SignalReceived {
                envelope: envelope(14),
                name: String::from("approve"),
                payload: payload("signal")?,
            },
            Event::ChildWorkflowStarted {
                envelope: envelope(15),
                child_workflow_id: child_workflow_id.clone(),
                workflow_type: String::from("fulfillment"),
                input: payload("child-input")?,
            },
            Event::ChildWorkflowCompleted {
                envelope: envelope(16),
                child_workflow_id: child_workflow_id.clone(),
                result: payload("child-result")?,
            },
            Event::ChildWorkflowFailed {
                envelope: envelope(17),
                child_workflow_id: child_workflow_id.clone(),
                error: workflow_error("child failed"),
            },
            Event::ChildWorkflowCancelled {
                envelope: envelope(18),
                child_workflow_id,
            },
        ];

        for event in events {
            round_trip(&event)?;
        }
        Ok(())
    }

    #[test]
    fn extended_events_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(2));
        let triggered_workflow_id = WorkflowId::new(uuid::Uuid::from_u128(3));
        let triggered_run_id = RunId::new(uuid::Uuid::from_u128(4));
        let events = vec![
            Event::WorkflowContinuedAsNew {
                envelope: envelope(19),
                input: payload("continued-input")?,
                workflow_type: Some(String::from("checkout-v2")),
                parent_run_id: RunId::new(uuid::Uuid::from_u128(2)),
            },
            Event::SearchAttributesUpdated {
                envelope: envelope(20),
                workflow_id: WorkflowId::new(uuid::Uuid::nil()),
                attributes: HashMap::from([(
                    String::from("customer_id"),
                    SearchAttributeValue::String(String::from("cust-123")),
                )]),
            },
            Event::ScheduleCreated {
                envelope: envelope(20),
                schedule_id: schedule_id.clone(),
                config: schedule_config("schedule-created")?,
            },
            Event::ScheduleUpdated {
                envelope: envelope(21),
                schedule_id: schedule_id.clone(),
                config: schedule_config("schedule-updated")?,
            },
            Event::SchedulePaused {
                envelope: envelope(22),
                schedule_id: schedule_id.clone(),
            },
            Event::ScheduleResumed {
                envelope: envelope(23),
                schedule_id: schedule_id.clone(),
            },
            Event::ScheduleDeleted {
                envelope: envelope(24),
                schedule_id: schedule_id.clone(),
            },
            Event::ScheduleTriggered {
                envelope: envelope(25),
                schedule_id,
                workflow_id: triggered_workflow_id,
                run_id: triggered_run_id,
            },
        ];

        for event in events {
            round_trip(&event)?;
        }
        Ok(())
    }
}
