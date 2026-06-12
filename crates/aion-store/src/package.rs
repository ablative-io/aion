//! Deployed-package persistence contract.
//!
//! Packages deployed at runtime (the live `Engine::load_package` seam) are
//! part of the engine's durable state: a run is pinned to the package version
//! recorded in its `WorkflowStarted` event, so startup recovery can only
//! resolve that pin if the deployed archive itself survives the restart.
//! This module defines the store-side contract for that durability — archive
//! rows keyed by `(workflow_type, content_hash)` plus the per-type route
//! pointer that decides which version new starts resolve.
//!
//! The store treats both values as opaque engine truth: the archive bytes are
//! the canonical `.aion` container (re-validated by the engine on reload) and
//! the content hash is its 64-hex textual form. No store backend parses
//! either.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::StoreError;

/// One persisted deployed-package archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageRecord {
    /// Logical workflow type the package's manifest entry module names.
    pub workflow_type: String,
    /// Canonical 64-hex textual content hash identifying this version.
    pub content_hash: String,
    /// Complete `.aion` archive bytes as deployed.
    pub archive: Vec<u8>,
    /// When this version was (last) deployed.
    pub deployed_at: DateTime<Utc>,
}

/// One persisted route pointer: the version new starts of a type resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageRouteRecord {
    /// Workflow type the pointer routes.
    pub workflow_type: String,
    /// Canonical 64-hex textual content hash the route points at.
    pub content_hash: String,
}

/// Durable persistence contract for runtime-deployed workflow packages.
///
/// Every Aion event store must implement this: deployed packages are part of
/// the same durability promise as event history, and a backend that kept
/// history but dropped packages would strand every recovered run on a
/// version the catalog cannot resolve.
#[async_trait]
pub trait PackageStore: Send + Sync + 'static {
    /// Persists `record` and atomically points the type's route at it.
    ///
    /// This mirrors the engine's load semantics one-to-one: a successful
    /// load always re-points the route of `record.workflow_type` at
    /// `record.content_hash`, so the persisted package and the persisted
    /// route pointer must commit together — a crash between them would
    /// resurrect a stale route on restart. Re-persisting an existing
    /// `(workflow_type, content_hash)` replaces the row (idempotent
    /// re-deploy) and still re-points the route.
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError>;

    /// Lists every persisted package in ascending `deployed_at` order
    /// (ties broken by `(workflow_type, content_hash)` text order), so
    /// startup reload re-applies deploys deterministically.
    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError>;

    /// Deletes the persisted archive for `(workflow_type, content_hash)`.
    ///
    /// Deleting an absent row is a no-op, never an error: unload must be
    /// idempotent and versions loaded from operator-supplied files were
    /// never persisted.
    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError>;

    /// Upserts the route pointer for `workflow_type` to `content_hash`.
    ///
    /// Used by explicit route re-points (rollback / roll-forward) targeting
    /// an already-loaded version; the pointed-at version is not required to
    /// have a persisted archive (it may be an operator-file load), and the
    /// engine resolves that loudly at reload time.
    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError>;

    /// Lists every persisted route pointer in `workflow_type` text order.
    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError>;
}
