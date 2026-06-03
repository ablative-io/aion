//! Idempotent schema DDL for the libSQL event store.

use aion_store::StoreError;

/// Append-only workflow event table.
pub const CREATE_EVENTS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS events (
    workflow_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    event BLOB NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (workflow_id, seq)
)";

/// Conservative event index for history and projection scans.
///
/// AS-005 may amend this index once the final query/list-active SQL shape is implemented; this
/// brief intentionally avoids adding mutable status columns or a workflow summary table.
pub const CREATE_EVENTS_PROJECTION_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_events_workflow_recorded_at
ON events (workflow_id, recorded_at)";

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

const DDL_STATEMENTS: [&str; 4] = [
    CREATE_EVENTS_TABLE,
    CREATE_EVENTS_PROJECTION_INDEX,
    CREATE_TIMERS_TABLE,
    CREATE_TIMERS_FIRE_AT_INDEX,
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
        assert_schema_object(&conn, "index", "idx_events_workflow_recorded_at").await?;
        assert_schema_object(&conn, "table", "timers").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_timers_1").await?;
        assert_schema_object(&conn, "index", "idx_timers_fire_at").await?;

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

        open_connection(&config).await
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
