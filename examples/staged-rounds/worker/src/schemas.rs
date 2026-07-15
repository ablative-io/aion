//! The norn `--output-schema` documents for the four driven agent roles,
//! embedded at compile time with `include_str!`.
//!
//! The files under `../schemas/` are this package's own contract; the
//! shell handlers' serde types in [`crate::types`] decode exactly these
//! shapes. Each is passed INLINE to `norn --output-schema` (Norn treats a
//! value starting with `{` as inline JSON) so there is no schema file to
//! resolve in the agent's workspace at runtime.

/// `schemas/plan.schema.json`: the planner's phased plan.
pub const PLAN: &str = include_str!("../schemas/plan.schema.json");

/// `schemas/item-report.schema.json`: the dev agent's round report.
pub const ITEM_REPORT: &str = include_str!("../schemas/item-report.schema.json");

/// `schemas/item-verdict.schema.json`: one item's review verdict.
pub const ITEM_VERDICT: &str = include_str!("../schemas/item-verdict.schema.json");

/// `schemas/remediation.schema.json`: the remediator's resolution report.
pub const REMEDIATION: &str = include_str!("../schemas/remediation.schema.json");
