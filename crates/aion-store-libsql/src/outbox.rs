//! libSQL-backed durable outbox: staging, claim, and terminal transitions.
//!
//! Rows are inserted with `INSERT OR IGNORE` so a duplicate `dispatch_key` is silently dropped,
//! preserving at-most-once dispatch. Claims run under an `IMMEDIATE` transaction (the single-writer
//! `SQLite` equivalent of `SELECT ... FOR UPDATE SKIP LOCKED`): pending rows are flipped to `claimed`
//! and returned in one atomic step so no two dispatchers observe the same row as claimable.

use aion_store::{OutboxRow, OutboxStatus, Payload, StoreError, WorkflowId};
use chrono::{DateTime, SecondsFormat, Utc};
use libsql::{Connection, Row, Transaction, TransactionBehavior, params};

const INSERT_OUTBOX_SQL: &str = "
INSERT OR IGNORE INTO outbox
    (dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const SELECT_CLAIMABLE_SQL: &str = "
SELECT dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after
FROM outbox
WHERE status = 'pending' AND visible_after <= ?1
ORDER BY visible_after ASC, dispatch_key ASC
LIMIT ?2";

const CLAIM_ROW_SQL: &str = "
UPDATE outbox SET status = 'claimed' WHERE dispatch_key = ?1 AND status = 'pending'";

const COMPLETE_ROW_SQL: &str = "
UPDATE outbox SET status = 'done' WHERE dispatch_key = ?1";

const RETRY_ROW_SQL: &str = "
UPDATE outbox SET status = 'pending', attempt = ?2, visible_after = ?3 WHERE dispatch_key = ?1";

const FAIL_ROW_SQL: &str = "
UPDATE outbox SET status = 'failed' WHERE dispatch_key = ?1";

/// Insert a single outbox row inside an existing transaction.
///
/// Shared by the standalone [`append_outbox_batch`] and the atomic-with-history `append_with_outbox`
/// path so both honour the `INSERT OR IGNORE` idempotency guard identically.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when the input payload cannot be encoded and
/// `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn insert_outbox_row(tx: &Transaction, row: &OutboxRow) -> Result<(), StoreError> {
    let input = encode_payload(&row.input)?;
    tx.execute(
        INSERT_OUTBOX_SQL,
        params![
            row.dispatch_key.clone(),
            row.workflow_id.to_string(),
            i64::try_from(row.ordinal).map_err(|_| StoreError::Backend(format!(
                "outbox ordinal overflow: {}",
                row.ordinal
            )))?,
            row.activity_type.clone(),
            input,
            row.status.as_str(),
            i64::from(row.attempt),
            encode_instant(row.visible_after)
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::error::libsql_error(&error))
}

/// Insert `rows` under a dedicated `IMMEDIATE` transaction, ignoring duplicate `dispatch_key`s.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when a row cannot be encoded and `StoreError::Backend` for
/// libSQL boundary failures.
pub(crate) async fn append_outbox_batch(
    conn: &Connection,
    rows: &[OutboxRow],
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    for row in rows {
        if let Err(error) = insert_outbox_row(&tx, row).await {
            rollback(tx).await?;
            return Err(error);
        }
    }

    tx.commit()
        .await
        .map_err(|error| crate::error::libsql_error(&error))
}

/// Claim up to `limit` due pending rows, flipping them to `claimed` in one `IMMEDIATE` transaction.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures and `StoreError::Serialization` when a
/// stored row cannot be decoded.
pub(crate) async fn claim_outbox_rows(
    conn: &Connection,
    limit: u32,
) -> Result<Vec<OutboxRow>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let claimed = match select_and_claim(&tx, limit).await {
        Ok(claimed) => claimed,
        Err(error) => {
            rollback(tx).await?;
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(claimed)
}

async fn select_and_claim(tx: &Transaction, limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
    let now = encode_instant(Utc::now());
    let mut rows = tx
        .query(SELECT_CLAIMABLE_SQL, params![now, i64::from(limit)])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut claimed = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let decoded = decode_row(&row)?;
        tx.execute(CLAIM_ROW_SQL, params![decoded.dispatch_key.clone()])
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        claimed.push(OutboxRow {
            status: OutboxStatus::Claimed,
            ..decoded
        });
    }

    Ok(claimed)
}

/// Mark the row identified by `dispatch_key` as `done`.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn complete_outbox_row(
    conn: &Connection,
    dispatch_key: &str,
) -> Result<(), StoreError> {
    conn.execute(COMPLETE_ROW_SQL, params![dispatch_key.to_string()])
        .await
        .map(|_| ())
        .map_err(|error| crate::error::libsql_error(&error))
}

/// Return the row identified by `dispatch_key` to `pending` with updated attempt and backoff fence.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn retry_outbox_row(
    conn: &Connection,
    dispatch_key: &str,
    next_attempt: u32,
    visible_after: DateTime<Utc>,
) -> Result<(), StoreError> {
    conn.execute(
        RETRY_ROW_SQL,
        params![
            dispatch_key.to_string(),
            i64::from(next_attempt),
            encode_instant(visible_after)
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::error::libsql_error(&error))
}

/// Mark the row identified by `dispatch_key` as `failed` (dead letter).
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn fail_outbox_row(
    conn: &Connection,
    dispatch_key: &str,
) -> Result<(), StoreError> {
    conn.execute(FAIL_ROW_SQL, params![dispatch_key.to_string()])
        .await
        .map(|_| ())
        .map_err(|error| crate::error::libsql_error(&error))
}

/// Out-of-band snapshot of one outbox row's lifecycle bookkeeping.
///
/// Read by [`LibSqlStore::outbox_row_state`](crate::LibSqlStore::outbox_row_state)
/// for tests and operator inspection. It carries only the mutable
/// dispatch-state columns — status, attempt count, and the retry-backoff fence —
/// not the full row payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRowState {
    /// Current lifecycle state of the row.
    pub status: OutboxStatus,
    /// Dispatch attempt count recorded on the row.
    pub attempt: u32,
    /// Earliest instant at which the row becomes claimable again.
    pub visible_after: DateTime<Utc>,
}

const SELECT_ROW_STATE_SQL: &str = "
SELECT status, attempt, visible_after FROM outbox WHERE dispatch_key = ?1";

/// Read the `(status, attempt, visible_after)` bookkeeping for one row, or `None` when absent.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures and `StoreError::Serialization` when
/// the stored status token or timestamp cannot be decoded.
pub(crate) async fn outbox_row_state(
    conn: &Connection,
    dispatch_key: &str,
) -> Result<Option<OutboxRowState>, StoreError> {
    let mut rows = conn
        .query(SELECT_ROW_STATE_SQL, params![dispatch_key.to_string()])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    else {
        return Ok(None);
    };
    let status: String = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let attempt: i64 = row
        .get(1)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let visible_after: String = row
        .get(2)
        .map_err(|error| crate::error::libsql_error(&error))?;
    Ok(Some(OutboxRowState {
        status: OutboxStatus::parse_token(&status)?,
        attempt: u32::try_from(attempt)
            .map_err(|_| StoreError::Backend(format!("outbox attempt out of range: {attempt}")))?,
        visible_after: decode_instant(&visible_after)?,
    }))
}

fn decode_row(row: &Row) -> Result<OutboxRow, StoreError> {
    let dispatch_key: String = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let workflow_id: String = row
        .get(1)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let ordinal: i64 = row
        .get(2)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let activity_type: String = row
        .get(3)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let input: Vec<u8> = row
        .get(4)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let status: String = row
        .get(5)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let attempt: i64 = row
        .get(6)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let visible_after: String = row
        .get(7)
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(OutboxRow {
        dispatch_key,
        workflow_id: decode_workflow_id(&workflow_id)?,
        ordinal: u64::try_from(ordinal)
            .map_err(|_| StoreError::Backend(format!("outbox ordinal was negative: {ordinal}")))?,
        activity_type,
        input: decode_payload(&input)?,
        status: OutboxStatus::parse_token(&status)?,
        attempt: u32::try_from(attempt)
            .map_err(|_| StoreError::Backend(format!("outbox attempt out of range: {attempt}")))?,
        visible_after: decode_instant(&visible_after)?,
    })
}

fn encode_payload(payload: &Payload) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(payload).map_err(|error| crate::error::serde_json_error(&error))
}

fn decode_payload(bytes: &[u8]) -> Result<Payload, StoreError> {
    serde_json::from_slice(bytes).map_err(|error| crate::error::serde_json_error(&error))
}

fn decode_workflow_id(value: &str) -> Result<WorkflowId, StoreError> {
    uuid::Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|error| StoreError::Serialization(format!("invalid outbox workflow id: {error}")))
}

fn encode_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_instant(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

async fn rollback(tx: Transaction) -> Result<(), StoreError> {
    tx.rollback()
        .await
        .map_err(|error| crate::error::libsql_error(&error))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::{
        ContentType, Event, OutboxRow, OutboxStatus, OutboxStore, Payload, StoreError, WorkflowId,
        WriteToken,
    };
    use chrono::{DateTime, TimeZone, Utc};
    use libsql::params;
    use serde_json::{Value, json};

    use crate::LibSqlStore;

    #[tokio::test]
    async fn append_outbox_batch_ignores_duplicate_dispatch_key() -> Result<(), StoreError> {
        let store = open_test_store("dup-key").await?;
        let workflow_id = WorkflowId::new_v4();
        let first = pending_row(&workflow_id, 0, "charge", instant(1)?);
        let duplicate = pending_row(&workflow_id, 0, "different-activity", instant(2)?);

        store
            .append_outbox_batch(std::slice::from_ref(&first))
            .await?;
        store.append_outbox_batch(&[duplicate]).await?;

        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 1);
        // The original row survived; the duplicate was silently ignored, not overwritten.
        assert_eq!(claimed[0].activity_type, "charge");
        assert_eq!(claimed[0].dispatch_key, first.dispatch_key);
        Ok(())
    }

    #[tokio::test]
    async fn claim_complete_retry_round_trip() -> Result<(), StoreError> {
        let store = open_test_store("round-trip").await?;
        let workflow_id = WorkflowId::new_v4();
        let row_a = pending_row(&workflow_id, 0, "a", instant(1)?);
        let row_b = pending_row(&workflow_id, 1, "b", instant(2)?);

        store
            .append_outbox_batch(&[row_a.clone(), row_b.clone()])
            .await?;

        // Claim flips both rows to claimed and returns them in visible_after order.
        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 2);
        assert!(
            claimed
                .iter()
                .all(|row| row.status == OutboxStatus::Claimed)
        );
        assert_eq!(claimed[0].ordinal, 0);
        assert_eq!(claimed[1].ordinal, 1);

        // A second claim sees nothing pending.
        assert!(store.claim_outbox_rows(10).await?.is_empty());

        // Complete one row; it leaves the claimable set permanently.
        store.complete_outbox_row(&row_a.dispatch_key).await?;
        assert_eq!(
            status_of(store.connection(), &row_a.dispatch_key).await?,
            Some(String::from("done"))
        );

        // Retry the other with a future fence: it returns to pending but is not yet claimable.
        // The claim path compares `visible_after` against the wall clock, so the fence must be a
        // real future instant relative to `Utc::now()`, not one of the tiny synthetic timestamps.
        let future = Utc::now() + chrono::Duration::hours(1);
        store
            .retry_outbox_row(&row_b.dispatch_key, 1, future)
            .await?;
        assert_eq!(
            status_of(store.connection(), &row_b.dispatch_key).await?,
            Some(String::from("pending"))
        );
        assert!(store.claim_outbox_rows(10).await?.is_empty());

        // Retry into the past: now claimable again with the bumped attempt.
        store
            .retry_outbox_row(&row_b.dispatch_key, 2, instant(1)?)
            .await?;
        let reclaimed = store.claim_outbox_rows(10).await?;
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].dispatch_key, row_b.dispatch_key);
        assert_eq!(reclaimed[0].attempt, 2);

        // Fail it: terminal, never claimable again.
        store.fail_outbox_row(&row_b.dispatch_key).await?;
        assert_eq!(
            status_of(store.connection(), &row_b.dispatch_key).await?,
            Some(String::from("failed"))
        );
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn claim_respects_limit() -> Result<(), StoreError> {
        let store = open_test_store("claim-limit").await?;
        let workflow_id = WorkflowId::new_v4();
        let mut rows: Vec<OutboxRow> = Vec::new();
        for ordinal in 0..5_u64 {
            let visible_after = instant(i64::try_from(ordinal).unwrap_or(0) + 1)?;
            rows.push(pending_row(&workflow_id, ordinal, "a", visible_after));
        }
        store.append_outbox_batch(&rows).await?;

        let first = store.claim_outbox_rows(2).await?;
        assert_eq!(first.len(), 2);
        let rest = store.claim_outbox_rows(10).await?;
        assert_eq!(rest.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn append_with_outbox_commits_events_and_rows_atomically() -> Result<(), StoreError> {
        let store = open_test_store("atomic-commit").await?;
        let workflow_id = WorkflowId::new_v4();
        let events = vec![workflow_started(&workflow_id, 1)?];
        let row = pending_row(&workflow_id, 0, "charge", instant(1)?);

        store
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                &events,
                0,
                Some(std::slice::from_ref(&row)),
            )
            .await?;

        assert_eq!(event_count(store.connection(), &workflow_id).await?, 1);
        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].dispatch_key, row.dispatch_key);
        Ok(())
    }

    #[tokio::test]
    async fn append_with_outbox_rolls_back_both_on_failure() -> Result<(), StoreError> {
        let store = open_test_store("atomic-rollback").await?;
        let workflow_id = WorkflowId::new_v4();
        let events = vec![workflow_started(&workflow_id, 1)?];
        let row = pending_row(&workflow_id, 0, "charge", instant(1)?);

        // Force a mid-transaction failure AFTER the events insert succeeds: dropping the outbox
        // table makes the outbox insert fail, which must roll back the already-inserted events too.
        store
            .connection()
            .execute("DROP TABLE outbox", ())
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;

        let result = store
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                &events,
                0,
                Some(&[row]),
            )
            .await;

        assert!(result.is_err(), "outbox insert failure must surface as Err");
        // Neither the events nor the outbox rows were committed: the events table is empty.
        assert_eq!(event_count(store.connection(), &workflow_id).await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn event_only_append_with_outbox_matches_plain_append() -> Result<(), StoreError> {
        let store = open_test_store("event-only").await?;
        let workflow_id = WorkflowId::new_v4();
        let events = vec![workflow_started(&workflow_id, 1)?];

        store
            .append_with_outbox(WriteToken::recorder(), &workflow_id, &events, 0, None)
            .await?;

        assert_eq!(event_count(store.connection(), &workflow_id).await?, 1);
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    async fn open_test_store(name: &str) -> Result<LibSqlStore, StoreError> {
        LibSqlStore::open(unique_temp_path(name)).await
    }

    fn pending_row(
        workflow_id: &WorkflowId,
        ordinal: u64,
        activity_type: &str,
        visible_after: DateTime<Utc>,
    ) -> OutboxRow {
        OutboxRow::pending(
            workflow_id.clone(),
            ordinal,
            String::from(activity_type),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            visible_after,
        )
    }

    async fn status_of(
        conn: &libsql::Connection,
        dispatch_key: &str,
    ) -> Result<Option<String>, StoreError> {
        let mut rows = conn
            .query(
                "SELECT status FROM outbox WHERE dispatch_key = ?1",
                params![dispatch_key.to_string()],
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        match rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
        {
            Some(row) => Ok(Some(
                row.get(0)
                    .map_err(|error| crate::error::libsql_error(&error))?,
            )),
            None => Ok(None),
        }
    }

    async fn event_count(
        conn: &libsql::Connection,
        workflow_id: &WorkflowId,
    ) -> Result<i64, StoreError> {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM events WHERE workflow_id = ?1",
                params![workflow_id.to_string()],
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .ok_or_else(|| StoreError::Backend(String::from("event count returned no row")))?;
        row.get(0)
            .map_err(|error| crate::error::libsql_error(&error))
    }

    fn workflow_started(workflow_id: &WorkflowId, seq: u64) -> Result<Event, StoreError> {
        event_from_json(json!({
            "type": "WorkflowStarted",
            "data": {
                "envelope": {
                    "seq": seq,
                    "recorded_at": DateTime::<Utc>::from(UNIX_EPOCH).to_rfc3339(),
                    "workflow_id": workflow_id,
                },
                "workflow_type": "test-outbox",
                "input": {
                    "content_type": "Json",
                    "bytes": serde_json::to_vec(&json!({ "label": "outbox" }))
                        .map_err(|error| StoreError::Serialization(error.to_string()))?,
                },
                "run_id": uuid::Uuid::from_u128(seq.into()).to_string(),
                "parent_run_id": null,
                "package_version": "a".repeat(64),
            }
        }))
    }

    fn event_from_json(value: Value) -> Result<Event, StoreError> {
        serde_json::from_value(value).map_err(|error| StoreError::Serialization(error.to_string()))
    }

    fn instant(seconds: i64) -> Result<DateTime<Utc>, StoreError> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| StoreError::Serialization(String::from("invalid test instant")))
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-outbox-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
