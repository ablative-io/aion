//! Archive validation, content hashing, and namespacing for Aion packages.
//!
//! This crate loads `.aion` archives, validates manifests and BEAM entries,
//! computes stable content hashes, and derives deployed module names for engine
//! registration.
//!
//! # Example
//!
//! ```no_run
//! use aion_package::Package;
//!
//! let package = Package::load_from_path("workflow.aion")?;
//! println!("entry module: {}", package.manifest().entry_module);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

/// Compiled BEAM module records extracted from packages.
pub mod beam;
/// Archive builder utilities for tests and packaging tools.
pub mod builder;
/// Package validation and archive-loading errors.
pub mod error;
/// Stable content-hash calculation for package contents.
pub mod hash;
/// Manifest structures and format-version constants.
pub mod manifest;
/// Deployment namespace helpers for compiled module names.
pub mod namespace;
/// Validated in-memory package loading and accessors.
pub mod package;
/// Workflow version identifiers derived from package content.
pub mod version;

pub use beam::{BeamModule, BeamSet, RESERVED_MODULE_NAMES};
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
