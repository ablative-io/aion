//! Shared, atomically-swappable workflow package catalog.
//!
//! The catalog is the single routing authority for loaded workflow packages.
//! Readers resolve against an immutable snapshot behind one `Arc` clone, so a
//! dispatch sees the catalog entirely-before or entirely-after any mutation —
//! never torn state. Writers serialize on a mutation lock, build a fresh
//! snapshot, and commit it with a single pointer swap: that swap *is* the
//! atomic route flip for new starts, while in-flight runs keep the version
//! they already resolved (loads never unregister anything).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

#[path = "catalog_load.rs"]
mod catalog_load;
#[path = "catalog_snapshot.rs"]
mod catalog_snapshot;

use aion_core::PackageVersion;
use aion_package::{ContentHash, ManifestDigest, ManifestVersion};
use chrono::{DateTime, Utc};

use super::load::{LoadedWorkflow, load_error};
use super::version_info::WorkflowVersionInfo;
use crate::error::EngineError;

/// In-flight start pins keyed by `(workflow type, version)`.
type StartPins = Arc<Mutex<HashMap<(String, ContentHash), usize>>>;

/// Shared, atomically-swappable workflow package catalog.
pub struct WorkflowCatalog {
    /// Immutable snapshot; readers clone the `Arc` under a short read lock
    /// and resolve against a consistent view.
    snapshot: RwLock<Arc<CatalogSnapshot>>,
    /// Serializes load / route / unload. Mutation paths are async (unload
    /// verification scans the store), so this is a tokio mutex; dispatch is
    /// never blocked by it — readers only touch `snapshot`.
    mutations: tokio::sync::Mutex<()>,
    /// Starts that resolved a version but have not yet registered a handle
    /// (the registration birth window). Unload refuses while any pin for the
    /// target version is held.
    pinned_starts: StartPins,
}

/// One immutable catalog view.
#[derive(Clone, Default)]
struct CatalogSnapshot {
    by_version: HashMap<(String, ContentHash), CatalogEntry>,
    /// Explicit route pointer per workflow type — replaces the old
    /// insertion-order "latest" reading.
    routed: HashMap<String, ContentHash>,
    /// Deployed-module collision index over every loaded version.
    registered_modules: HashMap<String, ContentHash>,
    /// First-class archive membership, keyed independently from shared beams.
    package_groups: HashMap<(String, ContentHash), PackageGroup>,
}

#[derive(Clone, Debug)]
struct PackageGroup {
    primary_workflow_type: String,
    workflow_types: Vec<String>,
}

/// One loaded package version retained by the catalog.
#[derive(Clone, Debug)]
struct CatalogEntry {
    workflow: LoadedWorkflow,
    manifest_version: ManifestVersion,
    /// Canonical digest of the manifest this version was loaded with. The
    /// content hash covers beams only, so this digest is what detects a
    /// same-hash-different-manifest re-load (the silent-wrong-deploy tripwire).
    manifest_digest: ManifestDigest,
    loaded_at: DateTime<Utc>,
}

/// A resolved workflow holding its in-flight start pin.
///
/// The pin keeps the resolved version visible to unload verification until
/// the start path has inserted the registry handle (or failed); dropping the
/// value releases it.
pub struct PinnedWorkflow {
    workflow: LoadedWorkflow,
    _pin: StartPin,
}

impl PinnedWorkflow {
    /// The resolved workflow record.
    #[must_use]
    pub fn workflow(&self) -> &LoadedWorkflow {
        &self.workflow
    }
}

/// RAII start pin: registered on resolve, released on drop.
struct StartPin {
    pins: StartPins,
    key: (String, ContentHash),
}

/// A version swapped out of the snapshot during unload verification.
///
/// Restoring it is the same single-pointer commit as removing it was.
#[derive(Debug)]
pub(crate) struct RemovedPackage {
    primary_workflow_type: String,
    version: ContentHash,
    entries: Vec<(String, CatalogEntry)>,
    modules: Vec<(String, ContentHash)>,
}

impl RemovedPackage {
    /// Deployed module names registered for the removed version.
    pub(crate) fn module_names(&self) -> impl Iterator<Item = &str> {
        self.modules.iter().map(|(name, _)| name.as_str())
    }

    /// Every workflow type implemented by the archive group.
    pub(crate) fn workflow_types(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .map(|(workflow_type, _)| workflow_type.as_str())
    }

    /// Primary type used as the durable package-record key.
    pub(crate) fn primary_workflow_type(&self) -> &str {
        &self.primary_workflow_type
    }
}

impl Default for WorkflowCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowCatalog {
    /// Creates an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(CatalogSnapshot::default())),
            mutations: tokio::sync::Mutex::new(()),
            pinned_starts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn current(&self) -> Result<Arc<CatalogSnapshot>, EngineError> {
        let guard = self
            .snapshot
            .read()
            .map_err(|_| EngineError::CatalogPoisoned)?;
        Ok(Arc::clone(&guard))
    }

    fn install(&self, snapshot: CatalogSnapshot) -> Result<(), EngineError> {
        *self
            .snapshot
            .write()
            .map_err(|_| EngineError::CatalogPoisoned)? = Arc::new(snapshot);
        Ok(())
    }

    /// Workflow currently routed for `workflow_type`, without a start pin.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the snapshot lock is poisoned.
    pub fn routed(&self, workflow_type: &str) -> Result<Option<LoadedWorkflow>, EngineError> {
        let snapshot = self.current()?;
        Ok(snapshot
            .routed_entry(workflow_type)
            .map(|entry| entry.workflow.clone()))
    }

    /// Durable textual version currently routed for `workflow_type`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the snapshot lock is poisoned.
    pub fn routed_version(
        &self,
        workflow_type: &str,
    ) -> Result<Option<PackageVersion>, EngineError> {
        Ok(self
            .routed(workflow_type)?
            .map(|workflow| super::package_version_of(workflow.version())))
    }

    /// Exact `(type, version)` lookup, without a start pin.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the snapshot lock is poisoned.
    pub fn get(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<Option<LoadedWorkflow>, EngineError> {
        let snapshot = self.current()?;
        Ok(snapshot
            .by_version
            .get(&(workflow_type.to_owned(), version.clone()))
            .map(|entry| entry.workflow.clone()))
    }

    /// Every retained loaded workflow record.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the snapshot lock is poisoned.
    pub fn workflows(&self) -> Result<Vec<LoadedWorkflow>, EngineError> {
        let snapshot = self.current()?;
        Ok(snapshot
            .by_version
            .values()
            .map(|entry| entry.workflow.clone())
            .collect())
    }

    /// Every loaded version with its route flag, sorted by `(type, loaded_at)`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the snapshot lock is poisoned.
    pub fn versions(&self) -> Result<Vec<WorkflowVersionInfo>, EngineError> {
        let snapshot = self.current()?;
        let mut versions: Vec<WorkflowVersionInfo> = snapshot
            .by_version
            .values()
            .map(|entry| WorkflowVersionInfo {
                workflow_type: entry.workflow.workflow_type().to_owned(),
                content_hash: entry.workflow.version().clone(),
                deployed_entry_module: entry.workflow.deployed_entry_module().to_owned(),
                entry_function: entry.workflow.entry_function().to_owned(),
                manifest_version: entry.manifest_version.clone(),
                loaded_at: entry.loaded_at,
                route_active: snapshot.routed.get(entry.workflow.workflow_type())
                    == Some(entry.workflow.version()),
            })
            .collect();
        versions.sort_by(|left, right| {
            left.workflow_type
                .cmp(&right.workflow_type)
                .then(left.loaded_at.cmp(&right.loaded_at))
                .then_with(|| {
                    left.content_hash
                        .to_string()
                        .cmp(&right.content_hash.to_string())
                })
        });
        Ok(versions)
    }

    /// Resolves the routed version of `workflow_type`, holding a start pin.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when a catalog lock is poisoned.
    pub(crate) fn resolve_routed(
        &self,
        workflow_type: &str,
    ) -> Result<Option<PinnedWorkflow>, EngineError> {
        let snapshot = self.current()?;
        let Some(entry) = snapshot.routed_entry(workflow_type) else {
            return Ok(None);
        };
        self.pin_validated(entry.workflow.clone())
    }

    /// Resolves an exact `(type, version)`, holding a start pin.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when a catalog lock is poisoned.
    pub(crate) fn resolve_exact(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<Option<PinnedWorkflow>, EngineError> {
        let snapshot = self.current()?;
        let Some(entry) = snapshot
            .by_version
            .get(&(workflow_type.to_owned(), version.clone()))
        else {
            return Ok(None);
        };
        self.pin_validated(entry.workflow.clone())
    }

    /// Pins the resolution, then re-validates it against the CURRENT
    /// snapshot. An unload that swapped the version out between this
    /// reader's snapshot clone and its pin insert would not have seen the
    /// pin; re-checking after the insert closes that window — either the
    /// unload sees the pin and refuses, or this resolution observes the
    /// removal and reports the version as not loaded. Never both, never
    /// neither, never a dispatch into a deleted module.
    fn pin_validated(
        &self,
        workflow: LoadedWorkflow,
    ) -> Result<Option<PinnedWorkflow>, EngineError> {
        let pinned = self.pin(workflow)?;
        let key = (
            pinned.workflow.workflow_type().to_owned(),
            pinned.workflow.version().clone(),
        );
        if self.current()?.by_version.contains_key(&key) {
            Ok(Some(pinned))
        } else {
            drop(pinned);
            Ok(None)
        }
    }

    fn pin(&self, workflow: LoadedWorkflow) -> Result<PinnedWorkflow, EngineError> {
        let key = (
            workflow.workflow_type().to_owned(),
            workflow.version().clone(),
        );
        {
            let mut pins = self
                .pinned_starts
                .lock()
                .map_err(|_| EngineError::CatalogPoisoned)?;
            *pins.entry(key.clone()).or_insert(0) += 1;
        }
        Ok(PinnedWorkflow {
            workflow,
            _pin: StartPin {
                pins: Arc::clone(&self.pinned_starts),
                key,
            },
        })
    }

    /// Whether any in-flight start currently pins `(type, version)`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] when the pin lock is poisoned.
    pub(crate) fn has_pinned_starts(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<bool, EngineError> {
        let pins = self
            .pinned_starts
            .lock()
            .map_err(|_| EngineError::CatalogPoisoned)?;
        Ok(pins
            .get(&(workflow_type.to_owned(), version.clone()))
            .is_some_and(|count| *count > 0))
    }

    /// Re-points the route for `workflow_type` at an already-loaded version.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnknownVersion`] naming the loaded set when the
    /// version is not loaded, and [`EngineError::CatalogPoisoned`] on lock
    /// poison.
    pub(crate) async fn route_version(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<(), EngineError> {
        let _mutation = self.mutations.lock().await;
        let snapshot = self.current()?;
        let key = (workflow_type.to_owned(), version.clone());
        if !snapshot.by_version.contains_key(&key) {
            return Err(EngineError::UnknownVersion {
                workflow_type: workflow_type.to_owned(),
                version: version.clone(),
                loaded: snapshot.loaded_versions_of(workflow_type),
            });
        }
        if snapshot.routed.get(workflow_type) == Some(version) {
            return Ok(());
        }
        let mut next = (*snapshot).clone();
        next.routed
            .insert(workflow_type.to_owned(), version.clone());
        self.install(next)
    }

    /// Acquires the catalog mutation lock for a multi-step protocol (unload).
    pub(crate) async fn begin_mutation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutations.lock().await
    }

    /// Swaps an entire non-routed archive group out so no sibling entry remains
    /// reachable without the group's shared modules. Caller holds the mutation lock.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnknownVersion`] when the version is not loaded
    /// and [`EngineError::RouteActive`] when it is the route-active version of
    /// its type.
    pub(crate) fn swap_out_package(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<RemovedPackage, EngineError> {
        let snapshot = self.current()?;
        let key = (workflow_type.to_owned(), version.clone());
        if !snapshot.by_version.contains_key(&key) {
            return Err(EngineError::UnknownVersion {
                workflow_type: workflow_type.to_owned(),
                version: version.clone(),
                loaded: snapshot.loaded_versions_of(workflow_type),
            });
        }
        let group = snapshot
            .package_groups
            .iter()
            .find(|((_, hash), group)| {
                hash == version
                    && group
                        .workflow_types
                        .iter()
                        .any(|member| member == workflow_type)
            })
            .map(|(_, group)| group)
            .ok_or_else(|| {
                load_error(format!(
                    "package group for workflow `{workflow_type}` version `{version}` is missing"
                ))
            })?;
        for member in &group.workflow_types {
            if snapshot.routed.get(member) == Some(version) {
                return Err(EngineError::RouteActive {
                    workflow_type: member.clone(),
                    version: version.clone(),
                });
            }
        }
        let mut next = (*snapshot).clone();
        let mut entries = Vec::with_capacity(group.workflow_types.len());
        for member in &group.workflow_types {
            let member_key = (member.clone(), version.clone());
            let Some(member_entry) = next.by_version.remove(&member_key) else {
                return Err(load_error(format!(
                    "package group `{version}` is partially registered: missing `{member}`"
                )));
            };
            entries.push((member.clone(), member_entry));
        }
        next.package_groups
            .remove(&(group.primary_workflow_type.clone(), version.clone()));
        let hash_still_referenced = next.package_groups.keys().any(|(_, hash)| hash == version);
        let modules: Vec<(String, ContentHash)> = if hash_still_referenced {
            Vec::new()
        } else {
            next.registered_modules
                .iter()
                .filter(|(_, hash)| *hash == version)
                .map(|(name, hash)| (name.clone(), hash.clone()))
                .collect()
        };
        for (name, _) in &modules {
            next.registered_modules.remove(name);
        }
        self.install(next)?;
        Ok(RemovedPackage {
            primary_workflow_type: group.primary_workflow_type.clone(),
            version: version.clone(),
            entries,
            modules,
        })
    }

    /// Restores a group swapped out by [`Self::swap_out_package`] after a
    /// failed unload check. Caller must hold the mutation lock.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] on lock poison.
    pub(crate) fn restore_package(&self, removed: RemovedPackage) -> Result<(), EngineError> {
        let snapshot = self.current()?;
        let mut next = (*snapshot).clone();
        let workflow_types = removed
            .entries
            .iter()
            .map(|(workflow_type, _)| workflow_type.clone())
            .collect();
        for (workflow_type, entry) in removed.entries {
            next.by_version
                .insert((workflow_type, removed.version.clone()), entry);
        }
        for (name, hash) in removed.modules {
            next.registered_modules.insert(name, hash);
        }
        next.package_groups.insert(
            (removed.primary_workflow_type.clone(), removed.version),
            PackageGroup {
                primary_workflow_type: removed.primary_workflow_type,
                workflow_types,
            },
        );
        self.install(next)
    }
}

#[cfg(test)]
#[path = "catalog_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "catalog_multi_entry_tests.rs"]
mod catalog_multi_entry_tests;
#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
#[cfg(test)]
#[path = "catalog_timeout_tests.rs"]
mod catalog_timeout_tests;
