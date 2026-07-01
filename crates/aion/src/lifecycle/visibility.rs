//! Visibility projection updates for workflow lifecycle state changes.

use std::collections::HashMap;
use std::sync::Arc;

use aion_core::{Event, RunId, WorkflowId, current_lease_terminal, status_from_events};
use aion_store::EventStore;
use aion_store::visibility::{ListWorkflowsFilter, VisibilityRecord, VisibilityStore};
use chrono::{DateTime, Utc};

use crate::EngineError;

/// Rebuilds and upserts the full visibility projection for a workflow execution.
///
/// # Errors
///
/// Returns store errors when history cannot be read or visibility cannot be recorded, and a load
/// error if the workflow history has no `WorkflowStarted` event to project.
pub async fn upsert_workflow_visibility(
    event_store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<(), EngineError> {
    let history = event_store.read_history(workflow_id).await?;
    let record = visibility_record_from_history(&history, run_id)?;
    visibility_store.record_visibility(record).await?;
    Ok(())
}

/// Reconciles the visibility projection with authoritative event history.
///
/// # Errors
///
/// Returns store errors while reading histories or visibility rows, and load errors for malformed
/// workflow histories that cannot be projected.
pub async fn reconcile_visibility(
    event_store: Arc<dyn EventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
) -> Result<(), EngineError> {
    let existing = visibility_store
        .list_workflows(ListWorkflowsFilter::default())
        .await?
        .into_iter()
        .map(|summary| {
            (
                (summary.workflow_id.clone(), summary.run_id.clone()),
                summary,
            )
        })
        .collect::<HashMap<_, _>>();

    for workflow_id in event_store.list_workflow_ids().await? {
        let history = event_store.read_history(&workflow_id).await?;
        let run_id = started_run_id(&history)?;
        let record = visibility_record_from_history(&history, &run_id)?;
        let key = (record.workflow_id.clone(), record.run_id.clone());
        let projected = aion_store::visibility::WorkflowSummary::from(record.clone());

        if existing.get(&key) != Some(&projected) {
            visibility_store.record_visibility(record).await?;
        }
    }

    Ok(())
}

fn visibility_record_from_history(
    history: &[Event],
    run_id: &RunId,
) -> Result<VisibilityRecord, EngineError> {
    let (workflow_id, workflow_type, start_time) = started_projection(history)?;
    let (failed_step, failure_reason) = aion_core::failure_projection(history);
    Ok(VisibilityRecord {
        workflow_id,
        run_id: run_id.clone(),
        workflow_type,
        status: status_from_events(history),
        start_time,
        close_time: terminal_recorded_at(history),
        failed_step,
        failure_reason,
        search_attributes: aion_core::search_attributes_from_events(history),
    })
}

fn started_projection(
    history: &[Event],
) -> Result<(WorkflowId, String, DateTime<Utc>), EngineError> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted {
                envelope,
                workflow_type,
                ..
            } => Some((
                envelope.workflow_id.clone(),
                workflow_type.clone(),
                envelope.recorded_at,
            )),
            _ => None,
        })
        .ok_or_else(|| EngineError::Load {
            reason: String::from(
                "workflow history has no WorkflowStarted event for visibility projection",
            ),
        })
}

fn started_run_id(history: &[Event]) -> Result<RunId, EngineError> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .ok_or_else(|| EngineError::Load {
            reason: String::from(
                "workflow history has no WorkflowStarted event for visibility projection",
            ),
        })
}

fn terminal_recorded_at(history: &[Event]) -> Option<DateTime<Utc>> {
    // Reset-aware close time: the current lease's terminal event, aligned with
    // the projected status. A reopened workflow has no close time.
    current_lease_terminal(history).map(|event| *event.recorded_at())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::error::Error;

    use aion_core::{
        Event, EventEnvelope, PackageVersion, Payload, RunId, SearchAttributeValue, WorkflowError,
        WorkflowId, WorkflowStatus, search_attributes_from_events,
    };
    use chrono::{TimeZone, Utc};

    use super::{started_projection, terminal_recorded_at, visibility_record_from_history};
    use crate::EngineError;

    type TestResult = Result<(), Box<dyn Error>>;

    fn envelope(workflow_id: &WorkflowId, seq: u64) -> Result<EventEnvelope, Box<dyn Error>> {
        let base = Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .ok_or("test timestamp should be unambiguous")?;
        Ok(EventEnvelope {
            seq,
            recorded_at: base + chrono::Duration::seconds(i64::try_from(seq)?),
            workflow_id: workflow_id.clone(),
        })
    }

    fn payload() -> Result<Payload, Box<dyn Error>> {
        Ok(Payload::from_json(&serde_json::json!({}))?)
    }

    fn package_version() -> PackageVersion {
        PackageVersion::new("a".repeat(64))
    }

    fn workflow_started(workflow_id: &WorkflowId, run_id: &RunId) -> Result<Event, Box<dyn Error>> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(workflow_id, 1)?,
            workflow_type: String::from("order_processing"),
            input: payload()?,
            run_id: run_id.clone(),
            parent_run_id: None,
            package_version: package_version(),
        })
    }

    #[test]
    fn started_projection_extracts_fields_from_workflow_started() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let history = vec![workflow_started(&wf_id, &run_id)?];

        let (projected_id, projected_type, projected_time) = started_projection(&history)?;
        assert_eq!(projected_id, wf_id);
        assert_eq!(projected_type, "order_processing");
        assert_eq!(projected_time, envelope(&wf_id, 1)?.recorded_at);
        Ok(())
    }

    #[test]
    fn started_projection_returns_load_error_for_empty_history() {
        let result = started_projection(&[]);
        assert!(
            matches!(result, Err(EngineError::Load { .. })),
            "expected Load error, got {result:?}"
        );
    }

    #[test]
    fn started_projection_returns_load_error_when_no_started_event() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let history = vec![Event::WorkflowCompleted {
            envelope: envelope(&wf_id, 1)?,
            result: payload()?,
        }];
        let result = started_projection(&history);
        assert!(matches!(result, Err(EngineError::Load { .. })));
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_returns_completed_timestamp() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let env = envelope(&wf_id, 2)?;
        let expected = env.recorded_at;
        let history = vec![Event::WorkflowCompleted {
            envelope: env,
            result: payload()?,
        }];
        assert_eq!(terminal_recorded_at(&history), Some(expected));
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_returns_failed_timestamp() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let env = envelope(&wf_id, 2)?;
        let expected = env.recorded_at;
        let history = vec![Event::WorkflowFailed {
            envelope: env,
            error: WorkflowError {
                message: String::from("boom"),
                details: None,
            },
        }];
        assert_eq!(terminal_recorded_at(&history), Some(expected));
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_returns_cancelled_timestamp() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let env = envelope(&wf_id, 2)?;
        let expected = env.recorded_at;
        let history = vec![Event::WorkflowCancelled {
            envelope: env,
            reason: String::from("user request"),
        }];
        assert_eq!(terminal_recorded_at(&history), Some(expected));
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_returns_timed_out_timestamp() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let env = envelope(&wf_id, 2)?;
        let expected = env.recorded_at;
        let history = vec![Event::WorkflowTimedOut {
            envelope: env,
            timeout: String::from("workflow_execution"),
        }];
        assert_eq!(terminal_recorded_at(&history), Some(expected));
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_returns_continued_as_new_timestamp() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let parent_run = RunId::new_v4();
        let env = envelope(&wf_id, 2)?;
        let expected = env.recorded_at;
        let history = vec![Event::WorkflowContinuedAsNew {
            envelope: env,
            input: payload()?,
            workflow_type: None,
            parent_run_id: parent_run,
        }];
        assert_eq!(terminal_recorded_at(&history), Some(expected));
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_returns_none_for_non_terminal_history() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let history = vec![workflow_started(&wf_id, &run_id)?];
        assert_eq!(terminal_recorded_at(&history), None);
        Ok(())
    }

    #[test]
    fn terminal_recorded_at_finds_terminal_after_interleaved_non_terminal_events() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let terminal_env = envelope(&wf_id, 4)?;
        let expected = terminal_env.recorded_at;
        let history = vec![
            workflow_started(&wf_id, &run_id)?,
            Event::SignalReceived {
                envelope: envelope(&wf_id, 2)?,
                name: String::from("wake"),
                payload: payload()?,
            },
            Event::TimerFired {
                envelope: envelope(&wf_id, 3)?,
                timer_id: aion_core::TimerId::named("t1")?,
            },
            Event::WorkflowCompleted {
                envelope: terminal_env,
                result: payload()?,
            },
        ];
        assert_eq!(terminal_recorded_at(&history), Some(expected));
        Ok(())
    }

    #[test]
    fn search_attributes_collects_from_multiple_updates() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let mut first_attrs = HashMap::new();
        first_attrs.insert(
            String::from("customer"),
            SearchAttributeValue::String(String::from("acme")),
        );
        let mut second_attrs = HashMap::new();
        second_attrs.insert(String::from("priority"), SearchAttributeValue::Int(5));
        second_attrs.insert(
            String::from("customer"),
            SearchAttributeValue::String(String::from("globex")),
        );

        let history = vec![
            Event::SearchAttributesUpdated {
                envelope: envelope(&wf_id, 2)?,
                workflow_id: wf_id.clone(),
                attributes: first_attrs,
            },
            Event::SearchAttributesUpdated {
                envelope: envelope(&wf_id, 3)?,
                workflow_id: wf_id.clone(),
                attributes: second_attrs,
            },
        ];

        let result = search_attributes_from_events(&history);
        assert_eq!(result.len(), 2);
        assert_eq!(
            result.get("customer"),
            Some(&SearchAttributeValue::String(String::from("globex")))
        );
        assert_eq!(result.get("priority"), Some(&SearchAttributeValue::Int(5)));
        Ok(())
    }

    #[test]
    fn search_attributes_returns_empty_when_no_updates() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let history = vec![workflow_started(&wf_id, &run_id)?];
        assert!(search_attributes_from_events(&history).is_empty());
        Ok(())
    }

    #[test]
    fn visibility_record_from_history_projects_running_workflow() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let history = vec![workflow_started(&wf_id, &run_id)?];

        let record = visibility_record_from_history(&history, &run_id)?;
        assert_eq!(record.workflow_id, wf_id);
        assert_eq!(record.run_id, run_id);
        assert_eq!(record.workflow_type, "order_processing");
        assert_eq!(record.status, WorkflowStatus::Running);
        assert!(record.close_time.is_none());
        assert!(record.search_attributes.is_empty());
        Ok(())
    }

    #[test]
    fn visibility_record_from_history_projects_completed_workflow_with_attributes() -> TestResult {
        let wf_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let mut attrs = HashMap::new();
        attrs.insert(
            String::from("region"),
            SearchAttributeValue::String(String::from("eu-west-1")),
        );
        let terminal_env = envelope(&wf_id, 3)?;
        let expected_close = terminal_env.recorded_at;

        let history = vec![
            workflow_started(&wf_id, &run_id)?,
            Event::SearchAttributesUpdated {
                envelope: envelope(&wf_id, 2)?,
                workflow_id: wf_id.clone(),
                attributes: attrs,
            },
            Event::WorkflowCompleted {
                envelope: terminal_env,
                result: payload()?,
            },
        ];

        let record = visibility_record_from_history(&history, &run_id)?;
        assert_eq!(record.status, WorkflowStatus::Completed);
        assert_eq!(record.close_time, Some(expected_close));
        assert_eq!(
            record.search_attributes.get("region"),
            Some(&SearchAttributeValue::String(String::from("eu-west-1")))
        );
        Ok(())
    }
}
