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

#[path = "catalog_snapshot.rs"]
mod catalog_snapshot;

use aion_core::PackageVersion;
use aion_package::{ContentHash, ManifestDigest, ManifestVersion, Package};
use chrono::{DateTime, Utc};

use super::load::{LoadOutcome, LoadedWorkflow, StagedLoad, load_error, rollback_registered};
use super::version_info::WorkflowVersionInfo;
use crate::{error::EngineError, runtime::RuntimeHandle};

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
pub(crate) struct RemovedVersion {
    workflow_type: String,
    version: ContentHash,
    entry: CatalogEntry,
    modules: Vec<(String, ContentHash)>,
}

impl RemovedVersion {
    /// Deployed module names registered for the removed version.
    pub(crate) fn module_names(&self) -> impl Iterator<Item = &str> {
        self.modules.iter().map(|(name, _)| name.as_str())
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

    /// Loads a validated package into the runtime and atomically routes its
    /// workflow type's new dispatches to it.
    ///
    /// Re-loading an already-loaded hash registers nothing and returns the
    /// existing record with `freshly_loaded = false`, but still re-points the
    /// route at it ("deploy archive X" is a routing intent); loading the
    /// currently-routed hash is a full no-op (`route_changed = false`). An
    /// idempotent re-load whose manifest differs from the resident version's
    /// manifest is refused typed ([`EngineError::ManifestMismatch`]) — the
    /// content hash covers beams only, so a differing manifest means the
    /// archive is not the version the catalog holds.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Load`] for validation, collision, registration,
    /// or entry-verification failures, and [`EngineError::ManifestMismatch`]
    /// for the same-hash-different-manifest refusal. On failure the snapshot
    /// is untouched: routing, existing versions, and in-flight dispatches are
    /// unaffected.
    pub async fn load_package(
        &self,
        runtime: &RuntimeHandle,
        package: &Package,
    ) -> Result<LoadOutcome, EngineError> {
        let hash = package.content_hash();
        let nif_modules = runtime.registered_nif_modules();

        let originals: Vec<&str> = package
            .beams()
            .iter()
            .map(aion_package::BeamModule::name)
            .filter(|name| !nif_modules.contains(&(*name).to_owned()))
            .collect();
        let deployed: Vec<String> = originals
            .iter()
            .map(|name| aion_package::deployed_name(name, hash))
            .collect();
        let deployed_refs: Vec<&str> = deployed.iter().map(String::as_str).collect();
        let rename_map = runtime.package_rename_map(&originals, &deployed_refs);

        let nif_set: std::collections::HashSet<&str> =
            nif_modules.iter().map(String::as_str).collect();
        let is_nif = |name: &str| {
            let original = name.split('$').next().unwrap_or(name);
            nif_set.contains(original)
        };

        self.load_package_with(
            package,
            |name, bytes| {
                if is_nif(name) {
                    return Ok(());
                }
                runtime.register_module_with_renames(name, bytes, &rename_map)
            },
            |name| {
                if is_nif(name) {
                    return Ok(());
                }
                runtime.unregister_module(name)
            },
            |entry_module, entry_function| {
                if runtime.module_exports_function(entry_module, entry_function) {
                    Ok(())
                } else {
                    Err(load_error(format!(
                        "deployed entry module `{entry_module}` does not export entry function `{entry_function}`"
                    )))
                }
            },
        )
        .await
    }

    /// Load protocol over caller-supplied register/rollback/verify seams.
    pub(crate) async fn load_package_with<F, R, V>(
        &self,
        package: &Package,
        mut register: F,
        mut rollback: R,
        verify_entry: V,
    ) -> Result<LoadOutcome, EngineError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
        R: FnMut(&str) -> Result<(), EngineError>,
        V: FnMut(&str, &str) -> Result<(), EngineError>,
    {
        let _mutation = self.mutations.lock().await;
        let staged = StagedLoad::new(package)?;
        let snapshot = self.current()?;

        // Preflight: a deployed name already committed for a DIFFERENT hash
        // is a collision; the same hash means this version (or a shared
        // module of it) is already registered and is skipped below.
        for module in &staged.modules {
            if let Some(existing) = snapshot.registered_modules.get(&module.deployed_name) {
                if existing != &staged.version {
                    return Err(load_error(format!(
                        "deployed module `{}` is already registered for content hash `{existing}`, not `{}`",
                        module.deployed_name, staged.version
                    )));
                }
            }
        }

        let existing: Vec<_> = staged
            .workflows
            .iter()
            .filter_map(|workflow| {
                snapshot
                    .by_version
                    .get(&(workflow.workflow_type.clone(), staged.version.clone()))
            })
            .collect();
        if !existing.is_empty() && existing.len() != staged.workflows.len() {
            return Err(load_error(format!(
                "package version `{}` is only partially registered ({}/{} workflow entries)",
                staged.version,
                existing.len(),
                staged.workflows.len()
            )));
        }
        if let Some(first) = existing.first() {
            // Same-hash-different-manifest tripwire: the content hash covers
            // the beam set only, so an "idempotent" re-load can carry a
            // manifest the resident version was never loaded with. Refuse
            // typed instead of silently ignoring the incoming manifest.
            if existing
                .iter()
                .any(|entry| entry.manifest_digest != staged.manifest_digest)
            {
                return Err(EngineError::ManifestMismatch {
                    workflow_type: first.workflow.workflow_type().to_owned(),
                    version: staged.version.clone(),
                    resident_digest: first.manifest_digest.to_string(),
                    incoming_digest: staged.manifest_digest.to_string(),
                });
            }
            // Idempotent re-load: nothing registers, but re-deploying a
            // previously rolled-back package re-points every entry atomically.
            let record = first.workflow.clone();
            let route_changed = staged.workflows.iter().any(|workflow| {
                snapshot.routed.get(&workflow.workflow_type) != Some(&staged.version)
            });
            if route_changed {
                let mut next = (*snapshot).clone();
                for workflow in &staged.workflows {
                    next.routed
                        .insert(workflow.workflow_type.clone(), staged.version.clone());
                }
                self.install(next)?;
            }
            return Ok(LoadOutcome {
                record,
                freshly_loaded: false,
                route_changed,
            });
        }

        let mut registered_now = Vec::new();
        for module in &staged.modules {
            if snapshot
                .registered_modules
                .contains_key(&module.deployed_name)
            {
                continue;
            }
            if let Err(error) = register(&module.deployed_name, module.bytes) {
                let rollback_errors = rollback_registered(&mut rollback, &registered_now);
                return Err(load_error(format!(
                    "runtime rejected deployed module `{}` after {} staged registrations: {error}{}",
                    module.deployed_name,
                    registered_now.len(),
                    rollback_errors
                )));
            }
            registered_now.push(module.deployed_name.clone());
        }

        // Entry-point verification before the route commit: a package whose
        // entry module loads but exports nothing routable must fail the
        // load, not the first dispatch.
        let mut verify_entry = verify_entry;
        for workflow in &staged.workflows {
            if let Err(error) =
                verify_entry(&workflow.deployed_entry_module, &workflow.entry_function)
            {
                let rollback_errors = rollback_registered(&mut rollback, &registered_now);
                return Err(load_error(format!(
                    "entry verification failed for workflow `{}`: {error}{rollback_errors}",
                    workflow.workflow_type
                )));
            }
        }

        let records = staged.records();
        let Some(record) = records.first().cloned() else {
            return Err(load_error("package staged no workflow entries".to_owned()));
        };
        let mut next = (*snapshot).clone();
        for module in &staged.modules {
            next.registered_modules
                .entry(module.deployed_name.clone())
                .or_insert_with(|| staged.version.clone());
        }
        let loaded_at = Utc::now();
        for workflow in &records {
            next.by_version.insert(
                (workflow.workflow_type().to_owned(), staged.version.clone()),
                CatalogEntry {
                    workflow: workflow.clone(),
                    manifest_version: staged.manifest_version.clone(),
                    manifest_digest: staged.manifest_digest.clone(),
                    loaded_at,
                },
            );
        }
        // A fresh load always commits the route pointer; `route_changed`
        // reports whether any entry moved; all pointers commit in one swap.
        let route_changed = records
            .iter()
            .any(|workflow| snapshot.routed.get(workflow.workflow_type()) != Some(&staged.version));
        for workflow in &records {
            next.routed
                .insert(workflow.workflow_type().to_owned(), staged.version.clone());
        }
        self.install(next)?;
        Ok(LoadOutcome {
            record,
            freshly_loaded: true,
            route_changed,
        })
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

    /// Swaps a non-routed version out of the snapshot so no new resolution
    /// can produce it. Caller must hold the mutation lock.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::UnknownVersion`] when the version is not loaded
    /// and [`EngineError::RouteActive`] when it is the route-active version of
    /// its type.
    pub(crate) fn swap_out_version(
        &self,
        workflow_type: &str,
        version: &ContentHash,
    ) -> Result<RemovedVersion, EngineError> {
        let snapshot = self.current()?;
        let key = (workflow_type.to_owned(), version.clone());
        let Some(entry) = snapshot.by_version.get(&key) else {
            return Err(EngineError::UnknownVersion {
                workflow_type: workflow_type.to_owned(),
                version: version.clone(),
                loaded: snapshot.loaded_versions_of(workflow_type),
            });
        };
        if snapshot.routed.get(workflow_type) == Some(version) {
            return Err(EngineError::RouteActive {
                workflow_type: workflow_type.to_owned(),
                version: version.clone(),
            });
        }
        let mut next = (*snapshot).clone();
        next.by_version.remove(&key);
        let modules: Vec<(String, ContentHash)> = next
            .registered_modules
            .iter()
            .filter(|(_, hash)| *hash == version)
            .map(|(name, hash)| (name.clone(), hash.clone()))
            .collect();
        for (name, _) in &modules {
            next.registered_modules.remove(name);
        }
        self.install(next)?;
        Ok(RemovedVersion {
            workflow_type: workflow_type.to_owned(),
            version: version.clone(),
            entry: entry.clone(),
            modules,
        })
    }

    /// Restores a version swapped out by [`Self::swap_out_version`] after a
    /// failed unload check. Caller must hold the mutation lock.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::CatalogPoisoned`] on lock poison.
    pub(crate) fn restore_version(&self, removed: RemovedVersion) -> Result<(), EngineError> {
        let snapshot = self.current()?;
        let mut next = (*snapshot).clone();
        next.by_version.insert(
            (removed.workflow_type.clone(), removed.version.clone()),
            removed.entry,
        );
        for (name, hash) in removed.modules {
            next.registered_modules.insert(name, hash);
        }
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
