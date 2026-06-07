//! Idempotent schema DDL for the libSQL event store.

use aion_store::StoreError;

/// Append-only workflow event table.
pub const CREATE_EVENTS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS events (
    workflow_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    event BLOB NOT NULL,
    recorded_at TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    is_queryable_event INTEGER NOT NULL,
    workflow_type TEXT,
    child_workflow_id TEXT,
    PRIMARY KEY (workflow_id, seq)
)";

/// Event index supporting lifecycle projection scans and filter subqueries.
pub const CREATE_EVENTS_PROJECTION_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_events_queryable_filter
ON events (is_queryable_event, workflow_id, seq, event_kind, workflow_type, recorded_at, child_workflow_id)";

/// Durable workflow timers table.
pub const CREATE_TIMERS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS timers (
    workflow_id TEXT NOT NULL,
    timer_id TEXT NOT NULL,
    fire_at TEXT NOT NULL,
    PRIMARY KEY (workflow_id, timer_id)
)";

/// Timer index supporting due-timer range scans.
pub const CREATE_TIMERS_FIRE_AT_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_timers_fire_at
ON timers (fire_at)";

/// Workflow visibility projection table.
pub const CREATE_VISIBILITY_TABLE: &str = "
CREATE TABLE IF NOT EXISTS visibility (
    workflow_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    workflow_type TEXT NOT NULL,
    status TEXT NOT NULL,
    start_time TEXT NOT NULL,
    close_time TEXT,
    search_attributes TEXT NOT NULL CHECK (json_valid(search_attributes))
)";

/// Visibility index supporting workflow-type equality filters.
pub const CREATE_VISIBILITY_WORKFLOW_TYPE_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_visibility_workflow_type
ON visibility (workflow_type)";

/// Visibility index supporting status equality filters.
pub const CREATE_VISIBILITY_STATUS_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_visibility_status
ON visibility (status)";

/// Visibility index supporting start-time range filters and ordering.
pub const CREATE_VISIBILITY_START_TIME_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_visibility_start_time
ON visibility (start_time)";

/// Visibility index supporting close-time range filters.
pub const CREATE_VISIBILITY_CLOSE_TIME_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_visibility_close_time
ON visibility (close_time)";

const DDL_STATEMENTS: [&str; 9] = [
    CREATE_EVENTS_TABLE,
    CREATE_EVENTS_PROJECTION_INDEX,
    CREATE_TIMERS_TABLE,
    CREATE_TIMERS_FIRE_AT_INDEX,
    CREATE_VISIBILITY_TABLE,
    CREATE_VISIBILITY_WORKFLOW_TYPE_INDEX,
    CREATE_VISIBILITY_STATUS_INDEX,
    CREATE_VISIBILITY_START_TIME_INDEX,
    CREATE_VISIBILITY_CLOSE_TIME_INDEX,
];

/// Ensure the libSQL schema exists on a fresh or previously-created database.
///
/// # Errors
///
/// Returns `StoreError::Backend` when any idempotent DDL statement fails at the libSQL boundary.
pub async fn ensure_schema(conn: &libsql::Connection) -> Result<(), StoreError> {
    for statement in DDL_STATEMENTS {
        conn.execute(statement, ())
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::StoreError;

    use super::ensure_schema;
    use crate::config::{LibSqlConfig, LibSqlMode};
    use crate::connection::open_connection;

    #[tokio::test]
    async fn ensure_schema_is_idempotent() -> Result<(), StoreError> {
        let conn = open_test_connection("idempotent").await?;

        ensure_schema(&conn).await?;
        ensure_schema(&conn).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ensure_schema_creates_tables_and_indexes() -> Result<(), StoreError> {
        let conn = open_test_connection("objects").await?;

        ensure_schema(&conn).await?;

        assert_schema_object(&conn, "table", "events").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_events_1").await?;
        assert_schema_object(&conn, "index", "idx_events_queryable_filter").await?;
        assert_schema_object(&conn, "table", "timers").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_timers_1").await?;
        assert_schema_object(&conn, "index", "idx_timers_fire_at").await?;
        assert_schema_object(&conn, "table", "visibility").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_visibility_1").await?;
        assert_schema_object(&conn, "index", "idx_visibility_workflow_type").await?;
        assert_schema_object(&conn, "index", "idx_visibility_status").await?;
        assert_schema_object(&conn, "index", "idx_visibility_start_time").await?;
        assert_schema_object(&conn, "index", "idx_visibility_close_time").await?;

        Ok(())
    }

    async fn open_test_connection(name: &str) -> Result<libsql::Connection, StoreError> {
        let config = LibSqlConfig {
            mode: LibSqlMode::Embedded {
                path: unique_temp_path(name),
            },
            journal_mode: None,
            synchronous: None,
            sync_interval_seconds: None,
        };

        open_connection(&config)
            .await
            .map(|opened| opened.connection)
    }

    async fn assert_schema_object(
        conn: &libsql::Connection,
        object_type: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = ?1 AND name = ?2",
                (object_type, name),
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let found = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .is_some();

        if found {
            Ok(())
        } else {
            Err(StoreError::Backend(format!(
                "schema object {object_type} {name} was not created"
            )))
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-schema-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
