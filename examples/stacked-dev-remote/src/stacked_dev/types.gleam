//// Shared domain types for the stacked-dev workflow family.
////
//// Every type that crosses the engine boundary (workflow inputs/outputs,
//// activity inputs/outputs, the review signal payload, and typed workflow
//// errors) lives here so the three workflow modules and the activity layer
//// share one vocabulary. Codecs live in `stacked_dev/codecs_core` and
//// `stacked_dev/codecs_flow`.

import aion_stacked_dev_io as stage_io
import gleam/option

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
    clone_url: option.Option(String),
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

/// Input to the `dev` activity (norn run). The prompt is the projected dev
/// prompt built in workflow code from BD-002's pure functions — the four
/// document strings no longer ride this input (BD-003).
pub type DevInput {
  DevInput(workspace: Workspace, prompt: String)
}

/// Input to the `scout` activity (read-only norn run). The prompt is the
/// projected scout prompt built in workflow code from BD-002's pure
/// functions (BD-003).
pub type ScoutInput {
  ScoutInput(workspace: Workspace, prompt: String)
}

/// Input to the `dev_review` activity (adversarial reviewer norn run). The
/// prompt is the projected review prompt built in workflow code from BD-002's
/// pure functions (BD-003).
pub type ReviewInput {
  ReviewInput(workspace: Workspace, prompt: String)
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
/// `Developed` now carries the dev-stage report (the dev/dev_resume activities
/// return the dev-report shape — BD-003).
pub type StartupResult {
  Warmed(build_warm: BuildWarm)
  Developed(dev_report: stage_io.DevReport)
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
    reviewers: List(String),
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

/// Input to the `land` activity: an approved workspace, the repository the
/// merge runs from, and the dev result whose work is being landed.
///
/// `repo_root` matters: `yg branch merge` removes the branch's worktree as
/// part of landing, so it must run from the main repository — run from
/// inside the worktree it deletes its own git context mid-merge (confirmed
/// live, 2026-06-13).
pub type LandInput {
  LandInput(
    workspace: Workspace,
    repo_root: String,
    base_ref: String,
    dev_result: DevResult,
    clone_url: option.Option(String),
  )
}

/// Output of the `land` activity.
pub type Landed {
  Landed(branch: String, merged_into: String)
}

/// Input to the `teardown_workspace` activity: the workspace to clean up
/// and the repo root (needed for yg branch teardown on local placement).
pub type TeardownInput {
  TeardownInput(workspace: Workspace, repo_root: String)
}

/// Output of the `teardown_workspace` activity.
pub type TornDown {
  TornDown(branch: String, cleaned: Bool)
}

/// Input to the `brief_dev` child workflow (also independently dispatchable
/// as a top-level run). It carries the v2 brief document and the pre-resolved
/// reference context in place of the four document strings (ADR-008).
///
/// `verify_fix_cap` and `round_backoff_ms` are required inputs, never baked
/// defaults (CN2, ADR-001/ADR-003).
pub type BriefDevInput {
  BriefDevInput(
    workspace: Workspace,
    document: BriefDocument,
    context: ResolvedContext,
    verify_fix_cap: Int,
    round_backoff_ms: Int,
  )
}

/// Output of the `brief_dev` child workflow: the three stage reports (the
/// generated stage contracts), how many verify rounds it took, and the
/// advisory warm-build outcome (C18).
pub type BriefDevResult {
  BriefDevResult(
    scout: stage_io.ScoutReport,
    dev: stage_io.DevReport,
    review: stage_io.ReviewReport,
    verify_rounds: Int,
    build_warm: BuildWarm,
  )
}

/// One review-found requirement left `drifted` after the harden pass: its R#
/// id and the issues the reviewer recorded. Carried by `ReviewDrifted`.
pub type DriftedRequirement {
  DriftedRequirement(id: String, issues: List(String))
}

/// Typed errors of the `brief_dev` child workflow (C18, plus the C15-mandated
/// `HardenRegressed`). Exactly six constructors.
pub type BriefDevError {
  /// The read-only scout stage failed to execute.
  ScoutFailed(message: String)
  /// The dev report marked one or more requirements `blocked`; carries those
  /// R# ids.
  DevBlocked(requirement_ids: List(String))
  /// The bounded verify-fix loop spent its attempt budget; carries the last
  /// scoped-check diagnostics so the failure is actionable.
  VerifyFixExhausted(rounds: Int, diagnostics: String)
  /// The review report left one or more requirements `drifted` after the
  /// harden pass; carries each drifted requirement's id and issues.
  ReviewDrifted(drifted: List(DriftedRequirement))
  /// A harden pass (re-running scoped checks after review fixes) broke
  /// verification; carries the regression diagnostics (C15).
  HardenRegressed(diagnostics: String)
  /// Any other stage failure, tagged with the stage that raised it. The
  /// startup fan-out shape violation folds into this variant.
  BriefDevStageFailed(stage: String, message: String)
}

/// Input to the `stacked_dev` top-level workflow.
///
/// Resolves open question Q5 (loop caps and backoff): `verify_fix_cap`,
/// `review_cap`, `round_backoff_ms`, and `review_deadline_ms` are REQUIRED
/// input fields. The no-arbitrary-defaults rule applies to workflow inputs:
/// the caller decides every cap, backoff, and deadline.
pub type StackedDevInput {
  StackedDevInput(
    repo_root: String,
    brief_id: String,
    reviewers: List(String),
    base_ref: String,
    placement: Placement,
    isolation: Isolation,
    brief_document: BriefDocument,
    resolved_context: ResolvedContext,
    verify_fix_cap: Int,
    review_cap: Int,
    round_backoff_ms: Int,
    review_deadline_ms: Int,
    clone_url: option.Option(String),
  )
}

/// Output of a landed `stacked_dev` run.
pub type StackedDevResult {
  StackedDevResult(
    branch: String,
    merged_into: String,
    session_id: String,
    build_warm: BuildWarm,
    verify_rounds: Int,
    review_rounds: Int,
  )
}

/// Typed errors of the `stacked_dev` top-level workflow. The `brief_dev`
/// child's typed errors are lifted here variant-by-variant, payloads intact
/// (BD-005 R4): scout failure, dev block, verify exhaustion, review drift,
/// harden regression, and stage failure each have a distinct lifting.
pub type StackedDevError {
  /// Workspace provisioning failed.
  ProvisionFailed(message: String)
  /// The `brief_dev` child's read-only scout stage failed; lifted from
  /// `ScoutFailed`.
  ScoutFailedInChild(message: String)
  /// The `brief_dev` child reported one or more requirements `blocked`;
  /// lifted from `DevBlocked` with those R# ids attached.
  DevBlockedInChild(requirement_ids: List(String))
  /// The `brief_dev` child failed outside the typed taxonomy below.
  DevFailed(message: String)
  /// The child's verify-fix loop spent its budget; lifted from
  /// `VerifyFixExhausted` with the last diagnostics attached.
  VerifyExhausted(rounds: Int, diagnostics: String)
  /// The child's review report left one or more requirements `drifted`;
  /// lifted from `ReviewDrifted` with every drifted R# id and its issues.
  ReviewDriftedInChild(drifted: List(DriftedRequirement))
  /// The child's harden pass broke verification; lifted from
  /// `HardenRegressed` with the regression diagnostics.
  HardenRegressedInChild(diagnostics: String)
  /// The authoritative gate executed and failed. A converged verify loop
  /// that still fails the gate surfaces loudly instead of looping.
  GateRejected(report: String)
  /// The reviewer rejected the work.
  ReviewRejected(reason: String)
  /// No verdict arrived before the durable review deadline.
  ReviewTimedOut(deadline_ms: Int)
  /// The bounded review loop spent its round budget.
  ReviewCapExhausted(rounds: Int)
  /// Landing (merging the approved branch into its tree parent) failed.
  LandFailed(message: String)
  /// Any other stage failure, tagged with the stage that raised it.
  StageFailed(stage: String, message: String)
}

/// Live status answered by the `stacked_dev_status` query.
pub type StackedDevStatus {
  StackedDevStatus(phase: String, round: Int)
}

/// Live status answered by the `brief_dev_status` query.
pub type BriefDevStatus {
  BriefDevStatus(phase: String, round: Int)
}

/// A v2 implementation brief: a self-contained unit of work AND its
/// execution record, one living document (ADR-007). Mirrors
/// `docs/design-system/schemas/brief.schema.json` field-for-field. Authors
/// write every non-optional field; the pipeline appends the optional
/// enrichment blocks in place and never rewrites authored fields. Emptiness
/// is authored, never defaulted (ADR-001).
pub type BriefDocument {
  BriefDocument(
    id: String,
    cluster: String,
    title: String,
    depends_on: List(String),
    blocked_by: List(String),
    checklist: List(String),
    stories: List(String),
    design_anchor: List(String),
    purpose: String,
    task: String,
    requirements: List(BriefRequirement),
    boundaries: List(String),
    verification: List(String),
    execution: option.Option(ExecutionBlock),
  )
}

/// One numbered requirement (R#) of a brief. The `scout`, `dev`, and
/// `review` blocks are appended by the pipeline stages; everything else is
/// authored.
pub type BriefRequirement {
  BriefRequirement(
    id: String,
    title: String,
    spec: String,
    acceptance: List(String),
    files: RequirementFiles,
    checklist: List(String),
    stories: List(String),
    scout: option.Option(ScoutBlock),
    dev: option.Option(DevBlock),
    review: option.Option(ReviewBlock),
  )
}

/// The authored file plan of a requirement.
pub type RequirementFiles {
  RequirementFiles(
    create: List(String),
    modify: List(String),
    delete: List(String),
  )
}

/// Scout-stage findings appended to a requirement.
pub type ScoutBlock {
  ScoutBlock(
    files: List(String),
    context: List(String),
    approach: String,
    notes: String,
  )
}

/// Dev-stage record appended to a requirement.
pub type DevBlock {
  DevBlock(
    status: DevStatus,
    files_changed: List(FileChange),
    how: String,
    deviation: String,
    checklist: List(ChecklistClaim),
    stories: List(StoryClaim),
  )
}

/// Outcome of the dev stage for one requirement.
pub type DevStatus {
  Implemented
  Blocked
}

/// One file the dev stage touched.
pub type FileChange {
  FileChange(path: String, change: ChangeKind, note: String)
}

/// What the dev stage did to a file.
pub type ChangeKind {
  Created
  Modified
  Deleted
}

/// The dev agent's delivery claim for one checklist item.
pub type ChecklistClaim {
  ChecklistClaim(id: String, done: Bool, note: String)
}

/// The dev agent's satisfaction claim for one user story.
pub type StoryClaim {
  StoryClaim(id: String, satisfied: Bool, note: String)
}

/// Adversarial-review verdict appended to a requirement.
pub type ReviewBlock {
  ReviewBlock(
    alignment: Alignment,
    acceptance: List(AcceptanceVerdict),
    checklist: List(String),
    stories: List(String),
    issues: List(String),
    fixes: List(String),
  )
}

/// How the implementation relates to the spec after review.
pub type Alignment {
  Aligned
  Drifted
  Fixed
}

/// The reviewer's verdict on one acceptance criterion.
pub type AcceptanceVerdict {
  AcceptanceVerdict(criterion: String, met: Bool, evidence: String)
}

/// Run-level execution record appended to a brief before land. The gate
/// block holds what the workflow MEASURED; the attestation block holds what
/// the dev agent BELIEVED — divergence between them is review signal.
pub type ExecutionBlock {
  ExecutionBlock(
    status: ExecutionStatus,
    workflow_id: String,
    branch: String,
    session_id: String,
    gate: GateBlock,
    attestation: AttestationBlock,
    review_verdict: ExecutionVerdict,
    landed_commit: String,
    merged_into: String,
    completed_at: String,
  )
}

/// Run status recorded in the execution block. Constructors are prefixed
/// because this module already defines a `Landed` constructor.
pub type ExecutionStatus {
  ExecutionInFlight
  ExecutionLanded
  ExecutionFailed
}

/// Human (or quorum) review verdict recorded in the execution block.
/// Constructors are prefixed because `ReviewDecision` already defines
/// `Approve`/`Reject`.
pub type ExecutionVerdict {
  VerdictApproved
  VerdictChangesRequested
  VerdictRejected
}

/// What the workflow measured at the authoritative gate.
pub type GateBlock {
  GateBlock(fmt: Bool, clippy: Bool, tests: Bool, fix_rounds: Int)
}

/// What the dev agent attested — never trusted as the gate (P1).
pub type AttestationBlock {
  AttestationBlock(
    no_panics: Bool,
    no_unsafe: Bool,
    boundaries_respected: Bool,
    tests_pass: Bool,
  )
}

/// One ADR resolved to its text at dispatch time (CN1). `decided_by` rides
/// along so projections can attribute quotes to their speaker (P6).
pub type ResolvedAdr {
  ResolvedAdr(
    id: String,
    title: String,
    decision: String,
    quote: String,
    decided_by: String,
  )
}

/// One checklist item, story, or constraint resolved to its text.
pub type ResolvedItem {
  ResolvedItem(id: String, text: String)
}

/// Where the briefed work came from: the roadmap requester and their
/// verbatim words (P6).
pub type ResolvedProvenance {
  ResolvedProvenance(requested_by: String, quote: String)
}

/// Input to the `enrich_brief` activity: the workspace whose worktree holds
/// the brief file, the brief document the workflow currently carries, and
/// the stage payload to append. One activity serves all four write points —
/// the `Enrichment` variant selects the merge.
pub type EnrichInput {
  EnrichInput(
    workspace: Workspace,
    document: BriefDocument,
    enrichment: Enrichment,
  )
}

/// The stage payload appended by one `enrich_brief` call. The report types
/// are BD-001's generated stage contracts and `ExecutionBlock` is BD-001's
/// brief type, consumed as-is. `ExecutionEnrichment` carries the measured
/// gate results and the believed dev attestation as the separate fields
/// `ExecutionBlock` defines — never collapsed into one flag (P1).
pub type Enrichment {
  ScoutEnrichment(report: stage_io.ScoutReport)
  DevEnrichment(report: stage_io.DevReport)
  ReviewEnrichment(report: stage_io.ReviewReport)
  ExecutionEnrichment(block: ExecutionBlock)
}

/// Pre-resolved reference context assembled by dispatcher activities before
/// any workflow logic sees it (CN1): the brief's anchored ADR texts, C#/S#
/// texts, cluster constraints, the cluster intention, and the design file
/// reference every stage prompt carries.
pub type ResolvedContext {
  ResolvedContext(
    adrs: List(ResolvedAdr),
    checklist: List(ResolvedItem),
    stories: List(ResolvedItem),
    constraints: List(ResolvedItem),
    intention: String,
    design_path: String,
    provenance: ResolvedProvenance,
  )
}

// --- dispatch wave types (BD-006) -------------------------------------------

/// Input to the `assemble_wave` dispatcher activity: the design directory the
/// ledgers and cluster documents live under, and the wave as an ordered list
/// of brief ids. This is the ONLY place ledger reading and reference
/// resolution enter the family (CN1) — the activity returns a fully resolved,
/// dependency-ordered `AssembledWave` the dispatch workflow body consumes.
pub type AssembleInput {
  AssembleInput(design_dir: String, wave: List(String))
}

/// One assembled wave entry: a decoded v2 brief document and the reference
/// context the dispatcher resolved for it (CN1). The dispatch workflow turns
/// each entry into a `StackedDevInput` for one `stacked_dev` child.
pub type WaveEntry {
  WaveEntry(brief_document: BriefDocument, resolved_context: ResolvedContext)
}

/// Output of the `assemble_wave` activity: the wave entries in dependency
/// order (every within-wave `depends_on` precedes its dependent, the caller's
/// order preserved among independents).
pub type AssembledWave {
  AssembledWave(entries: List(WaveEntry))
}

/// Input to the `dispatch` top-level workflow: a wave of brief ids plus the
/// shared child parameters every `stacked_dev` child needs and a required
/// `halt_on_failure`. Every field is required by construction — no cap,
/// deadline, or flag is defaulted (ADR-001), and there is deliberately no
/// concurrency-limit field: delivery is serial-only until parent-close
/// (RM-001/ADR-004) gives the dispatcher cancellation over in-flight children.
pub type DispatchInput {
  DispatchInput(
    design_dir: String,
    wave: List(String),
    repo_root: String,
    base_ref: String,
    reviewers: List(String),
    placement: Placement,
    isolation: Isolation,
    verify_fix_cap: Int,
    review_cap: Int,
    round_backoff_ms: Int,
    review_deadline_ms: Int,
    halt_on_failure: Bool,
    clone_url: option.Option(String),
  )
}

/// One brief's machine-readable outcome in a dispatched wave. A child's typed
/// failure is recorded data here, never a dispatch failure (P3): `BriefFailed`
/// embeds the child's `StackedDevError` itself, not a stringified message.
pub type BriefOutcome {
  /// The brief's `stacked_dev` child landed: the merged branch and its tree
  /// parent. No commit hash rides along — the live-proven land contract
  /// returns none and extending it is barred (CN8).
  BriefLanded(brief_id: String, branch: String, merged_into: String)
  /// The brief's child failed with a typed error, carried verbatim.
  BriefFailed(brief_id: String, error: StackedDevError)
  /// The brief was never started because an earlier failure halted the wave;
  /// `after` names the failed brief that stopped it.
  BriefSkipped(brief_id: String, after: String)
}

/// Output of a dispatched wave: exactly one outcome per wave entry, in wave
/// order.
pub type DispatchResult {
  DispatchResult(outcomes: List(BriefOutcome))
}

/// Typed errors of the `dispatch` workflow. Only can't-execute conditions are
/// dispatch errors: an `assemble_wave` failure becomes `AssemblyRefused`, and
/// engine-level child errors (output/error decode failures, engine failures)
/// become `DispatchStageFailed`. A child's typed business failure is recorded
/// as a `BriefFailed` outcome, never lifted here (P3).
pub type DispatchError {
  /// `assemble_wave` refused or could not assemble the wave.
  AssemblyRefused(message: String)
  /// An engine-level failure spawning or awaiting a child, tagged with the
  /// stage that raised it.
  DispatchStageFailed(stage: String, message: String)
}

/// Live status answered by the `dispatch_status` query: the brief currently in
/// flight, its 1-based position, the wave total, and the per-brief outcomes
/// recorded so far.
pub type DispatchStatus {
  DispatchStatus(
    current_brief: String,
    position: Int,
    total: Int,
    outcomes: List(BriefOutcome),
  )
}
