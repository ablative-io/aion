//! Atomic append with the sequence guard.

use aion_store::{Event, StoreError, WorkflowId};

/// Append is wired through `LibSqlStore`; AS-004 replaces this placeholder with atomic SQL.
pub(crate) async fn append(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    events: &[Event],
    expected_seq: u64,
) -> Result<(), StoreError> {
    let _ = (conn, workflow_id, events, expected_seq);
    Err(StoreError::Backend(String::from(
        "LibSqlStore::append is wired; atomic append is implemented by AS-004",
    )))
}
