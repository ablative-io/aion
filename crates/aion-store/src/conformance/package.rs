//! Deployed-package persistence scenarios of the conformance suite.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::package::{PackageRecord, PackageRouteRecord};
use crate::{EventStore, StoreError};

fn record(
    workflow_type: &str,
    content_hash: &str,
    archive: &[u8],
    deployed_at: DateTime<Utc>,
) -> PackageRecord {
    PackageRecord {
        workflow_type: workflow_type.to_owned(),
        content_hash: content_hash.to_owned(),
        archive: archive.to_vec(),
        deployed_at,
    }
}

fn deployed_at(offset_seconds: i64) -> Result<DateTime<Utc>, StoreError> {
    DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).ok_or_else(|| {
        StoreError::Backend(format!(
            "conformance fixture timestamp offset {offset_seconds} out of range"
        ))
    })
}

fn expect<T: PartialEq + std::fmt::Debug>(
    found: T,
    wanted: &T,
    contract: &str,
) -> Result<(), StoreError> {
    if &found == wanted {
        Ok(())
    } else {
        Err(StoreError::Backend(format!(
            "{contract}: expected {wanted:?}, found {found:?}"
        )))
    }
}

pub(super) async fn put_and_list_packages_round_trip_in_deploy_order(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let later = record("checkout", &"b".repeat(64), b"archive-b", deployed_at(20)?);
    let earlier = record("billing", &"a".repeat(64), b"archive-a", deployed_at(10)?);

    store.put_package(later.clone()).await?;
    store.put_package(earlier.clone()).await?;

    expect(
        store.list_packages().await?,
        &vec![earlier, later],
        "list_packages must return complete records in ascending deployed_at order",
    )
}

pub(super) async fn put_package_replaces_existing_row(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let hash = "c".repeat(64);
    store
        .put_package(record("checkout", &hash, b"first", deployed_at(10)?))
        .await?;
    let replacement = record("checkout", &hash, b"second", deployed_at(30)?);
    store.put_package(replacement.clone()).await?;

    expect(
        store.list_packages().await?,
        &vec![replacement],
        "re-persisting the same (type, hash) must replace the row, not duplicate it",
    )
}

pub(super) async fn put_package_points_route_at_persisted_version(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    store
        .put_package(record("checkout", &first, b"v1", deployed_at(10)?))
        .await?;
    store
        .put_package(record("checkout", &second, b"v2", deployed_at(20)?))
        .await?;

    expect(
        store.list_package_routes().await?,
        &vec![PackageRouteRecord {
            workflow_type: "checkout".to_owned(),
            content_hash: second,
        }],
        "put_package must atomically re-point the type's route at the persisted version",
    )
}

pub(super) async fn put_package_route_repoints_without_touching_archives(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let v1 = record("checkout", &first, b"v1", deployed_at(10)?);
    let v2 = record("checkout", &second, b"v2", deployed_at(20)?);
    store.put_package(v1.clone()).await?;
    store.put_package(v2.clone()).await?;

    store.put_package_route("checkout", &first).await?;

    expect(
        store.list_package_routes().await?,
        &vec![PackageRouteRecord {
            workflow_type: "checkout".to_owned(),
            content_hash: first,
        }],
        "put_package_route must re-point the route (rollback)",
    )?;
    expect(
        store.list_packages().await?,
        &vec![v1, v2],
        "a route re-point must leave persisted archives untouched",
    )
}

pub(super) async fn delete_package_removes_only_target_and_is_idempotent(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let v1 = record("checkout", &first, b"v1", deployed_at(10)?);
    let v2 = record("checkout", &second, b"v2", deployed_at(20)?);
    store.put_package(v1).await?;
    store.put_package(v2.clone()).await?;

    store.delete_package("checkout", &first).await?;
    expect(
        store.list_packages().await?,
        &vec![v2.clone()],
        "delete_package must remove exactly the target version",
    )?;

    // Deleting an absent row (already deleted, or a never-persisted
    // file-sourced version) is a no-op, never an error.
    store.delete_package("checkout", &first).await?;
    store.delete_package("never-persisted", &second).await?;
    expect(
        store.list_packages().await?,
        &vec![v2],
        "idempotent deletes must leave surviving rows untouched",
    )
}

pub(super) async fn routes_list_in_workflow_type_order(
    store: Arc<dyn EventStore>,
) -> Result<(), StoreError> {
    let hash = "d".repeat(64);
    store.put_package_route("zeta", &hash).await?;
    store.put_package_route("alpha", &hash).await?;

    expect(
        store.list_package_routes().await?,
        &vec![
            PackageRouteRecord {
                workflow_type: "alpha".to_owned(),
                content_hash: hash.clone(),
            },
            PackageRouteRecord {
                workflow_type: "zeta".to_owned(),
                content_hash: hash,
            },
        ],
        "list_package_routes must order by workflow type",
    )
}
