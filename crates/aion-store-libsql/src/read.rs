//! Event read operations: history retrieval, active workflow listing, filtered queries.

use std::collections::HashMap;

use aion_store::{
    Event, RunSummary, StoreError, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary,
    status_from_events,
};
use libsql::{Value, params, params_from_iter};

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
    let rows = conn
        .query(
            "SELECT event FROM events WHERE workflow_id = ?1 ORDER BY seq ASC",
            params![workflow_id],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    collect_events(rows).await
}

/// Read a workflow's event history restricted to `seq >= from_seq`, in sequence order.
///
/// The range scan is served by the `events` primary key `(workflow_id, seq)`: `SQLite` plans it as
/// `SEARCH events USING INDEX sqlite_autoindex_events_1 (workflow_id=? AND seq>?)`, so resume
/// reads cost O(delta), not O(history).
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` when a stored
/// event blob cannot be decoded as an Aion event. Unknown workflows and `from_seq` beyond the
/// current head both return an empty history.
pub(crate) async fn read_history_from(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    from_seq: u64,
) -> Result<Vec<Event>, StoreError> {
    let workflow_id = workflow_id.to_string();
    let rows = conn
        .query(
            "SELECT event FROM events WHERE workflow_id = ?1 AND seq >= ?2 ORDER BY seq ASC",
            params![workflow_id, from_seq],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    collect_events(rows).await
}

async fn collect_events(mut rows: libsql::Rows) -> Result<Vec<Event>, StoreError> {
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

/// Read a workflow's concrete run chain in continuation order.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures, `StoreError::Serialization` when a stored
/// event blob cannot be decoded, or a backend projection error for malformed chains.
pub(crate) async fn read_run_chain(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<Vec<RunSummary>, StoreError> {
    let history = read_history(conn, workflow_id).await?;
    aion_store::run_chain::run_chain_from_history(&history)
}

/// Return every workflow id with at least one stored event.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` when a stored
/// workflow id cannot be decoded.
pub(crate) async fn list_workflow_ids(
    conn: &libsql::Connection,
) -> Result<Vec<WorkflowId>, StoreError> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT workflow_id FROM events ORDER BY workflow_id ASC",
            (),
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let mut workflow_ids = Vec::new();

    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let raw: String = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        workflow_ids.push(parse_workflow_id(&raw)?);
    }

    Ok(workflow_ids)
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

/// Validate that every stored event blob can be decoded by the current Aion event schema.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL failures and `StoreError::Serialization` when any
/// stored event blob is incompatible with the current event schema.
pub(crate) async fn validate_all_events(conn: &libsql::Connection) -> Result<(), StoreError> {
    let mut rows = conn
        .query(
            "SELECT event FROM events ORDER BY workflow_id ASC, seq ASC",
            (),
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let blob: Vec<u8> = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        decode_event(&blob)?;
    }

    Ok(())
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
            // A reopen changes the projected status (Failed -> Running), so it
            // must feed status_from_events on the read side just like the other
            // lifecycle events, or a reopened workflow would still read as Failed.
            | Event::WorkflowReopened { .. }
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

fn decode_event(blob: &[u8]) -> Result<Event, StoreError> {
    serde_json::from_slice(blob).map_err(|error| crate::error::serde_json_error(&error))
}

fn parse_workflow_id(raw: &str) -> Result<WorkflowId, StoreError> {
    uuid::Uuid::parse_str(raw)
        .map(WorkflowId::new)
        .map_err(|error| StoreError::Serialization(format!("invalid workflow_id `{raw}`: {error}")))
}

#[cfg(test)]
mod tests;
