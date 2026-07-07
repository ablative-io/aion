//! The norn `--output-schema` documents for the two driven agent roles,
//! embedded at compile time with `include_str!`.
//!
//! The files under `../../schemas/` are this package's own contract; the
//! Gleam output codecs in `codecs.gleam` decode exactly these shapes. Each is
//! passed INLINE to `norn --output-schema` (Norn treats a value starting with
//! `{` as inline JSON) so there is no schema file to resolve in the agent's
//! workspace at runtime.

/// `schemas/dev-report.schema.json`: the developer's round report.
pub const DEV_REPORT: &str = include_str!("../../schemas/dev-report.schema.json");

/// `schemas/lens-verdict.schema.json`: one adversarial lens's verdict.
pub const LENS_VERDICT: &str = include_str!("../../schemas/lens-verdict.schema.json");
