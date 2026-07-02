//! Types-first codec generation (`aion generate`).
//!
//! The authored source of truth is the project's Gleam types module
//! `src/<package>_io.gleam` (ADR-014, resolved types-first on 2026-07-02).
//! [`boundary_types_from_interface`] maps the `gleam export package-interface`
//! JSON — the CLI drives the toolchain; this library never spawns a process —
//! into the boundary-type model, from which every artifact is generated:
//! the codecs module ([`generate_codecs`]), the emitted `schemas/*.json`
//! ([`emit_schemas`]), and the declaration-driven activity plumbing
//! ([`generate_activities`], [`generate_test_scaffold`]).
//! [`CodegenMode::Check`] verifies the on-disk artifacts instead of writing,
//! for CI drift gates. Everything observable is in the returned reports or
//! [`CodegenError`].

mod activity_golden;
mod activity_model;
mod activity_project;
mod activity_worker_python;
mod activity_worker_rust;
mod activity_wrappers;
mod codec_module;
mod declaration;
mod error;
mod input_skeleton;
mod interface;
mod model;
mod names;
mod project;
mod schema_emit;
mod test_scaffold;

pub use activity_project::{
    ActivityArtifact, ActivityReport, CodecReport, TestScaffoldReport, generate_activities,
    generate_codecs, generate_test_scaffold,
};
pub use declaration::{ActivityDeclaration, Tier, parse_declarations};
pub use error::CodegenError;
pub use input_skeleton::build_input_skeleton;
pub use interface::boundary_types_from_interface;
pub use model::BoundaryType;
pub use project::CodegenMode;
pub use schema_emit::{SchemaEmitReport, emit_schemas};
