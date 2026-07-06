//! The norn `--output-schema` documents for the four driven agent roles,
//! embedded at compile time with `include_str!`.
//!
//! The files under `../../schemas/` are copies of yggdrasil's
//! `docs/design/remediation-flow/schemas/` set — yggdrasil's copies are the
//! source of truth until the schemas ship in a crate (see the example
//! README). They are never re-typed here, and the Gleam output codecs in
//! `codecs.gleam` decode exactly these shapes. Each is passed INLINE to
//! `norn --output-schema` (Norn treats a value starting with `{` as inline
//! JSON) so there is no schema file to resolve in the agent's workspace at
//! runtime.

/// `schemas/test-manifest.schema.json`: the test-author's manifest.
pub const TEST_MANIFEST: &str = include_str!("../../schemas/test-manifest.schema.json");

/// `schemas/fix-report.schema.json`: the developer's fix report.
pub const FIX_REPORT: &str = include_str!("../../schemas/fix-report.schema.json");

/// `schemas/verdict.schema.json`: the verifier's per-finding rulings.
pub const VERDICT: &str = include_str!("../../schemas/verdict.schema.json");

/// `schemas/re-audit-findings.schema.json`: the re-auditor's fresh finding
/// set (authored in this example — yggdrasil does not ship a findings schema
/// yet).
pub const RE_AUDIT_FINDINGS: &str = include_str!("../../schemas/re-audit-findings.schema.json");
