//! Deployed-package persistence over libSQL.

use aion_store::{PackageRecord, PackageRouteRecord, StoreError};
use chrono::{DateTime, SecondsFormat, Utc};
use libsql::TransactionBehavior;

const UPSERT_PACKAGE_SQL: &str = "
INSERT INTO packages (workflow_type, content_hash, archive, deployed_at)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(workflow_type, content_hash)
DO UPDATE SET archive = excluded.archive, deployed_at = excluded.deployed_at";

const UPSERT_ROUTE_SQL: &str = "
INSERT INTO package_routes (workflow_type, content_hash)
VALUES (?1, ?2)
ON CONFLICT(workflow_type) DO UPDATE SET content_hash = excluded.content_hash";

const LIST_PACKAGES_SQL: &str = "
SELECT workflow_type, content_hash, archive, deployed_at
FROM packages
ORDER BY deployed_at ASC, workflow_type ASC, content_hash ASC";

const DELETE_PACKAGE_SQL: &str = "
DELETE FROM packages WHERE workflow_type = ?1 AND content_hash = ?2";

const LIST_ROUTES_SQL: &str = "
SELECT workflow_type, content_hash FROM package_routes ORDER BY workflow_type ASC";

/// Persist a deployed package and atomically re-point its type's route.
///
/// The archive row and the route pointer commit in one transaction: a crash
/// between them would otherwise resurrect a stale route on restart.
///
/// # Errors
///
/// Returns `StoreError::Backend` when libSQL rejects the transaction or
/// either upsert.
pub(crate) async fn put_package(
    conn: &libsql::Connection,
    record: PackageRecord,
) -> Result<(), StoreError> {
    let primary = record.workflow_type.clone();
    put_package_with_routes(conn, record, &[primary]).await
}

/// Persist a deployed package and every group-member route in one transaction.
pub(crate) async fn put_package_with_routes(
    conn: &libsql::Connection,
    record: PackageRecord,
    route_workflow_types: &[String],
) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let result = async {
        tx.execute(
            UPSERT_PACKAGE_SQL,
            libsql::params![
                record.workflow_type.clone(),
                record.content_hash.clone(),
                record.archive,
                encode_deployed_at(record.deployed_at),
            ],
        )
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
        for workflow_type in route_workflow_types {
            tx.execute(
                UPSERT_ROUTE_SQL,
                libsql::params![workflow_type.clone(), record.content_hash.clone()],
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| crate::error::libsql_error(&error)),
        Err(error) => {
            rollback(tx).await?;
            Err(error)
        }
    }
}

/// Return every persisted package in ascending `deployed_at` order.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL read failures and
/// `StoreError::Serialization` when a persisted row cannot be reconstructed.
pub(crate) async fn list_packages(
    conn: &libsql::Connection,
) -> Result<Vec<PackageRecord>, StoreError> {
    let mut rows = conn
        .query(LIST_PACKAGES_SQL, ())
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut records = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let workflow_type: String = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let content_hash: String = row
            .get(1)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let archive: Vec<u8> = row
            .get(2)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let deployed_at: String = row
            .get(3)
            .map_err(|error| crate::error::libsql_error(&error))?;

        records.push(PackageRecord {
            workflow_type,
            content_hash,
            archive,
            deployed_at: decode_deployed_at(&deployed_at)?,
        });
    }

    Ok(records)
}

/// Delete one persisted package row; absent rows are a successful no-op.
///
/// # Errors
///
/// Returns `StoreError::Backend` when libSQL rejects the delete.
pub(crate) async fn delete_package(
    conn: &libsql::Connection,
    workflow_type: &str,
    content_hash: &str,
) -> Result<(), StoreError> {
    conn.execute(
        DELETE_PACKAGE_SQL,
        libsql::params![workflow_type, content_hash],
    )
    .await
    .map_err(|error| crate::error::libsql_error(&error))?;
    Ok(())
}

/// Upsert the route pointer for a workflow type.
///
/// # Errors
///
/// Returns `StoreError::Backend` when libSQL rejects the upsert.
pub(crate) async fn put_package_route(
    conn: &libsql::Connection,
    workflow_type: &str,
    content_hash: &str,
) -> Result<(), StoreError> {
    conn.execute(
        UPSERT_ROUTE_SQL,
        libsql::params![workflow_type, content_hash],
    )
    .await
    .map_err(|error| crate::error::libsql_error(&error))?;
    Ok(())
}

/// Return every persisted route pointer in workflow-type order.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL read failures.
pub(crate) async fn list_package_routes(
    conn: &libsql::Connection,
) -> Result<Vec<PackageRouteRecord>, StoreError> {
    let mut rows = conn
        .query(LIST_ROUTES_SQL, ())
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut routes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let workflow_type: String = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let content_hash: String = row
            .get(1)
            .map_err(|error| crate::error::libsql_error(&error))?;
        routes.push(PackageRouteRecord {
            workflow_type,
            content_hash,
        });
    }

    Ok(routes)
}

async fn rollback(tx: libsql::Transaction) -> Result<(), StoreError> {
    tx.rollback()
        .await
        .map_err(|error| crate::error::libsql_error(&error))
}

fn encode_deployed_at(deployed_at: DateTime<Utc>) -> String {
    deployed_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_deployed_at(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::{PackageRecord, StoreError};
    use chrono::{TimeZone, Utc};

    use crate::config::{LibSqlConfig, LibSqlMode};

    #[tokio::test]
    async fn deployed_at_round_trips_through_persisted_row() -> Result<(), StoreError> {
        let conn = open_test_connection("round-trip").await?;
        let deployed_at = Utc
            .with_ymd_and_hms(2026, 6, 12, 9, 30, 0)
            .single()
            .ok_or_else(|| StoreError::Serialization(String::from("invalid test instant")))?;
        let record = PackageRecord {
            workflow_type: "checkout".to_owned(),
            content_hash: "a".repeat(64),
            archive: b"archive-bytes".to_vec(),
            deployed_at,
        };

        super::put_package(&conn, record.clone()).await?;
        let listed = super::list_packages(&conn).await?;

        assert_eq!(listed, vec![record]);
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
        let conn = crate::connection::open_connection(&config)
            .await?
            .connection;
        crate::schema::ensure_schema(&conn).await?;
        Ok(conn)
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-package-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
