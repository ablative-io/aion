//! Workflow query filters and lightweight workflow summaries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ActivityId, Event, WorkflowId, WorkflowStatus, current_lease_terminal, status_from_events,
};

/// Query input for listing workflow executions.
///
/// A default filter has every field unset and matches all workflow summaries.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
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
    /// The step (activity type) that failed, populated ONLY for a workflow whose
    /// current-lease terminal is [`Event::WorkflowFailed`] and whose failure has a
    /// terminal activity failure to attribute it to (e.g. `dev_review`). This is
    /// the workflow *step*, never a brief id or label. `None` for every
    /// healthy/running/completed/cancelled workflow, so list renderers show no
    /// empty failure column for non-failed rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    /// The terminal `WorkflowFailed` error message, populated ONLY for a workflow
    /// whose current-lease terminal is [`Event::WorkflowFailed`]. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl WorkflowSummary {
    /// Builds a workflow summary from a workflow event history.
    ///
    /// Returns [`None`] when the history does not contain a
    /// [`Event::WorkflowStarted`] event. The projected status and end timestamp
    /// are derived from the current run in the history, matching
    /// [`status_from_events`]. Parent linkage is not present in a child
    /// workflow's own history, so this helper leaves `parent` unset.
    ///
    /// `failed_step` and `failure_reason` are populated ONLY when the current
    /// lease's terminal event is [`Event::WorkflowFailed`] (see
    /// [`failure_projection`]); a reopened, running, completed, or cancelled
    /// workflow leaves both `None`.
    #[must_use]
    pub fn from_history(events: &[Event]) -> Option<Self> {
        let (workflow_id, workflow_type, started_at) = events.iter().rev().find_map(|event| {
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

        let (failed_step, failure_reason) = failure_projection(events);
        Some(Self {
            workflow_id,
            workflow_type,
            status: status_from_events(events),
            started_at,
            ended_at: current_lease_terminal(events).map(|event| *event.recorded_at()),
            parent: None,
            failed_step,
            failure_reason,
        })
    }
}

/// Projects `(failed_step, failure_reason)` from a workflow history, reset-aware.
///
/// Returns `(None, None)` unless the current lease's terminal event (per
/// [`current_lease_terminal`], which a later [`Event::WorkflowReopened`]
/// supersedes) is [`Event::WorkflowFailed`]. When it is, `failure_reason` is that
/// event's error message and `failed_step` is the *activity type* of the step
/// that ended in a terminal failure in the current lease with no later success —
/// the actual failed step (e.g. `dev_review`), never a brief id. When a failed
/// workflow has no attributable terminal activity failure (the failure came from
/// workflow code itself), `failed_step` is `None` while `failure_reason` is set.
#[must_use]
pub fn failure_projection(events: &[Event]) -> (Option<String>, Option<String>) {
    let Some(Event::WorkflowFailed { error, .. }) = current_lease_terminal(events) else {
        return (None, None);
    };
    (failed_activity_step(events), Some(error.message.clone()))
}

/// Returns the activity type of the step that ended in a terminal failure in the
/// current lease with no later successful attempt, scanning only the events since
/// the last reset point (run start or reopen) so a superseded prior-lease failure
/// is never attributed. When several activities failed terminally, the latest is
/// reported. Returns `None` when no activity carries a terminal failure.
fn failed_activity_step(events: &[Event]) -> Option<String> {
    // Restrict to the current lease: everything after the last WorkflowStarted or
    // WorkflowReopened. current_lease_terminal already established there is a
    // WorkflowFailed after that reset, so the lease is well-formed.
    let lease_start = events
        .iter()
        .rposition(|event| {
            matches!(
                event,
                Event::WorkflowStarted { .. } | Event::WorkflowReopened { .. }
            )
        })
        .map_or(0, |index| index + 1);
    let lease = &events[lease_start..];

    // Collect the ids that reached a terminal ActivityFailed and the ids that
    // later completed/cancelled successfully; a step counts as failed only if it
    // has no superseding success after its failure.
    let mut latest_failed: Option<(&ActivityId, &str)> = None;
    let mut scheduled_types: std::collections::HashMap<&ActivityId, &str> =
        std::collections::HashMap::new();
    let mut succeeded: std::collections::HashSet<&ActivityId> = std::collections::HashSet::new();
    for event in lease {
        match event {
            Event::ActivityScheduled {
                activity_id,
                activity_type,
                ..
            } => {
                scheduled_types.insert(activity_id, activity_type.as_str());
            }
            Event::ActivityCompleted { activity_id, .. }
            | Event::ActivityCancelled { activity_id, .. } => {
                succeeded.insert(activity_id);
            }
            Event::ActivityFailed { activity_id, .. } => {
                let step = scheduled_types
                    .get(activity_id)
                    .copied()
                    .unwrap_or_default();
                latest_failed = Some((activity_id, step));
            }
            _ => {}
        }
    }

    latest_failed.and_then(|(activity_id, step)| {
        if succeeded.contains(activity_id) || step.is_empty() {
            None
        } else {
            Some(step.to_owned())
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{WorkflowFilter, WorkflowSummary, failure_projection};
    use crate::{
        ActivityError, ActivityErrorKind, ActivityId, Event, EventEnvelope, Payload, RunId,
        ScheduleId, SearchAttributeValue, WorkflowError, WorkflowId, WorkflowStatus,
    };

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
            failed_step: None,
            failure_reason: None,
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
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
            Event::SearchAttributesUpdated {
                envelope: envelope(2, &workflow_id),
                workflow_id: workflow_id.clone(),
                attributes: HashMap::from([(
                    String::from("customer_id"),
                    SearchAttributeValue::String(String::from("customer-123")),
                )]),
            },
            Event::WorkflowCompleted {
                envelope: envelope(3, &workflow_id),
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
        assert_eq!(summary.ended_at, Some(recorded_at(3)));
        assert_eq!(summary.parent, None);
        Ok(())
    }

    #[test]
    fn summary_from_history_without_start_returns_none() {
        assert!(WorkflowSummary::from_history(&[]).is_none());
    }

    #[test]
    fn summary_end_time_uses_last_terminal_event() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1, &workflow_id),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
                package_version: crate::PackageVersion::new("a".repeat(64)),
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

    #[test]
    fn schedule_events_do_not_set_summary_end_time() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1, &workflow_id),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
            Event::ScheduleTriggered {
                envelope: envelope(2, &workflow_id),
                schedule_id: ScheduleId::new(uuid::Uuid::from_u128(2)),
                workflow_id: WorkflowId::new(uuid::Uuid::from_u128(3)),
                run_id: crate::RunId::new(uuid::Uuid::from_u128(4)),
            },
        ];

        let Some(summary) = WorkflowSummary::from_history(&events) else {
            return Err("history should contain workflow start".into());
        };

        assert_eq!(summary.status, WorkflowStatus::Running);
        assert_eq!(summary.ended_at, None);
        Ok(())
    }

    #[test]
    fn summary_projects_continued_as_new() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let continued_at = recorded_at(2);
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1, &workflow_id),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2, &workflow_id),
                input: payload("continued-input")?,
                workflow_type: Some(String::from("checkout-v2")),
                parent_run_id: RunId::new(uuid::Uuid::from_u128(2)),
            },
        ];

        let Some(summary) = WorkflowSummary::from_history(&events) else {
            return Err("history should contain workflow start".into());
        };

        assert_eq!(summary.status, WorkflowStatus::ContinuedAsNew);
        assert_eq!(summary.ended_at, Some(continued_at));
        Ok(())
    }

    #[test]
    fn summary_projects_current_run_after_continue_as_new() -> Result<(), Box<dyn std::error::Error>>
    {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
        let parent_run_id = RunId::new(uuid::Uuid::from_u128(2));
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(1, &workflow_id),
                workflow_type: String::from("checkout"),
                input: payload("input")?,
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: None,
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowContinuedAsNew {
                envelope: envelope(2, &workflow_id),
                input: payload("continued-input")?,
                workflow_type: Some(String::from("checkout-v2")),
                parent_run_id: parent_run_id.clone(),
            },
            Event::WorkflowStarted {
                envelope: envelope(3, &workflow_id),
                workflow_type: String::from("checkout-v2"),
                input: payload("continued-input")?,
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                parent_run_id: Some(parent_run_id),
                package_version: crate::PackageVersion::new("a".repeat(64)),
            },
        ];

        let Some(summary) = WorkflowSummary::from_history(&events) else {
            return Err("history should contain workflow start".into());
        };

        assert_eq!(summary.workflow_id, workflow_id);
        assert_eq!(summary.workflow_type, "checkout-v2");
        assert_eq!(summary.status, WorkflowStatus::Running);
        assert_eq!(summary.started_at, recorded_at(3));
        assert_eq!(summary.ended_at, None);
        Ok(())
    }

    fn started(workflow_id: &WorkflowId) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(1, workflow_id),
            workflow_type: String::from("stacked_dev"),
            input: payload("input")?,
            run_id: RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: crate::PackageVersion::new("a".repeat(64)),
        })
    }

    fn scheduled(
        workflow_id: &WorkflowId,
        seq: u64,
        ordinal: u64,
        activity_type: &str,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ActivityScheduled {
            envelope: envelope(seq, workflow_id),
            activity_id: ActivityId::from_sequence_position(ordinal),
            activity_type: String::from(activity_type),
            input: payload("activity")?,
            task_queue: String::from("default"),
            node: None,
        })
    }

    fn activity_failed(workflow_id: &WorkflowId, seq: u64, ordinal: u64) -> Event {
        Event::ActivityFailed {
            envelope: envelope(seq, workflow_id),
            activity_id: ActivityId::from_sequence_position(ordinal),
            error: ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: String::from("provider error: rate limited"),
                details: None,
            },
            attempt: 1,
        }
    }

    fn workflow_failed(workflow_id: &WorkflowId, seq: u64) -> Event {
        Event::WorkflowFailed {
            envelope: envelope(seq, workflow_id),
            error: WorkflowError {
                message: String::from("norn review failed"),
                details: None,
            },
        }
    }

    #[test]
    fn failure_projection_attributes_failed_step_and_reason()
    -> Result<(), Box<dyn std::error::Error>> {
        let wf = WorkflowId::new(uuid::Uuid::from_u128(1));
        let events = vec![
            started(&wf)?,
            scheduled(&wf, 2, 0, "scout")?,
            Event::ActivityCompleted {
                envelope: envelope(3, &wf),
                activity_id: ActivityId::from_sequence_position(0),
                result: payload("scout-result")?,
                attempt: 1,
            },
            scheduled(&wf, 4, 1, "dev_review")?,
            activity_failed(&wf, 5, 1),
            workflow_failed(&wf, 6),
        ];

        let (failed_step, failure_reason) = failure_projection(&events);
        assert_eq!(failed_step.as_deref(), Some("dev_review"));
        assert_eq!(failure_reason.as_deref(), Some("norn review failed"));

        let summary = WorkflowSummary::from_history(&events).ok_or("summary")?;
        assert_eq!(summary.status, WorkflowStatus::Failed);
        assert_eq!(summary.failed_step.as_deref(), Some("dev_review"));
        assert_eq!(
            summary.failure_reason.as_deref(),
            Some("norn review failed")
        );
        Ok(())
    }

    #[test]
    fn failure_projection_is_none_for_non_failed() -> Result<(), Box<dyn std::error::Error>> {
        let wf = WorkflowId::new(uuid::Uuid::from_u128(1));
        // A healthy, completed workflow leaves both fields None.
        let events = vec![
            started(&wf)?,
            scheduled(&wf, 2, 0, "scout")?,
            Event::WorkflowCompleted {
                envelope: envelope(3, &wf),
                result: payload("result")?,
            },
        ];
        let (failed_step, failure_reason) = failure_projection(&events);
        assert!(failed_step.is_none());
        assert!(failure_reason.is_none());

        let summary = WorkflowSummary::from_history(&events).ok_or("summary")?;
        assert!(summary.failed_step.is_none());
        assert!(summary.failure_reason.is_none());
        Ok(())
    }

    #[test]
    fn failure_projection_is_reset_by_reopen() -> Result<(), Box<dyn std::error::Error>> {
        let wf = WorkflowId::new(uuid::Uuid::from_u128(1));
        // Failed then reopened: the current lease is Running, so no failure fields.
        let events = vec![
            started(&wf)?,
            scheduled(&wf, 2, 0, "dev_review")?,
            activity_failed(&wf, 3, 0),
            workflow_failed(&wf, 4),
            Event::WorkflowReopened {
                envelope: envelope(5, &wf),
                run_id: RunId::new(uuid::Uuid::from_u128(1)),
                reopened: vec![ActivityId::from_sequence_position(0)],
            },
        ];
        let (failed_step, failure_reason) = failure_projection(&events);
        assert!(failed_step.is_none(), "a reopened run is not failed");
        assert!(failure_reason.is_none());
        Ok(())
    }

    #[test]
    fn failure_projection_has_reason_but_no_step_when_no_activity_failed()
    -> Result<(), Box<dyn std::error::Error>> {
        let wf = WorkflowId::new(uuid::Uuid::from_u128(1));
        // Workflow-code failure with no terminal activity failure to attribute.
        let events = vec![started(&wf)?, workflow_failed(&wf, 2)];
        let (failed_step, failure_reason) = failure_projection(&events);
        assert!(failed_step.is_none());
        assert_eq!(failure_reason.as_deref(), Some("norn review failed"));
        Ok(())
    }

    #[test]
    fn failure_projection_ignores_a_failed_step_that_later_succeeded()
    -> Result<(), Box<dyn std::error::Error>> {
        let wf = WorkflowId::new(uuid::Uuid::from_u128(1));
        // dev_review failed (attempt 1) then succeeded (attempt 2); the workflow
        // later failed on workflow code. The recovered step is not the failed step.
        let events = vec![
            started(&wf)?,
            scheduled(&wf, 2, 0, "dev_review")?,
            activity_failed(&wf, 3, 0),
            Event::ActivityCompleted {
                envelope: envelope(4, &wf),
                activity_id: ActivityId::from_sequence_position(0),
                result: payload("dev-review-result")?,
                attempt: 2,
            },
            workflow_failed(&wf, 5),
        ];
        let (failed_step, failure_reason) = failure_projection(&events);
        assert!(
            failed_step.is_none(),
            "a step that recovered is not the failed step"
        );
        assert_eq!(failure_reason.as_deref(), Some("norn review failed"));
        Ok(())
    }
}
