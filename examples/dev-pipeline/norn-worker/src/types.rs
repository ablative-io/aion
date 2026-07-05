//! Wire types for the brief-forge agent activities, byte-compatible with the
//! Gleam codecs in `../src/dev_pipeline/codecs.gleam` and shaped by the
//! package's `schemas/` copies of the prospekt doctrine schemas.
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
