//! Open embedded and embedded-replica libSQL connections.

use std::path::Path;
use std::time::Duration;

use aion_store::StoreError;

use crate::config::{LibSqlConfig, LibSqlMode};

/// Open the configured libSQL database and return a mode-agnostic connection.
///
/// Operator-provided journal and synchronous settings are applied only when present on the
/// configuration. This crate intentionally does not choose durability defaults for omitted
/// tunables.
///
/// # Errors
///
/// Returns `StoreError::Backend` when libSQL cannot build/connect the database or when applying an
/// explicitly configured PRAGMA fails.
pub async fn open_connection(config: &LibSqlConfig) -> Result<libsql::Connection, StoreError> {
    let conn = match &config.mode {
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

    apply_pragmas(
        &conn,
        config.journal_mode.as_deref(),
        config.synchronous.as_deref(),
    )
    .await?;

    Ok(conn)
}

async fn open_embedded(path: &Path) -> Result<libsql::Connection, StoreError> {
    let db = libsql::Builder::new_local(path)
        .build()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    db.connect()
        .map_err(|error| crate::error::libsql_error(&error))
}

async fn open_embedded_replica(
    path: &Path,
    primary_url: String,
    auth_token: String,
    sync_interval_seconds: Option<u64>,
) -> Result<libsql::Connection, StoreError> {
    let mut builder = libsql::Builder::new_remote_replica(path, primary_url, auth_token);
    if let Some(seconds) = sync_interval_seconds {
        builder = builder.sync_interval(Duration::from_secs(seconds));
    }

    let db = builder
        .build()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    db.connect()
        .map_err(|error| crate::error::libsql_error(&error))
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

        let conn = open_connection(&config).await?;
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
