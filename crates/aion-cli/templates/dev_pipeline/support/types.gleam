//// Shared domain types for the dev-pipeline workflow family.
////
//// Every type that crosses the engine boundary (workflow inputs/outputs,
//// activity inputs/outputs, the review signal payload, and typed workflow
//// errors) lives here so the three workflow modules and the activity layer
//// share one vocabulary. Codecs live in `{{name}}/codecs_core` and
//// `{{name}}/codecs_flow`.

/// Where the provisioned workspace runs.
pub type Placement {
  Local
  Remote
}

/// How the provisioned workspace is isolated from the source repository.
///
/// Only `Worktree` has a working local implementation today; the other
/// variants are typed seams for Meridian's exchange-VM/CoW dispatch.
pub type Isolation {
  Worktree
  Copy
  Overlay
  Vm
}

/// Input to the `provision_workspace` activity.
pub type ProvisionInput {
  ProvisionInput(
    repo_root: String,
    brief_id: String,
    base_ref: String,
    placement: Placement,
    isolation: Isolation,
  )
}

/// A provisioned, isolated workspace.
///
/// Seam point (brief section 4): downstream steps must not care which
/// isolation mode produced the workspace — only that they hold one.
pub type Workspace {
  Workspace(
    path: String,
    branch: String,
    placement: Placement,
    isolation: Isolation,
  )
}

/// Advisory result of the `warm_build` activity.
///
/// Resolves open question Q4 (warm cache): the warm build is advisory data,
/// never a run-failing error — `ok: False` simply forfeits the warm cache.
/// // TODO(meridian): decide whether the warmed target dir can be shared
/// // with `gate`/`scoped_checks` under Copy/Overlay/Vm isolation, or
/// // whether CoW/VM boundaries break cache sharing and make `warm_build`
/// // worthless in those modes.
pub type BuildWarm {
  BuildWarm(ok: Bool, duration_ms: Int)
}

/// Input to the `dev` activity (norn run).
pub type DevInput {
  DevInput(
    workspace: Workspace,
    brief: String,
    design: String,
    checklist: String,
    stories: List(String),
  )
}

/// Result of a dev round. `session_id` is essential: later rounds resume the
/// same agent session with feedback instead of starting over.
pub type DevResult {
  DevResult(session_id: String, files_touched: List(String), summary: String)
}

/// Envelope input for the concurrent startup fan-out.
///
/// `workflow.all` collects a homogeneous activity list, so the two startup
/// activities (`warm_build` and `dev`) share this tagged input type. Each
/// deployed worker receives only its own variant.
pub type StartupTask {
  WarmTask(workspace: Workspace)
  DevTask(dev_input: DevInput)
}

/// Envelope output for the concurrent startup fan-out, mirroring
/// `StartupTask`: `warm_build` answers `Warmed`, `dev` answers `Developed`.
pub type StartupResult {
  Warmed(build_warm: BuildWarm)
  Developed(dev_result: DevResult)
}

/// Input to the `scoped_checks` activity: the fast inner verification loop
/// limited to the modules affected by the touched files.
pub type ScopedInput {
  ScopedInput(workspace: Workspace, files_touched: List(String))
}

/// Verdict of one scoped check round.
pub type CheckVerdict {
  CheckPass
  CheckFail(diagnostics: String)
}

/// Result of the `scoped_checks` activity.
///
/// Resolves open question Q1 (scoping seam): the affected set is computed by
/// the CLI the activity shells to, and the workflow stays pure — it only
/// consumes `affected_modules` from this result. `checked_scope` names the
/// scope that actually ran, so a loud workspace-wide fallback is visible
/// data, never a silent widening.
pub type CheckResult {
  CheckResult(
    verdict: CheckVerdict,
    affected_modules: List(String),
    checked_scope: String,
  )
}

/// Input to the `dev_resume` activity: scoped-check diagnostics or encoded
/// review notes, fed back into the same agent session.
pub type ResumeInput {
  ResumeInput(session_id: String, feedback: String)
}

/// Scope of the authoritative gate run.
///
/// Resolves open question Q2 (gate scope): the gate runs workspace-wide
/// today; `AffectedClosure` is the typed seam for a complete-but-narrower
/// graph-derived scope. Only `WorkspaceWide` is exercised — nothing guessed.
pub type GateScope {
  WorkspaceWide
  AffectedClosure(modules: List(String))
}

/// Input to the `gate` child workflow and its `full_checks` activity.
pub type GateInput {
  GateInput(workspace: Workspace, files_touched: List(String), scope: GateScope)
}

/// Verdict of the authoritative gate.
pub type GateVerdict {
  GatePass
  GateFail(report: String)
}

/// Output of the `gate` child workflow. A failing gate is recorded data; the
/// parent decides what a `GateFail` means for the run.
pub type GateResult {
  GateResult(verdict: GateVerdict)
}

/// Typed error of the `gate` child workflow: the checks could not be
/// executed at all (infrastructure), as opposed to executing and failing.
pub type GateError {
  GateStageFailed(stage: String, message: String)
}

/// Input to the `request_review` activity. It only requests; the verdict
/// arrives later on the `review_verdict` signal.
pub type ReviewRequest {
  ReviewRequest(
    workspace: Workspace,
    brief_id: String,
    dev_result: DevResult,
    gate_result: GateResult,
  )
}

/// Acknowledgement that a review request was emitted.
pub type ReviewAck {
  ReviewAck(request_id: String)
}

/// One structured review finding.
///
/// Resolves open question Q3 (verdict payload): the verdict is structured
/// per-finding data that `dev_resume` consumes directly, not a bare string.
pub type ReviewNote {
  ReviewNote(file: String, line: Int, note: String)
}

/// The reviewer's decision carried by the `review_verdict` signal.
pub type ReviewDecision {
  Approve
  RequestChanges(notes: List(ReviewNote))
  Reject(reason: String)
}

/// Payload of the `review_verdict` signal.
pub type ReviewVerdict {
  ReviewVerdict(decision: ReviewDecision)
}

/// Input to the `land` activity: an approved workspace and the dev result
/// whose work is being landed.
pub type LandInput {
  LandInput(workspace: Workspace, dev_result: DevResult)
}

/// Output of the `land` activity.
pub type Landed {
  Landed(pr_url: String, merge_commit: String)
}

/// Input to the `{{name}}_dev` child workflow (also independently
/// dispatchable as a top-level run — open question Q6).
///
/// `verify_fix_cap` and `round_backoff_ms` are required inputs, never baked
/// defaults (open question Q5).
pub type DevFlowInput {
  DevFlowInput(
    workspace: Workspace,
    brief: String,
    design: String,
    checklist: String,
    stories: List(String),
    verify_fix_cap: Int,
    round_backoff_ms: Int,
  )
}

/// Output of the `{{name}}_dev` child workflow: the converged dev result,
/// the advisory warm-build outcome, and how many verify rounds it took.
pub type DevFlowResult {
  DevFlowResult(
    dev_result: DevResult,
    build_warm: BuildWarm,
    verify_rounds: Int,
  )
}

/// Typed errors of the `{{name}}_dev` child workflow.
pub type DevFlowError {
  /// The concurrent warm-build/dev startup fan-out failed.
  StartupFailed(message: String)
  /// The bounded verify-fix loop spent its attempt budget; carries the last
  /// scoped-check diagnostics so the failure is actionable.
  VerifyFixExhausted(rounds: Int, diagnostics: String)
  /// Any other stage failure, tagged with the stage that raised it.
  DevFlowStageFailed(stage: String, message: String)
}

/// Input to the top-level pipeline workflow.
///
/// Resolves open question Q5 (loop caps and backoff): `verify_fix_cap`,
/// `review_cap`, `round_backoff_ms`, and `review_deadline_ms` are REQUIRED
/// input fields. The no-arbitrary-defaults rule applies to workflow inputs:
/// the caller decides every cap, backoff, and deadline.
pub type PipelineInput {
  PipelineInput(
    repo_root: String,
    brief_id: String,
    base_ref: String,
    placement: Placement,
    isolation: Isolation,
    brief: String,
    design: String,
    checklist: String,
    stories: List(String),
    verify_fix_cap: Int,
    review_cap: Int,
    round_backoff_ms: Int,
    review_deadline_ms: Int,
  )
}

/// Output of a landed pipeline run.
pub type PipelineResult {
  PipelineResult(
    pr_url: String,
    merge_commit: String,
    session_id: String,
    build_warm: BuildWarm,
    verify_rounds: Int,
    review_rounds: Int,
  )
}

/// Typed errors of the top-level pipeline workflow.
pub type PipelineError {
  /// Workspace provisioning failed.
  ProvisionFailed(message: String)
  /// The `{{name}}_dev` child failed outside its verify-fix budget.
  DevFailed(message: String)
  /// The child's verify-fix loop spent its budget; lifted from
  /// `VerifyFixExhausted` with the last diagnostics attached.
  VerifyExhausted(rounds: Int, diagnostics: String)
  /// The authoritative gate executed and failed. A converged verify loop
  /// that still fails the gate surfaces loudly instead of looping.
  GateRejected(report: String)
  /// The reviewer rejected the work.
  ReviewRejected(reason: String)
  /// No verdict arrived before the durable review deadline.
  ReviewTimedOut(deadline_ms: Int)
  /// The bounded review loop spent its round budget.
  ReviewCapExhausted(rounds: Int)
  /// Submitting or landing the approved stack failed.
  LandFailed(message: String)
  /// Any other stage failure, tagged with the stage that raised it.
  StageFailed(stage: String, message: String)
}

/// Live status answered by the `{{name}}_status` query.
pub type PipelineStatus {
  PipelineStatus(phase: String, round: Int)
}

/// Live status answered by the `{{name}}_dev_status` query.
pub type DevFlowStatus {
  DevFlowStatus(phase: String, round: Int)
}
