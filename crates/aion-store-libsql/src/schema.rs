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

/// Runtime-deployed package archives keyed by `(workflow_type, content_hash)`.
pub const CREATE_PACKAGES_TABLE: &str = "
CREATE TABLE IF NOT EXISTS packages (
    workflow_type TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    archive BLOB NOT NULL,
    deployed_at TEXT NOT NULL,
    PRIMARY KEY (workflow_type, content_hash)
)";

/// Per-workflow-type route pointer for new workflow starts.
pub const CREATE_PACKAGE_ROUTES_TABLE: &str = "
CREATE TABLE IF NOT EXISTS package_routes (
    workflow_type TEXT PRIMARY KEY,
    content_hash TEXT NOT NULL
)";

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

/// Durable fan-out dispatch outbox.
///
/// `dispatch_key` (`"{workflow_id}:{ordinal}"`) is `UNIQUE`: it is the database-level idempotency
/// guard, so a re-issued append of the same fan-out batch silently ignores the duplicate rows via
/// `INSERT OR IGNORE`. `status` is one of `pending`/`claimed`/`done`/`failed`; `visible_after`
/// fences retry backoff so a row is not re-claimed before its delay elapses; nullable
/// `claimed_at` records the durable claim instant for live stale-claim reconciliation; nullable
/// `run_id` records the concrete run that staged the row when known.
pub const CREATE_OUTBOX_TABLE: &str = "
CREATE TABLE IF NOT EXISTS outbox (
    dispatch_key TEXT NOT NULL UNIQUE,
    workflow_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    activity_type TEXT NOT NULL,
    input BLOB NOT NULL,
    status TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    visible_after TEXT NOT NULL,
    run_id TEXT,
    claimed_at TEXT,
    PRIMARY KEY (dispatch_key)
)";

/// Partial index over claimable rows, supporting the dispatcher's `status='pending'` claim scan
/// ordered by `visible_after`. Restricting the index to pending rows keeps it small as completed
/// rows accumulate.
pub const CREATE_OUTBOX_PENDING_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_outbox_pending
ON outbox (status, visible_after)
WHERE status = 'pending'";

/// Partial index over stale-claim reconciliation candidates.
pub const CREATE_OUTBOX_CLAIMED_INDEX: &str = "
CREATE INDEX IF NOT EXISTS idx_outbox_claimed_at
ON outbox (status, claimed_at)
WHERE status = 'claimed' AND claimed_at IS NOT NULL";

const DDL_STATEMENTS: [&str; 13] = [
    CREATE_EVENTS_TABLE,
    CREATE_EVENTS_PROJECTION_INDEX,
    CREATE_TIMERS_TABLE,
    CREATE_TIMERS_FIRE_AT_INDEX,
    CREATE_PACKAGES_TABLE,
    CREATE_PACKAGE_ROUTES_TABLE,
    CREATE_VISIBILITY_TABLE,
    CREATE_VISIBILITY_WORKFLOW_TYPE_INDEX,
    CREATE_VISIBILITY_STATUS_INDEX,
    CREATE_VISIBILITY_START_TIME_INDEX,
    CREATE_VISIBILITY_CLOSE_TIME_INDEX,
    CREATE_OUTBOX_TABLE,
    CREATE_OUTBOX_PENDING_INDEX,
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

    ensure_outbox_claimed_at_column(conn).await?;
    ensure_outbox_run_id_column(conn).await?;
    conn.execute(CREATE_OUTBOX_CLAIMED_INDEX, ())
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(())
}

async fn ensure_outbox_claimed_at_column(conn: &libsql::Connection) -> Result<(), StoreError> {
    if outbox_column_exists(conn, "claimed_at").await? {
        return Ok(());
    }

    conn.execute("ALTER TABLE outbox ADD COLUMN claimed_at TEXT", ())
        .await
        .map(|_| ())
        .map_err(|error| crate::error::libsql_error(&error))
}

async fn ensure_outbox_run_id_column(conn: &libsql::Connection) -> Result<(), StoreError> {
    if outbox_column_exists(conn, "run_id").await? {
        return Ok(());
    }

    conn.execute("ALTER TABLE outbox ADD COLUMN run_id TEXT", ())
        .await
        .map(|_| ())
        .map_err(|error| crate::error::libsql_error(&error))
}

async fn outbox_column_exists(
    conn: &libsql::Connection,
    column_name: &str,
) -> Result<bool, StoreError> {
    let mut rows = conn
        .query("PRAGMA table_info(outbox)", ())
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let name: String = row
            .get(1)
            .map_err(|error| crate::error::libsql_error(&error))?;
        if name == column_name {
            return Ok(true);
        }
    }

    Ok(false)
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
        assert_schema_object(&conn, "table", "packages").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_packages_1").await?;
        assert_schema_object(&conn, "table", "package_routes").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_package_routes_1").await?;
        assert_schema_object(&conn, "table", "visibility").await?;
        assert_schema_object(&conn, "index", "sqlite_autoindex_visibility_1").await?;
        assert_schema_object(&conn, "index", "idx_visibility_workflow_type").await?;
        assert_schema_object(&conn, "index", "idx_visibility_status").await?;
        assert_schema_object(&conn, "index", "idx_visibility_start_time").await?;
        assert_schema_object(&conn, "index", "idx_visibility_close_time").await?;
        assert_schema_object(&conn, "table", "outbox").await?;
        assert_schema_object(&conn, "index", "idx_outbox_pending").await?;
        assert_schema_object(&conn, "index", "idx_outbox_claimed_at").await?;

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
