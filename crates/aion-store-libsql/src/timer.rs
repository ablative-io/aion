//! Durable timer scheduling and expiry retrieval.

use aion_store::{StoreError, TimerEntry, TimerId, WorkflowId};
use chrono::{DateTime, Utc};

/// Timer scheduling is wired through `LibSqlStore`; AS-006 replaces this placeholder with SQL.
pub(crate) async fn schedule_timer(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    timer_id: &TimerId,
    fire_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let _ = (conn, workflow_id, timer_id, fire_at);
    Err(StoreError::Backend(String::from(
        "LibSqlStore::schedule_timer is wired; durable timers are implemented by AS-006",
    )))
}

/// Due timer reads are wired through `LibSqlStore`; AS-006 replaces this placeholder with SQL.
pub(crate) async fn expired_timers(
    conn: &libsql::Connection,
    as_of: DateTime<Utc>,
) -> Result<Vec<TimerEntry>, StoreError> {
    let _ = (conn, as_of);
    Err(StoreError::Backend(String::from(
        "LibSqlStore::expired_timers is wired; durable timers are implemented by AS-006",
    )))
}
