//! Wire types for the dev-pipeline activities (brief-forge and
//! implement-and-gate), byte-compatible with the Gleam codecs in
//! `../src/dev_pipeline/codecs.gleam` and shaped by the package's `schemas/`
//! copies of the prospekt doctrine schemas.
//!
//! Optional scalar schema fields are `Option` values that serialize by
//! omission (never `null`); optional array schema fields default to empty on
//! deserialize and always serialize — the same convention the Gleam side
//! documents in `dev_pipeline/types`.

use serde::{Deserialize, Serialize};

/// One norn agent round: the shared input of all three activities. The
/// worker shells `norn --print` in `repo_root` with this session id and
/// prompt; the prompt (projected by the workflow) is what differs per stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRound {
    /// Absolute repository root the norn session is confined to.
    pub repo_root: String,
    /// Deterministic session id (`<task_ref>-scout`, `<task_ref>-design`,
    /// `<task_ref>-refute-r<N>`), created or resumed via
    /// `--resume-if-exists`.
    pub session_id: String,
    /// The full projected prompt, profile preamble included.
    pub prompt: String,
}

// --- scout report (schemas/scout-report.schema.json) -------------------------

/// One relevant file the scout recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelevantFile {
    /// Repo-relative path, optionally `path:line`.
    pub path: String,
    /// Why this file matters to the task.
    pub role: String,
    /// Functions/types downstream steps will need by name.
    #[serde(default)]
    pub key_symbols: Vec<String>,
}

/// One observed behavior with its evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedBehavior {
    /// What the code currently does (present tense, falsifiable).
    pub claim: String,
    /// `path:line`, command output, or test name that demonstrates it.
    pub evidence: String,
}

/// One ranked root-cause hypothesis (bug-shaped work only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCauseHypothesis {
    /// The hypothesis.
    pub hypothesis: String,
    /// Evidence for.
    pub supporting: String,
    /// The observation that would kill this hypothesis.
    pub would_falsify: String,
}

/// The scout's grounding pass over the actual tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoutReport {
    /// One sentence: what was scouted and why.
    pub subject: String,
    /// The files that matter, each with role and key symbols.
    pub relevant_files: Vec<RelevantFile>,
    /// Falsifiable behavior claims, each with evidence.
    pub observed_behavior: Vec<ObservedBehavior>,
    /// Ranked root-cause hypotheses, strongest first (bug-shaped work only).
    #[serde(default)]
    pub root_cause_hypotheses: Vec<RootCauseHypothesis>,
    /// Invariants the fix must not break.
    pub constraints: Vec<String>,
    /// Existing in-tree patterns the design should match.
    #[serde(default)]
    pub prior_art: Vec<String>,
    /// Directories not read, behaviors not exercised, claims taken on faith.
    pub not_covered: Vec<String>,
}

// --- brief (schemas/brief.schema.json) ---------------------------------------

/// The brief's problem kind. `Bug` requires a pinned root cause (schema
/// if/then).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// A defect with a root cause to pin.
    Bug,
    /// New capability.
    Feature,
    /// Behavior-preserving restructure.
    Refactor,
    /// Documentation work.
    Docs,
    /// Design work.
    Design,
}

/// The pinned root cause — mandatory when `kind = bug`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCause {
    /// The root cause, stated.
    pub statement: String,
    /// Step-by-step from trigger to symptom, each step falsifiable.
    pub causal_chain: Vec<String>,
    /// The demonstration that pinned it.
    pub evidence: String,
}

/// What is wrong or missing, as observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// The problem statement.
    pub statement: String,
    /// The problem kind.
    pub kind: ProblemKind,
    /// The pinned root cause (required by schema when `kind = bug`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<RootCause>,
}

/// A named rejected alternative — a decision that doesn't name what it
/// rejected was not a decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedAlternative {
    /// The alternative considered.
    pub alternative: String,
    /// Why it was rejected.
    pub why_rejected: String,
}

/// The fix design at implementation altitude.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixDesign {
    /// The design, at no-further-judgment-calls detail.
    pub approach: String,
    /// Files/modules to change, path per entry.
    pub touch_points: Vec<String>,
    /// Invariants the change must preserve.
    #[serde(default)]
    pub invariants_to_preserve: Vec<String>,
    /// The alternatives this design rejected, and why.
    pub rejected_alternatives: Vec<RejectedAlternative>,
    /// Known risks.
    #[serde(default)]
    pub risks: Vec<String>,
}

/// How a gate is judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// Exit-status activity.
    Command,
    /// Test asserting forward progress.
    OutcomeTest,
    /// Truth-pass row judged by the operator.
    LiveOperator,
}

/// What a gate asserts — shape-assertions are not a legal value on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAsserts {
    /// Forward progress happened.
    Outcome,
    /// The bad thing does not happen.
    Absence,
    /// Old data/protocol still works.
    Compatibility,
}

/// One acceptance gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceGate {
    /// Stable id within the brief (G1, G2...), never renumbered.
    pub id: String,
    /// What must be TRUE, phrased as an outcome.
    pub statement: String,
    /// How the gate is judged.
    pub kind: GateKind,
    /// What the gate asserts.
    pub asserts: GateAsserts,
    /// For `kind = command`: the exact command whose OWN exit status judges
    /// the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The dispatchable unit of work. `refutation_survived` is stamped by the
/// WORKFLOW, never the designer; this worker only relays what norn returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    /// Brief title.
    pub title: String,
    /// Ledger/task id this brief discharges.
    pub task_ref: String,
    /// What is wrong or missing.
    pub problem: Problem,
    /// The fix design.
    pub fix_design: FixDesign,
    /// Outcome-asserting acceptance gates.
    pub acceptance_gates: Vec<AcceptanceGate>,
    /// Explicit non-goals.
    pub out_of_scope: Vec<String>,
    /// Reference to the refutation this design survived (workflow-stamped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refutation_survived: Option<String>,
    /// What the designer did not cover.
    pub not_covered: Vec<String>,
}

// --- refutation (schemas/refutation.schema.json) ------------------------------

/// How one attack resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackOutcome {
    /// The design must change.
    Lands,
    /// The design already handles it.
    Deflected,
    /// The attack was wrong.
    Withdrawn,
}

/// Severity when an attack lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackSeverity {
    /// The design dies.
    Fatal,
    /// The design must address it.
    MustAddress,
    /// Recorded, not blocking.
    Note,
}

/// One attack attempted, including the deflected ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attack {
    /// The specific claim/decision attacked (quoted).
    pub target: String,
    /// The concrete failure: inputs/state → wrong result.
    pub argument: String,
    /// `path:line` or observed behavior backing the attack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// How the attack resolved.
    pub outcome: AttackOutcome,
    /// Severity if the attack lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_if_lands: Option<AttackSeverity>,
}

/// The independent gate audit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateAudit {
    /// Do ALL acceptance gates assert outcomes rather than history shape?
    pub gates_assert_outcomes: bool,
    /// Ways the implementation could pass every gate and still be wrong.
    pub holes: Vec<String>,
}

/// The refuter's verdict on a draft brief — assent without argument does not
/// typecheck.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refutation {
    /// Whether the design survives.
    pub design_survives: bool,
    /// Every attack attempted, including the failed ones.
    pub attacks: Vec<Attack>,
    /// The independent gate audit.
    pub gate_audit: GateAudit,
    /// Attack surfaces not probed.
    pub not_covered: Vec<String>,
}

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
