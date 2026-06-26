//! libSQL outbox terminal and retry transition helpers.

use aion_store::StoreError;
use chrono::{DateTime, SecondsFormat, Utc};
use libsql::{Connection, params};

const COMPLETE_ROW_SQL: &str = "
UPDATE outbox SET status = 'done', claimed_at = NULL WHERE dispatch_key = ?1";

const RETRY_ROW_SQL: &str = "
UPDATE outbox SET status = 'pending', attempt = ?2, visible_after = ?3, claimed_at = NULL WHERE dispatch_key = ?1";

const FAIL_ROW_SQL: &str = "
UPDATE outbox SET status = 'failed', claimed_at = NULL WHERE dispatch_key = ?1";

const SETTLE_CANCELLED_ROW_SQL: &str = "
UPDATE outbox SET status = 'cancelled', claimed_at = NULL
WHERE dispatch_key = ?1 AND status IN ('pending', 'claimed')";

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

/// Idempotently settle a live row identified by `dispatch_key` as `cancelled`.
///
/// Only `pending` and `claimed` rows transition. Absent, already-cancelled, `done`, and `failed`
/// rows are safe no-ops.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn settle_outbox_row_cancelled(
    conn: &Connection,
    dispatch_key: &str,
) -> Result<(), StoreError> {
    conn.execute(SETTLE_CANCELLED_ROW_SQL, params![dispatch_key.to_string()])
        .await
        .map(|_| ())
        .map_err(|error| crate::error::libsql_error(&error))
}

fn encode_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
