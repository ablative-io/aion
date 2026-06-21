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

mod activity_golden;
mod activity_model;
mod activity_project;
mod activity_worker_python;
mod activity_worker_rust;
mod activity_wrappers;
mod declaration;
mod emit;
mod error;
mod input_skeleton;
mod json;
mod names;
mod project;
mod schema;
mod test_scaffold;

pub use activity_project::{
    ActivityArtifact, ActivityReport, CodecReport, TestScaffoldReport, generate_activities,
    generate_codecs, generate_test_scaffold,
};
pub use declaration::{ActivityDeclaration, Tier, parse_declarations};
pub use error::CodegenError;
pub use input_skeleton::build_input_skeleton;
pub use project::{CodegenMode, CodegenReport, codegen_project};
