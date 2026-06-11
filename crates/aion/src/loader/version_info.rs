//! Serde-ready listing record for loaded workflow versions.

use aion_package::{ContentHash, ManifestVersion};
use chrono::{DateTime, Utc};

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
