//! Atomic append with the sequence guard.

use aion_store::{Event, OutboxRow, StoreError, WorkflowId};
use libsql::{Transaction, TransactionBehavior, params};

mod metadata;

/// Append `events` for `workflow_id` under the expected-head sequence guard.
///
/// # Errors
///
/// Returns `StoreError::SequenceConflict` when the stored head differs from `expected_seq`,
/// `StoreError::Serialization` when an event cannot be serialized, and `StoreError::Backend` for
/// libSQL boundary failures that are not normalized into sequence conflicts.
pub(crate) async fn append(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    events: &[Event],
    expected_seq: u64,
) -> Result<(), StoreError> {
    append_with_outbox(conn, workflow_id, events, expected_seq, None).await
}

/// Append `events` for `workflow_id` and, when `outbox_rows` is `Some`, the outbox rows in the
/// **same** `IMMEDIATE` transaction, under the expected-head sequence guard.
///
/// Atomicity is load-bearing: a committed fan-out batch always carries both the
/// `ActivityScheduled`/`ActivityStarted` events and the matching outbox rows, or neither. Passing
/// `None` is byte-for-byte equivalent to the event-only [`append`] path, so existing callers are
/// untouched. Outbox rows use `INSERT OR IGNORE`, so a re-issued append of the same fan-out batch
/// silently ignores duplicate `dispatch_key`s.
///
/// # Errors
///
/// Returns `StoreError::SequenceConflict` when the stored head differs from `expected_seq`,
/// `StoreError::Serialization` when an event or outbox payload cannot be serialized, and
/// `StoreError::Backend` for libSQL boundary failures that are not normalized into sequence
/// conflicts.
pub(crate) async fn append_with_outbox(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    events: &[Event],
    expected_seq: u64,
    outbox_rows: Option<&[OutboxRow]>,
) -> Result<(), StoreError> {
    let tx = match conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
    {
        Ok(tx) => tx,
        Err(error) => {
            return conflict_after_begin_error(conn, workflow_id, expected_seq, error).await;
        }
    };

    let stored_head = match current_head(&tx, workflow_id).await {
        Ok(stored_head) => stored_head,
        Err(error) => {
            rollback(tx).await?;
            return Err(error);
        }
    };
    if stored_head != expected_seq {
        rollback(tx).await?;
        return Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: stored_head,
        });
    }

    let has_outbox = outbox_rows.is_some_and(|rows| !rows.is_empty());
    if events.is_empty() && !has_outbox {
        rollback(tx).await?;
        return Ok(());
    }

    if let Err(error) = validate_contiguous(events, expected_seq) {
        rollback(tx).await?;
        return Err(error);
    }

    for event in events {
        if let Err(error) = insert_event(&tx, workflow_id, event).await {
            rollback(tx).await?;
            return normalize_store_write_error(conn, workflow_id, expected_seq, error).await;
        }
    }

    if let Some(rows) = outbox_rows {
        for row in rows {
            if let Err(error) = crate::outbox::insert_outbox_row(&tx, row).await {
                rollback(tx).await?;
                return Err(error);
            }
        }
    }

    match tx.commit().await {
        Ok(()) => Ok(()),
        Err(error) => normalize_libsql_write_error(conn, workflow_id, expected_seq, error).await,
    }
}

async fn current_head(tx: &Transaction, workflow_id: &WorkflowId) -> Result<u64, StoreError> {
    let workflow_id = workflow_id.to_string();
    let mut rows = tx
        .query(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE workflow_id = ?1",
            params![workflow_id],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
        .ok_or_else(|| StoreError::Backend(String::from("event head query returned no rows")))?;
    let head: i64 = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;

    u64::try_from(head).map_err(|_| StoreError::Backend(format!("event head was negative: {head}")))
}

async fn current_head_outside_transaction(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<u64, StoreError> {
    let workflow_id = workflow_id.to_string();
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE workflow_id = ?1",
            params![workflow_id],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
        .ok_or_else(|| StoreError::Backend(String::from("event head query returned no rows")))?;
    let head: i64 = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;

    u64::try_from(head).map_err(|_| StoreError::Backend(format!("event head was negative: {head}")))
}

fn validate_contiguous(events: &[Event], expected_seq: u64) -> Result<(), StoreError> {
    let mut next_seq = expected_seq + 1;
    for event in events {
        if event.seq() != next_seq {
            return Err(StoreError::Backend(format!(
                "event sequence must be contiguous: expected {next_seq}, got {}",
                event.seq()
            )));
        }
        next_seq += 1;
    }

    Ok(())
}

async fn insert_event(
    tx: &Transaction,
    workflow_id: &WorkflowId,
    event: &Event,
) -> Result<(), StoreError> {
    let serialized =
        serde_json::to_vec(event).map_err(|error| crate::error::serde_json_error(&error))?;
    let workflow_id = workflow_id.to_string();
    let recorded_at = event.recorded_at().to_rfc3339();
    let seq = event.seq();

    tx.execute(
        "INSERT INTO events (workflow_id, seq, event, recorded_at, event_kind, is_queryable_event, workflow_type, child_workflow_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            workflow_id,
            seq,
            serialized,
            recorded_at,
            metadata::event_kind(event),
            metadata::queryable_flag(event),
            metadata::workflow_type(event),
            metadata::child_workflow_id(event)
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::error::libsql_error(&error))
}

async fn normalize_store_write_error(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    expected_seq: u64,
    error: StoreError,
) -> Result<(), StoreError> {
    match error {
        StoreError::Backend(message) => {
            conflict_after_store_error(conn, workflow_id, expected_seq, message).await
        }
        other => Err(other),
    }
}

async fn normalize_libsql_write_error(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    expected_seq: u64,
    error: libsql::Error,
) -> Result<(), StoreError> {
    conflict_after_store_error(conn, workflow_id, expected_seq, error.to_string()).await
}

async fn conflict_after_store_error(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    expected_seq: u64,
    message: String,
) -> Result<(), StoreError> {
    match advanced_head(conn, workflow_id, expected_seq).await? {
        Some(found) => Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found,
        }),
        None => Err(StoreError::Backend(message)),
    }
}

async fn conflict_after_begin_error(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    expected_seq: u64,
    error: libsql::Error,
) -> Result<(), StoreError> {
    match advanced_head(conn, workflow_id, expected_seq).await? {
        Some(found) => Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found,
        }),
        None => Err(crate::error::libsql_error(&error)),
    }
}

async fn advanced_head(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    expected_seq: u64,
) -> Result<Option<u64>, StoreError> {
    for _ in 0..3 {
        let found = current_head_outside_transaction(conn, workflow_id).await?;
        if found != expected_seq {
            return Ok(Some(found));
        }
        std::thread::yield_now();
    }

    Ok(None)
}

async fn rollback(tx: Transaction) -> Result<(), StoreError> {
    tx.rollback()
        .await
        .map_err(|error| crate::error::libsql_error(&error))
}

#[cfg(test)]
mod tests;
