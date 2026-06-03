//! Workflow query filters and lightweight workflow summaries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Event, WorkflowId, WorkflowStatus, status_from_events};

/// Query input for listing workflow executions.
///
/// A default filter has every field unset and matches all workflow summaries.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowFilter {
    /// Match workflows with this workflow type exactly.
    pub workflow_type: Option<String>,
    /// Match workflows whose status projection equals this status.
    pub status: Option<WorkflowStatus>,
    /// Match workflows started at or after this timestamp.
    pub started_after: Option<DateTime<Utc>>,
    /// Match workflows started at or before this timestamp.
    pub started_before: Option<DateTime<Utc>>,
    /// Match workflows started as children of this parent workflow.
    pub parent: Option<WorkflowId>,
}

impl WorkflowFilter {
    /// Returns whether a summary satisfies all constraints in this filter.
    #[must_use]
    pub fn matches(&self, summary: &WorkflowSummary) -> bool {
        self.matches_workflow_type(summary)
            && self.matches_status(summary)
            && self.matches_started_after(summary)
            && self.matches_started_before(summary)
            && self.matches_parent(summary)
    }

    fn matches_workflow_type(&self, summary: &WorkflowSummary) -> bool {
        self.workflow_type
            .as_ref()
            .is_none_or(|workflow_type| workflow_type == &summary.workflow_type)
    }

    fn matches_status(&self, summary: &WorkflowSummary) -> bool {
        self.status.is_none_or(|status| status == summary.status)
    }

    fn matches_started_after(&self, summary: &WorkflowSummary) -> bool {
        self.started_after
            .is_none_or(|started_after| summary.started_at >= started_after)
    }

    fn matches_started_before(&self, summary: &WorkflowSummary) -> bool {
        self.started_before
            .is_none_or(|started_before| summary.started_at <= started_before)
    }

    fn matches_parent(&self, summary: &WorkflowSummary) -> bool {
        self.parent
            .as_ref()
            .is_none_or(|parent| summary.parent.as_ref() == Some(parent))
    }
}

/// Lightweight projection of a workflow execution for query results.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WorkflowSummary {
    /// Workflow execution identifier.
    pub workflow_id: WorkflowId,
    /// Workflow type recorded when the execution started.
    pub workflow_type: String,
    /// Status projected from authoritative workflow history.
    pub status: WorkflowStatus,
    /// Timestamp recorded on the workflow start event.
    pub started_at: DateTime<Utc>,
    /// Timestamp recorded on the terminal lifecycle event, if any.
    pub ended_at: Option<DateTime<Utc>>,
    /// Parent workflow identifier for child-workflow executions, if the store has one.
    pub parent: Option<WorkflowId>,
}

impl WorkflowSummary {
    /// Builds a workflow summary from a workflow event history.
    ///
    /// Returns [`None`] when the history does not contain a
    /// [`Event::WorkflowStarted`] event. The projected status and end timestamp
    /// are derived from the last terminal workflow lifecycle event in the
    /// history, matching [`status_from_events`]. Parent linkage is not present in
    /// a child workflow's own history, so this helper leaves `parent` unset.
    #[must_use]
    pub fn from_history(events: &[Event]) -> Option<Self> {
        let (workflow_id, workflow_type, started_at) = events.iter().find_map(|event| {
            if let Event::WorkflowStarted {
                envelope,
                workflow_type,
                ..
            } = event
            {
                Some((
                    envelope.workflow_id.clone(),
                    workflow_type.clone(),
                    envelope.recorded_at,
                ))
            } else {
                None
            }
        })?;

        Some(Self {
            workflow_id,
            workflow_type,
            status: status_from_events(events),
            started_at,
            ended_at: terminal_recorded_at(events),
            parent: None,
        })
    }
}

fn terminal_recorded_at(events: &[Event]) -> Option<DateTime<Utc>> {
    events.iter().rev().find_map(|event| match event {
        Event::WorkflowCompleted { envelope, .. }
        | Event::WorkflowFailed { envelope, .. }
        | Event::WorkflowCancelled { envelope, .. }
        | Event::WorkflowTimedOut { envelope, .. } => Some(envelope.recorded_at),
        Event::WorkflowStarted { .. }
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
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{WorkflowFilter, WorkflowSummary};
    use crate::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};

    fn recorded_at(offset_seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: recorded_at(i64::try_from(seq).unwrap_or(0)),
            workflow_id: workflow_id.clone(),
        }
    }

    fn payload(label: &str) -> Result<Payload, crate::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn summary(
        workflow_type: &str,
        status: WorkflowStatus,
        started_at: DateTime<Utc>,
        parent: Option<WorkflowId>,
    ) -> WorkflowSummary {
        WorkflowSummary {
            workflow_id: WorkflowId::new(uuid::Uuid::from_u128(1)),
            workflow_type: String::from(workflow_type),
            status,
            started_at,
            ended_at: None,
            parent,
        }
    }

    #[test]
    fn default_filter_matches_all_summaries() {
        let parent = WorkflowId::new(uuid::Uuid::from_u128(2));
        let summaries = [
            summary(
                "checkout",
                WorkflowStatus::Running,
                recorded_at(1),
                Some(parent),
            ),
            summary("billing", WorkflowStatus::Completed, recorded_at(2), None),
        ];
        let filter = WorkflowFilter::default();

        assert!(summaries.iter().all(|summary| filter.matches(summary)));
    }

    #[test]
    fn workflow_type_filter_matches_exact_type() {
        let filter = WorkflowFilter {
            workflow_type: Some(String::from("checkout")),
            ..WorkflowFilter::default()
        };

        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(1),
            None
        )));
        assert!(!filter.matches(&summary(
            "billing",
            WorkflowStatus::Running,
            recorded_at(1),
            None
        )));
    }

    #[test]
    fn status_filter_matches_projected_status() {
        let filter = WorkflowFilter {
            status: Some(WorkflowStatus::Completed),
            ..WorkflowFilter::default()
        };

        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Completed,
            recorded_at(1),
            None
        )));
        assert!(!filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(1),
            None
        )));
    }

    #[test]
    fn started_after_filter_matches_start_time_inclusively() {
        let filter = WorkflowFilter {
            started_after: Some(recorded_at(10)),
            ..WorkflowFilter::default()
        };

        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(10),
            None
        )));
        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(11),
            None
        )));
        assert!(!filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(9),
            None
        )));
    }

    #[test]
    fn started_before_filter_matches_start_time_inclusively() {
        let filter = WorkflowFilter {
            started_before: Some(recorded_at(20)),
            ..WorkflowFilter::default()
        };

        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(19),
            None
        )));
        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(20),
            None
        )));
        assert!(!filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(21),
            None
        )));
    }

    #[test]
    fn parent_filter_matches_parent_workflow_id() {
        let parent = WorkflowId::new(uuid::Uuid::from_u128(2));
        let other_parent = WorkflowId::new(uuid::Uuid::from_u128(3));
        let filter = WorkflowFilter {
            parent: Some(parent.clone()),
            ..WorkflowFilter::default()
        };

        assert!(filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(1),
            Some(parent)
        )));
        assert!(!filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(1),
            Some(other_parent)
        )));
        assert!(!filter.matches(&summary(
            "checkout",
            WorkflowStatus::Running,
            recorded_at(1),
            None
        )));
    }

    #[test]
    fn combined_filter_requires_every_field_to_match() {
        let parent = WorkflowId::new(uuid::Uuid::from_u128(2));
        let filter = WorkflowFilter {
            workflow_type: Some(String::from("checkout")),
            status: Some(WorkflowStatus::Completed),
            started_after: Some(recorded_at(10)),
            started_before: Some(recorded_at(20)),
            parent: Some(parent.clone()),
        };
        let matching_summary = summary(
            "checkout",
            WorkflowStatus::Completed,
            recorded_at(15),
            Some(parent.clone()),
        );

        assert!(filter.matches(&matching_summary));
        assert!(!filter.matches(&WorkflowSummary {
            workflow_type: String::from("billing"),
            ..matching_summary.clone()
        }));
        assert!(!filter.matches(&WorkflowSummary {
            status: WorkflowStatus::Running,
            ..matching_summary.clone()
        }));
        assert!(!filter.matches(&WorkflowSummary {
            started_at: recorded_at(9),
            ..matching_summary.clone()
        }));
        assert!(!filter.matches(&WorkflowSummary {
            started_at: recorded_at(21),
            ..matching_summary.clone()
        }));
        assert!(!filter.matches(&WorkflowSummary {
            parent: None,
            ..matching_summary
        }));
    }

    #[test]
    fn summary_from_history_projects_required_fields() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1, &workflow_id),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
            },
            Event::WorkflowCompleted {
                envelope: envelope(2, &workflow_id),
                result: payload("result")?,
            },
        ];

        let Some(summary) = WorkflowSummary::from_history(&events) else {
            return Err("history should contain workflow start".into());
        };

        assert_eq!(summary.workflow_id, workflow_id);
        assert_eq!(summary.workflow_type, "checkout");
        assert_eq!(summary.status, WorkflowStatus::Completed);
        assert_eq!(summary.started_at, recorded_at(1));
        assert_eq!(summary.ended_at, Some(recorded_at(2)));
        assert_eq!(summary.parent, None);
        Ok(())
    }

    #[test]
    fn summary_from_history_without_start_returns_none() {
        assert!(WorkflowSummary::from_history(&[]).is_none());
    }

    #[test]
    fn summary_end_time_uses_last_terminal_lifecycle_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1, &workflow_id),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
            },
            Event::WorkflowCompleted {
                envelope: envelope(2, &workflow_id),
                result: payload("result")?,
            },
            Event::WorkflowTimedOut {
                envelope: envelope(3, &workflow_id),
                timeout: String::from("execution"),
            },
        ];

        let Some(summary) = WorkflowSummary::from_history(&events) else {
            return Err("history should contain workflow start".into());
        };

        assert_eq!(summary.status, WorkflowStatus::TimedOut);
        assert_eq!(summary.ended_at, Some(recorded_at(3)));
        Ok(())
    }
}
