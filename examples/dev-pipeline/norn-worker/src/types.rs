//! Wire types for the dev-pipeline SHELL-path activities (implement-and-gate:
//! workspace provisioning, the implementer rounds, gates, teardown),
//! byte-compatible with the Gleam codecs in
//! `../src/dev_pipeline/codecs.gleam` and shaped by the package's `schemas/`
//! copies of the prospekt doctrine schemas.
//!
//! The brief-forge stage reports (`ScoutReport`/`Brief`/`Refutation`) are no
//! longer mirrored here: `scout`/`design`/`refute` route through the
//! driven-mode agent harness in `main.rs`, whose terminal stop envelope
//! carries the schema-validated report object straight through to the Gleam
//! codecs — the worker never decodes those shapes anymore, and a Rust mirror
//! nobody decodes would only invite drift.
//!
//! Optional scalar schema fields are `Option` values that serialize by
//! omission (never `null`); optional array schema fields default to empty on
//! deserialize and always serialize — the same convention the Gleam side
//! documents in `dev_pipeline/types`.

use serde::{Deserialize, Serialize};

// --- implement-and-gate: activity inputs/outputs ------------------------------

/// Isolated-workspace mode. A shared-checkout run is not a legal value, on
/// purpose — concurrent runs on one checkout collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// `git worktree add` off the source repository.
    Worktree,
    /// `git clone` of the source repository.
    Clone,
}

/// Input to `provision_workspace`: create an isolated worktree/clone of
/// `repo_root` at `base_ref` under the scratch path
/// `<repo_root>/.dev-pipeline-workspaces/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionInput {
    /// Absolute path of the source repository.
    pub repo_root: String,
    /// Ref the workspace is created at.
    pub base_ref: String,
    /// Worktree or clone.
    pub isolation: Isolation,
    /// Names the workspace deterministically (`dev-pipeline-<task_ref>`).
    pub task_ref: String,
}

/// The provisioned isolated workspace every downstream step runs inside.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Absolute workspace path.
    pub path: String,
}

/// One implementer round (initial or resume): the worker shells `norn
/// --print` INSIDE `workspace_path` with this session id and prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplementRound {
    /// Absolute workspace path the norn session runs in and is confined to.
    pub workspace_path: String,
    /// Deterministic session id (`<task_ref>-implement`), created or
    /// resumed via `--resume-if-exists` — fix rounds keep the session.
    pub session_id: String,
    /// The full projected prompt (initial: profile + brief verbatim; resume:
    /// the failing gate's captured output).
    pub prompt: String,
    /// Invocation-level model override from the workflow input (the frontier
    /// escape hatch); the worker pins its pilot model when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Input to `run_gate`: shell exactly `command` in `workspace_path`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRun {
    /// Absolute workspace path the command runs in.
    pub workspace_path: String,
    /// Stable gate id within the run (fmt / clippy / test / ...).
    pub gate_id: String,
    /// The exact command whose OWN exit status judges the gate.
    pub command: String,
}

/// One completed gate command, the stacked-dev `CliRun` pattern: a non-zero
/// `exit_status` is recorded DATA the workflow routes to the fix loop, never
/// an activity error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCliRun {
    /// The command's own exit status (`128 + signal` when signal-killed).
    pub exit_status: i32,
    /// Combined stdout+stderr, tail-bounded at capture — the durable record
    /// a fix round is handed, never a paraphrase.
    pub output: String,
    /// Wall-clock duration of the command.
    pub duration_ms: u64,
}

/// Input to `teardown_workspace` (a declared seam; the workflow deliberately
/// never dispatches it — both termini preserve the workspace).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeardownInput {
    /// The source repository the workspace was provisioned from.
    pub repo_root: String,
    /// The workspace to reclaim.
    pub workspace_path: String,
}

/// `teardown_workspace`'s best-effort receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TornDown {
    /// Whether the workspace directory is gone.
    pub cleaned: bool,
}

// --- implementation report (schemas/implementation-report.schema.json) --------

/// One changed file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Repo-relative path.
    pub path: String,
    /// One line: what changed here and why.
    pub change: String,
}

/// Mapping from one brief acceptance gate to the work discharging it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateAddressed {
    /// Acceptance-gate id from the brief (G1, G2...).
    pub gate_id: String,
    /// The test/change that discharges it, by name.
    pub how: String,
}

/// One declared departure from the brief — an undeclared deviation found in
/// review is a defect regardless of whether the code is right.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportDeviation {
    /// What the brief specified.
    pub from: String,
    /// What was done instead.
    pub to: String,
    /// Why.
    pub why: String,
}

/// The implementer's structured return. Note what is NOT here: gate results
/// — gates are command activities with their own recorded exit statuses; the
/// implementer never certifies them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplementationReport {
    /// The brief this report discharges.
    pub brief_ref: String,
    /// What was built, in complete sentences a reviewer can orient from.
    pub summary: String,
    /// Every changed file with its one-line why.
    pub files_changed: Vec<FileChange>,
    /// Gate → discharging work, by name.
    pub gates_addressed: Vec<GateAddressed>,
    /// Every departure from the brief, however small.
    pub deviations: Vec<ReportDeviation>,
    /// Test names added, each asserting an outcome.
    #[serde(default)]
    pub new_tests: Vec<String>,
    /// What the implementer is unsure about — the reviewer reads this FIRST.
    pub concerns: Vec<String>,
    /// What was not covered.
    pub not_covered: Vec<String>,
}
