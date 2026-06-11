//! Project-level packaging: `workflow.toml` descriptors → built `.aion` archives.
//!
//! [`package_project`] consumes an already-built Gleam workflow project: it
//! parses the project's `workflow.toml`, discovers the production-dependency
//! closure of compiled modules under `build/dev/erlang`, and writes one
//! deterministic, verify-after-write `.aion` archive per declared workflow.
//! The library never spawns processes and never writes to stdout or stderr;
//! everything observable is in the returned [`ProjectReport`] or
//! [`PackagingError`].

mod assemble;
mod config;
mod discover;
mod error;

#[cfg(test)]
mod fixture;

pub use assemble::{
    ExcludedModule, ExcludedReason, PackageOptions, PackagedWorkflow, ProjectReport,
    package_project,
};
pub use error::PackagingError;
