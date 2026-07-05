//// Domain types for the dev-pipeline workflows, mirroring the doctrine
//// schemas copied into `schemas/` (scout-report, brief, refutation,
//// implementation-report, and the brief-forge / implement-and-gate
//// input/output pairs).
////
//// Optional SCALAR schema fields are `Option` values encoded by omission
//// (never `null` — the schemas are `additionalProperties: false` with typed
//// properties). Optional ARRAY schema fields decode with an empty-list
//// default and always encode; an empty array is schema-legal in every such
//// position (no optional array carries `minItems`), which keeps the codec
//// surface materially smaller without leaving schema shape.

import gleam/option.{type Option}

// --- workflow input ---------------------------------------------------------

/// The brief-forge workflow input (`schemas/brief-forge.input.schema.json`).
/// Every cap is REQUIRED — the workflow bakes in no defaults, per stacked-dev
/// convention. `related_refs` and `emphases` are the schema's two optional
/// arrays.
pub type BriefForgeInput {
  BriefForgeInput(
    task_statement: String,
    task_ref: String,
    repo_root: String,
    base_ref: String,
    related_refs: List(String),
    refute_cap: Int,
    diagnose_only: Bool,
    emphases: List(String),
  )
}

// --- scout report -----------------------------------------------------------

/// One relevant file the scout recorded (`relevant_files[]`).
pub type RelevantFile {
  RelevantFile(path: String, role: String, key_symbols: List(String))
}

/// One observed behavior with its evidence (`observed_behavior[]`).
pub type ObservedBehavior {
  ObservedBehavior(claim: String, evidence: String)
}

/// One ranked root-cause hypothesis (`root_cause_hypotheses[]`) — only for
/// bug-shaped work; a hypothesis without a falsifier is a hunch.
pub type RootCauseHypothesis {
  RootCauseHypothesis(
    hypothesis: String,
    supporting: String,
    would_falsify: String,
  )
}

/// The scout's grounding pass over the actual tree
/// (`schemas/scout-report.schema.json`).
pub type ScoutReport {
  ScoutReport(
    subject: String,
    relevant_files: List(RelevantFile),
    observed_behavior: List(ObservedBehavior),
    root_cause_hypotheses: List(RootCauseHypothesis),
    constraints: List(String),
    prior_art: List(String),
    not_covered: List(String),
  )
}

// --- brief ------------------------------------------------------------------

/// The brief's problem kind (`problem.kind`). `Bug` requires a pinned root
/// cause — enforced by the schema's if/then clause and re-checked nowhere
/// else: the refuter attacks it, the workflow does not police it.
pub type ProblemKind {
  Bug
  Feature
  Refactor
  Docs
  Design
}

/// The pinned root cause (`problem.root_cause`) — mandatory when
/// `kind = bug` (rigid step 2).
pub type RootCause {
  RootCause(statement: String, causal_chain: List(String), evidence: String)
}

/// What is wrong or missing, as observed (`problem`).
pub type Problem {
  Problem(statement: String, kind: ProblemKind, root_cause: Option(RootCause))
}

/// A named rejected alternative (`fix_design.rejected_alternatives[]`) — a
/// decision that doesn't name what it rejected was not a decision.
pub type RejectedAlternative {
  RejectedAlternative(alternative: String, why_rejected: String)
}

/// The fix design at implementation altitude (`fix_design`).
pub type FixDesign {
  FixDesign(
    approach: String,
    touch_points: List(String),
    invariants_to_preserve: List(String),
    rejected_alternatives: List(RejectedAlternative),
    risks: List(String),
  )
}

/// How a gate is judged (`acceptance_gates[].kind`).
pub type GateKind {
  Command
  OutcomeTest
  LiveOperator
}

/// What a gate asserts (`acceptance_gates[].asserts`). Shape-assertions are
/// not a legal value on purpose (rigid step 3).
pub type GateAsserts {
  Outcome
  Absence
  Compatibility
}

/// One acceptance gate (`acceptance_gates[]`).
pub type AcceptanceGate {
  AcceptanceGate(
    id: String,
    statement: String,
    kind: GateKind,
    asserts: GateAsserts,
    command: Option(String),
  )
}

/// The dispatchable unit of work (`schemas/brief.schema.json`).
/// `refutation_survived` is stamped by THIS WORKFLOW on acceptance, never by
/// the designer — the workflow clears any designer-set value on receipt.
pub type Brief {
  Brief(
    title: String,
    task_ref: String,
    problem: Problem,
    fix_design: FixDesign,
    acceptance_gates: List(AcceptanceGate),
    out_of_scope: List(String),
    refutation_survived: Option(String),
    not_covered: List(String),
  )
}

// --- refutation ---------------------------------------------------------

/// How one attack resolved (`attacks[].outcome`).
pub type AttackOutcome {
  Lands
  Deflected
  Withdrawn
}

/// Severity when an attack lands (`attacks[].severity_if_lands`).
pub type AttackSeverity {
  Fatal
  MustAddress
  Note
}

/// One attack attempted, including the failed ones (`attacks[]`) — deflected
/// attacks are the evidence the design was actually tested.
pub type Attack {
  Attack(
    target: String,
    argument: String,
    evidence: Option(String),
    outcome: AttackOutcome,
    severity_if_lands: Option(AttackSeverity),
  )
}

/// The independent gate audit (`gate_audit`): could the implementation pass
/// every gate and still be wrong?
pub type GateAudit {
  GateAudit(gates_assert_outcomes: Bool, holes: List(String))
}

/// The refuter's verdict on a draft brief
/// (`schemas/refutation.schema.json`). Assent without argument does not
/// typecheck: `attacks` carries `minItems: 1` either way.
pub type Refutation {
  Refutation(
    design_survives: Bool,
    attacks: List(Attack),
    gate_audit: GateAudit,
    not_covered: List(String),
  )
}

// --- activity inputs ----------------------------------------------------

/// One norn agent round: the worker shells `norn --print` in `repo_root`
/// with this session id and prompt. All three agent activities share the
/// shape; the prompt (built by `dev_pipeline/prompts`) is what differs.
pub type AgentRound {
  AgentRound(repo_root: String, session_id: String, prompt: String)
}

// --- workflow output ------------------------------------------------------

/// How the forge run ended (`outcome`): `Converged` means the design
/// survived refutation and the brief carries the workflow's
/// `refutation_survived` stamp; `Contested` means `refute_cap` rounds were
/// exhausted — a finding surfaced to the operator with both sides, never an
/// error crash.
pub type ForgeOutcome {
  Converged
  Contested
}

/// The brief-forge result (`schemas/brief-forge.output.schema.json`): the
/// brief verbatim, the surviving/last refutation, rounds used, and
/// `diagnose_only` passed through.
pub type BriefForgeResult {
  BriefForgeResult(
    outcome: ForgeOutcome,
    brief: Brief,
    refutation: Refutation,
    rounds: Int,
    diagnose_only: Bool,
  )
}

/// The single typed failure surface: which stage failed and why. Contested
/// designs are NOT errors (they are `Contested` results); this covers
/// input-decode failures and agent activities that failed terminally.
pub type BriefForgeError {
  BriefForgeStageFailed(stage: String, message: String)
}

// --- implement-and-gate: workflow input --------------------------------------

/// Isolated-workspace mode (`isolation`). A shared-checkout run is not a
/// legal value, on purpose — concurrent runs on one checkout collide.
pub type Isolation {
  Worktree
  Clone
}

/// One gate of the battery (`gates[]`): the exact command whose OWN exit
/// status judges it. Language-specific batteries are a parameter here, not a
/// fork of the workflow.
pub type GateSpec {
  GateSpec(id: String, command: String)
}

/// The implement-and-gate workflow input
/// (`schemas/implement-and-gate.input.schema.json`). The brief is EMBEDDED
/// (an object conforming to `schemas/brief.schema.json`), never referenced
/// by id. `fix_cap` is REQUIRED — no baked defaults, per package convention.
/// `implementer_model` is the frontier escape hatch: it overrides the
/// worker's pilot model at harness invocation and, being input, the tier a
/// diff was written on is always in history.
pub type ImplementAndGateInput {
  ImplementAndGateInput(
    brief: Brief,
    repo_root: String,
    base_ref: String,
    isolation: Isolation,
    node: Option(String),
    fix_cap: Int,
    gates: List(GateSpec),
    implementer_model: Option(String),
  )
}

// --- implement-and-gate: activity inputs/outputs ------------------------------

/// Input to `provision_workspace`: create an isolated worktree/clone of
/// `repo_root` at `base_ref` under a scratch path. `task_ref` names the
/// workspace deterministically (`dev-pipeline-<task_ref>`).
pub type ProvisionInput {
  ProvisionInput(
    repo_root: String,
    base_ref: String,
    isolation: Isolation,
    task_ref: String,
  )
}

/// The provisioned isolated workspace every downstream step runs inside.
pub type Workspace {
  Workspace(path: String)
}

/// One implementer round (initial or resume): the worker shells `norn
/// --print` INSIDE `workspace_path` with this session id and prompt. `model`
/// is the invocation-level override from the workflow input; when absent the
/// worker pins its pilot model.
pub type ImplementRound {
  ImplementRound(
    workspace_path: String,
    session_id: String,
    prompt: String,
    model: Option(String),
  )
}

/// Input to `run_gate`: shell exactly `command` in `workspace_path`.
pub type GateRun {
  GateRun(workspace_path: String, gate_id: String, command: String)
}

/// One completed gate command, the stacked-dev `CliRun` pattern: a non-zero
/// `exit_status` is DATA routed to the fix loop, never an activity error
/// (only a missing binary/workspace is terminal). `output` is the captured
/// combined stdout+stderr, tail-bounded by the worker at capture — the
/// durable record a fix round is handed, never a paraphrase.
pub type GateCliRun {
  GateCliRun(exit_status: Int, output: String, duration_ms: Int)
}

/// Input to `teardown_workspace` (declared but deliberately never dispatched
/// — see `implement_and_gate`'s module doc).
pub type TeardownInput {
  TeardownInput(repo_root: String, workspace_path: String)
}

/// `teardown_workspace`'s best-effort receipt.
pub type TornDown {
  TornDown(cleaned: Bool)
}

// --- implementation report ----------------------------------------------------

/// One changed file (`files_changed[]`).
pub type FileChange {
  FileChange(path: String, change: String)
}

/// Mapping from one brief acceptance gate to the work discharging it
/// (`gates_addressed[]`).
pub type GateAddressed {
  GateAddressed(gate_id: String, how: String)
}

/// One declared departure from the brief (`deviations[]`) — an undeclared
/// deviation found in review is a defect regardless of whether the code is
/// right.
pub type ReportDeviation {
  ReportDeviation(from: String, to: String, why: String)
}

/// The implementer's structured return
/// (`schemas/implementation-report.schema.json`). Note what is NOT here:
/// gate results — gates are command activities with their own recorded exit
/// statuses; the implementer never certifies them.
pub type ImplementationReport {
  ImplementationReport(
    brief_ref: String,
    summary: String,
    files_changed: List(FileChange),
    gates_addressed: List(GateAddressed),
    deviations: List(ReportDeviation),
    new_tests: List(String),
    concerns: List(String),
    not_covered: List(String),
  )
}

// --- implement-and-gate: workflow output ---------------------------------------

/// One row of the battery record (`gate_record[]`): the gate, its exact
/// command, and the command's OWN exit status as recorded fact.
/// `output_tail` rides only on the failing entry of a `GatesExhausted`
/// record (encoded by omission otherwise).
pub type GateRecordEntry {
  GateRecordEntry(
    id: String,
    command: String,
    exit_status: Int,
    duration_ms: Int,
    output_tail: Option(String),
  )
}

/// How the run ended (`outcome`): `GatesGreen` means every gate exited 0 on
/// the final round — the workflow cannot reach this state with a red gate,
/// which is topology, not policy; `GatesExhausted` means `fix_cap` fix
/// rounds were spent and a gate is still red — surfaced to the operator with
/// the full gate record, never an error crash.
pub type GateOutcome {
  GatesGreen
  GatesExhausted
}

/// The implement-and-gate result
/// (`schemas/implement-and-gate.output.schema.json`): the implementer's last
/// report, the final round's gate record, rounds spent, and the workspace —
/// which is intentionally NOT torn down on either terminus (review inspects
/// success; the operator inspects failure).
pub type ImplementAndGateResult {
  ImplementAndGateResult(
    outcome: GateOutcome,
    implementation_report: ImplementationReport,
    gate_record: List(GateRecordEntry),
    rounds: Int,
    workspace_path: String,
  )
}

/// The single typed failure surface: which stage failed and why. An
/// exhausted fix cap is NOT an error (it is a `GatesExhausted` result); this
/// covers input-decode failures, a provision that could not produce a
/// workspace, gate commands whose binary/workspace is missing, and
/// implementer rounds that failed terminally.
pub type ImplementAndGateError {
  ImplementAndGateStageFailed(stage: String, message: String)
}
