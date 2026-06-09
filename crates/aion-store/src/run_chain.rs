//! Run-chain projection helpers shared by store implementations.

use std::collections::{HashMap, HashSet};

use aion_core::{Event, RunId, status_from_events};

use crate::{RunSummary, StoreError};

/// Projects run summaries from a workflow history and orders them oldest to newest.
///
/// The supplied history may be unsorted; events are first ordered by workflow sequence and then split
/// at each `WorkflowStarted` event. Malformed histories return a backend error rather than inventing
/// a partial chain.
///
/// # Errors
///
/// Returns `StoreError::Backend` when starts do not form a single parent-linked chain.
pub fn run_chain_from_history(history: &[Event]) -> Result<Vec<RunSummary>, StoreError> {
    let mut ordered = history.to_vec();
    ordered.sort_by_key(Event::seq);
    let mut summaries = Vec::new();
    let mut current_run_start = None;

    for (index, event) in ordered.iter().enumerate() {
        if matches!(event, Event::WorkflowStarted { .. }) {
            if let Some(start_index) = current_run_start.replace(index) {
                summaries.push(project_run_summary(&ordered[start_index..index])?);
            }
        }
    }

    if let Some(start_index) = current_run_start {
        summaries.push(project_run_summary(&ordered[start_index..])?);
    }

    order_by_parent_chain(summaries)
}

fn project_run_summary(events: &[Event]) -> Result<RunSummary, StoreError> {
    let Some(Event::WorkflowStarted {
        envelope,
        run_id,
        parent_run_id,
        ..
    }) = events.first()
    else {
        return Err(StoreError::Backend(String::from(
            "run slice does not begin with WorkflowStarted",
        )));
    };

    Ok(RunSummary {
        run_id: run_id.clone(),
        parent_run_id: parent_run_id.clone(),
        status: status_from_events(events),
        started_at: envelope.recorded_at,
        closed_at: events.iter().rev().find_map(terminal_recorded_at),
    })
}

fn terminal_recorded_at(event: &Event) -> Option<chrono::DateTime<chrono::Utc>> {
    match event {
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
    }
}

fn order_by_parent_chain(summaries: Vec<RunSummary>) -> Result<Vec<RunSummary>, StoreError> {
    if summaries.is_empty() {
        return Ok(Vec::new());
    }

    let roots = summaries
        .iter()
        .filter(|summary| summary.parent_run_id.is_none())
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err(StoreError::Backend(format!(
            "run chain must contain exactly one root, found {}",
            roots.len()
        )));
    }

    let mut children_by_parent = HashMap::<RunId, RunSummary>::new();
    let mut root = None;
    for summary in summaries {
        if let Some(parent_run_id) = &summary.parent_run_id {
            if children_by_parent
                .insert(parent_run_id.clone(), summary)
                .is_some()
            {
                return Err(StoreError::Backend(String::from(
                    "run chain contains multiple children for the same parent run",
                )));
            }
        } else {
            root = Some(summary);
        }
    }

    let Some(mut current) = root else {
        return Err(StoreError::Backend(String::from(
            "run chain root disappeared during ordering",
        )));
    };
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current.run_id.clone()) {
            return Err(StoreError::Backend(String::from(
                "run chain contains a cycle",
            )));
        }
        let next = children_by_parent.remove(&current.run_id);
        ordered.push(current);
        match next {
            Some(child) => current = child,
            None => break,
        }
    }

    if !children_by_parent.is_empty() {
        return Err(StoreError::Backend(String::from(
            "run chain contains parent links that are not reachable from the root",
        )));
    }

    Ok(ordered)
}
