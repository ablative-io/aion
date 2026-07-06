//! The norn `--output-schema` documents for the four driven agent activities,
//! embedded at compile time with `include_str!`.
//!
//! The files under `../../schemas/` are the single source of truth — never
//! re-typed here, and the Gleam output codecs in `codecs.gleam` decode exactly
//! these shapes. Each is passed INLINE to `norn --output-schema` (Norn treats a
//! value starting with `{` as inline JSON) so there is no schema file to resolve
//! in the agent's workspace at runtime.

/// `schemas/scout_output.json`: the scout agent's grounding findings.
pub const SCOUT_OUTPUT: &str = include_str!("../../schemas/scout_output.json");

/// `schemas/stack_plan.json`: the plan agent's dependency-ordered stack.
pub const STACK_PLAN: &str = include_str!("../../schemas/stack_plan.json");

/// `schemas/dev_output.json`: one dev round's report.
pub const DEV_OUTPUT: &str = include_str!("../../schemas/dev_output.json");

/// `schemas/review_output.json`: the reviewer's adversarial verdict.
pub const REVIEW_OUTPUT: &str = include_str!("../../schemas/review_output.json");
