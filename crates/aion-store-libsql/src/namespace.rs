//! Durable minted-on-use namespace registry over libSQL (Control-Plane Phase 1).
//!
//! The single-node libSQL backend satisfies the [`NamespaceStore`](aion_store::NamespaceStore)
//! contract with a plain local upsert — there is no quorum to reach. Records are
//! stored as opaque [`NamespaceRecord`](aion_store::NamespaceRecord) codec bytes
//! in a `record` BLOB (the store's truth, decoded only to satisfy `list`/`get`);
//! `created_at`/`last_seen` are projected into RFC 3339 text columns so the list
//! ordering is index-served and the staleness signal is human-legible.

use aion_store::{MintOutcome, NamespaceOrigin, NamespaceRecord, NamespaceState, StoreError};
use chrono::{SecondsFormat, Utc};
use libsql::{Connection, Row, TransactionBehavior};

const SELECT_RECORD_SQL: &str = "SELECT record FROM namespaces WHERE name = ?1";

const INSERT_IF_ABSENT_SQL: &str = "
INSERT OR IGNORE INTO namespaces (name, created_at, last_seen, record)
VALUES (?1, ?2, ?3, ?4)";

const TOUCH_LAST_SEEN_SQL: &str = "
UPDATE namespaces SET last_seen = ?2, record = ?3 WHERE name = ?1";

const LIST_RECORDS_SQL: &str = "
SELECT record FROM namespaces ORDER BY created_at ASC, name ASC";

const DEPRECATE_SQL: &str = "UPDATE namespaces SET record = ?2 WHERE name = ?1";

/// Idempotent minted-on-use upsert: create-if-absent else bump `last_seen`.
///
/// A fresh `name` is minted with `origin`; an existing one has its `last_seen`
/// refreshed (origin and `created_at` left untouched). Runs under one
/// `IMMEDIATE` transaction so the read-then-write is atomic against a concurrent
/// minter on the same single-node connection.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when the record cannot be encoded and
/// `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn register_namespace(
    conn: &Connection,
    name: &str,
    origin: NamespaceOrigin,
) -> Result<MintOutcome, StoreError> {
    let now = Utc::now();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let outcome = match load_record(&tx, name).await {
        Ok(Some(mut existing)) => {
            existing.bump_last_seen(now);
            let bytes = existing.encode()?;
            match touch_last_seen(&tx, name, now, &bytes).await {
                Ok(()) => Ok(MintOutcome::AlreadyExisted),
                Err(error) => Err(error),
            }
        }
        Ok(None) => {
            let record = NamespaceRecord::new_minted(name, origin, now);
            match insert_record(&tx, &record).await {
                Ok(()) => Ok(MintOutcome::Created),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };

    finish(tx, outcome).await
}

/// Explicit upsert carrying a caller-supplied record. Idempotent on an existing
/// name: a present row is reconciled as `AlreadyExisted` rather than overwritten.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when the record cannot be encoded and
/// `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn put_namespace(
    conn: &Connection,
    record: NamespaceRecord,
) -> Result<MintOutcome, StoreError> {
    let now = Utc::now();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let outcome = match load_record(&tx, &record.name).await {
        Ok(Some(mut existing)) => {
            // Idempotent on an existing name: refresh the staleness signal but
            // never overwrite the durable record body wholesale.
            existing.bump_last_seen(now);
            let bytes = match existing.encode() {
                Ok(bytes) => bytes,
                Err(error) => return finish(tx, Err(error)).await,
            };
            match touch_last_seen(&tx, &existing.name, now, &bytes).await {
                Ok(()) => Ok(MintOutcome::AlreadyExisted),
                Err(error) => Err(error),
            }
        }
        Ok(None) => match insert_record(&tx, &record).await {
            Ok(()) => Ok(MintOutcome::Created),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };

    finish(tx, outcome).await
}

/// Return the live durable set, ascending `created_at` (ties broken by `name`).
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL read failures and
/// `StoreError::Serialization` when a persisted record cannot be decoded.
pub(crate) async fn list_namespaces(conn: &Connection) -> Result<Vec<NamespaceRecord>, StoreError> {
    let mut rows = conn
        .query(LIST_RECORDS_SQL, ())
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut records = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        records.push(decode_record(&row)?);
    }

    Ok(records)
}

/// Look up a single namespace by `name`; an absent name returns `None`.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL read failures and
/// `StoreError::Serialization` when the persisted record cannot be decoded.
pub(crate) async fn get_namespace(
    conn: &Connection,
    name: &str,
) -> Result<Option<NamespaceRecord>, StoreError> {
    let mut rows = conn
        .query(SELECT_RECORD_SQL, libsql::params![name])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_record(&row)?))
}

/// Transition `name` to [`NamespaceState::Deprecated`]; absent or already-
/// deprecated rows are an idempotent no-op.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when the record cannot be re-encoded and
/// `StoreError::Backend` for libSQL boundary failures.
pub(crate) async fn deprecate_namespace(conn: &Connection, name: &str) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let outcome = match load_record(&tx, name).await {
        Ok(Some(mut existing)) => {
            existing.state = NamespaceState::Deprecated;
            match existing.encode() {
                Ok(bytes) => tx
                    .execute(DEPRECATE_SQL, libsql::params![name, bytes])
                    .await
                    .map(|_| ())
                    .map_err(|error| crate::error::libsql_error(&error)),
                Err(error) => Err(error),
            }
        }
        // A missing row is an idempotent no-op: deprecation never strands
        // durable history and an absent registry entry is not an error.
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };

    finish(tx, outcome).await
}

async fn load_record(
    tx: &libsql::Transaction,
    name: &str,
) -> Result<Option<NamespaceRecord>, StoreError> {
    let mut rows = tx
        .query(SELECT_RECORD_SQL, libsql::params![name])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_record(&row)?))
}

async fn insert_record(
    tx: &libsql::Transaction,
    record: &NamespaceRecord,
) -> Result<(), StoreError> {
    let bytes = record.encode()?;
    tx.execute(
        INSERT_IF_ABSENT_SQL,
        libsql::params![
            record.name.clone(),
            encode_instant(record.created_at),
            encode_instant(record.last_seen),
            bytes
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::error::libsql_error(&error))
}

async fn touch_last_seen(
    tx: &libsql::Transaction,
    name: &str,
    last_seen: chrono::DateTime<Utc>,
    record: &[u8],
) -> Result<(), StoreError> {
    tx.execute(
        TOUCH_LAST_SEEN_SQL,
        libsql::params![name, encode_instant(last_seen), record.to_vec()],
    )
    .await
    .map(|_| ())
    .map_err(|error| crate::error::libsql_error(&error))
}

async fn finish<T>(
    tx: libsql::Transaction,
    outcome: Result<T, StoreError>,
) -> Result<T, StoreError> {
    match outcome {
        Ok(value) => tx
            .commit()
            .await
            .map(|()| value)
            .map_err(|error| crate::error::libsql_error(&error)),
        Err(error) => {
            tx.rollback()
                .await
                .map_err(|rollback_error| crate::error::libsql_error(&rollback_error))?;
            Err(error)
        }
    }
}

fn decode_record(row: &Row) -> Result<NamespaceRecord, StoreError> {
    let bytes: Vec<u8> = row
        .get(0)
        .map_err(|error| crate::error::libsql_error(&error))?;
    NamespaceRecord::decode(&bytes)
}

fn encode_instant(instant: chrono::DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::{MintOutcome, NamespaceOrigin, NamespaceRecord, NamespaceState, StoreError};
    use chrono::{TimeZone, Utc};

    use crate::config::{LibSqlConfig, LibSqlMode};

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
            "aion-store-libsql-namespace-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn register_creates_if_absent_and_persists() -> Result<(), StoreError> {
        let conn = open_test_connection("create").await?;

        let outcome =
            super::register_namespace(&conn, "orders", NamespaceOrigin::WorkerMint).await?;
        assert_eq!(outcome, MintOutcome::Created);

        let record = super::get_namespace(&conn, "orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(record.name, "orders");
        assert_eq!(record.origin, NamespaceOrigin::WorkerMint);
        assert_eq!(record.state, NamespaceState::Active);
        assert_eq!(record.created_at, record.last_seen);
        Ok(())
    }

    #[tokio::test]
    async fn second_register_already_existed_bumps_last_seen_only() -> Result<(), StoreError> {
        let conn = open_test_connection("touch").await?;

        let first = super::register_namespace(&conn, "orders", NamespaceOrigin::WorkerMint).await?;
        assert_eq!(first, MintOutcome::Created);
        let original = super::get_namespace(&conn, "orders")
            .await?
            .expect("namespace must persist");

        // A different origin on re-register must NOT overwrite the recorded origin.
        let second = super::register_namespace(&conn, "orders", NamespaceOrigin::Explicit).await?;
        assert_eq!(second, MintOutcome::AlreadyExisted);

        let touched = super::get_namespace(&conn, "orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(touched.created_at, original.created_at);
        assert_eq!(touched.origin, NamespaceOrigin::WorkerMint);
        assert!(touched.last_seen >= original.last_seen);
        Ok(())
    }

    #[tokio::test]
    async fn put_namespace_is_idempotent_on_existing_name() -> Result<(), StoreError> {
        let conn = open_test_connection("put-idempotent").await?;
        let now = Utc
            .with_ymd_and_hms(2026, 6, 30, 12, 0, 0)
            .single()
            .expect("valid instant");

        let mut record = NamespaceRecord::new_minted("billing", NamespaceOrigin::Explicit, now);
        record.config.kind = Some("tenant".to_owned());
        assert_eq!(
            super::put_namespace(&conn, record).await?,
            MintOutcome::Created
        );

        // A second put with a DIFFERENT body must reconcile as AlreadyExisted
        // and must not overwrite the stored record wholesale.
        let mut replacement =
            NamespaceRecord::new_minted("billing", NamespaceOrigin::WorkerMint, now);
        replacement.config.kind = None;
        assert_eq!(
            super::put_namespace(&conn, replacement).await?,
            MintOutcome::AlreadyExisted
        );

        let stored = super::get_namespace(&conn, "billing")
            .await?
            .expect("namespace must persist");
        assert_eq!(stored.origin, NamespaceOrigin::Explicit);
        assert_eq!(stored.config.kind.as_deref(), Some("tenant"));
        Ok(())
    }

    #[tokio::test]
    async fn list_orders_by_created_at_then_name() -> Result<(), StoreError> {
        let conn = open_test_connection("list-order").await?;
        let earlier = Utc
            .with_ymd_and_hms(2026, 6, 30, 12, 0, 0)
            .single()
            .expect("valid instant");
        let later = Utc
            .with_ymd_and_hms(2026, 6, 30, 13, 0, 0)
            .single()
            .expect("valid instant");

        super::put_namespace(
            &conn,
            NamespaceRecord::new_minted("zeta", NamespaceOrigin::Explicit, earlier),
        )
        .await?;
        super::put_namespace(
            &conn,
            NamespaceRecord::new_minted("alpha", NamespaceOrigin::Explicit, earlier),
        )
        .await?;
        super::put_namespace(
            &conn,
            NamespaceRecord::new_minted("beta", NamespaceOrigin::Explicit, later),
        )
        .await?;

        let listed: Vec<String> = super::list_namespaces(&conn)
            .await?
            .into_iter()
            .map(|record| record.name)
            .collect();
        assert_eq!(listed, vec!["alpha", "zeta", "beta"]);
        Ok(())
    }

    #[tokio::test]
    async fn get_returns_none_for_absent_name() -> Result<(), StoreError> {
        let conn = open_test_connection("get-miss").await?;
        assert!(super::get_namespace(&conn, "missing").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn deprecate_sets_state_and_is_idempotent() -> Result<(), StoreError> {
        let conn = open_test_connection("deprecate").await?;
        super::register_namespace(&conn, "orders", NamespaceOrigin::WorkerMint).await?;

        super::deprecate_namespace(&conn, "orders").await?;
        let deprecated = super::get_namespace(&conn, "orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(deprecated.state, NamespaceState::Deprecated);

        // Deprecating again is a no-op, not an error.
        super::deprecate_namespace(&conn, "orders").await?;
        let still = super::get_namespace(&conn, "orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(still.state, NamespaceState::Deprecated);

        // Deprecating an absent namespace is an idempotent no-op.
        super::deprecate_namespace(&conn, "never-seen").await?;
        assert!(super::get_namespace(&conn, "never-seen").await?.is_none());
        Ok(())
    }
}
