//! Visibility projection updates for workflow lifecycle state changes.

use std::collections::HashMap;
use std::sync::Arc;

use aion_core::{Event, RunId, SearchAttributeValue, WorkflowId, status_from_events};
use aion_store::ReadableEventStore;
use aion_store::visibility::{VisibilityRecord, VisibilityStore};
use chrono::{DateTime, Utc};

use crate::EngineError;

/// Rebuilds and upserts the full visibility projection for a workflow execution.
///
/// # Errors
///
/// Returns store errors when history cannot be read or visibility cannot be recorded, and a load
/// error if the workflow history has no `WorkflowStarted` event to project.
pub async fn upsert_workflow_visibility(
    event_store: Arc<dyn ReadableEventStore>,
    visibility_store: Arc<dyn VisibilityStore>,
    workflow_id: &WorkflowId,
    run_id: &RunId,
) -> Result<(), EngineError> {
    let history = event_store.read_history(workflow_id).await?;
    let record = visibility_record_from_history(&history, run_id)?;
    visibility_store.record_visibility(record).await?;
    Ok(())
}

fn visibility_record_from_history(
    history: &[Event],
    run_id: &RunId,
) -> Result<VisibilityRecord, EngineError> {
    let (workflow_id, workflow_type, start_time) = started_projection(history)?;
    Ok(VisibilityRecord {
        workflow_id,
        run_id: run_id.clone(),
        workflow_type,
        status: status_from_events(history),
        start_time,
        close_time: terminal_recorded_at(history),
        search_attributes: search_attributes_from_history(history),
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

fn terminal_recorded_at(history: &[Event]) -> Option<DateTime<Utc>> {
    history.iter().rev().find_map(|event| match event {
        Event::WorkflowCompleted { envelope, .. }
        | Event::WorkflowFailed { envelope, .. }
        | Event::WorkflowCancelled { envelope, .. }
        | Event::WorkflowTimedOut { envelope, .. }
        | Event::WorkflowContinuedAsNew { envelope, .. } => Some(envelope.recorded_at),
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
}

fn search_attributes_from_history(history: &[Event]) -> HashMap<String, SearchAttributeValue> {
    let mut search_attributes = HashMap::new();
    for event in history {
        if let Event::SearchAttributesUpdated { attributes, .. } = event {
            search_attributes.extend(attributes.clone());
        }
    }
    search_attributes
}
