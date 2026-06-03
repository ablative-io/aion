//! The `.aion` package format: archive validation, content-hash versioning, and module namespacing.

pub mod beam;
pub mod error;

pub use beam::{BeamModule, BeamSet};
pub use error::PackageError;
