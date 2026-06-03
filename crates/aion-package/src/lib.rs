//! The `.aion` package format: archive validation, content-hash versioning, and module namespacing.

pub mod beam;
pub mod builder;
pub mod error;
pub mod hash;
pub mod manifest;

pub use beam::{BeamModule, BeamSet};
pub use builder::PackageBuilder;
pub use error::PackageError;
pub use hash::{ContentHash, content_hash};
pub use manifest::{CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion};
