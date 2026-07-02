//// Boundary types for the {{name}} dev pipeline — the authored source of
//// truth (ADR-014, types-first).
////
//// Declare types only here: `aion generate` derives the JSON codecs
//// (`src/{{name}}_codecs.gleam`) and the emitted `schemas/*.json` artifacts
//// from these types. Edit a type, run `aion generate`, and commit the type
//// with its regenerated artifacts together. One input/output pair per
//// workflow entry: the pipeline parent (`Input`/`Output`), the dev child
//// (`DevInput`/`DevOutput`), and the gate child (`GateInput`/`GateOutput`).
//// The workspace shapes are duplicated per entry rather than shared so each
//// entry's wire contract stands alone.

import gleam/option

/// Top-level pipeline input. Every loop cap, backoff, and deadline is a
/// required field — the caller decides them, the workflow bakes no defaults.
pub type Input {
  Input(
    repo_root: String,
    brief_id: String,
    reviewers: List(String),
    base_ref: String,
    placement: InputPlacement,
    isolation: InputIsolation,
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

/// Where the provisioned workspace runs.
pub type InputPlacement {
  InputPlacementLocal
  InputPlacementRemote
}

/// How the provisioned workspace is isolated.
pub type InputIsolation {
  InputIsolationWorktree
  InputIsolationCopy
  InputIsolationOverlay
  InputIsolationVm
}

/// Top-level pipeline output.
pub type Output {
  Output(
    branch: String,
    merged_into: String,
    session_id: String,
    build_warm: OutputBuildWarm,
    verify_rounds: Int,
    review_rounds: Int,
  )
}

/// The warm-build result recorded on the pipeline output.
pub type OutputBuildWarm {
  OutputBuildWarm(ok: Bool, duration_ms: Int)
}

/// Input of the dev child workflow.
pub type DevInput {
  DevInput(
    workspace: DevInputWorkspace,
    brief: String,
    design: String,
    checklist: String,
    stories: List(String),
    verify_fix_cap: Int,
    round_backoff_ms: Int,
  )
}

/// The provisioned workspace the dev child works in.
pub type DevInputWorkspace {
  DevInputWorkspace(
    path: String,
    branch: String,
    placement: DevInputWorkspacePlacement,
    isolation: DevInputWorkspaceIsolation,
  )
}

/// Where the dev child's workspace runs.
pub type DevInputWorkspacePlacement {
  DevInputWorkspacePlacementLocal
  DevInputWorkspacePlacementRemote
}

/// How the dev child's workspace is isolated.
pub type DevInputWorkspaceIsolation {
  DevInputWorkspaceIsolationWorktree
  DevInputWorkspaceIsolationCopy
  DevInputWorkspaceIsolationOverlay
  DevInputWorkspaceIsolationVm
}

/// Output of the dev child workflow.
pub type DevOutput {
  DevOutput(
    dev_result: DevOutputDevResult,
    build_warm: DevOutputBuildWarm,
    verify_rounds: Int,
  )
}

/// The dev agent's session result.
pub type DevOutputDevResult {
  DevOutputDevResult(
    session_id: String,
    files_touched: List(String),
    summary: String,
  )
}

/// The warm-build result recorded on the dev child output.
pub type DevOutputBuildWarm {
  DevOutputBuildWarm(ok: Bool, duration_ms: Int)
}

/// Input of the gate child workflow. The gate runs workspace-wide today;
/// the affected_closure scope is a typed seam awaiting the trusted
/// graph-derived closure.
pub type GateInput {
  GateInput(
    workspace: GateInputWorkspace,
    files_touched: List(String),
    scope: GateInputScope,
  )
}

/// The workspace the gate sweeps.
pub type GateInputWorkspace {
  GateInputWorkspace(
    path: String,
    branch: String,
    placement: GateInputWorkspacePlacement,
    isolation: GateInputWorkspaceIsolation,
  )
}

/// Where the gate's workspace runs.
pub type GateInputWorkspacePlacement {
  GateInputWorkspacePlacementLocal
  GateInputWorkspacePlacementRemote
}

/// How the gate's workspace is isolated.
pub type GateInputWorkspaceIsolation {
  GateInputWorkspaceIsolationWorktree
  GateInputWorkspaceIsolationCopy
  GateInputWorkspaceIsolationOverlay
  GateInputWorkspaceIsolationVm
}

/// The gate's check scope. The type cannot express "`modules` is required
/// exactly when `kind` is `affected_closure`"; `{{name}}/io_convert`
/// enforces that invariant at the boundary.
pub type GateInputScope {
  GateInputScope(
    kind: GateInputScopeKind,
    modules: option.Option(List(String)),
  )
}

/// Which closure the gate checks.
pub type GateInputScopeKind {
  GateInputScopeKindWorkspaceWide
  GateInputScopeKindAffectedClosure
}

/// Output of the gate child workflow.
pub type GateOutput {
  GateOutput(verdict: GateOutputVerdict)
}

/// The gate's verdict. The type cannot express "`report` is required exactly
/// when `outcome` is `fail`"; `{{name}}/io_convert` enforces that invariant
/// at the boundary.
pub type GateOutputVerdict {
  GateOutputVerdict(
    outcome: GateOutputVerdictOutcome,
    report: option.Option(String),
  )
}

/// Whether the gate passed.
pub type GateOutputVerdictOutcome {
  GateOutputVerdictOutcomePass
  GateOutputVerdictOutcomeFail
}
