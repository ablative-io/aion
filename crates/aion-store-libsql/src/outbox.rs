//! libSQL-backed durable outbox: staging, claim, and terminal transitions.
//!
//! Rows are inserted with `INSERT OR IGNORE` so a duplicate `dispatch_key` is silently dropped,
//! preserving at-most-once dispatch. Claims run under an `IMMEDIATE` transaction (the single-writer
//! `SQLite` equivalent of `SELECT ... FOR UPDATE SKIP LOCKED`): pending rows are flipped to `claimed`
//! and returned in one atomic step so no two dispatchers observe the same row as claimable.

use aion_store::{
    ClaimScope, DEFAULT_OUTBOX_ROUTE, OutboxRow, OutboxStatus, Payload, RunId, StoreError,
    WorkflowId,
};
use chrono::{DateTime, SecondsFormat, Utc};
use libsql::{Connection, Row, Transaction, TransactionBehavior, params};

mod transitions;
pub(crate) use transitions::{
    complete_outbox_row, fail_outbox_row, retry_outbox_row, settle_outbox_row_cancelled,
};

const INSERT_OUTBOX_SQL: &str = "
INSERT OR IGNORE INTO outbox
    (dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after, claimed_at, run_id, namespace, task_queue, node)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";

const REARM_OUTBOX_SQL: &str = "
INSERT INTO outbox
    (dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after, claimed_at, namespace, task_queue, node)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11)
ON CONFLICT(dispatch_key) DO UPDATE SET status = 'pending', visible_after = ?8, claimed_at = NULL";

const SELECT_CLAIMABLE_SQL: &str = "
SELECT dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after, claimed_at, run_id, namespace, task_queue, node
FROM outbox
WHERE status = 'pending' AND visible_after <= ?1
ORDER BY visible_after ASC, dispatch_key ASC
LIMIT ?2";

// Scoped claim (LSUB-1a): additionally constrain by the pool's `(namespace, task_queue)` and the
// node predicate. The node clause is appended per-scope: a node-bearing scope claims rows pinned to
// that node OR unpinned rows (`node IS NULL`); a node-less scope claims only unpinned rows. This is
// the byte-identical due/order/limit claim with an extra WHERE conjunction.
const SELECT_CLAIMABLE_SCOPED_PREFIX_SQL: &str = "
SELECT dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after, claimed_at, run_id, namespace, task_queue, node
FROM outbox
WHERE status = 'pending' AND visible_after <= ?1 AND namespace = ?3 AND task_queue = ?4";

const SELECT_CLAIMABLE_SCOPED_SUFFIX_SQL: &str = "
ORDER BY visible_after ASC, dispatch_key ASC
LIMIT ?2";

// Node predicate fragments spliced between the prefix and suffix above. `?5` binds the scope node.
const NODE_CLAUSE_PINNED_OR_UNPINNED: &str = " AND (node IS NULL OR node = ?5)";
const NODE_CLAUSE_UNPINNED_ONLY: &str = " AND node IS NULL";

const CLAIM_ROW_SQL: &str = "
UPDATE outbox SET status = 'claimed', claimed_at = ?2 WHERE dispatch_key = ?1 AND status = 'pending'";

// In-flight count (CP2-Q1.5): rows in this namespace that are dispatched-but-not-terminal, i.e.
// `pending` OR `claimed`. Terminal rows (`done`, `failed`, `cancelled`) are excluded by the IN list,
// and the predicate is strictly scoped by `namespace = ?1` so no other namespace bleeds in.
const COUNT_INFLIGHT_OUTBOX_SQL: &str = "
SELECT COUNT(*) FROM outbox WHERE namespace = ?1 AND status IN ('pending', 'claimed')";

const SELECT_STALE_CLAIMED_SQL: &str = "
SELECT dispatch_key, workflow_id, ordinal, activity_type, input, status, attempt, visible_after, claimed_at, run_id, namespace, task_queue, node
FROM outbox
WHERE status = 'claimed' AND claimed_at IS NOT NULL AND claimed_at < ?1
ORDER BY claimed_at ASC, dispatch_key ASC
LIMIT ?2";

const REARM_STALE_CLAIMED_ROW_SQL: &str = "
UPDATE outbox SET status = 'pending', visible_after = ?2, claimed_at = NULL
WHERE dispatch_key = ?1 AND status = 'claimed'";

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
            encode_instant(row.visible_after),
            row.claimed_at.map(encode_instant),
            row.run_id.as_ref().map(ToString::to_string),
            row.namespace.clone(),
            row.task_queue.clone(),
            row.node.clone()
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

/// Re-arm `rows` to claimable `pending` under one `IMMEDIATE` transaction (crash-recovery re-stage).
///
/// Each row is upserted: a brand-new `dispatch_key` is inserted as `pending` with the row's
/// `attempt` (zero for a fresh [`OutboxRow::pending`]); an existing `dispatch_key` is flipped back to
/// `status = 'pending'` with `visible_after` reset to the row's instant so it is immediately
/// claimable. The UPDATE branch deliberately does NOT touch `attempt`, preserving the dispatch retry
/// budget so a workflow that reliably crashes the server still eventually dead-letters.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when a row cannot be encoded and `StoreError::Backend` for
/// libSQL boundary failures.
pub(crate) async fn rearm_outbox_pending(
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
        if let Err(error) = rearm_outbox_row(&tx, row).await {
            rollback(tx).await?;
            return Err(error);
        }
    }

    tx.commit()
        .await
        .map_err(|error| crate::error::libsql_error(&error))
}

async fn rearm_outbox_row(tx: &Transaction, row: &OutboxRow) -> Result<(), StoreError> {
    let input = encode_payload(&row.input)?;
    tx.execute(
        REARM_OUTBOX_SQL,
        params![
            row.dispatch_key.clone(),
            row.workflow_id.to_string(),
            i64::try_from(row.ordinal).map_err(|_| StoreError::Backend(format!(
                "outbox ordinal overflow: {}",
                row.ordinal
            )))?,
            row.activity_type.clone(),
            input,
            OutboxStatus::Pending.as_str(),
            i64::from(row.attempt),
            encode_instant(row.visible_after),
            row.namespace.clone(),
            row.task_queue.clone(),
            row.node.clone()
        ],
    )
    .await
    .map(|_| ())
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
    let claimed_at = Utc::now();
    let now = encode_instant(claimed_at);
    let mut rows = tx
        .query(SELECT_CLAIMABLE_SQL, params![now.clone(), i64::from(limit)])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut claimed = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let decoded = decode_row(&row)?;
        tx.execute(
            CLAIM_ROW_SQL,
            params![decoded.dispatch_key.clone(), now.clone()],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
        claimed.push(OutboxRow {
            status: OutboxStatus::Claimed,
            claimed_at: Some(claimed_at),
            ..decoded
        });
    }

    Ok(claimed)
}

/// Claim up to `limit` due pending rows that are in `scope`, atomically (LSUB-1a).
///
/// Identical to [`claim_outbox_rows`] but with an additional `(namespace, task_queue, node)` filter
/// pushed into the SELECT WHERE clause. The unscoped path is untouched.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures and `StoreError::Serialization` when a
/// stored row cannot be decoded.
pub(crate) async fn claim_outbox_rows_scoped(
    conn: &Connection,
    scope: &ClaimScope,
    limit: u32,
) -> Result<Vec<OutboxRow>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let claimed = match select_and_claim_scoped(&tx, scope, limit).await {
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

async fn select_and_claim_scoped(
    tx: &Transaction,
    scope: &ClaimScope,
    limit: u32,
) -> Result<Vec<OutboxRow>, StoreError> {
    let claimed_at = Utc::now();
    let now = encode_instant(claimed_at);

    // Splice the node predicate into the scoped SELECT. A node-bearing scope serves rows pinned to
    // that node OR unpinned rows; a node-less scope serves only unpinned rows.
    let node_clause = if scope.node.is_some() {
        NODE_CLAUSE_PINNED_OR_UNPINNED
    } else {
        NODE_CLAUSE_UNPINNED_ONLY
    };
    let sql = format!(
        "{SELECT_CLAIMABLE_SCOPED_PREFIX_SQL}{node_clause}{SELECT_CLAIMABLE_SCOPED_SUFFIX_SQL}"
    );

    // `?5` is bound only when the scope carries a node; the node-less clause references no `?5`.
    let mut rows = if let Some(node) = scope.node.as_deref() {
        tx.query(
            &sql,
            params![
                now.clone(),
                i64::from(limit),
                scope.namespace.clone(),
                scope.task_queue.clone(),
                node.to_string()
            ],
        )
        .await
    } else {
        tx.query(
            &sql,
            params![
                now.clone(),
                i64::from(limit),
                scope.namespace.clone(),
                scope.task_queue.clone()
            ],
        )
        .await
    }
    .map_err(|error| crate::error::libsql_error(&error))?;

    let mut claimed = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let decoded = decode_row(&row)?;
        tx.execute(
            CLAIM_ROW_SQL,
            params![decoded.dispatch_key.clone(), now.clone()],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
        claimed.push(OutboxRow {
            status: OutboxStatus::Claimed,
            claimed_at: Some(claimed_at),
            ..decoded
        });
    }

    Ok(claimed)
}

/// Re-arm stale claimed rows to claimable `pending` without changing attempt count.
///
/// Rows with `NULL claimed_at` are ignored because no durable claim instant can be compared to the
/// caller-supplied threshold.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures and `StoreError::Serialization` when a
/// stored row cannot be decoded.
pub(crate) async fn rearm_stale_claimed_outbox_rows(
    conn: &Connection,
    older_than: DateTime<Utc>,
    visible_after: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<OutboxRow>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let rows = match select_and_rearm_stale_claimed(&tx, older_than, visible_after, limit).await {
        Ok(rows) => rows,
        Err(error) => {
            rollback(tx).await?;
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(rows)
}

async fn select_and_rearm_stale_claimed(
    tx: &Transaction,
    older_than: DateTime<Utc>,
    visible_after: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<OutboxRow>, StoreError> {
    let mut rows = tx
        .query(
            SELECT_STALE_CLAIMED_SQL,
            params![encode_instant(older_than), i64::from(limit)],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut rearmed = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let decoded = decode_row(&row)?;
        tx.execute(
            REARM_STALE_CLAIMED_ROW_SQL,
            params![decoded.dispatch_key.clone(), encode_instant(visible_after)],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
        rearmed.push(OutboxRow {
            status: OutboxStatus::Pending,
            visible_after,
            claimed_at: None,
            ..decoded
        });
    }

    Ok(rearmed)
}

/// Out-of-band snapshot of one outbox row's mutable lifecycle bookkeeping.
///
/// Read by [`LibSqlStore::outbox_row_state`](crate::LibSqlStore::outbox_row_state) for tests and
/// operator inspection; payload columns are intentionally omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRowState {
    /// Current lifecycle state.
    pub status: OutboxStatus,
    /// Dispatch attempt count.
    pub attempt: u32,
    /// Earliest instant at which the row becomes claimable again.
    pub visible_after: DateTime<Utc>,
}

const SELECT_ROW_STATE_SQL: &str = "
SELECT status, attempt, visible_after FROM outbox WHERE dispatch_key = ?1";

/// Read `(status, attempt, visible_after)` for one row, or `None` when absent.
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

/// Count the in-flight (`pending` OR `claimed`) outbox rows for `namespace` (CP2-Q1.5).
///
/// Pushes the status and namespace predicate into a single `COUNT(*)` so the count is computed
/// in-engine. A stuck-`claimed` row (dispatched but `mark_done` never landed) is still `claimed`
/// and so still counts; terminal rows do not.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures and `StoreError::Serialization` when
/// the engine returns a negative count.
pub(crate) async fn count_inflight_outbox_rows(
    conn: &Connection,
    namespace: &str,
) -> Result<u64, StoreError> {
    let mut rows = conn
        .query(COUNT_INFLIGHT_OUTBOX_SQL, params![namespace.to_string()])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    else {
        return Ok(0);
    };
    let count: i64 = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;
    u64::try_from(count)
        .map_err(|_| StoreError::Serialization(format!("outbox in-flight count negative: {count}")))
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
    let claimed_at: Option<String> = row
        .get(8)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let run_id: Option<String> = row
        .get(9)
        .map_err(|error| crate::error::libsql_error(&error))?;
    // Legacy rows persisted before NSTQ-2 added these columns read back as NULL; resolve them to the
    // `"default"` routing identity so the dispatcher has a concrete namespace + task queue.
    let namespace: Option<String> = row
        .get(10)
        .map_err(|error| crate::error::libsql_error(&error))?;
    let task_queue: Option<String> = row
        .get(11)
        .map_err(|error| crate::error::libsql_error(&error))?;
    // Node affinity is OPTIONAL: a NULL column (including legacy rows persisted before NODE-2 added
    // the column) decodes to `None` = no affinity. There is no sentinel string.
    let node: Option<String> = row
        .get(12)
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
        claimed_at: claimed_at.as_deref().map(decode_instant).transpose()?,
        run_id: run_id.as_deref().map(decode_run_id).transpose()?,
        namespace: namespace.unwrap_or_else(|| String::from(DEFAULT_OUTBOX_ROUTE)),
        task_queue: task_queue.unwrap_or_else(|| String::from(DEFAULT_OUTBOX_ROUTE)),
        node,
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

fn decode_run_id(value: &str) -> Result<RunId, StoreError> {
    uuid::Uuid::parse_str(value)
        .map(RunId::new)
        .map_err(|error| StoreError::Serialization(format!("invalid outbox run id: {error}")))
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
