//! Gleam type + JSON codec generation from a workflow project's JSON
//! Schemas (`aion codegen`).
//!
//! [`codegen_project`] reads a project's `workflow.toml` and every
//! `schemas/*.json`, and writes one deterministic Gleam module
//! (`src/<package>_io.gleam`) containing a type plus an encoder/decoder
//! pair per schema — the schema files stay the single source of truth, and
//! the generated codecs cannot drift from them. [`CodegenMode::Check`]
//! verifies the on-disk module instead of writing, for CI gates. The
//! library never spawns processes; everything observable is in the returned
//! [`CodegenReport`] or [`CodegenError`].

mod emit;
mod error;
mod json;
mod names;
mod project;
mod schema;

pub use error::CodegenError;
pub use project::{CodegenMode, CodegenReport, codegen_project};
