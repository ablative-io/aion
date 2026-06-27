use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aion_store::{
    ContentType, Event, OutboxRow, OutboxStatus, OutboxStore, Payload, StoreError, WorkflowId,
    WritableEventStore, WriteToken,
};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
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
async fn staged_row_round_trips_namespace_and_task_queue() -> Result<(), StoreError> {
    // NSTQ-2: a row staged with an explicit routing identity persists and reads
    // back both `namespace` and `task_queue` verbatim through claim.
    let store = open_test_store("ns-tq-round-trip").await?;
    let workflow_id = WorkflowId::new_v4();
    let row = pending_row(&workflow_id, 0, "charge", instant(1)?)
        .with_namespace("remote")
        .with_task_queue("gpu");

    store
        .append_outbox_batch(std::slice::from_ref(&row))
        .await?;

    let claimed = store.claim_outbox_rows(10).await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].namespace, "remote");
    assert_eq!(claimed[0].task_queue, "gpu");
    Ok(())
}

#[tokio::test]
async fn legacy_null_namespace_and_task_queue_read_back_as_default() -> Result<(), StoreError> {
    // NSTQ-2 legacy fallback: a pre-migration row whose `namespace`/`task_queue`
    // columns are NULL reads back as the `"default"` routing identity at the
    // store-read layer, so the dispatcher always sees a concrete pool.
    let store = open_test_store("ns-tq-legacy-null").await?;
    let workflow_id = WorkflowId::new_v4();
    let dispatch_key = OutboxRow::dispatch_key_for(&workflow_id, 0);
    // Insert a row with NULL namespace + task_queue, exactly as a row persisted
    // before the additive columns existed would read back.
    store
        .connection()
        .execute(
            "INSERT INTO outbox \
             (dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, \
              visible_after, claimed_at, run_id, namespace, task_queue) \
             VALUES (?1, ?2, 0, 'charge', ?3, 'pending', 0, ?4, NULL, NULL, NULL, NULL)",
            params![
                dispatch_key.clone(),
                workflow_id.to_string(),
                serde_json::to_vec(&Payload::new(ContentType::Json, b"{}".to_vec()))
                    .map_err(|error| StoreError::Serialization(error.to_string()))?,
                encode_instant(instant(1)?),
            ],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let claimed = store.claim_outbox_rows(10).await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].dispatch_key, dispatch_key);
    assert_eq!(claimed[0].namespace, "default");
    assert_eq!(claimed[0].task_queue, "default");
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
    assert!(claimed.iter().all(|row| row.claimed_at.is_some()));

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
async fn stale_claim_rearm_touches_only_old_claimed_rows_and_preserves_attempt()
-> Result<(), StoreError> {
    let store = open_test_store("stale-claim").await?;
    let workflow_id = WorkflowId::new_v4();
    let stale = pending_row(&workflow_id, 0, "stale", instant(1)?);
    let fresh = pending_row(&workflow_id, 1, "fresh", instant(1)?);
    let done = pending_row(&workflow_id, 2, "done", instant(1)?);
    let failed = pending_row(&workflow_id, 3, "failed", instant(1)?);
    store
        .append_outbox_batch(&[stale.clone(), fresh.clone(), done.clone(), failed.clone()])
        .await?;

    let claimed = store.claim_outbox_rows(10).await?;
    assert_eq!(claimed.len(), 4);
    store.complete_outbox_row(&done.dispatch_key).await?;
    store.fail_outbox_row(&failed.dispatch_key).await?;

    set_outbox_bookkeeping(
        store.connection(),
        &stale.dispatch_key,
        "claimed",
        7,
        instant(10)?,
    )
    .await?;
    set_outbox_bookkeeping(
        store.connection(),
        &fresh.dispatch_key,
        "claimed",
        3,
        instant(150)?,
    )
    .await?;
    set_outbox_bookkeeping(
        store.connection(),
        &done.dispatch_key,
        "done",
        5,
        instant(10)?,
    )
    .await?;
    set_outbox_bookkeeping(
        store.connection(),
        &failed.dispatch_key,
        "failed",
        6,
        instant(10)?,
    )
    .await?;

    let rearmed = store
        .rearm_stale_claimed_outbox_rows(instant(100)?, instant(200)?, 10)
        .await?;
    assert_eq!(rearmed.len(), 1);
    assert_eq!(rearmed[0].dispatch_key, stale.dispatch_key);
    assert_eq!(rearmed[0].status, OutboxStatus::Pending);
    assert_eq!(rearmed[0].attempt, 7);
    assert_eq!(rearmed[0].visible_after, instant(200)?);
    assert_eq!(rearmed[0].claimed_at, None);
    assert_eq!(
        store
            .outbox_row_state(&stale.dispatch_key)
            .await?
            .map(|state| state.attempt),
        Some(7)
    );
    assert_eq!(
        status_of(store.connection(), &fresh.dispatch_key).await?,
        Some(String::from("claimed"))
    );
    assert_eq!(
        status_of(store.connection(), &done.dispatch_key).await?,
        Some(String::from("done"))
    );
    assert_eq!(
        status_of(store.connection(), &failed.dispatch_key).await?,
        Some(String::from("failed"))
    );
    assert_eq!(
        claimed_at_of(store.connection(), &stale.dispatch_key).await?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn settle_cancelled_is_idempotent_and_terminal() -> Result<(), StoreError> {
    let store = open_test_store("settle-cancelled").await?;
    let workflow_id = WorkflowId::new_v4();
    let pending = pending_row(&workflow_id, 0, "pending", instant(1)?);
    let claimed = pending_row(&workflow_id, 1, "claimed", instant(1)?);
    let done = pending_row(&workflow_id, 2, "done", instant(1)?);
    let failed = pending_row(&workflow_id, 3, "failed", instant(1)?);

    store
        .append_outbox_batch(&[
            pending.clone(),
            claimed.clone(),
            done.clone(),
            failed.clone(),
        ])
        .await?;

    store
        .settle_outbox_row_cancelled(&pending.dispatch_key)
        .await?;
    store
        .settle_outbox_row_cancelled(&pending.dispatch_key)
        .await?;
    assert_eq!(
        status_of(store.connection(), &pending.dispatch_key).await?,
        Some(String::from("cancelled"))
    );
    let claimed_rows = store.claim_outbox_rows(10).await?;
    assert!(
        !claimed_rows
            .iter()
            .any(|row| row.dispatch_key == pending.dispatch_key),
        "cancelled pending row must not be claimable"
    );
    assert!(
        claimed_rows
            .iter()
            .any(|row| row.dispatch_key == claimed.dispatch_key),
        "claimed test row should have been claimed before settlement"
    );
    store
        .settle_outbox_row_cancelled(&claimed.dispatch_key)
        .await?;
    assert_eq!(
        status_of(store.connection(), &claimed.dispatch_key).await?,
        Some(String::from("cancelled"))
    );
    assert_eq!(
        claimed_at_of(store.connection(), &claimed.dispatch_key).await?,
        None,
        "cancelling a claimed row clears claimed_at"
    );
    let rearmed = store
        .rearm_stale_claimed_outbox_rows(Utc::now() + chrono::Duration::hours(1), instant(200)?, 10)
        .await?;
    assert!(
        !rearmed
            .iter()
            .any(|row| row.dispatch_key == claimed.dispatch_key),
        "cancelled claimed row must not be stale-rearmed"
    );

    store.complete_outbox_row(&done.dispatch_key).await?;
    store.fail_outbox_row(&failed.dispatch_key).await?;
    store
        .settle_outbox_row_cancelled(&done.dispatch_key)
        .await?;
    store
        .settle_outbox_row_cancelled(&failed.dispatch_key)
        .await?;
    store
        .settle_outbox_row_cancelled("absent-dispatch-key")
        .await?;
    assert_eq!(
        status_of(store.connection(), &done.dispatch_key).await?,
        Some(String::from("done"))
    );
    assert_eq!(
        status_of(store.connection(), &failed.dispatch_key).await?,
        Some(String::from("failed"))
    );
    Ok(())
}

#[tokio::test]
async fn rearm_outbox_pending_revives_a_done_row_and_inserts_a_fresh_one() -> Result<(), StoreError>
{
    use aion_store::WritableEventStore;

    let store = open_test_store("rearm").await?;
    let workflow_id = WorkflowId::new_v4();

    // Stage one row, drive it through claim -> done so it has left the claimable set.
    let original = pending_row(&workflow_id, 0, "charge", instant(1)?);
    store
        .append_outbox_batch(std::slice::from_ref(&original))
        .await?;
    let claimed = store.claim_outbox_rows(10).await?;
    assert_eq!(claimed.len(), 1);
    store.complete_outbox_row(&original.dispatch_key).await?;
    assert_eq!(
        status_of(store.connection(), &original.dispatch_key).await?,
        Some(String::from("done"))
    );
    assert!(store.claim_outbox_rows(10).await?.is_empty());

    // Re-arm the SAME dispatch_key (UPDATE branch) plus a brand-new ordinal (INSERT branch).
    let revived = pending_row(&workflow_id, 0, "charge", Utc::now());
    let fresh = pending_row(&workflow_id, 1, "settle", Utc::now());
    store
        .rearm_outbox_pending(&[revived.clone(), fresh.clone()])
        .await?;

    // The previously-done row is back to pending...
    assert_eq!(
        status_of(store.connection(), &revived.dispatch_key).await?,
        Some(String::from("pending"))
    );
    // ...and the brand-new dispatch_key was inserted as pending.
    assert_eq!(
        status_of(store.connection(), &fresh.dispatch_key).await?,
        Some(String::from("pending"))
    );

    // Both are now claimable again.
    let reclaimed = store.claim_outbox_rows(10).await?;
    let mut keys: Vec<String> = reclaimed.into_iter().map(|row| row.dispatch_key).collect();
    keys.sort();
    let mut expected = vec![revived.dispatch_key.clone(), fresh.dispatch_key.clone()];
    expected.sort();
    assert_eq!(keys, expected);
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

async fn set_outbox_bookkeeping(
    conn: &libsql::Connection,
    dispatch_key: &str,
    status: &str,
    attempt: u32,
    claimed_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE outbox SET status = ?2, attempt = ?3, claimed_at = ?4 WHERE dispatch_key = ?1",
        params![
            dispatch_key.to_string(),
            status.to_string(),
            i64::from(attempt),
            encode_instant(claimed_at),
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::error::libsql_error(&error))
}

async fn claimed_at_of(
    conn: &libsql::Connection,
    dispatch_key: &str,
) -> Result<Option<String>, StoreError> {
    let mut rows = conn
        .query(
            "SELECT claimed_at FROM outbox WHERE dispatch_key = ?1",
            params![dispatch_key.to_string()],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    match rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        Some(row) => row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error)),
        None => Ok(None),
    }
}

fn encode_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
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
