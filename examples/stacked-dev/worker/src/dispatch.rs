//! Dispatch wire types (BD-006).
//!
//! These serde types mirror the hand-written Gleam dispatch codecs in
//! `../../src/stacked_dev/codecs_dispatch.gleam` byte for byte (field names,
//! order, and the internally-tagged `outcome`/`error` unions). The
//! `assemble_wave` handler that produces an [`AssembledWave`] lives in
//! [`crate::assemble`]. `tests/wire_compat.rs` pins each shape against the
//! Gleam codec source.

use serde::{Deserialize, Serialize};

use crate::types::{Isolation, Placement};

// --- dispatch IO (mirrors codecs_dispatch) -----------------------------------

/// Input to the `dispatch` workflow (`codecs_dispatch.dispatch_input_codec`).
/// Every field is required (ADR-001); there is deliberately no
/// concurrency-limit field (serial-only delivery).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchInput {
    /// Directory holding the ledgers and cluster documents.
    pub design_dir: String,
    /// The wave as an ordered list of brief ids.
    pub wave: Vec<String>,
    /// The repository the worktrees are provisioned from.
    pub repo_root: String,
    /// Ref the provisioned branches are added under.
    pub base_ref: String,
    /// Member names or UUIDs to request review from.
    pub reviewers: Vec<String>,
    /// Where each child workspace runs.
    pub placement: Placement,
    /// How each child workspace is isolated.
    pub isolation: Isolation,
    /// The verify-fix loop cap.
    pub verify_fix_cap: i64,
    /// The review loop cap.
    pub review_cap: i64,
    /// The durable backoff between rounds.
    pub round_backoff_ms: i64,
    /// The durable review deadline.
    pub review_deadline_ms: i64,
    /// Whether a child failure halts the rest of the wave.
    pub halt_on_failure: bool,
}

/// One brief's outcome (`codecs_dispatch` `brief_outcome_to_json`): a tagged
/// object whose `outcome` is exactly `landed`, `failed`, or `skipped`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome")]
pub enum BriefOutcome {
    /// The brief's child landed.
    #[serde(rename = "landed")]
    Landed {
        /// The brief id.
        brief_id: String,
        /// The merged branch.
        branch: String,
        /// The tree parent it merged into.
        merged_into: String,
    },
    /// The brief's child failed with a typed error, carried verbatim.
    #[serde(rename = "failed")]
    Failed {
        /// The brief id.
        brief_id: String,
        /// The child's typed `stacked_dev` error.
        error: StackedDevError,
    },
    /// The brief was never started because an earlier failure halted the wave.
    #[serde(rename = "skipped")]
    Skipped {
        /// The brief id.
        brief_id: String,
        /// The failed brief that halted the wave.
        after: String,
    },
}

/// Output of the `dispatch` workflow
/// (`codecs_dispatch.dispatch_result_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchResult {
    /// One outcome per wave entry, in wave order.
    pub outcomes: Vec<BriefOutcome>,
}

/// The `dispatch` workflow's typed error
/// (`codecs_dispatch.dispatch_error_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum DispatchError {
    /// `assemble_wave` refused or could not assemble the wave.
    AssemblyRefused {
        /// The diagnostic naming every offending brief and reason.
        message: String,
    },
    /// An engine-level failure spawning or awaiting a child.
    DispatchStageFailed {
        /// The stage that raised it.
        stage: String,
        /// The diagnostic.
        message: String,
    },
}

/// Live status answered by the `dispatch_status` query
/// (`codecs_dispatch.dispatch_status_codec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchStatus {
    /// The brief currently in flight.
    pub current_brief: String,
    /// Its 1-based position.
    pub position: i64,
    /// The wave total.
    pub total: i64,
    /// The per-brief outcomes recorded so far.
    pub outcomes: Vec<BriefOutcome>,
}

/// The child `stacked_dev` typed error, embedded under a failed outcome's
/// `error` (`codecs_workflows.stacked_dev_error_to_json`). The internally
/// tagged `error` strings match the Gleam encoder one for one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum StackedDevError {
    /// Workspace provisioning failed.
    ProvisionFailed {
        /// The diagnostic.
        message: String,
    },
    /// The child's scout stage failed.
    ScoutFailed {
        /// The diagnostic.
        message: String,
    },
    /// The child reported one or more requirements blocked.
    DevBlocked {
        /// The blocked R# ids.
        requirement_ids: Vec<String>,
    },
    /// The child failed outside the typed taxonomy.
    DevFailed {
        /// The diagnostic.
        message: String,
    },
    /// The child's verify-fix loop spent its budget.
    VerifyExhausted {
        /// Rounds spent.
        rounds: i64,
        /// The last diagnostics.
        diagnostics: String,
    },
    /// The child's review left requirements drifted.
    ReviewDrifted {
        /// Each drifted requirement and its issues.
        drifted: Vec<DriftedRequirement>,
    },
    /// The child's harden pass broke verification.
    HardenRegressed {
        /// The regression diagnostics.
        diagnostics: String,
    },
    /// The authoritative gate executed and failed.
    GateRejected {
        /// The gate report.
        report: String,
    },
    /// The reviewer rejected the work.
    ReviewRejected {
        /// The stated reason.
        reason: String,
    },
    /// No verdict arrived before the durable deadline.
    ReviewTimedOut {
        /// The deadline that expired.
        deadline_ms: i64,
    },
    /// The bounded review loop spent its budget.
    ReviewCapExhausted {
        /// Rounds spent.
        rounds: i64,
    },
    /// Landing failed.
    LandFailed {
        /// The diagnostic.
        message: String,
    },
    /// Any other stage failure.
    StageFailed {
        /// The stage that raised it.
        stage: String,
        /// The diagnostic.
        message: String,
    },
}

/// One review-found requirement left drifted
/// (`codecs_workflows` `drifted_requirement_to_json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriftedRequirement {
    /// The R# id.
    pub id: String,
    /// The issues the reviewer recorded.
    pub issues: Vec<String>,
}
