//! Package -> namespaced beams -> runtime register

use std::collections::{BTreeMap, HashMap};

use aion_package::{ContentHash, Package};

use crate::{error::EngineError, runtime::RuntimeHandle};

/// Workflow package entrypoint registered in the embedded runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedWorkflow {
    workflow_type: String,
    deployed_entry_module: String,
    entry_function: String,
    version: ContentHash,
}

impl LoadedWorkflow {
    /// Logical workflow type from the package manifest entry module.
    #[must_use]
    pub fn workflow_type(&self) -> &str {
        &self.workflow_type
    }

    /// Namespaced module name to spawn for this package version.
    #[must_use]
    pub fn deployed_entry_module(&self) -> &str {
        &self.deployed_entry_module
    }

    /// Exported function to spawn for this package version.
    #[must_use]
    pub fn entry_function(&self) -> &str {
        &self.entry_function
    }

    /// Content-hash version identifying this package.
    #[must_use]
    pub fn version(&self) -> &ContentHash {
        &self.version
    }
}

/// Loader-owned record of package versions registered in a runtime.
#[derive(Clone, Debug, Default)]
pub struct LoadedWorkflows {
    by_version: HashMap<(String, ContentHash), LoadedWorkflow>,
    by_type: BTreeMap<String, Vec<ContentHash>>,
    registered_modules: HashMap<String, ContentHash>,
}

impl LoadedWorkflows {
    /// Create an empty loaded-workflow collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a package into the runtime and record its deployed entrypoint.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Load`] when the package cannot be registered as an
    /// engine workflow or if BEAM registration fails.
    pub fn load_package(
        &mut self,
        runtime: &RuntimeHandle,
        package: &Package,
    ) -> Result<LoadedWorkflow, EngineError> {
        self.load_with_rollback(
            package,
            |deployed_name, bytes| runtime.register_module(deployed_name, bytes),
            |deployed_name| runtime.unregister_module(deployed_name),
        )
    }

    /// Look up an exact workflow type and content-hash version.
    #[must_use]
    pub fn get(&self, workflow_type: &str, version: &ContentHash) -> Option<&LoadedWorkflow> {
        self.by_version
            .get(&(workflow_type.to_owned(), version.clone()))
    }

    /// Look up the most recently loaded version for a workflow type.
    #[must_use]
    pub fn latest(&self, workflow_type: &str) -> Option<&LoadedWorkflow> {
        let versions = self.by_type.get(workflow_type)?;
        let version = versions.last()?;
        self.get(workflow_type, version)
    }

    /// Iterate all retained loaded workflow records.
    pub fn iter(&self) -> impl Iterator<Item = &LoadedWorkflow> {
        self.by_version.values()
    }

    /// Return true when the loader has committed the deployed module name.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn has_registered_module(&self, deployed_name: &str) -> bool {
        self.registered_modules.contains_key(deployed_name)
    }

    /// Record a loaded workflow entry without runtime registration for lifecycle tests.
    #[cfg(test)]
    pub(crate) fn note_loaded_workflow_for_test(
        &mut self,
        workflow_type: impl Into<String>,
        deployed_entry_module: impl Into<String>,
        entry_function: impl Into<String>,
        version: ContentHash,
    ) -> LoadedWorkflow {
        let record = LoadedWorkflow {
            workflow_type: workflow_type.into(),
            deployed_entry_module: deployed_entry_module.into(),
            entry_function: entry_function.into(),
            version,
        };
        let key = (record.workflow_type.clone(), record.version.clone());
        self.by_type
            .entry(record.workflow_type.clone())
            .or_default()
            .push(record.version.clone());
        self.by_version.insert(key, record.clone());
        record
    }

    /// Force a committed module-name mapping for tests and integration recovery.
    ///
    /// This lets callers reconstruct the loader-side collision index from a
    /// runtime they know was previously populated. Normal loading should use
    /// [`Self::load_package`].
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Load`] when the name is already mapped to a
    /// different version.
    #[cfg(test)]
    pub(crate) fn note_registered_module(
        &mut self,
        deployed_name: impl Into<String>,
        version: ContentHash,
    ) -> Result<(), EngineError> {
        let deployed_name = deployed_name.into();
        match self.registered_modules.get(&deployed_name) {
            Some(existing) if existing != &version => Err(load_error(format!(
                "deployed module `{deployed_name}` is already registered for content hash `{existing}`, not `{version}`"
            ))),
            Some(_) => Ok(()),
            None => {
                self.registered_modules.insert(deployed_name, version);
                Ok(())
            }
        }
    }

    fn load_with_rollback<F, R>(
        &mut self,
        package: &Package,
        mut register: F,
        mut rollback: R,
    ) -> Result<LoadedWorkflow, EngineError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), EngineError>,
        R: FnMut(&str) -> Result<(), EngineError>,
    {
        let staged = StagedLoad::new(package)?;
        self.preflight(&staged)?;

        let already_committed = staged.modules.iter().all(|module| {
            self.registered_modules.get(&module.deployed_name) == Some(&staged.version)
        });
        if !already_committed {
            let mut registered_now = Vec::new();
            for module in &staged.modules {
                if self.registered_modules.contains_key(&module.deployed_name) {
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
        }

        Ok(self.commit(staged))
    }

    fn preflight(&self, staged: &StagedLoad<'_>) -> Result<(), EngineError> {
        for module in &staged.modules {
            if let Some(existing) = self.registered_modules.get(&module.deployed_name) {
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

    fn commit(&mut self, staged: StagedLoad<'_>) -> LoadedWorkflow {
        for module in staged.modules {
            self.registered_modules
                .entry(module.deployed_name)
                .or_insert_with(|| staged.version.clone());
        }

        let record = LoadedWorkflow {
            workflow_type: staged.workflow_type.clone(),
            deployed_entry_module: staged.deployed_entry_module,
            entry_function: staged.entry_function,
            version: staged.version.clone(),
        };
        let key = (record.workflow_type.clone(), record.version.clone());
        let versions = self
            .by_type
            .entry(record.workflow_type.clone())
            .or_default();
        if !versions.contains(&record.version) {
            versions.push(record.version.clone());
        }
        self.by_version.entry(key).or_insert(record).clone()
    }
}

struct StagedLoad<'a> {
    workflow_type: String,
    deployed_entry_module: String,
    entry_function: String,
    version: ContentHash,
    modules: Vec<StagedModule<'a>>,
}

impl<'a> StagedLoad<'a> {
    fn new(package: &'a Package) -> Result<Self, EngineError> {
        let manifest = package.manifest();
        if package.beams().get(&manifest.entry_module).is_none() {
            return Err(load_error(format!(
                "manifest entry module `{}` is absent from package beams",
                manifest.entry_module
            )));
        }

        let version = package.content_hash().clone();
        let modules = package
            .deployed_modules()
            .into_iter()
            .map(|(deployed_name, bytes)| StagedModule {
                deployed_name,
                bytes,
            })
            .collect();

        Ok(Self {
            workflow_type: manifest.entry_module.clone(),
            deployed_entry_module: package.deployed_entry_module(),
            entry_function: manifest.entry_function.clone(),
            version,
            modules,
        })
    }
}

struct StagedModule<'a> {
    deployed_name: String,
    bytes: &'a [u8],
}

fn load_error(reason: String) -> EngineError {
    EngineError::Load { reason }
}

fn rollback_registered<R>(rollback: &mut R, registered_now: &[String]) -> String
where
    R: FnMut(&str) -> Result<(), EngineError>,
{
    let mut errors = Vec::new();
    for deployed_name in registered_now.iter().rev() {
        if let Err(error) = rollback(deployed_name) {
            errors.push(format!("{deployed_name}: {error}"));
        }
    }

    if errors.is_empty() {
        String::new()
    } else {
        format!("; rollback failed for {}", errors.join(", "))
    }
}

/// Load a package into the runtime and return a single-entry loaded collection.
///
/// # Errors
///
/// Returns [`EngineError::Load`] for loader validation failures, or a typed
/// runtime-derived load error if module registration fails.
pub fn load_package(
    runtime: &RuntimeHandle,
    package: &Package,
) -> Result<LoadedWorkflows, EngineError> {
    let mut loaded = LoadedWorkflows::new();
    loaded.load_package(runtime, package)?;
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap, time::Duration};

    use aion_package::{
        content_hash, deployed_name, parse_deployed_name, BeamModule, BeamSet, DeclaredActivity,
        Manifest, ManifestVersion, Package, PackageBuilder, PackageError, CURRENT_FORMAT_VERSION,
    };
    use serde_json::json;

    use super::LoadedWorkflows;
    use crate::runtime::{RuntimeConfig, RuntimeHandle, RuntimeInput};
    use crate::EngineError;

    fn manifest(entry_module: &str) -> Manifest {
        Manifest {
            entry_module: entry_module.to_owned(),
            entry_function: "run".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            timeout: Duration::from_secs(30),
            activities: vec![DeclaredActivity {
                activity_type: "activity/send".to_owned(),
            }],
            version: ManifestVersion::new("placeholder"),
            format_version: CURRENT_FORMAT_VERSION,
        }
    }

    fn package(entry_module: &str, entry_bytes: Vec<u8>) -> Result<Package, PackageError> {
        let beams = BeamSet::new(vec![
            BeamModule::new("workflow/support", vec![4, 5, 6]),
            BeamModule::new(entry_module, entry_bytes),
        ])?;
        let bytes = PackageBuilder::with_source(
            manifest(entry_module),
            beams,
            BTreeMap::<String, Vec<u8>>::new(),
        )
        .write_to_bytes()?;
        Package::load_from_bytes(bytes)
    }

    fn entry_only_package(entry_module: &str, bytes: Vec<u8>) -> Result<Package, PackageError> {
        let beams = BeamSet::new(vec![BeamModule::new(entry_module, bytes)])?;
        let archive = PackageBuilder::new(manifest(entry_module), beams).write_to_bytes()?;
        Package::load_from_bytes(archive)
    }

    fn fixture_workflow_beam() -> &'static [u8] {
        include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam")
    }

    fn fixture_workflow_package() -> Result<Package, PackageError> {
        let mut manifest = manifest("aion_fixture_workflow");
        manifest.entry_function = "complete".to_owned();
        let beams = BeamSet::new(vec![BeamModule::new(
            "aion_fixture_workflow",
            fixture_workflow_beam().to_vec(),
        )])?;
        let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
        Package::load_from_bytes(archive)
    }

    #[test]
    fn registers_every_package_derived_deployed_module() -> Result<(), Box<dyn std::error::Error>> {
        let package = package("workflow/order", vec![1, 2, 3])?;
        let registered = RefCell::new(Vec::<String>::new());
        let mut loaded = LoadedWorkflows::new();

        let record = loaded.load_with_rollback(
            &package,
            |deployed_name, _bytes| {
                registered.borrow_mut().push(deployed_name.to_owned());
                Ok(())
            },
            |_deployed_name| Ok(()),
        )?;

        let registered = registered.into_inner();
        let expected: Vec<String> = package
            .deployed_modules()
            .into_iter()
            .map(|(name, _bytes)| name)
            .collect();
        assert_eq!(registered, expected);
        for deployed_name in registered {
            let parsed = parse_deployed_name(&deployed_name)?;
            assert_eq!(parsed.hash(), package.content_hash());
            assert!(package.beams().get(parsed.logical()).is_some());
            assert!(loaded.has_registered_module(&deployed_name));
        }
        assert_eq!(record.workflow_type(), "workflow/order");
        Ok(())
    }

    #[test]
    fn records_deployed_entry_function_and_version() -> Result<(), Box<dyn std::error::Error>> {
        let package = package("workflow/order", vec![1, 2, 3])?;
        let mut loaded = LoadedWorkflows::new();

        let record = loaded.load_with_rollback(
            &package,
            |_deployed_name, _bytes| Ok(()),
            |_deployed_name| Ok(()),
        )?;

        assert_eq!(record.workflow_type(), package.manifest().entry_module);
        assert_eq!(
            record.deployed_entry_module(),
            deployed_name(&package.manifest().entry_module, package.content_hash())
        );
        assert_eq!(record.entry_function(), package.manifest().entry_function);
        assert_eq!(record.version(), package.content_hash());
        assert_eq!(loaded.latest("workflow/order"), Some(&record));
        assert_eq!(
            loaded.get("workflow/order", package.content_hash()),
            Some(&record)
        );
        Ok(())
    }

    #[test]
    fn retains_two_versions_for_same_workflow_type() -> Result<(), Box<dyn std::error::Error>> {
        let first = package("workflow/order", vec![1, 2, 3])?;
        let second = package("workflow/order", vec![1, 2, 4])?;
        let mut loaded = LoadedWorkflows::new();

        let first_record = loaded.load_with_rollback(
            &first,
            |_deployed_name, _bytes| Ok(()),
            |_deployed_name| Ok(()),
        )?;
        let second_record = loaded.load_with_rollback(
            &second,
            |_deployed_name, _bytes| Ok(()),
            |_deployed_name| Ok(()),
        )?;

        assert_ne!(first.content_hash(), second.content_hash());
        assert_ne!(
            first_record.deployed_entry_module(),
            second_record.deployed_entry_module()
        );
        assert!(loaded.has_registered_module(first_record.deployed_entry_module()));
        assert!(loaded.has_registered_module(second_record.deployed_entry_module()));
        assert_eq!(
            loaded.get("workflow/order", first.content_hash()),
            Some(&first_record)
        );
        assert_eq!(
            loaded.get("workflow/order", second.content_hash()),
            Some(&second_record)
        );
        assert_eq!(loaded.iter().count(), 2);
        Ok(())
    }

    #[test]
    fn package_loaded_under_content_hash_namespace_spawns_entrypoint(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let package = fixture_workflow_package()?;
        let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
        let mut loaded = LoadedWorkflows::new();

        let record = loaded.load_package(&runtime, &package)?;
        let pid = runtime.spawn_workflow(
            record.deployed_entry_module(),
            record.entry_function(),
            RuntimeInput::default(),
        )?;
        let (reason, result) = runtime.run_until_exit_for_test(pid);

        assert_eq!(reason, beamr::process::ExitReason::Normal);
        assert_eq!(result, beamr::term::Term::atom(beamr::atom::Atom::OK));
        runtime.shutdown()?;
        Ok(())
    }

    #[test]
    fn missing_entry_module_returns_load_error() -> Result<(), Box<dyn std::error::Error>> {
        let package = package("workflow/order", vec![1, 2, 3])?;
        let missing = package_with_missing_entry(&package, "workflow/missing");
        let mut loaded = LoadedWorkflows::new();

        let result = loaded.load_with_rollback(
            &missing,
            |_deployed_name, _bytes| Ok(()),
            |_deployed_name| Ok(()),
        );

        assert!(
            matches!(&result, Err(EngineError::Load { reason }) if reason.contains("workflow/missing")),
            "missing entry should fail with EngineError::Load"
        );
        assert_eq!(loaded.iter().count(), 0);
        assert!(!loaded.has_registered_module(&missing.deployed_entry_module()));
        Ok(())
    }

    #[test]
    fn collision_from_different_hash_fails_before_registration(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let first = entry_only_package("workflow/order", vec![1, 2, 3])?;
        let second = entry_only_package("workflow/order", vec![1, 2, 4])?;
        let colliding_name = first.deployed_entry_module();
        let calls = RefCell::new(0_usize);
        let mut loaded = LoadedWorkflows::new();
        loaded.note_registered_module(colliding_name.clone(), second.content_hash().clone())?;

        let result = loaded.load_with_rollback(
            &first,
            |_deployed_name, _bytes| {
                *calls.borrow_mut() += 1;
                Ok(())
            },
            |_deployed_name| Ok(()),
        );

        let expected_hash = first.content_hash().to_string();
        assert!(
            matches!(&result, Err(EngineError::Load { reason }) if reason.contains(&colliding_name) && reason.contains(&expected_hash)),
            "different hash collision should fail with EngineError::Load"
        );
        assert_eq!(*calls.borrow(), 0);
        assert_eq!(loaded.iter().count(), 0);
        Ok(())
    }

    #[test]
    fn identical_reload_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let package = package("workflow/order", vec![1, 2, 3])?;
        let calls = RefCell::new(0_usize);
        let mut loaded = LoadedWorkflows::new();

        let first = loaded.load_with_rollback(
            &package,
            |_deployed_name, _bytes| {
                *calls.borrow_mut() += 1;
                Ok(())
            },
            |_deployed_name| Ok(()),
        )?;
        let after_first = *calls.borrow();
        let second = loaded.load_with_rollback(
            &package,
            |_deployed_name, _bytes| {
                *calls.borrow_mut() += 1;
                Ok(())
            },
            |_deployed_name| Ok(()),
        )?;

        assert_eq!(first, second);
        assert_eq!(*calls.borrow(), after_first);
        assert_eq!(loaded.iter().count(), 1);
        Ok(())
    }

    #[test]
    fn runtime_failure_does_not_commit_loader_state() -> Result<(), Box<dyn std::error::Error>> {
        let package = package("workflow/order", vec![1, 2, 3])?;
        let mut loaded = LoadedWorkflows::new();

        let result = loaded.load_with_rollback(
            &package,
            |_deployed_name, _bytes| {
                Err(EngineError::Runtime {
                    reason: "boom".to_owned(),
                })
            },
            |_deployed_name| Ok(()),
        );

        assert!(
            matches!(&result, Err(EngineError::Load { reason }) if reason.contains("boom")),
            "runtime failure should fail load with EngineError::Load"
        );
        assert_eq!(loaded.iter().count(), 0);
        for (deployed_name, _bytes) in package.deployed_modules() {
            assert!(!loaded.has_registered_module(&deployed_name));
        }
        Ok(())
    }

    fn package_with_missing_entry(original: &Package, missing_entry: &str) -> Package {
        let mut manifest = original.manifest().clone();
        manifest.entry_module = missing_entry.to_owned();
        Package::from_validated_parts_for_test(
            manifest,
            original.beams().clone(),
            BTreeMap::new(),
            original.content_hash().clone(),
        )
    }

    #[test]
    fn content_hash_fixture_changes_when_bytes_change() -> Result<(), PackageError> {
        let first = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 3])])?;
        let second = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 4])])?;
        assert_ne!(content_hash(&first), content_hash(&second));
        Ok(())
    }
}
