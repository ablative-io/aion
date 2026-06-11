//! Workflow package loading surfaces.

use aion_core::PackageVersion;
use aion_package::ContentHash;

use crate::error::EngineError;

/// Shared, atomically-swappable workflow package catalog.
pub mod catalog;
/// Package staging and workflow entry discovery.
pub mod load;
/// Serde-ready listing record for loaded workflow versions.
pub mod version_info;

pub use catalog::{PinnedWorkflow, WorkflowCatalog};
pub use load::LoadedWorkflow;
pub use version_info::WorkflowVersionInfo;

/// Canonical durable form of a loaded package version.
#[must_use]
pub fn package_version_of(hash: &ContentHash) -> PackageVersion {
    PackageVersion::new(hash.to_string())
}

/// Parses a durably recorded package version back to a typed content hash.
///
/// # Errors
///
/// Returns [`EngineError::Load`] naming the workflow type and the malformed
/// version text when the recorded value is not a canonical content hash.
pub fn parse_package_version(
    workflow_type: &str,
    version: &PackageVersion,
) -> Result<ContentHash, EngineError> {
    version
        .as_str()
        .parse::<ContentHash>()
        .map_err(|error| EngineError::Load {
            reason: format!(
                "workflow `{workflow_type}` recorded package version `{version}` that is not a canonical content hash: {error}"
            ),
        })
}
