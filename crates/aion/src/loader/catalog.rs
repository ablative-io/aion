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
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use aion_core::PackageVersion;
use aion_package::{ContentHash, ManifestVersion, Package};
use chrono::{DateTime, Utc};

use super::load::{LoadedWorkflow, StagedLoad, load_error, rollback_registered};
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
    loaded_at: DateTime<Utc>,
}

/// One loaded version of one workflow type, as reported by the catalog.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowVersionInfo {
    /// Logical workflow type this version belongs to.
    pub workflow_type: String,
    /// Content hash identifying the package version (textual when serialized).
    pub content_hash: ContentHash,
    /// Namespaced module name spawned for this version.
    pub deployed_entry_module: String,
    /// Exported entry function spawned for this version.
    pub entry_function: String,
    /// Author-declared manifest version label.
    pub manifest_version: ManifestVersion,
    /// Engine-local instant this version was loaded.
    pub loaded_at: DateTime<Utc>,
    /// Whether new dispatches of this type currently route to this version.
    pub route_active: bool,
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

impl Drop for StartPin {
    fn drop(&mut self) {
        let mut pins = self.pins.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = pins.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pins.remove(&self.key);
            }
        }
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
        snapshot
            .routed_entry(workflow_type)
            .map(|entry| self.pin(entry.workflow.clone()))
            .transpose()
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
        snapshot
            .by_version
            .get(&(workflow_type.to_owned(), version.clone()))
            .map(|entry| self.pin(entry.workflow.clone()))
            .transpose()
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
    #[cfg(test)]
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
    /// existing record, but still re-points the route at it ("deploy archive
    /// X" is a routing intent); loading the currently-routed hash is a full
    /// no-op.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Load`] for validation, collision, registration,
    /// or entry-verification failures. On failure the snapshot is untouched:
    /// routing, existing versions, and in-flight dispatches are unaffected.
    pub async fn load_package(
        &self,
        runtime: &RuntimeHandle,
        package: &Package,
    ) -> Result<LoadedWorkflow, EngineError> {
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
    ) -> Result<LoadedWorkflow, EngineError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
        R: FnMut(&str) -> Result<(), EngineError>,
        V: FnOnce(&str, &str) -> Result<(), EngineError>,
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

        let key = (staged.workflow_type.clone(), staged.version.clone());
        if let Some(existing) = snapshot.by_version.get(&key) {
            // Idempotent re-load: nothing registers, but re-deploying a
            // previously rolled-back version re-points the route at it.
            let record = existing.workflow.clone();
            if snapshot.routed.get(&staged.workflow_type) != Some(&staged.version) {
                let mut next = (*snapshot).clone();
                next.routed
                    .insert(staged.workflow_type.clone(), staged.version.clone());
                self.install(next)?;
            }
            return Ok(record);
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
        if let Err(error) = verify_entry(&staged.deployed_entry_module, &staged.entry_function) {
            let rollback_errors = rollback_registered(&mut rollback, &registered_now);
            return Err(load_error(format!(
                "entry verification failed for `{}`: {error}{}",
                staged.deployed_entry_module, rollback_errors
            )));
        }

        let record = staged.record();
        let mut next = (*snapshot).clone();
        for module in &staged.modules {
            next.registered_modules
                .entry(module.deployed_name.clone())
                .or_insert_with(|| staged.version.clone());
        }
        next.by_version.insert(
            key,
            CatalogEntry {
                workflow: record.clone(),
                manifest_version: staged.manifest_version.clone(),
                loaded_at: Utc::now(),
            },
        );
        next.routed
            .insert(staged.workflow_type.clone(), staged.version.clone());
        self.install(next)?;
        Ok(record)
    }

    /// Returns true when the catalog has committed the deployed module name.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn has_registered_module(&self, deployed_name: &str) -> bool {
        self.current()
            .map(|snapshot| snapshot.registered_modules.contains_key(deployed_name))
            .unwrap_or(false)
    }

    /// Records a loaded workflow entry without runtime registration for tests.
    #[cfg(test)]
    pub(crate) fn note_loaded_workflow_for_test(
        &self,
        workflow_type: impl Into<String>,
        deployed_entry_module: impl Into<String>,
        entry_function: impl Into<String>,
        version: ContentHash,
    ) -> LoadedWorkflow {
        let record = LoadedWorkflow::from_parts(
            workflow_type.into(),
            deployed_entry_module.into(),
            entry_function.into(),
            version,
        );
        let Ok(snapshot) = self.current() else {
            return record;
        };
        let mut next = (*snapshot).clone();
        next.by_version.insert(
            (record.workflow_type().to_owned(), record.version().clone()),
            CatalogEntry {
                workflow: record.clone(),
                manifest_version: ManifestVersion::new("test"),
                loaded_at: Utc::now(),
            },
        );
        next.routed
            .insert(record.workflow_type().to_owned(), record.version().clone());
        let _ = self.install(next);
        record
    }

    /// Forces a committed module-name mapping for collision tests.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Load`] when the name is already mapped to a
    /// different version.
    #[cfg(test)]
    pub(crate) fn note_registered_module(
        &self,
        deployed_name: impl Into<String>,
        version: ContentHash,
    ) -> Result<(), EngineError> {
        let deployed_name = deployed_name.into();
        let snapshot = self.current()?;
        match snapshot.registered_modules.get(&deployed_name) {
            Some(existing) if existing != &version => Err(load_error(format!(
                "deployed module `{deployed_name}` is already registered for content hash `{existing}`, not `{version}`"
            ))),
            Some(_) => Ok(()),
            None => {
                let mut next = (*snapshot).clone();
                next.registered_modules.insert(deployed_name, version);
                self.install(next)
            }
        }
    }
}

impl CatalogSnapshot {
    fn routed_entry(&self, workflow_type: &str) -> Option<&CatalogEntry> {
        let version = self.routed.get(workflow_type)?;
        self.by_version
            .get(&(workflow_type.to_owned(), version.clone()))
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
