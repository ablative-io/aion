//! The `--output-schema` text for the three norn-backed stages.
//!
//! Unlike stacked-dev (which pins byte-inlined string constants plus a
//! drift-check script), these are `include_str!` reads of the package's own
//! `schemas/` copies of the prospekt doctrine schemas — one source of truth
//! inside the package, so constant/file drift is impossible by construction.
//! Passing the text inline means there is no schema file to resolve in the
//! target workspace.

/// The scout-report stage contract (`schemas/scout-report.schema.json`).
pub const SCOUT_OUTPUT_SCHEMA: &str = include_str!("../../schemas/scout-report.schema.json");

/// The brief stage contract (`schemas/brief.schema.json`).
pub const BRIEF_OUTPUT_SCHEMA: &str = include_str!("../../schemas/brief.schema.json");

/// The refutation stage contract (`schemas/refutation.schema.json`).
pub const REFUTATION_OUTPUT_SCHEMA: &str = include_str!("../../schemas/refutation.schema.json");
