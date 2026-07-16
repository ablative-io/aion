//! Package module staging, verification, and atomic route publication.

use aion_package::{ContentHash, Package};
use chrono::Utc;

use super::{CatalogEntry, CatalogSnapshot, PackageGroup, WorkflowCatalog};
use crate::loader::load::{LoadOutcome, StagedLoad, load_error, rollback_registered};
use crate::{error::EngineError, runtime::RuntimeHandle};

impl WorkflowCatalog {
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
        self.load_package_mode(runtime, package, true).await
    }

    /// Stages verified modules and version entries without publishing routes.
    pub(crate) async fn stage_package(
        &self,
        runtime: &RuntimeHandle,
        package: &Package,
    ) -> Result<LoadOutcome, EngineError> {
        self.load_package_mode(runtime, package, false).await
    }

    async fn load_package_mode(
        &self,
        runtime: &RuntimeHandle,
        package: &Package,
        publish_routes: bool,
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

        self.load_package_with_mode(
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
            publish_routes,
        )
        .await
    }

    /// Load protocol over caller-supplied register/rollback/verify seams.
    #[cfg(test)]
    pub(crate) async fn load_package_with<F, R, V>(
        &self,
        package: &Package,
        register: F,
        rollback: R,
        verify_entry: V,
    ) -> Result<LoadOutcome, EngineError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
        R: FnMut(&str) -> Result<(), EngineError>,
        V: FnMut(&str, &str) -> Result<(), EngineError>,
    {
        self.load_package_with_mode(package, register, rollback, verify_entry, true)
            .await
    }

    /// Test seam for the durable-first gap between verified staging and route publication.
    #[cfg(test)]
    pub(crate) async fn stage_package_with<F, R, V>(
        &self,
        package: &Package,
        register: F,
        rollback: R,
        verify_entry: V,
    ) -> Result<LoadOutcome, EngineError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
        R: FnMut(&str) -> Result<(), EngineError>,
        V: FnMut(&str, &str) -> Result<(), EngineError>,
    {
        self.load_package_with_mode(package, register, rollback, verify_entry, false)
            .await
    }

    async fn load_package_with_mode<F, R, V>(
        &self,
        package: &Package,
        mut register: F,
        mut rollback: R,
        verify_entry: V,
        publish_routes: bool,
    ) -> Result<LoadOutcome, EngineError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
        R: FnMut(&str) -> Result<(), EngineError>,
        V: FnMut(&str, &str) -> Result<(), EngineError>,
    {
        let mutation_guard = self.mutations.lock().await;
        let staged = StagedLoad::new(package)?;
        let snapshot = self.current()?;

        validate_staged_module_names(&staged, &snapshot)?;
        if let Some(outcome) = self.resolve_existing_load(&staged, &snapshot, publish_routes)? {
            drop(mutation_guard);
            return Ok(outcome);
        }

        let registered_now =
            register_staged_modules(&staged, &snapshot, &mut register, &mut rollback)?;
        verify_staged_entries(&staged, verify_entry, &mut rollback, &registered_now)?;

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
        next.package_groups.insert(
            (
                package.manifest().entry_module.clone(),
                staged.version.clone(),
            ),
            PackageGroup {
                primary_workflow_type: package.manifest().entry_module.clone(),
                workflow_types: records
                    .iter()
                    .map(|workflow| workflow.workflow_type().to_owned())
                    .collect(),
            },
        );
        // A fresh load always commits the route pointer; `route_changed`
        // reports whether any entry moved; all pointers commit in one swap.
        let route_changed = records
            .iter()
            .any(|workflow| snapshot.routed.get(workflow.workflow_type()) != Some(&staged.version));
        if publish_routes {
            for workflow in &records {
                next.routed
                    .insert(workflow.workflow_type().to_owned(), staged.version.clone());
            }
        }
        self.install(next)?;
        drop(mutation_guard);
        Ok(LoadOutcome {
            record,
            freshly_loaded: true,
            route_changed,
        })
    }

    fn resolve_existing_load(
        &self,
        staged: &StagedLoad<'_>,
        snapshot: &CatalogSnapshot,
        publish_routes: bool,
    ) -> Result<Option<LoadOutcome>, EngineError> {
        let existing: Vec<_> = staged
            .workflows
            .iter()
            .filter_map(|workflow| {
                snapshot
                    .by_version
                    .get(&(workflow.workflow_type.clone(), staged.version.clone()))
            })
            .collect();
        if existing.is_empty() {
            return Ok(None);
        }
        if existing.len() != staged.workflows.len() {
            return Err(load_error(format!(
                "package version `{}` is only partially registered ({}/{} workflow entries)",
                staged.version,
                existing.len(),
                staged.workflows.len()
            )));
        }

        let first = existing[0];
        // The content hash covers beams only; an idempotent load with a
        // different manifest is not the resident package and must be refused.
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

        let route_changed = staged
            .workflows
            .iter()
            .any(|workflow| snapshot.routed.get(&workflow.workflow_type) != Some(&staged.version));
        if publish_routes && route_changed {
            let mut next = snapshot.clone();
            for workflow in &staged.workflows {
                next.routed
                    .insert(workflow.workflow_type.clone(), staged.version.clone());
            }
            self.install(next)?;
        }
        Ok(Some(LoadOutcome {
            record: first.workflow.clone(),
            freshly_loaded: false,
            route_changed,
        }))
    }

    /// Atomically publishes every route in a previously staged package group.
    pub(crate) async fn publish_package_routes(
        &self,
        primary_workflow_type: &str,
        version: &ContentHash,
    ) -> Result<(), EngineError> {
        let mutation_guard = self.mutations.lock().await;
        let snapshot = self.current()?;
        let group = snapshot
            .package_groups
            .get(&(primary_workflow_type.to_owned(), version.clone()))
            .ok_or_else(|| load_error(format!("staged package group `{version}` is not loaded")))?;
        for workflow_type in &group.workflow_types {
            if !snapshot
                .by_version
                .contains_key(&(workflow_type.clone(), version.clone()))
            {
                return Err(load_error(format!(
                    "staged package group `{version}` is missing workflow `{workflow_type}`"
                )));
            }
        }
        let mut next = (*snapshot).clone();
        for workflow_type in &group.workflow_types {
            next.routed.insert(workflow_type.clone(), version.clone());
        }
        self.install(next)?;
        drop(mutation_guard);
        Ok(())
    }
}

fn validate_staged_module_names(
    staged: &StagedLoad<'_>,
    snapshot: &CatalogSnapshot,
) -> Result<(), EngineError> {
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
    Ok(())
}

fn register_staged_modules<F, R>(
    staged: &StagedLoad<'_>,
    snapshot: &CatalogSnapshot,
    register: &mut F,
    rollback: &mut R,
) -> Result<Vec<String>, EngineError>
where
    F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
    R: FnMut(&str) -> Result<(), EngineError>,
{
    let mut registered = Vec::new();
    for module in &staged.modules {
        if snapshot
            .registered_modules
            .contains_key(&module.deployed_name)
        {
            continue;
        }
        if let Err(error) = register(&module.deployed_name, module.bytes) {
            let rollback_errors = rollback_registered(rollback, &registered);
            return Err(load_error(format!(
                "runtime rejected deployed module `{}` after {} staged registrations: {error}{rollback_errors}",
                module.deployed_name,
                registered.len()
            )));
        }
        registered.push(module.deployed_name.clone());
    }
    Ok(registered)
}

fn verify_staged_entries<V, R>(
    staged: &StagedLoad<'_>,
    mut verify: V,
    rollback: &mut R,
    registered: &[String],
) -> Result<(), EngineError>
where
    V: FnMut(&str, &str) -> Result<(), EngineError>,
    R: FnMut(&str) -> Result<(), EngineError>,
{
    for workflow in &staged.workflows {
        if let Err(error) = verify(&workflow.deployed_entry_module, &workflow.entry_function) {
            let rollback_errors = rollback_registered(rollback, registered);
            return Err(load_error(format!(
                "entry verification failed for workflow `{}`: {error}{rollback_errors}",
                workflow.workflow_type
            )));
        }
    }
    Ok(())
}
