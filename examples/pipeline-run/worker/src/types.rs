//! Wire types for the pipeline-run SHELL activity payloads.
//!
//! Every type here serializes/deserializes byte-compatibly with the Gleam
//! codecs in `../../src/pipeline_run/codecs.gleam` — those codecs are the
//! authoritative contract (field names in `snake_case`, emitted in declaration
//! order). The four DRIVEN AGENT activities need no wire types here: their
//! input is the prompt string and their output is produced by Norn against the
//! embedded `--output-schema`, decoded by the workflow's Gleam output codec.

use serde::{Deserialize, Serialize};

/// Input to `provision_workspace` (`codecs.provision_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionInput {
    /// The repository the worktree is created from.
    pub repo_root: String,
    /// The branch the unit branch is created on top of (a prior stratum's
    /// landed branch, or the integration base for a root unit).
    pub base_branch: String,
    /// The branch to create and check out for this unit.
    pub unit_branch: String,
    /// The absolute path the worktree is created at — `<base>/<child id>`,
    /// matching the `--workspace-root` the dev/review harnesses give Norn.
    pub workspace_path: String,
}

/// Result of `provision_workspace` (`codecs.workspace_info_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// The worktree path the unit's activities run in.
    pub workspace_path: String,
    /// The branch checked out there.
    pub branch: String,
}

/// Input to `gate` (`codecs.gate_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInput {
    /// The workspace the cargo gate runs in.
    pub workspace_path: String,
}

/// Result of `gate` (`codecs.gate_outcome_codec`). `pass` is true only when both
/// `cargo clippy -D warnings` and `cargo test` exit zero; a non-zero cargo exit
/// is recorded pass/fail DATA here, never an activity error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOutcome {
    /// Whether the gate passed.
    pub pass: bool,
    /// Combined captured output on fail; empty on pass.
    pub diagnostics: String,
}

/// One unit branch to land, in dependency order (`codecs.land_unit_*`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandUnit {
    /// The unit's id.
    pub unit_id: String,
    /// The unit's branch.
    pub branch: String,
}

/// Input to `land` (`codecs.land_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandInput {
    /// The repository the merges run in.
    pub repo_root: String,
    /// The branch the integration branch is created from if it does not exist.
    pub base_branch: String,
    /// The branch the unit branches merge onto, in order.
    pub integration_branch: String,
    /// The unit branches to merge, in dependency order.
    pub units: Vec<LandUnit>,
}

/// Result of `land` (`codecs.land_outcome_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandOutcome {
    /// The unit ids that merged, in order.
    pub landed: Vec<String>,
    /// The integration branch they landed on.
    pub integration_branch: String,
    /// A human-readable detail of what happened (per-branch merge status).
    pub detail: String,
}

/// Input to `notify` (`codecs.notify_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyInput {
    /// The brief id; the notification subject is `<brief_id> pipeline complete`.
    pub brief_id: String,
    /// The run's verdict summary.
    pub summary: String,
}

/// Result of `notify` (`codecs.notify_outcome_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyOutcome {
    /// Whether a notification was sent (best-effort; a log line always happens).
    pub sent: bool,
    /// A human-readable detail (which channel, or why it was log-only).
    pub detail: String,
}
