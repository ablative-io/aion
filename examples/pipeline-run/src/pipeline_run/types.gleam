//// The typed data model shared across the pipeline-run family (parent
//// `pipeline_run` and child `pipeline_unit`) and the activities they dispatch.
////
//// The contract is JSON-on-the-wire: every type here has a codec in
//// `pipeline_run/codecs.gleam`, and the agent-activity output types
//// (`ScoutFindings`, `StackPlan`, `DevReport`, `ReviewVerdict`) are the Gleam
//// mirror of the four norn `--output-schema` documents under `schemas/`. The
//// schemas are the drift-guarded source of truth; these types decode what a
//// schema-constrained driven agent returns.

import gleam/option.{type Option}

// --- parent input: the prospekt dev-cycle brief ----------------------------

/// The parent workflow input: a prospekt effective `dev-cycle/brief` document
/// (payload + injected `id`/`state`), plus the operational fields the run needs
/// to touch a real tree. Prospekt-injected fields the workflow does not read
/// (`model`, `model_version`, execution slots) are permitted and ignored — the
/// decoder is tolerant of extra fields.
///
/// `repo_root` and `base_branch` are operational inputs injected alongside the
/// document: the brief says WHAT to build; these say WHERE. `dev_review_cap`
/// and `gate_cap` are overridable process defaults (never baked deeper than a
/// default — rigid-steps discipline): absent, they resolve to
/// [`default_dev_review_cap`] / [`default_gate_cap`].
pub type PipelineBrief {
  PipelineBrief(
    id: String,
    title: String,
    intent: String,
    scope_in: List(String),
    scope_out: List(String),
    acceptance_criteria: List(String),
    constraints: List(String),
    state: String,
    repo_root: String,
    base_branch: String,
    dev_review_cap: Int,
    gate_cap: Int,
  )
}

/// The workflow default for the cumulative dev<->review budget when the input
/// omits `dev_review_cap`. An overridable default, never a hidden constant.
pub fn default_dev_review_cap() -> Int {
  4
}

/// The workflow default for the gate budget when the input omits `gate_cap`.
pub fn default_gate_cap() -> Int {
  2
}

// --- scout output ----------------------------------------------------------

/// One grounded observation the scout made in the actual tree: a path (with a
/// line where it could pin one) and what it observed there. Rigid step 1:
/// ground before you touch, with evidence not paraphrase.
pub type Observation {
  Observation(location: String, note: String)
}

/// The scout activity's structured result (`schemas/scout_output.json`): a
/// grounding pass over the real tree that the plan is composed from.
pub type ScoutFindings {
  ScoutFindings(
    summary: String,
    observations: List(Observation),
    integration_points: List(String),
    risks: List(String),
    not_covered: List(String),
  )
}

// --- plan output: the stack ------------------------------------------------

/// One unit of the stack the plan proposes: an atomic slice of the brief with
/// a stable `unit_id`, a concrete `goal`, the files it is expected to touch,
/// and the `depends_on` unit ids whose landed work it must branch on.
pub type PlanUnit {
  PlanUnit(
    unit_id: String,
    goal: String,
    files_hint: List(String),
    depends_on: List(String),
  )
}

/// The plan activity's structured result (`schemas/stack_plan.json`): the
/// ordered stack decomposing the brief into dependency-related units.
pub type StackPlan {
  StackPlan(units: List(PlanUnit), summary: String, not_covered: List(String))
}

// --- dev output ------------------------------------------------------------

/// The dev activity's structured result (`schemas/dev_output.json`): what one
/// dev round touched and did. Session continuity is the harness's job (the
/// driven norn session resumes across rounds); this is the round's report.
pub type DevReport {
  DevReport(
    files_touched: List(String),
    summary: String,
    not_covered: List(String),
  )
}

// --- review output ---------------------------------------------------------

/// One blocker the reviewer found: file:line evidence, what is wrong, and the
/// concrete failure scenario. Every blocker MUST carry evidence — "looks fine"
/// does not typecheck (rigid step 6 / adversarial-review doctrine).
pub type Blocker {
  Blocker(evidence: String, problem: String, scenario: String)
}

/// The review activity's structured result (`schemas/review_output.json`): an
/// adversarial verdict. `pass` is true ONLY when `blockers` is empty and the
/// work is production-ready.
pub type ReviewVerdict {
  ReviewVerdict(
    pass: Bool,
    blockers: List(Blocker),
    should_fix: List(String),
    summary: String,
    not_covered: List(String),
  )
}

// --- shell activity payloads -----------------------------------------------

/// Input to the `provision_workspace` shell activity: create the unit's
/// isolated workspace at `workspace_path`, checking out `unit_branch` branched
/// on top of `base_branch` (the prior stratum's landed branch, or the
/// integration base for a root unit). The child derives `workspace_path` as
/// `<workspace_base>/<child_workflow_id>` so it matches the `--workspace-root`
/// the driven dev/review harnesses point Norn at.
pub type ProvisionInput {
  ProvisionInput(
    repo_root: String,
    base_branch: String,
    unit_branch: String,
    workspace_path: String,
  )
}

/// Result of `provision_workspace`: the workspace path the unit's activities
/// run in and the branch checked out there.
pub type WorkspaceInfo {
  WorkspaceInfo(workspace_path: String, branch: String)
}

/// Input to the `gate` shell activity: the workspace to run the cargo gate in.
pub type GateInput {
  GateInput(workspace_path: String)
}

/// Result of the `gate` shell activity. `pass` is true only when both
/// `cargo clippy -D warnings` and `cargo test` exit zero; the combined output
/// rides in `diagnostics` on fail. A non-zero cargo exit is RECORDED DATA here,
/// never an activity error (rigid step 4: the exit code is the fact).
pub type GateOutcome {
  GateOutcome(pass: Bool, diagnostics: String)
}

/// One branch to land, in dependency order.
pub type LandUnit {
  LandUnit(unit_id: String, branch: String)
}

/// Input to the `land` shell activity: merge each unit branch, in order, onto
/// the integration branch. `base_branch` seeds the integration branch the first
/// time it is landed onto (it is created from `base_branch` if it does not yet
/// exist), so a later stratum branches on prior landed work. Freshness re-check
/// is the land activity's job.
pub type LandInput {
  LandInput(
    repo_root: String,
    base_branch: String,
    integration_branch: String,
    units: List(LandUnit),
  )
}

/// Result of the `land` shell activity: which unit ids merged, in order, and
/// the integration branch they landed on.
pub type LandOutcome {
  LandOutcome(landed: List(String), integration_branch: String, detail: String)
}

/// Input to the `notify` shell activity: a best-effort completion notice.
pub type NotifyInput {
  NotifyInput(brief_id: String, summary: String)
}

/// Result of the `notify` shell activity.
pub type NotifyOutcome {
  NotifyOutcome(sent: Bool, detail: String)
}

// --- child (pipeline_unit) input/output ------------------------------------

/// The terminal disposition of one unit's dev/review/gate cycle. A cap
/// exhaustion is not an error: it is a terminal disposition that STILL flows
/// up (and the run still notifies).
pub type Disposition {
  Passed
  ReviewCapExhausted
  GateCapExhausted
}

/// Input to the child `pipeline_unit` workflow: everything one unit needs to
/// run its full dev/review/gate cycle in isolation. `base_branch` is the branch
/// this unit stacks on (a dependency's landed branch, or the integration base);
/// `unit_branch` is the branch it produces. The brief/scout context is carried
/// so the dev and review agents compose their prompts from the structured
/// artifacts, never from prose.
pub type UnitInput {
  UnitInput(
    repo_root: String,
    base_branch: String,
    unit_branch: String,
    unit_id: String,
    goal: String,
    files_hint: List(String),
    brief_title: String,
    brief_intent: String,
    acceptance_criteria: List(String),
    constraints: List(String),
    scout_summary: String,
    dev_review_cap: Int,
    gate_cap: Int,
  )
}

/// The child `pipeline_unit` workflow's result: the unit's terminal
/// disposition, the branch it produced, the cumulative round counts, and the
/// last review/gate evidence.
pub type UnitResult {
  UnitResult(
    unit_id: String,
    branch: String,
    disposition: Disposition,
    dev_review_rounds: Int,
    gate_rounds: Int,
    last_review_summary: String,
    last_gate_diagnostics: String,
    files_touched: List(String),
    summary: String,
  )
}

// --- parent output ---------------------------------------------------------

/// The parent `pipeline_run` workflow's result: the plan's execution strata,
/// each unit's terminal result, the ids landed in order, an overall
/// disposition, and a human summary.
pub type PipelineResult {
  PipelineResult(
    disposition: Disposition,
    strata: List(List(String)),
    units: List(UnitResult),
    landed: List(String),
    summary: String,
  )
}

// --- typed errors ----------------------------------------------------------

/// Parent/child workflow failures, surfaced as typed data in the run history.
pub type PipelineError {
  /// A named stage (scout, plan, provision, gate, land, notify, dev, review)
  /// failed as an activity error.
  StageFailed(stage: String, message: String)
  /// The plan the plan-agent proposed is not a runnable DAG (see [`StackError`]).
  StackInvalid(reason: String)
  /// A child `pipeline_unit` run failed at the engine/child boundary (as
  /// opposed to returning a terminal cap-exhaustion disposition, which is not
  /// an error).
  StackFailed(reason: String)
  /// The workflow input could not be decoded into a [`PipelineBrief`].
  DecodeInputFailed(message: String)
}

/// Why a proposed stack is not a runnable DAG. Every variant names the offending
/// unit(s) so the failure is actionable, not "invalid".
pub type StackError {
  /// A `unit_id` appears more than once in the plan.
  DuplicateUnit(unit_id: String)
  /// A unit's `depends_on` names a unit id absent from the plan.
  UnknownDependency(unit_id: String, missing: String)
  /// A unit lists itself in its own `depends_on`.
  SelfDependency(unit_id: String)
  /// The remaining units form one or more cycles; no stratum can be extracted.
  DependencyCycle(remaining: List(String))
  /// The plan proposes no units at all.
  EmptyPlan
}

/// The disposition option carried when no unit was run for a slot (a placeholder
/// that reads honestly rather than faking a pass).
pub fn disposition_to_string(disposition: Disposition) -> String {
  case disposition {
    Passed -> "passed"
    ReviewCapExhausted -> "review_cap_exhausted"
    GateCapExhausted -> "gate_cap_exhausted"
  }
}

/// Resolve a disposition tag back to the typed value; unknown tags fail decode
/// upstream, so this maps only the three known tags and treats anything else as
/// the honest worst case (`GateCapExhausted` is never fabricated — callers pass
/// only validated tags).
pub fn disposition_from_string(tag: String) -> Option(Disposition) {
  case tag {
    "passed" -> option.Some(Passed)
    "review_cap_exhausted" -> option.Some(ReviewCapExhausted)
    "gate_cap_exhausted" -> option.Some(GateCapExhausted)
    _ -> option.None
  }
}
