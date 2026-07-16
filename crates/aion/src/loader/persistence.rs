//! Durable persistence for runtime-deployed packages.
//!
//! A run is pinned to the package version recorded in its `WorkflowStarted`
//! event, so the deployed archive itself is part of the durability promise:
//! startup recovery can only resolve that pin if every runtime-deployed
//! package survives the restart through the store. This module owns both
//! directions of that contract — persisting a successful deploy (archive
//! bytes plus the route pointer) and reloading the persisted set into the
//! catalog at startup, BEFORE recovery resolves pinned versions.
//!
//! Operator-file startup sources (`--workflow-package`) are deliberately not
//! persisted: they are supplied every boot and remain the trusted file-based
//! path.

use aion_package::{ExtractionLimits, Package};
use aion_store::{EventStore, PackageRecord};
use chrono::Utc;

use crate::error::EngineError;
use crate::runtime::RuntimeHandle;

use super::WorkflowCatalog;

/// Persists a verified staged deploy: the canonical archive bytes and,
/// atomically with them, the type's route pointer.
///
/// Called before in-memory routes are published, including for idempotent
/// re-loads (`freshly_loaded = false`): re-deploying a version is a routing
/// intent, and durability must lead the catalog pointer. A persistence failure
/// surfaces to the deploy caller; a freshly staged package is rolled back by
/// the caller, while re-sending the archive safely retries this idempotent
/// write.
///
/// # Errors
///
/// Returns [`EngineError::Load`] when the package cannot be re-serialised and
/// [`EngineError::Store`] when the store rejects the write.
pub(crate) async fn persist_deployed_package(
    store: &dyn EventStore,
    package: &Package,
) -> Result<(), EngineError> {
    let workflow_type = package.manifest().entry_module.clone();
    let content_hash = package.content_hash().to_string();
    let archive = package
        .to_archive_bytes()
        .map_err(|error| EngineError::Load {
            reason: format!(
                "failed to serialise deployed package `{workflow_type}` version `{content_hash}` for persistence: {error}"
            ),
        })?;
    store
        .put_package(PackageRecord {
            workflow_type,
            content_hash,
            archive,
            deployed_at: Utc::now(),
        })
        .await?;
    Ok(())
}

/// Reloads every persisted deployed package into `catalog`, then restores
/// the persisted route pointers.
///
/// Runs at engine build, before startup recovery resolves any run's pinned
/// version. Reload extracts with [`ExtractionLimits::unbounded`]: these
/// archives came from the engine's own store, where the deploy path already
/// enforced the operator's `deploy.max_inflated_bytes` ceiling at admission —
/// at reload time they are engine-trusted state, exactly like operator-file
/// startup packages.
///
/// Per-package isolation mirrors startup recovery's per-workflow isolation:
/// a row that fails validation or load (store corruption / manual tampering)
/// is skipped with a loud error so one bad row cannot prevent every other
/// deploy — and every other workflow — from coming back. Runs pinned to a
/// skipped version then fail their own recovery with the existing loud
/// not-loaded skip.
///
/// Route pointers naming a version that is not loaded after reload (an
/// operator-file version not supplied this boot, or a skipped corrupt row)
/// are warned and skipped; the affected type keeps the route committed by
/// its own reloads, and explicit startup sources loaded after this re-point
/// their types anyway.
///
/// # Errors
///
/// Returns [`EngineError::Store`] when the persisted package or route sets
/// cannot be read at all, and [`EngineError::CatalogPoisoned`] when the
/// catalog locks are poisoned.
pub(crate) async fn reload_persisted_packages(
    runtime: &RuntimeHandle,
    catalog: &WorkflowCatalog,
    store: &dyn EventStore,
) -> Result<(), EngineError> {
    for record in store.list_packages().await? {
        let package = match Package::load_from_bytes(&record.archive, ExtractionLimits::unbounded())
        {
            Ok(package) => package,
            Err(error) => {
                tracing::error!(
                    workflow_type = %record.workflow_type,
                    content_hash = %record.content_hash,
                    error = %error,
                    "persisted deployed package failed validation on reload (store corruption or manual tampering); skipping it — runs pinned to it will fail recovery loudly"
                );
                continue;
            }
        };
        let computed_hash = package.content_hash().to_string();
        if computed_hash != record.content_hash
            || package.manifest().entry_module != record.workflow_type
        {
            tracing::error!(
                workflow_type = %record.workflow_type,
                content_hash = %record.content_hash,
                computed_hash = %computed_hash,
                computed_type = %package.manifest().entry_module,
                "persisted deployed package does not match its store key; skipping it — runs pinned to it will fail recovery loudly"
            );
            continue;
        }
        match catalog.load_package(runtime, &package).await {
            Ok(outcome) => {
                tracing::info!(
                    workflow_type = outcome.record.workflow_type(),
                    content_hash = %outcome.record.version(),
                    "reloaded persisted deployed package"
                );
            }
            Err(error) => {
                tracing::error!(
                    workflow_type = %record.workflow_type,
                    content_hash = %record.content_hash,
                    error = %error,
                    "persisted deployed package failed catalog load on reload; skipping it — runs pinned to it will fail recovery loudly"
                );
            }
        }
    }

    restore_persisted_routes(catalog, store).await
}

/// Re-points each type's route at its persisted pointer, after all persisted
/// packages have reloaded (deploy-order loads leave the route on the last
/// deploy; an explicit rollback pointer must override that).
async fn restore_persisted_routes(
    catalog: &WorkflowCatalog,
    store: &dyn EventStore,
) -> Result<(), EngineError> {
    for route in store.list_package_routes().await? {
        let version = match route.content_hash.parse::<aion_package::ContentHash>() {
            Ok(version) => version,
            Err(error) => {
                tracing::error!(
                    workflow_type = %route.workflow_type,
                    content_hash = %route.content_hash,
                    error = %error,
                    "persisted route pointer is not a canonical content hash; skipping it"
                );
                continue;
            }
        };
        match catalog.route_version(&route.workflow_type, &version).await {
            Ok(()) => {}
            Err(EngineError::UnknownVersion { .. }) => {
                tracing::warn!(
                    workflow_type = %route.workflow_type,
                    content_hash = %route.content_hash,
                    "persisted route pointer names a version that is not loaded (an operator-file version absent this boot, or a skipped corrupt row); keeping the route committed by reloads"
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
