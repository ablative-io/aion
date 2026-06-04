//! Event read operations: history retrieval, active workflow listing, filtered queries.

use aion_store::{Event, StoreError, WorkflowFilter, WorkflowId, WorkflowSummary};

/// History reads are wired through `LibSqlStore`; AS-005 replaces this placeholder with SQL.
pub(crate) fn read_history(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, StoreError> {
    let _ = (conn, workflow_id);
    Err(StoreError::Backend(String::from(
        "LibSqlStore::read_history is wired; event reads are implemented by AS-005",
    )))
}

/// Active workflow listing is wired through `LibSqlStore`; AS-005 replaces this placeholder.
pub(crate) fn list_active(conn: &libsql::Connection) -> Result<Vec<WorkflowId>, StoreError> {
    let _ = conn;
    Err(StoreError::Backend(String::from(
        "LibSqlStore::list_active is wired; active listing is implemented by AS-005",
    )))
}

/// Workflow summary queries are wired through `LibSqlStore`; AS-005 replaces this placeholder.
pub(crate) fn query(
    conn: &libsql::Connection,
    filter: &WorkflowFilter,
) -> Result<Vec<WorkflowSummary>, StoreError> {
    let _ = (conn, filter);
    Err(StoreError::Backend(String::from(
        "LibSqlStore::query is wired; workflow queries are implemented by AS-005",
    )))
}
