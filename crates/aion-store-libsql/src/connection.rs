//! Open embedded and embedded-replica libSQL connections.

use std::path::Path;
use std::time::Duration;

use aion_store::StoreError;

use crate::config::{LibSqlConfig, LibSqlMode};

/// Busy-handler wait applied to every opened connection.
///
/// Aion routinely opens MULTIPLE connections against the SAME local database
/// file: production `run.rs` opens one `LibSqlStore` for the engine and a second
/// one for the outbox dispatcher on the identical `store.url`, and recovery /
/// inspection paths open further read handles. Every durable write runs under
/// `BEGIN IMMEDIATE`, which takes `SQLite`'s `RESERVED` lock up front. With the
/// libSQL default busy timeout of zero, a second connection attempting a write
/// while another holds that lock fails *immediately* with
/// `SQLITE_BUSY` ("database is locked") instead of waiting for the in-flight
/// transaction to commit.
///
/// Setting a busy timeout makes contending connections retry for the configured
/// window — the standard, correct way to share a SQLite/libSQL file across
/// connections. The per-handle `transaction_lock` only serialises writes within
/// a single handle; it cannot coordinate across the separate engine/dispatcher
/// handles, so this connection-level timeout is what makes concurrent same-file
/// access reliable.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// Opened libSQL database handle and its mode-agnostic connection.
pub struct OpenedConnection {
    /// Database handle used to create the connection and to trigger replica sync.
    pub database: libsql::Database,
    /// Connection used by the event-store implementation.
    pub connection: libsql::Connection,
}

/// Open the configured libSQL database and return its handle and connection.
///
/// Operator-provided journal and synchronous settings are applied only when present on the
/// configuration. This crate intentionally does not choose durability defaults for omitted
/// tunables.
///
/// # Errors
///
/// Returns `StoreError::Backend` when libSQL cannot build/connect the database or when applying an
/// explicitly configured PRAGMA fails.
pub async fn open_connection(config: &LibSqlConfig) -> Result<OpenedConnection, StoreError> {
    let opened = match &config.mode {
        LibSqlMode::Embedded { path } => open_embedded(path).await?,
        LibSqlMode::EmbeddedReplica {
            path,
            primary_url,
            auth_token,
        } => {
            open_embedded_replica(
                path,
                primary_url.clone(),
                auth_token.clone(),
                config.sync_interval_seconds,
            )
            .await?
        }
    };

    opened
        .connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| crate::error::libsql_error(&error))?;

    // Default the local file to WAL journaling unless the operator chose a
    // journal mode explicitly. Aion opens MULTIPLE connections on the same file
    // (engine store + outbox-dispatcher store on one `store.url`, plus recovery
    // and inspection handles). In the SQLite default rollback-journal mode a
    // writer holds an EXCLUSIVE lock for the whole commit and the busy handler
    // is not always retried for a contending writer, so concurrent same-file
    // writes can still fail with `SQLITE_BUSY` even with a busy timeout. WAL
    // lets writers and readers proceed against a write-ahead log and makes the
    // busy-timeout retry reliable for the writer-vs-writer case. Operator config
    // still wins: `apply_pragmas` below re-applies an explicit `journal_mode`.
    let default_journal_mode =
        matches!(config.mode, LibSqlMode::Embedded { .. }) && config.journal_mode.is_none();
    if default_journal_mode {
        execute_pragma(&opened.connection, "journal_mode", "wal").await?;
    }

    apply_pragmas(
        &opened.connection,
        config.journal_mode.as_deref(),
        config.synchronous.as_deref(),
    )
    .await?;

    Ok(opened)
}

async fn open_embedded(path: &Path) -> Result<OpenedConnection, StoreError> {
    let database = libsql::Builder::new_local(path)
        .build()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let connection = database
        .connect()
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(OpenedConnection {
        database,
        connection,
    })
}

async fn open_embedded_replica(
    path: &Path,
    primary_url: String,
    auth_token: String,
    sync_interval_seconds: Option<u64>,
) -> Result<OpenedConnection, StoreError> {
    let mut builder = libsql::Builder::new_remote_replica(path, primary_url, auth_token);
    if let Some(seconds) = sync_interval_seconds {
        builder = builder.sync_interval(Duration::from_secs(seconds));
    }

    let db = builder
        .build()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let connection = db
        .connect()
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(OpenedConnection {
        database: db,
        connection,
    })
}

async fn apply_pragmas(
    conn: &libsql::Connection,
    journal_mode: Option<&str>,
    synchronous: Option<&str>,
) -> Result<(), StoreError> {
    if let Some(value) = journal_mode {
        execute_pragma(conn, "journal_mode", value).await?;
    }

    if let Some(value) = synchronous {
        execute_pragma(conn, "synchronous", value).await?;
    }

    Ok(())
}

async fn execute_pragma(
    conn: &libsql::Connection,
    name: &str,
    value: &str,
) -> Result<(), StoreError> {
    let value = validate_pragma_value(value)?;
    let sql = format!("PRAGMA {name} = {value}");
    conn.query(&sql, ())
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(())
}

fn validate_pragma_value(value: &str) -> Result<&str, StoreError> {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        Ok(value)
    } else {
        Err(StoreError::Backend(format!(
            "invalid libSQL PRAGMA value {value:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::StoreError;

    use super::open_connection;
    use crate::config::{LibSqlConfig, LibSqlMode};

    #[tokio::test]
    async fn opens_embedded_connection_and_queries() -> Result<(), StoreError> {
        let config = LibSqlConfig {
            mode: LibSqlMode::Embedded {
                path: unique_temp_path("embedded-select"),
            },
            journal_mode: Some(String::from("wal")),
            synchronous: Some(String::from("normal")),
            sync_interval_seconds: None,
        };

        let opened = open_connection(&config).await?;
        let conn = opened.connection;
        let mut rows = conn
            .query("SELECT 1", ())
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .ok_or_else(|| StoreError::Backend(String::from("SELECT 1 returned no rows")))?;
        let value: i64 = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;

        assert_eq!(value, 1);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_on_the_same_file_do_not_lock_each_other_out()
    -> Result<(), StoreError> {
        // Two connections on the SAME file, exactly how production opens the
        // engine store and the outbox-dispatcher store on one `store.url`. Both
        // hammer short IMMEDIATE write transactions concurrently. Every durable
        // write takes the RESERVED/write lock up front; with the libSQL default
        // busy timeout of zero (and the default rollback journal) one of these
        // contending writers fails immediately with SQLITE_BUSY ("database is
        // locked"). The WAL + busy-timeout configuration applied in
        // `open_connection` makes the contending writer wait out the lock and
        // commit, so neither side errors.
        const ROUNDS: i64 = 40;
        let path = unique_temp_path("concurrent-writers-shared-file");
        let config = |path: &std::path::Path| LibSqlConfig {
            mode: LibSqlMode::Embedded {
                path: path.to_path_buf(),
            },
            journal_mode: None,
            synchronous: None,
            sync_interval_seconds: None,
        };

        let setup = open_connection(&config(&path)).await?;
        setup
            .connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, who INTEGER)", ())
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        drop(setup);

        let writer = |who: i64, path: std::path::PathBuf| async move {
            let opened = open_connection(&config(&path)).await?;
            for _ in 0..ROUNDS {
                let tx = opened
                    .connection
                    .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                    .await
                    .map_err(|error| crate::error::libsql_error(&error))?;
                tx.execute("INSERT INTO t (who) VALUES (?1)", [who])
                    .await
                    .map_err(|error| crate::error::libsql_error(&error))?;
                tx.commit()
                    .await
                    .map_err(|error| crate::error::libsql_error(&error))?;
            }
            Ok::<(), StoreError>(())
        };

        let a = tokio::spawn(writer(1, path.clone()));
        let b = tokio::spawn(writer(2, path.clone()));
        a.await
            .map_err(|error| StoreError::Backend(error.to_string()))??;
        b.await
            .map_err(|error| StoreError::Backend(error.to_string()))??;

        let reader = open_connection(&config(&path)).await?;
        let mut rows = reader
            .connection
            .query("SELECT COUNT(*) FROM t", ())
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .ok_or_else(|| StoreError::Backend(String::from("count returned no row")))?;
        let count: i64 = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        assert_eq!(count, ROUNDS * 2, "every concurrent write committed");
        Ok(())
    }

    #[tokio::test]
    async fn maps_replica_open_failure_to_backend() -> Result<(), Box<dyn std::error::Error>> {
        let config = LibSqlConfig {
            mode: LibSqlMode::EmbeddedReplica {
                path: unique_temp_path("replica-unavailable-primary"),
                primary_url: String::from("http://127.0.0.1:9"),
                auth_token: String::from("token"),
            },
            journal_mode: None,
            synchronous: None,
            sync_interval_seconds: Some(1),
        };

        match open_connection(&config).await {
            Ok(_) => Err("expected embedded-replica open to fail for an invalid URL".into()),
            Err(StoreError::Backend(_)) => Ok(()),
            Err(other) => Err(format!("expected backend error, got {other:?}").into()),
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
