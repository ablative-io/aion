//! Wire types for the dev-brief SHELL activity payloads.
//!
//! Every type here serializes/deserializes byte-compatibly with the Gleam
//! codecs in `../../src/dev_brief/codecs.gleam` — those codecs are the
//! authoritative contract (field names in `snake_case`, emitted in
//! declaration order). The two DRIVEN AGENT activities need no wire types
//! here: their input is the structured context JSON the workflow encoded
//! (assembled into the prompt by [`crate::harness::ProfiledNornHarness`]) and
//! their output is produced by Norn against the embedded `--output-schema`,
//! decoded by the workflow's Gleam output codec.

use serde::{Deserialize, Serialize};

/// Input to `provision_workspace` (`codecs.provision_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionInput {
    /// The repository the worktree is created from.
    pub repo_root: String,
    /// The branch the brief branch is created on top of.
    pub base_branch: String,
    /// The branch to create and check out for this brief (`dev/<brief-id>`).
    pub branch: String,
    /// The absolute path the worktree is created at — `<base>/<workflow id>`,
    /// matching the `--workspace-root` the driven harness gives Norn.
    pub workspace_path: String,
}

/// Result of `provision_workspace` (`codecs.workspace_info_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// The worktree path the brief's activities run in.
    pub workspace_path: String,
    /// The branch checked out there.
    pub branch: String,
    /// The commit the worktree started from — the gate computes the
    /// developer's diff against it.
    pub base_commit: String,
}

/// One configured gate command (`codecs` `GateCommand`): run in the workspace
/// root, pass = exit 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCommand {
    /// The operator's name for the command (rides in diagnostics).
    pub name: String,
    /// The argv to execute (`argv[0]` is the executable).
    pub argv: Vec<String>,
}

/// Input to `run_gates` (`codecs.gate_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInput {
    /// The workspace the gate battery runs in.
    pub workspace_path: String,
    /// The provisioned base commit the reviewers' diff is taken against.
    pub base_commit: String,
    /// The brief's configured commands, run in order. An EMPTY list is the
    /// operator's explicit choice: a recorded vacuous pass, never silent.
    pub gates: Vec<GateCommand>,
}

/// One gate command's recorded run (`codecs` `GateCommandRun`). A red command
/// is recorded DATA, never an activity error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCommandRun {
    /// The command's configured name.
    pub name: String,
    /// The raw exit code (-1 when the process died without one).
    pub exit_code: i64,
    /// Whether the command exited zero.
    pub passed: bool,
    /// The captured (clipped) output — loop-back diagnostics and reviewer
    /// evidence.
    pub output_tail: String,
}

/// Result of `run_gates` (`codecs.gate_outcome_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOutcome {
    /// True only when every configured command exited zero (vacuously true
    /// for an empty battery — recorded in `diagnostics`).
    pub pass: bool,
    /// Per-command records, in execution order.
    pub runs: Vec<GateCommandRun>,
    /// The developer's full change since the base commit — what the review
    /// lenses read.
    pub diff: String,
    /// A human-readable account of the battery's verdict.
    pub diagnostics: String,
}

/// Input to `cleanup_workspace` (`codecs.cleanup_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupInput {
    /// The repository whose worktree registration is removed.
    pub repo_root: String,
    /// The worktree path to remove.
    pub workspace_path: String,
}

/// Result of `cleanup_workspace` (`codecs.cleanup_outcome_codec`). A dirty
/// worktree is refused (uncommitted work must never be destroyed) and
/// reported as `removed: false` with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupOutcome {
    /// Whether the worktree was removed.
    pub removed: bool,
    /// Why, when it was not (dirty, absent, or the removal itself failed).
    pub detail: String,
}
