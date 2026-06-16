//! Server-side selector filtering for filtered subscriptions.
//!
//! `FilteredSubscription` advertises optional `workflow_type` and `status`
//! selectors. The engine's `EventFilter` has no type or status dimension, so
//! the selection runs at the socket seam, after the namespace gate proved
//! ownership and resolved the workflow's recorded type from the same durable
//! read.
//!
//! Selector semantics (documented in `docs/API.md`):
//!
//! - `workflow_type` matches when the event's workflow has that recorded type
//!   at the time the namespace gate resolved it: the initial durable read
//!   returns the head-of-history `WorkflowStarted` type at read time, which on
//!   a continue-as-new chain can briefly run ahead of an older delivered event
//!   (a one-event-loop forward-skew window) until the stream's own
//!   `WorkflowStarted` refresh self-heals the cached type. A workflow whose
//!   history records no started run never matches a type selector.
//! - `status` matches per event kind: each terminal lifecycle event matches
//!   exactly its projected status (`WorkflowCompleted` → `Completed`,
//!   `WorkflowFailed` → `Failed`, `WorkflowCancelled` → `Cancelled`,
//!   `WorkflowTimedOut` → `TimedOut`, `WorkflowContinuedAsNew` →
//!   `ContinuedAsNew`); every non-terminal event — including
//!   `WorkflowStarted` — matches `Running`.
//! - When both selectors are present they AND together.

use aion_core::{Event, WorkflowStatus};

/// Validated subscription selectors applied before frame encoding.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SubscriptionSelector {
    /// Deliver only events of workflows with this recorded type.
    pub workflow_type: Option<String>,
    /// Deliver only events whose kind projects to this status.
    pub status: Option<WorkflowStatus>,
}

impl SubscriptionSelector {
    /// Selector that admits every event (per-workflow and firehose
    /// subscriptions carry no selectors).
    #[must_use]
    pub const fn unrestricted() -> Self {
        Self {
            workflow_type: None,
            status: None,
        }
    }

    /// Decide whether an event passes the selector. `workflow_type` is the
    /// event's workflow's recorded type as resolved by the namespace gate.
    #[must_use]
    pub fn matches(&self, event: &Event, workflow_type: Option<&str>) -> bool {
        if let Some(selected_type) = &self.workflow_type {
            // No recorded type (no started run) can never satisfy a type
            // selector — absence is not a wildcard.
            if workflow_type != Some(selected_type.as_str()) {
                return false;
            }
        }
        if let Some(selected_status) = self.status {
            if event_status(event) != selected_status {
                return false;
            }
        }
        true
    }
}

/// Status projected by a single event's kind: terminal lifecycle events
/// project exactly their terminal status; every other event belongs to a
/// running workflow at the moment it was recorded.
const fn event_status(event: &Event) -> WorkflowStatus {
    match event {
        Event::WorkflowCompleted { .. } => WorkflowStatus::Completed,
        Event::WorkflowFailed { .. } => WorkflowStatus::Failed,
        Event::WorkflowCancelled { .. } => WorkflowStatus::Cancelled,
        Event::WorkflowTimedOut { .. } => WorkflowStatus::TimedOut,
        Event::WorkflowContinuedAsNew { .. } => WorkflowStatus::ContinuedAsNew,
        Event::WorkflowStarted { .. }
        // A reopen returns the workflow to Running at the moment it is recorded.
        | Event::WorkflowReopened { .. }
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
        | Event::ScheduleTriggered { .. } => WorkflowStatus::Running,
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};

    use super::SubscriptionSelector;

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: WorkflowId::new(uuid::Uuid::from_u128(1)),
        }
    }

    fn payload() -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&serde_json::json!({ "label": "x" }))
    }

    fn signal(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::SignalReceived {
            envelope: envelope(seq),
            name: "ship".to_owned(),
            payload: payload()?,
        })
    }

    fn started(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(seq),
            workflow_type: "checkout".to_owned(),
            input: payload()?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn completed(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowCompleted {
            envelope: envelope(seq),
            result: payload()?,
        })
    }

    fn failed(seq: u64) -> Event {
        Event::WorkflowFailed {
            envelope: envelope(seq),
            error: aion_core::WorkflowError {
                message: "boom".to_owned(),
                details: None,
            },
        }
    }

    #[test]
    fn unrestricted_selector_matches_everything() -> Result<(), Box<dyn std::error::Error>> {
        let selector = SubscriptionSelector::unrestricted();

        assert!(selector.matches(&signal(1)?, None));
        assert!(selector.matches(&completed(2)?, Some("checkout")));
        Ok(())
    }

    #[test]
    fn type_selector_matches_only_the_recorded_type() -> Result<(), Box<dyn std::error::Error>> {
        let selector = SubscriptionSelector {
            workflow_type: Some("checkout".to_owned()),
            status: None,
        };

        assert!(selector.matches(&signal(1)?, Some("checkout")));
        assert!(!selector.matches(&signal(1)?, Some("fulfillment")));
        assert!(
            !selector.matches(&signal(1)?, None),
            "a workflow with no recorded type never matches a type selector"
        );
        Ok(())
    }

    #[test]
    fn status_selector_matches_per_event_kind() -> Result<(), Box<dyn std::error::Error>> {
        let running = SubscriptionSelector {
            workflow_type: None,
            status: Some(WorkflowStatus::Running),
        };
        let completed_only = SubscriptionSelector {
            workflow_type: None,
            status: Some(WorkflowStatus::Completed),
        };
        let failed_only = SubscriptionSelector {
            workflow_type: None,
            status: Some(WorkflowStatus::Failed),
        };

        // Running matches every non-terminal event, including WorkflowStarted.
        assert!(running.matches(&started(1)?, Some("checkout")));
        assert!(running.matches(&signal(2)?, Some("checkout")));
        assert!(!running.matches(&completed(3)?, Some("checkout")));

        // Each terminal status matches exactly its terminal event kind.
        assert!(completed_only.matches(&completed(3)?, Some("checkout")));
        assert!(!completed_only.matches(&failed(3), Some("checkout")));
        assert!(!completed_only.matches(&signal(2)?, Some("checkout")));
        assert!(failed_only.matches(&failed(3), Some("checkout")));
        assert!(!failed_only.matches(&completed(3)?, Some("checkout")));
        Ok(())
    }

    #[test]
    fn combined_selectors_and_together() -> Result<(), Box<dyn std::error::Error>> {
        let selector = SubscriptionSelector {
            workflow_type: Some("checkout".to_owned()),
            status: Some(WorkflowStatus::Completed),
        };

        assert!(selector.matches(&completed(3)?, Some("checkout")));
        assert!(
            !selector.matches(&completed(3)?, Some("fulfillment")),
            "matching status with mismatched type must not pass"
        );
        assert!(
            !selector.matches(&signal(2)?, Some("checkout")),
            "matching type with mismatched status must not pass"
        );
        Ok(())
    }
}
