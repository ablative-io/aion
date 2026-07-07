//// The typed data model shared across the dev-brief family (the `dev_brief`
//// pipeline and the `review_lens` children it fans out) and the activities
//// they dispatch.
////
//// The contract is JSON-on-the-wire: every type here has a codec in
//// `dev_brief/codecs.gleam`. The agent-activity OUTPUT types ([`DevReport`],
//// [`LensVerdict`]) mirror the schemas under `schemas/`
//// (`dev-report.schema.json`, `lens-verdict.schema.json`) — the
//// drift-guarded source of truth the driven agents are constrained by via
//// `--output-schema`.
////
//// ADVERSARIAL INDEPENDENCE IS STRUCTURAL: each review lens runs as its own
//// CHILD WORKFLOW with its own `{workflow_id}`-keyed norn session, spawned
//// concurrently with its sibling lenses. A lens sees the brief, the diff,
//// the developer's report, and the gate evidence — never another lens's
//// verdict and never the developer's session.

import gleam/option.{type Option}

// --- the brief (workflow input, authored by the operator or brief_forge) ------

/// One development brief: the unit of dispatch. `pointers` names where to
/// look (files, docs); `scope_in`/`scope_out` bound what may and must not
/// change (`scope_out` carries the no-reorg boundaries for large files);
/// `acceptance` is the criteria the reviewers hold the diff to — every claim
/// the developer makes is checked against these, adversarially.
pub type Brief {
  Brief(
    id: String,
    title: String,
    objective: String,
    context: String,
    pointers: List(String),
    scope_in: List(String),
    scope_out: List(String),
    acceptance: List(String),
    notes: String,
  )
}

/// One mechanical gate command, run in the workspace root. Pass = exit 0.
/// Configured per brief (per repo), never hardcoded — e.g. cargo fmt, cargo
/// clippy -D warnings, cargo test, gleam test.
pub type GateCommand {
  GateCommand(name: String, argv: List(String))
}

/// One adversarial review lens: `charter` is the lens's marching orders (what
/// failure class it hunts). Lenses run concurrently as child workflows.
pub type Lens {
  Lens(name: String, charter: String)
}

/// Operational inputs carried alongside the brief. `base_branch`,
/// `max_fix_cycles`, and `lenses` are overridable process defaults, never
/// hidden constants: absent from the input they resolve to
/// [`default_base_branch`] / [`default_max_fix_cycles`] / [`default_lenses`].
/// An EMPTY `gates` list is honoured as the operator's explicit choice and
/// recorded as a vacuous pass — visible in the outcome, never silent.
pub type RunConfig {
  RunConfig(
    repo_root: String,
    base_branch: String,
    gates: List(GateCommand),
    max_fix_cycles: Int,
    lenses: List(Lens),
  )
}

/// The default branch per-brief worktrees are based on when the input omits
/// `base_branch`.
pub fn default_base_branch() -> String {
  "main"
}

/// The default developer-round budget (initial implementation + every
/// loop-back) when the input omits `max_fix_cycles`.
pub fn default_max_fix_cycles() -> Int {
  3
}

/// The default adversarial lens set when the input omits `lenses`: three
/// distinct failure-class hunters. Diversity catches what redundancy cannot.
pub fn default_lenses() -> List(Lens) {
  [
    Lens(
      name: "correctness",
      charter: "Hunt defects in the change itself: logic errors, unhandled "
        <> "cases, broken invariants, concurrency hazards, silent failure "
        <> "paths. Assume the change is wrong and try to construct the "
        <> "concrete input or state that breaks it.",
    ),
    Lens(
      name: "regressions",
      charter: "Hunt collateral damage: behaviour the diff changes that the "
        <> "brief did not ask to change. Read the surrounding code and its "
        <> "callers; name the existing behaviour at risk and the concrete "
        <> "scenario that regresses.",
    ),
    Lens(
      name: "brief_compliance",
      charter: "Hold the diff to the brief, adversarially: every acceptance "
        <> "criterion either demonstrably met or a blocking finding; every "
        <> "scope_out boundary respected; every claimed deviation actually "
        <> "declared. Pass-and-still-wrong is the failure class here — do "
        <> "not accept a claim without evidence in the diff or gate output.",
    ),
  ]
}

/// Input to the `dev_brief` workflow: one brief plus the run configuration.
pub type BriefInput {
  BriefInput(brief: Brief, config: RunConfig)
}

// --- agent activity payloads ----------------------------------------------------

/// One acceptance criterion the developer claims to have met, with how.
/// Claims are checked by the `brief_compliance` lens, never trusted.
pub type AcceptanceClaim {
  AcceptanceClaim(criterion: String, how: String)
}

/// One declared deviation from the brief (scope stretched, approach changed).
/// Undeclared deviations are what the reviewers hunt.
pub type Deviation {
  Deviation(what: String, why: String)
}

/// The developer's structured result per `dev-report.schema.json`. `commits`
/// is rewritten by the WORKER to the real branch head after it commits the
/// turn's work — never an agent-asserted hash.
pub type DevReport {
  DevReport(
    brief_id: String,
    summary: String,
    commits: List(String),
    acceptance_claims: List(AcceptanceClaim),
    deviations: List(Deviation),
  )
}

/// Input to the `developer` agent activity: the brief, and — on loop-back
/// rounds — the failing gate outcome and/or the adverse lens verdicts being
/// addressed. `workspace_path` is the brief's provisioned worktree: agents do
/// not run git, so after a successful round the WORKER commits the delta
/// there on the brief branch.
pub type DeveloperInput {
  DeveloperInput(
    brief: Brief,
    gate: Option(GateOutcome),
    verdicts: List(LensVerdict),
    workspace_path: String,
  )
}

// --- review lenses ----------------------------------------------------------------

/// A finding's severity. `Blocking` findings force the derived verdict to
/// reject; `Advisory` findings are recorded evidence that does not loop the
/// cycle on its own.
pub type Severity {
  Blocking
  Advisory
}

/// One concrete reviewer finding: what is wrong and the evidence (file/line,
/// scenario, quoted diff). "It looks fine" is not a finding; neither is a
/// vibe — the lens profiles demand constructed failure scenarios.
pub type ReviewFinding {
  ReviewFinding(severity: Severity, title: String, evidence: String)
}

/// A lens verdict's overall disposition. DERIVE-AND-CHECK: the workflow
/// derives this mechanically from the findings (`verdicts.derive_overall`)
/// and REJECTS a verdict whose asserted value disagrees — consistency is
/// checked, never trusted.
pub type Overall {
  Accept
  Reject
}

/// One lens's structured result per `lens-verdict.schema.json`.
pub type LensVerdict {
  LensVerdict(
    lens: String,
    findings: List(ReviewFinding),
    overall: Overall,
    reject_reason: Option(String),
  )
}

/// Input to the `review_lens` CHILD workflow (and, verbatim, to its single
/// `review_lens` agent activity): the lens, the brief, the developer's diff
/// and report, and the gate evidence. Structurally lens-blind: no sibling
/// verdicts, no developer session.
pub type LensInput {
  LensInput(
    lens: Lens,
    brief: Brief,
    diff: String,
    report: DevReport,
    gate_runs: List(GateCommandRun),
  )
}

// --- shell activity payloads --------------------------------------------------

/// Input to the `provision_workspace` shell activity: create the brief's
/// isolated git worktree at `workspace_path`, checking out `branch` freshly
/// based on `base_branch`. The workflow derives `workspace_path` as
/// `<workspace_base>/<workflow_id>` so it matches the `--workspace-root` the
/// driven developer harness points norn at.
pub type ProvisionInput {
  ProvisionInput(
    repo_root: String,
    base_branch: String,
    branch: String,
    workspace_path: String,
  )
}

/// Result of `provision_workspace`. `base_commit` pins the exact commit the
/// worktree started from — the gate computes the developer's diff against it.
pub type WorkspaceInfo {
  WorkspaceInfo(workspace_path: String, branch: String, base_commit: String)
}

/// Input to the `run_gates` shell activity: execute the brief's configured
/// gate commands in the workspace, in order, and capture the diff since
/// `base_commit` for the reviewers.
pub type GateInput {
  GateInput(
    workspace_path: String,
    base_commit: String,
    gates: List(GateCommand),
  )
}

/// One gate command's recorded run. `passed` is exit == 0; `output_tail`
/// carries the trailing output as loop-back diagnostics and reviewer
/// evidence. A red command is recorded DATA, never an activity error.
pub type GateCommandRun {
  GateCommandRun(
    name: String,
    exit_code: Int,
    passed: Bool,
    output_tail: String,
  )
}

/// Result of `run_gates`. `pass` is true only when every configured command
/// exited 0 (vacuously true for an empty gate list — recorded in
/// `diagnostics`, never silent). `diff` carries the developer's full change
/// since `base_commit` for the lenses.
pub type GateOutcome {
  GateOutcome(
    pass: Bool,
    runs: List(GateCommandRun),
    diff: String,
    diagnostics: String,
  )
}

/// Input to the `cleanup_workspace` shell activity: remove the brief's
/// worktree (the branch, and the work on it, remain).
pub type CleanupInput {
  CleanupInput(repo_root: String, workspace_path: String)
}

/// Result of `cleanup_workspace`. A dirty worktree is NOT removed (that
/// would destroy uncommitted work); `removed: False` with the reason is the
/// honest record.
pub type CleanupOutcome {
  CleanupOutcome(removed: Bool, detail: String)
}

// --- pipeline result ---------------------------------------------------------------

/// The terminal disposition of one brief's run. Exhaustion is a terminal
/// DISPOSITION recorded in durable history — never a silent success, and not
/// a workflow error.
pub type Disposition {
  /// Every lens accepted (derived, not asserted) on a green gate.
  Accepted
  /// The developer-round budget ran out before a full acceptance.
  CycleCapExhausted
}

/// The `dev_brief` workflow's result: the terminal disposition plus
/// everything the operator needs — cycle accounting, the final artifacts,
/// and every derive-and-check violation (`verdict_mismatches`, cycle-stamped
/// evidence, never silently accepted).
pub type BriefResult {
  BriefResult(
    brief_id: String,
    disposition: Disposition,
    fix_cycles: Int,
    first_pass_accepted: Bool,
    verdict_mismatches: List(String),
    branch: String,
    report: Option(DevReport),
    gate: Option(GateOutcome),
    verdicts: List(LensVerdict),
    workspace_removed: Bool,
    summary: String,
  )
}

// --- typed errors -----------------------------------------------------------------

/// Workflow failures, surfaced as typed data in the run history. Terminal
/// dispositions are results, not errors — these variants are the
/// engine/activity/input faults.
pub type DevBriefError {
  /// A named stage failed as an activity error.
  StageFailed(stage: String, message: String)
  /// A `review_lens` child run failed at the engine/child boundary.
  ChildFailed(reason: String)
  /// The workflow input could not be decoded.
  DecodeInputFailed(message: String)
}

// --- string renderings ----------------------------------------------------------

/// The wire tag for a severity.
pub fn severity_to_string(severity: Severity) -> String {
  case severity {
    Blocking -> "blocking"
    Advisory -> "advisory"
  }
}

/// Resolve a severity tag; unknown tags are a decode failure upstream.
pub fn severity_from_string(tag: String) -> Option(Severity) {
  case tag {
    "blocking" -> option.Some(Blocking)
    "advisory" -> option.Some(Advisory)
    _ -> option.None
  }
}

/// The wire tag for a verdict overall.
pub fn overall_to_string(overall: Overall) -> String {
  case overall {
    Accept -> "accept"
    Reject -> "reject"
  }
}

/// Resolve an overall tag; unknown tags are a decode failure upstream.
pub fn overall_from_string(tag: String) -> Option(Overall) {
  case tag {
    "accept" -> option.Some(Accept)
    "reject" -> option.Some(Reject)
    _ -> option.None
  }
}

/// The wire tag for a terminal disposition.
pub fn disposition_to_string(disposition: Disposition) -> String {
  case disposition {
    Accepted -> "accepted"
    CycleCapExhausted -> "cycle_cap_exhausted"
  }
}

/// Resolve a disposition tag; unknown tags are a decode failure upstream.
pub fn disposition_from_string(tag: String) -> Option(Disposition) {
  case tag {
    "accepted" -> option.Some(Accepted)
    "cycle_cap_exhausted" -> option.Some(CycleCapExhausted)
    _ -> option.None
  }
}
