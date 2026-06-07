//! Event read operations: history retrieval, active workflow listing, filtered queries.

use std::collections::HashMap;

use aion_core::RunId;
use aion_store::{
    Event, RunSummary, StoreError, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary,
    status_from_events,
};
use libsql::{Value, params, params_from_iter};
use uuid::Uuid;

/// Read a workflow's complete event history in sequence order.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` when a stored
/// event blob cannot be decoded as an Aion event. Unknown workflows return an empty history.
pub(crate) async fn read_history(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, StoreError> {
    let workflow_id = workflow_id.to_string();
    let mut rows = conn
        .query(
            "SELECT event FROM events WHERE workflow_id = ?1 ORDER BY seq ASC",
            params![workflow_id],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut events = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let blob: Vec<u8> = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        events.push(decode_event(&blob)?);
    }

    Ok(events)
}

/// Return workflow ids whose projected status is still running.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` for malformed
/// stored event blobs.
pub(crate) async fn list_active(conn: &libsql::Connection) -> Result<Vec<WorkflowId>, StoreError> {
    let mut active = load_summaries(conn, &WorkflowFilter::default(), false)
        .await?
        .into_iter()
        .filter(|summary| matches!(summary.status, WorkflowStatus::Running))
        .map(|summary| summary.workflow_id)
        .collect::<Vec<_>>();
    active.sort_by_key(ToString::to_string);
    Ok(active)
}

/// Read run summaries for one workflow in continuation order.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` for malformed
/// stored event blobs or run identifiers.
pub(crate) async fn read_run_chain(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<Vec<RunSummary>, StoreError> {
    let history = read_history(conn, workflow_id).await?;
    let current_status = status_from_events(&history);
    let current_run_id = current_visibility_run_id(conn, workflow_id, current_status).await?;

    run_chain_from_history(&history, current_run_id.as_ref())
}

/// Query workflow summaries using SQL-bound filter parameters plus authoritative projection.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` for malformed
/// stored event blobs.
pub(crate) async fn query(
    conn: &libsql::Connection,
    filter: &WorkflowFilter,
) -> Result<Vec<WorkflowSummary>, StoreError> {
    let mut summaries = load_summaries(conn, filter, true)
        .await?
        .into_iter()
        .filter(|summary| filter.matches(summary))
        .collect::<Vec<_>>();
    sort_summaries(&mut summaries);
    Ok(summaries)
}

async fn load_summaries(
    conn: &libsql::Connection,
    filter: &WorkflowFilter,
    include_parents: bool,
) -> Result<Vec<WorkflowSummary>, StoreError> {
    let mut histories = load_candidate_histories(conn, filter).await?;
    let parent_by_child = if include_parents {
        load_parent_links(conn).await?
    } else {
        HashMap::new()
    };
    let mut summaries = Vec::new();

    for history in histories.values_mut() {
        history.retain(is_queryable_event);
        if let Some(mut summary) = WorkflowSummary::from_history(history) {
            summary.status = status_from_events(history);
            summary.parent = parent_by_child.get(&summary.workflow_id).cloned();
            summaries.push(summary);
        }
    }

    Ok(summaries)
}

async fn load_candidate_histories(
    conn: &libsql::Connection,
    filter: &WorkflowFilter,
) -> Result<HashMap<WorkflowId, Vec<Event>>, StoreError> {
    let plan = QueryPlan::from_filter(filter);
    let mut rows = conn
        .query(&plan.sql, params_from_iter(plan.params))
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let mut histories = HashMap::<WorkflowId, Vec<Event>>::new();

    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let blob: Vec<u8> = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let event = decode_event(&blob)?;
        histories
            .entry(event.workflow_id().clone())
            .or_default()
            .push(event);
    }

    Ok(histories)
}

struct QueryPlan {
    sql: String,
    params: Vec<Value>,
}

impl QueryPlan {
    fn from_filter(filter: &WorkflowFilter) -> Self {
        let mut clauses = vec![String::from("is_queryable_event = 1")];
        let mut params = Vec::new();

        if let Some(workflow_type) = &filter.workflow_type {
            params.push(Value::Text(workflow_type.clone()));
            clauses.push(format!(
                "workflow_id IN (SELECT workflow_id FROM events WHERE event_kind = 'WorkflowStarted' AND workflow_type = ?{})",
                params.len()
            ));
        }
        if let Some(started_after) = filter.started_after {
            params.push(Value::Text(started_after.to_rfc3339()));
            clauses.push(format!(
                "workflow_id IN (SELECT workflow_id FROM events WHERE event_kind = 'WorkflowStarted' AND recorded_at >= ?{})",
                params.len()
            ));
        }
        if let Some(started_before) = filter.started_before {
            params.push(Value::Text(started_before.to_rfc3339()));
            clauses.push(format!(
                "workflow_id IN (SELECT workflow_id FROM events WHERE event_kind = 'WorkflowStarted' AND recorded_at <= ?{})",
                params.len()
            ));
        }
        if let Some(parent) = &filter.parent {
            params.push(Value::Text(parent.to_string()));
            clauses.push(format!(
                "(workflow_id = ?{} OR workflow_id IN (SELECT child_workflow_id FROM events WHERE event_kind = 'ChildWorkflowStarted' AND workflow_id = ?{}))",
                params.len(),
                params.len()
            ));
        }

        Self {
            sql: format!(
                "SELECT event FROM events WHERE {} ORDER BY workflow_id ASC, seq ASC",
                clauses.join(" AND ")
            ),
            params,
        }
    }
}

async fn load_parent_links(
    conn: &libsql::Connection,
) -> Result<HashMap<WorkflowId, WorkflowId>, StoreError> {
    let mut rows = conn
        .query(
            "SELECT event FROM events WHERE event_kind = 'ChildWorkflowStarted' ORDER BY workflow_id ASC, seq ASC",
            (),
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let mut links = HashMap::new();

    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let blob: Vec<u8> = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        if let Event::ChildWorkflowStarted {
            envelope,
            child_workflow_id,
            ..
        } = decode_event(&blob)?
        {
            links.insert(child_workflow_id, envelope.workflow_id);
        }
    }

    Ok(links)
}

fn is_queryable_event(event: &Event) -> bool {
    matches!(
        event,
        Event::WorkflowStarted { .. }
            | Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. }
    )
}

fn sort_summaries(summaries: &mut [WorkflowSummary]) {
    summaries.sort_by(|left, right| {
        left.started_at.cmp(&right.started_at).then_with(|| {
            left.workflow_id
                .to_string()
                .cmp(&right.workflow_id.to_string())
        })
    });
}

async fn current_visibility_run_id(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    current_status: WorkflowStatus,
) -> Result<Option<RunId>, StoreError> {
    let mut rows = conn
        .query(
            "SELECT run_id FROM visibility WHERE workflow_id = ?1 AND status = ?2 LIMIT 1",
            params![
                workflow_id.to_string(),
                serde_json::to_string(&current_status)
                    .map_err(|error| crate::error::serde_json_error(&error))?
            ],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    rows.next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
        .map(|row| {
            let run_id: String = row
                .get(0)
                .map_err(|error| crate::error::libsql_error(&error))?;
            decode_run_id(&run_id).map(Some)
        })
        .transpose()
        .map(Option::flatten)
}

fn run_chain_from_history(
    history: &[Event],
    current_run_id: Option<&RunId>,
) -> Result<Vec<RunSummary>, StoreError> {
    let starts = history
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            Event::WorkflowStarted {
                envelope,
                parent_run_id,
                ..
            } => Some((index, envelope.recorded_at, parent_run_id.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut summaries = Vec::with_capacity(starts.len());
    for (position, (start_index, started_at, parent_run_id)) in starts.iter().enumerate() {
        let end_index = starts
            .get(position + 1)
            .map_or(history.len(), |(next_start, _, _)| *next_start);
        let run_events = &history[*start_index..end_index];
        let run_id = run_events
            .iter()
            .find_map(|event| match event {
                Event::WorkflowContinuedAsNew { parent_run_id, .. } => Some(parent_run_id.clone()),
                _ => None,
            })
            .or_else(|| current_run_id.cloned())
            .ok_or_else(|| {
                StoreError::Backend(String::from(
                    "run chain cannot identify a run without a terminal continue-as-new event or visibility run id",
                ))
            })?;
        let closed_at = run_events.iter().rev().find_map(|event| match event {
            Event::WorkflowCompleted { envelope, .. }
            | Event::WorkflowFailed { envelope, .. }
            | Event::WorkflowCancelled { envelope, .. }
            | Event::WorkflowTimedOut { envelope, .. }
            | Event::WorkflowContinuedAsNew { envelope, .. } => Some(envelope.recorded_at),
            _ => None,
        });

        summaries.push(RunSummary {
            run_id,
            parent_run_id: parent_run_id.clone(),
            status: status_from_events(run_events),
            started_at: *started_at,
            closed_at,
        });
    }

    Ok(summaries)
}

fn decode_run_id(value: &str) -> Result<RunId, StoreError> {
    Uuid::parse_str(value)
        .map(RunId::new)
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn decode_event(blob: &[u8]) -> Result<Event, StoreError> {
    serde_json::from_slice(blob).map_err(|error| crate::error::serde_json_error(&error))
}

#[cfg(test)]
mod tests;
