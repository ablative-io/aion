//! The `.aion` package format: archive validation, content-hash versioning, and module namespacing.

pub mod beam;
pub mod builder;
pub mod error;
pub mod hash;
pub mod manifest;
pub mod namespace;
pub mod package;
pub mod version;

pub use beam::{BeamModule, BeamSet};
pub use builder::PackageBuilder;
pub use error::PackageError;
pub use hash::{ContentHash, content_hash};
pub use manifest::{CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion};
pub use namespace::{
    DEPLOYED_NAME_SEPARATOR, NamespaceError, ParsedDeployedName, deployed_name, deployed_names,
    parse_deployed_name,
};
pub use package::Package;
pub use version::WorkflowVersion;
