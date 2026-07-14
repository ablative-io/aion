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

/// Review-finding severity (`codecs.severity_codec`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Evidence that mechanically rejects the review round.
    Blocking,
    /// Recorded evidence that does not itself reject the round.
    Advisory,
}

/// A lens's asserted overall (`codecs.overall_codec`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    /// The lens asserts acceptance.
    Accept,
    /// The lens asserts rejection.
    Reject,
}

/// One concrete finding in a [`LensVerdict`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    /// Whether this finding is blocking or advisory.
    pub severity: Severity,
    /// The concise finding title used in formatted adverse evidence.
    pub title: String,
    /// The detailed file/line or command evidence supplied by the lens.
    pub evidence: String,
}

/// One review lens's collected verdict (`codecs.lens_verdict_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensVerdict {
    /// The configured lens name.
    pub lens: String,
    /// The lens's concrete findings.
    pub findings: Vec<ReviewFinding>,
    /// The lens's asserted overall.
    pub overall: Overall,
    /// Its rejection reason. Both an omitted AWL optional and a Gleam `null`
    /// deserialize to `None`.
    #[serde(default)]
    pub reject_reason: Option<String>,
}

/// Named-argument input to `format_verdict_evidence`.
///
/// The activity boundary deliberately accepts a record, never a bare verdict
/// list, matching AWL named-argument invocation semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatVerdictEvidenceInput {
    /// All collected lens verdicts for one review round, in lens order.
    pub verdicts: Vec<LensVerdict>,
}

/// Record result from `format_verdict_evidence`.
///
/// AWL consumes this as a record field rather than a bare string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatVerdictEvidenceOutput {
    /// The Gleam-compatible adverse evidence string. It is empty when the
    /// round has no adverse verdict evidence.
    pub evidence: String,
}

/// Named-argument input to `fold_round`.
///
/// This pure boundary packs one review round into the single value carried by
/// an AWL loop. It performs no shell I/O and owns no workflow disposition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldRoundInput {
    /// The developer report for this round, retained as raw JSON so optional
    /// field presence and extension fields survive the loop boundary.
    pub report: serde_json::Value,
    /// The mechanical gate outcome for this round, retained as raw JSON for a
    /// value-identical echo.
    pub gate: serde_json::Value,
    /// The collected review-lens verdicts in lens order. Raw values are echoed
    /// unchanged; the handler separately decodes typed views for formatting.
    pub verdicts: Vec<serde_json::Value>,
    /// Evidence carried from earlier rounds.
    pub prior_evidence: String,
    /// Workspace droppings observed while resetting after this round.
    pub droppings: Vec<String>,
}

/// Record result from `fold_round`.
///
/// The round values are echoed unchanged alongside the smart-joined evidence
/// carried into the next AWL iteration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldRoundOutput {
    /// The raw input developer report, unchanged.
    pub report: serde_json::Value,
    /// The raw input gate outcome, unchanged.
    pub gate: serde_json::Value,
    /// The raw input review-lens verdicts, unchanged.
    pub verdicts: Vec<serde_json::Value>,
    /// Prior and current adverse evidence joined without empty separators.
    pub evidence: String,
}

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
    /// The argv ACTUALLY executed — after `{base_commit}` token substitution,
    /// not the configured template. The run's evidence shows the real command
    /// (and the real SHA a scope fence pinned to `{base_commit}` diffed
    /// against), auditable without re-deriving the substitution.
    pub argv: Vec<String>,
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

/// Input to `reset_workspace` (`codecs.reset_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetInput {
    /// The repository the worktree belongs to — carried so the
    /// destructive-path guard can canonicalize and confirm the target is
    /// strictly under `<repo_root>/.yggdrasil-worktrees/dev-brief/`.
    pub repo_root: String,
    /// The worktree to restore (`git clean -fd` + `git checkout -- .`).
    pub workspace_path: String,
}

/// Result of `reset_workspace` (`codecs.reset_outcome_codec`). The lenses run
/// only on a green gate, so the tree is provably clean when they start:
/// `was_clean` is normally true. A false means a lens WROTE into the shared
/// worktree — recorded as evidence, never a failed run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetOutcome {
    /// Whether the worktree was already clean when the reset ran.
    pub was_clean: bool,
    /// The `git status --porcelain` lines observed before the restore (the
    /// droppings a misbehaving lens left). Empty in the normal case.
    pub droppings: Vec<String>,
    /// A human-readable account of what the reset did.
    pub detail: String,
}

/// Input to `verify_gates` (`codecs.verify_input_codec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyInput {
    /// The workspace the verification battery runs in (the clean branch head).
    pub workspace_path: String,
    /// The provisioned base commit — feeds the `{base_commit}` token.
    pub base_commit: String,
    /// The verification commands (`config.verify_gates`), run in order.
    pub gates: Vec<GateCommand>,
    /// Where the UNTRUNCATED per-command logs are written; the handler
    /// creates its parent directory.
    pub log_path: String,
}

/// Result of `verify_gates` (`codecs.verify_outcome_codec`). Recorded
/// evidence only: a red gate here never loops the developer back and never
/// changes the disposition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyOutcome {
    /// True only when every gate exited zero AND both clean assertions held.
    pub pass: bool,
    /// The handler's clean-tree assertion: `git status --porcelain` empty
    /// before the battery ran (the directory is the branch head, so the
    /// gates test exactly what merges).
    pub pre_clean: bool,
    /// Whether the tree was still clean AFTER the battery — there is no
    /// normalization commit in the verify stage, so a gate that mutated the
    /// tree is itself a recorded failure.
    pub post_clean: bool,
    /// Per-command records (the same clipped output the event payload keeps).
    pub runs: Vec<GateCommandRun>,
    /// The path of the untruncated log file.
    pub log_path: String,
    /// A human-readable account of the verification's verdict.
    pub detail: String,
}
