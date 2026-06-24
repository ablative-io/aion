//! Behavioural tests for the event-only append path and its sequence guard.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aion_store::{Event, StoreError, WorkflowId, WritableEventStore, WriteToken};
use chrono::{DateTime, Utc};
use libsql::params;
use serde_json::{Value, json};

use crate::LibSqlStore;

#[tokio::test]
async fn appends_first_batch_from_empty_history() -> Result<(), StoreError> {
    let store = open_test_store("first-batch").await?;
    let workflow_id = WorkflowId::new_v4();
    let events = vec![workflow_started(&workflow_id, 1, "first")?];

    store
        .append(WriteToken::recorder(), &workflow_id, &events, 0)
        .await?;

    let stats = event_stats(store.connection(), &workflow_id).await?;
    assert_eq!(stats, EventStats { count: 1, head: 1 });
    Ok(())
}

#[tokio::test]
async fn appends_multi_event_batch_with_contiguous_sequences() -> Result<(), StoreError> {
    let store = open_test_store("multi-event").await?;
    let workflow_id = WorkflowId::new_v4();
    let events = vec![
        workflow_started(&workflow_id, 1, "multi")?,
        signal_received(&workflow_id, 2, "wake")?,
        signal_received(&workflow_id, 3, "done")?,
    ];

    store
        .append(WriteToken::recorder(), &workflow_id, &events, 0)
        .await?;

    assert_eq!(
        stored_sequences(store.connection(), &workflow_id).await?,
        vec![1, 2, 3]
    );
    assert_eq!(
        event_stats(store.connection(), &workflow_id).await?,
        EventStats { count: 3, head: 3 }
    );
    Ok(())
}

#[tokio::test]
async fn empty_batch_checks_guard_and_leaves_head_unchanged() -> Result<(), StoreError> {
    let store = open_test_store("empty-batch").await?;
    let workflow_id = WorkflowId::new_v4();

    store
        .append(WriteToken::recorder(), &workflow_id, &[], 0)
        .await?;

    assert_eq!(
        event_stats(store.connection(), &workflow_id).await?,
        EventStats { count: 0, head: 0 }
    );
    Ok(())
}

#[tokio::test]
async fn stale_expected_sequence_rolls_back_without_writes() -> Result<(), StoreError> {
    let store = open_test_store("stale-conflict").await?;
    let workflow_id = WorkflowId::new_v4();
    let first = vec![workflow_started(&workflow_id, 1, "stale")?];
    let stale = vec![signal_received(&workflow_id, 2, "loser")?];

    store
        .append(WriteToken::recorder(), &workflow_id, &first, 0)
        .await?;
    let result = store
        .append(WriteToken::recorder(), &workflow_id, &stale, 0)
        .await;

    assert_sequence_conflict(&result, 0, 1)?;
    assert_eq!(
        event_stats(store.connection(), &workflow_id).await?,
        EventStats { count: 1, head: 1 }
    );
    assert_eq!(
        stored_sequences(store.connection(), &workflow_id).await?,
        vec![1]
    );
    Ok(())
}

#[tokio::test]
async fn ahead_expected_sequence_rolls_back_without_writes() -> Result<(), StoreError> {
    let store = open_test_store("ahead-conflict").await?;
    let workflow_id = WorkflowId::new_v4();
    let events = vec![workflow_started(&workflow_id, 1, "ahead")?];

    let result = store
        .append(WriteToken::recorder(), &workflow_id, &events, 2)
        .await;

    assert_sequence_conflict(&result, 2, 0)?;
    assert_eq!(
        event_stats(store.connection(), &workflow_id).await?,
        EventStats { count: 0, head: 0 }
    );
    Ok(())
}

#[tokio::test]
async fn non_contiguous_batch_rolls_back_without_writes() -> Result<(), StoreError> {
    let store = open_test_store("non-contiguous").await?;
    let workflow_id = WorkflowId::new_v4();
    let events = vec![
        workflow_started(&workflow_id, 1, "non-contiguous")?,
        signal_received(&workflow_id, 3, "gap")?,
    ];

    match store
        .append(WriteToken::recorder(), &workflow_id, &events, 0)
        .await
    {
        Err(StoreError::Backend(message)) => {
            assert!(message.contains("event sequence must be contiguous"));
        }
        Err(other) => {
            return Err(StoreError::Backend(format!(
                "expected backend error, got {other:?}"
            )));
        }
        Ok(()) => {
            return Err(StoreError::Backend(String::from(
                "expected non-contiguous batch to fail",
            )));
        }
    }

    assert_eq!(
        event_stats(store.connection(), &workflow_id).await?,
        EventStats { count: 0, head: 0 }
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_same_expected_sequence_has_one_winner() -> Result<(), StoreError> {
    let store = Arc::new(open_test_store("concurrent-race").await?);
    let workflow_id = WorkflowId::new_v4();
    let first_store = Arc::clone(&store);
    let first_workflow_id = workflow_id.clone();
    let second_store = Arc::clone(&store);
    let second_workflow_id = workflow_id.clone();

    let first = tokio::spawn(async move {
        let events = vec![workflow_started(&first_workflow_id, 1, "first")?];
        first_store
            .append(WriteToken::recorder(), &first_workflow_id, &events, 0)
            .await
    });
    let second = tokio::spawn(async move {
        let events = vec![workflow_started(&second_workflow_id, 1, "second")?];
        second_store
            .append(WriteToken::recorder(), &second_workflow_id, &events, 0)
            .await
    });

    let first = join_append(first).await?;
    let second = join_append(second).await?;
    let ok_count = usize::from(first.is_ok()) + usize::from(second.is_ok());
    let conflict_count = usize::from(is_sequence_conflict(&first, 0, 1))
        + usize::from(is_sequence_conflict(&second, 0, 1));

    assert_eq!(ok_count, 1);
    assert_eq!(conflict_count, 1);
    assert_eq!(
        event_stats(store.connection(), &workflow_id).await?,
        EventStats { count: 1, head: 1 }
    );
    assert_eq!(
        stored_sequences(store.connection(), &workflow_id).await?,
        vec![1]
    );
    Ok(())
}

async fn open_test_store(name: &str) -> Result<LibSqlStore, StoreError> {
    LibSqlStore::open(unique_temp_path(name)).await
}

async fn join_append(
    handle: tokio::task::JoinHandle<Result<(), StoreError>>,
) -> Result<Result<(), StoreError>, StoreError> {
    handle
        .await
        .map_err(|error| StoreError::Backend(format!("append task failed to join: {error}")))
}

fn assert_sequence_conflict(
    result: &Result<(), StoreError>,
    expected: u64,
    found: u64,
) -> Result<(), StoreError> {
    if is_sequence_conflict(result, expected, found) {
        Ok(())
    } else {
        Err(StoreError::Backend(format!(
            "expected SequenceConflict {{ expected: {expected}, found: {found} }}, got {result:?}"
        )))
    }
}

fn is_sequence_conflict(result: &Result<(), StoreError>, expected: u64, found: u64) -> bool {
    matches!(
        result,
        Err(StoreError::SequenceConflict {
            expected: actual_expected,
            found: actual_found,
        }) if *actual_expected == expected && *actual_found == found
    )
}

async fn event_stats(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<EventStats, StoreError> {
    let workflow_id = workflow_id.to_string();
    let mut rows = conn
        .query(
            "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM events WHERE workflow_id = ?1",
            params![workflow_id],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
        .ok_or_else(|| StoreError::Backend(String::from("event stats query returned no row")))?;
    let count: i64 = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let head: i64 = row
        .get(1)
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(EventStats { count, head })
}

async fn stored_sequences(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<Vec<i64>, StoreError> {
    let workflow_id = workflow_id.to_string();
    let mut rows = conn
        .query(
            "SELECT seq FROM events WHERE workflow_id = ?1 ORDER BY seq ASC",
            params![workflow_id],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let mut sequences = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        sequences.push(
            row.get(0)
                .map_err(|error| crate::error::libsql_error(&error))?,
        );
    }

    Ok(sequences)
}

fn workflow_started(workflow_id: &WorkflowId, seq: u64, label: &str) -> Result<Event, StoreError> {
    event_from_json(json!({
        "type": "WorkflowStarted",
        "data": {
            "envelope": envelope(workflow_id, seq),
            "workflow_type": format!("test-{label}"),
            "input": payload(label)?,
            "run_id": uuid::Uuid::from_u128(seq.into()).to_string(),
            "parent_run_id": null,
            "package_version": "a".repeat(64),
        }
    }))
}

fn signal_received(workflow_id: &WorkflowId, seq: u64, label: &str) -> Result<Event, StoreError> {
    event_from_json(json!({
        "type": "SignalReceived",
        "data": {
            "envelope": envelope(workflow_id, seq),
            "name": label,
            "payload": payload(label)?,
        }
    }))
}

fn event_from_json(value: Value) -> Result<Event, StoreError> {
    serde_json::from_value(value).map_err(|error| StoreError::Serialization(error.to_string()))
}

fn envelope(workflow_id: &WorkflowId, seq: u64) -> Value {
    json!({
        "seq": seq,
        "recorded_at": recorded_at(),
        "workflow_id": workflow_id,
    })
}

fn payload(label: &str) -> Result<Value, StoreError> {
    let bytes = serde_json::to_vec(&json!({ "label": label }))
        .map_err(|error| StoreError::Serialization(error.to_string()))?;
    Ok(json!({
        "content_type": "Json",
        "bytes": bytes,
    }))
}

fn recorded_at() -> DateTime<Utc> {
    DateTime::<Utc>::from(UNIX_EPOCH)
}

fn unique_temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "aion-store-libsql-append-{name}-{}-{nanos}.db",
        std::process::id()
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct EventStats {
    count: i64,
    head: i64,
}
