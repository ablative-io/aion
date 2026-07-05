//! The `--output-schema` text for the norn-backed stages.
//!
//! Unlike stacked-dev (which pins byte-inlined string constants plus a
//! drift-check script), these are `include_str!` reads of the package's own
//! `schemas/` copies of the prospekt doctrine schemas — one source of truth
//! inside the package, so constant/file drift is impossible by construction.
//!
//! Two consumption paths:
//!
//! - the SHELL-path implementer rounds pass
//!   [`IMPLEMENTATION_REPORT_OUTPUT_SCHEMA`] inline (no schema file to
//!   resolve in the target workspace);
//! - the DRIVEN-path stages (`scout`/`design`/`refute`) need per-activity
//!   schemas, but the harness's spawn arguments are fixed per process with
//!   only a `{activity_type}` placeholder — so `main.rs` MATERIALIZES the
//!   three stage constants as activity-type-named files
//!   (`<schemas-dir>/scout.json`, `design.json`, `refute.json`) at startup
//!   and spawns norn with `--output-schema <schemas-dir>/{activity_type}.json`.
//!   The binary stays the single schema source either way.

/// The scout-report stage contract (`schemas/scout-report.schema.json`) —
/// materialized as `<schemas-dir>/scout.json` for the driven harness.
pub const SCOUT_OUTPUT_SCHEMA: &str = include_str!("../../schemas/scout-report.schema.json");

/// The brief stage contract (`schemas/brief.schema.json`) — materialized as
/// `<schemas-dir>/design.json` for the driven harness.
pub const BRIEF_OUTPUT_SCHEMA: &str = include_str!("../../schemas/brief.schema.json");

/// The refutation stage contract (`schemas/refutation.schema.json`) —
/// materialized as `<schemas-dir>/refute.json` for the driven harness.
pub const REFUTATION_OUTPUT_SCHEMA: &str = include_str!("../../schemas/refutation.schema.json");

/// The implementer stage contract
/// (`schemas/implementation-report.schema.json`), shared by `implement` and
/// `implement_resume` — passed INLINE on the shell path.
pub const IMPLEMENTATION_REPORT_OUTPUT_SCHEMA: &str =
    include_str!("../../schemas/implementation-report.schema.json");
