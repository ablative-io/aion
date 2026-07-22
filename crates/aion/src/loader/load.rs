//! Package staging: validated load units shared by the workflow catalog.

use std::collections::HashSet;
use std::time::Duration;

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
    declared_timeout: Option<Duration>,
}

impl LoadedWorkflow {
    /// Assembles a loaded-workflow record from already-validated parts.
    ///
    /// `declared_timeout` is the entry's explicitly authored workflow timeout,
    /// or `None` when the package's content-hash identity does not commit to one
    /// (a legacy or defaulted manifest). It is the sole input the start path
    /// consults to decide whether to arm a deadline, so a non-declared entry can
    /// never arm.
    pub(crate) const fn from_parts(
        workflow_type: String,
        deployed_entry_module: String,
        entry_function: String,
        version: ContentHash,
        declared_timeout: Option<Duration>,
    ) -> Self {
        Self {
            workflow_type,
            deployed_entry_module,
            entry_function,
            version,
            declared_timeout,
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

    /// The entry's explicitly authored workflow timeout, or `None`.
    ///
    /// `Some` only when the package identity commits to a declared timeout; the
    /// start path arms a deadline exactly when this is `Some`, so a legacy or
    /// defaulted manifest — which resolves to `None` here — arms nothing.
    #[must_use]
    pub fn declared_timeout(&self) -> Option<Duration> {
        self.declared_timeout
    }
}

/// One workflow entry staged from a package manifest.
pub(crate) struct StagedWorkflow {
    pub(crate) workflow_type: String,
    pub(crate) deployed_entry_module: String,
    pub(crate) entry_function: String,
    pub(crate) declared_timeout: Option<Duration>,
}

/// One package validated and decomposed into deployable module units.
pub(crate) struct StagedLoad<'a> {
    pub(crate) workflows: Vec<StagedWorkflow>,
    pub(crate) manifest_version: ManifestVersion,
    pub(crate) manifest_digest: ManifestDigest,
    pub(crate) version: ContentHash,
    pub(crate) modules: Vec<StagedModule<'a>>,
}

impl<'a> StagedLoad<'a> {
    pub(crate) fn new(package: &'a Package) -> Result<Self, EngineError> {
        let manifest = package.manifest();
        let version = package.content_hash().clone();
        // Declaredness is a tamper-evident, package-level property of the
        // content-hash identity: when the package does not commit to a declared
        // timeout, EVERY entry's timeout is held non-arming (`None`), so a
        // legacy or defaulted manifest arms nothing regardless of what value its
        // `timeout` field happens to carry.
        let declared = package.has_declared_timeout();
        let mut seen = HashSet::new();
        let mut workflows = Vec::with_capacity(1 + manifest.additional_workflows.len());
        let entries = std::iter::once((
            manifest.entry_module.as_str(),
            manifest.entry_module.as_str(),
            manifest.entry_function.as_str(),
            manifest.timeout,
        ))
        .chain(manifest.additional_workflows.iter().map(|entry| {
            (
                entry.workflow_type.as_str(),
                entry.entry_module.as_str(),
                entry.entry_function.as_str(),
                entry.timeout,
            )
        }));
        for (workflow_type, entry_module, entry_function, entry_timeout) in entries {
            if !seen.insert(workflow_type) {
                return Err(load_error(format!(
                    "package declares workflow type `{workflow_type}` more than once"
                )));
            }
            if package.beams().get(entry_module).is_none() {
                return Err(load_error(format!(
                    "manifest entry module `{entry_module}` for workflow `{workflow_type}` is absent from package beams"
                )));
            }
            workflows.push(StagedWorkflow {
                workflow_type: workflow_type.to_owned(),
                deployed_entry_module: aion_package::deployed_name(entry_module, &version),
                entry_function: entry_function.to_owned(),
                declared_timeout: if declared { entry_timeout } else { None },
            });
        }
        let modules = package
            .deployed_modules()
            .into_iter()
            .map(|(deployed_name, bytes)| StagedModule {
                deployed_name,
                bytes,
            })
            .collect();

        Ok(Self {
            workflows,
            manifest_version: manifest.version.clone(),
            manifest_digest: manifest.canonical_digest()?,
            version,
            modules,
        })
    }

    /// Loaded-workflow records this package commits atomically.
    pub(crate) fn records(&self) -> Vec<LoadedWorkflow> {
        self.workflows
            .iter()
            .map(|entry| {
                LoadedWorkflow::from_parts(
                    entry.workflow_type.clone(),
                    entry.deployed_entry_module.clone(),
                    entry.entry_function.clone(),
                    self.version.clone(),
                    entry.declared_timeout,
                )
            })
            .collect()
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
