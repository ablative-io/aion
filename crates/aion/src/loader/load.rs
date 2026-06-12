//! Package staging: validated load units shared by the workflow catalog.

use aion_package::{ContentHash, ManifestDigest, ManifestVersion, Package};

use crate::error::EngineError;

/// Outcome of one package load, computed inside the catalog mutation lock.
///
/// `freshly_loaded` distinguishes a real registration from an idempotent
/// re-load of a resident hash; `route_changed` reports whether the call
/// re-pointed routing (false means the hash was already route-active and the
/// load was a full no-op). Both flags are race-free truth captured under the
/// same lock that committed the mutation, never a list-before/list-after read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadOutcome {
    /// The loaded (or already-resident) workflow record.
    pub record: LoadedWorkflow,
    /// True when this call registered the version; false on idempotent re-load.
    pub freshly_loaded: bool,
    /// True when this call re-pointed the type's route at the version.
    pub route_changed: bool,
}

/// Workflow package entrypoint registered in the embedded runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedWorkflow {
    workflow_type: String,
    deployed_entry_module: String,
    entry_function: String,
    version: ContentHash,
}

impl LoadedWorkflow {
    /// Assembles a loaded-workflow record from already-validated parts.
    pub(crate) const fn from_parts(
        workflow_type: String,
        deployed_entry_module: String,
        entry_function: String,
        version: ContentHash,
    ) -> Self {
        Self {
            workflow_type,
            deployed_entry_module,
            entry_function,
            version,
        }
    }

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

/// One package validated and decomposed into deployable module units.
pub(crate) struct StagedLoad<'a> {
    pub(crate) workflow_type: String,
    pub(crate) deployed_entry_module: String,
    pub(crate) entry_function: String,
    pub(crate) manifest_version: ManifestVersion,
    pub(crate) manifest_digest: ManifestDigest,
    pub(crate) version: ContentHash,
    pub(crate) modules: Vec<StagedModule<'a>>,
}

impl<'a> StagedLoad<'a> {
    pub(crate) fn new(package: &'a Package) -> Result<Self, EngineError> {
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
            manifest_version: manifest.version.clone(),
            manifest_digest: manifest.canonical_digest()?,
            version,
            modules,
        })
    }

    /// Loaded-workflow record this staged unit commits as.
    pub(crate) fn record(&self) -> LoadedWorkflow {
        LoadedWorkflow::from_parts(
            self.workflow_type.clone(),
            self.deployed_entry_module.clone(),
            self.entry_function.clone(),
            self.version.clone(),
        )
    }
}

/// One deployable module of a staged package.
pub(crate) struct StagedModule<'a> {
    pub(crate) deployed_name: String,
    pub(crate) bytes: &'a [u8],
}

pub(crate) fn load_error(reason: String) -> EngineError {
    EngineError::Load { reason }
}

/// Best-effort rollback of modules registered before a failed load step.
///
/// Returns a human-readable suffix describing rollback failures, empty when
/// every registration was unwound cleanly.
pub(crate) fn rollback_registered<R>(rollback: &mut R, registered_now: &[String]) -> String
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
