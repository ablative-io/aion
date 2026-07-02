//// Conversions between the authored wire types (`{{name}}_io`, the
//// types-first source of truth `aion generate` derives the codecs and
//// schemas from) and the hand-written domain types in `{{name}}/types`.
////
//// The types module owns every workflow-level wire shape; the domain
//// types stay the in-workflow vocabulary the three workflow bodies and the
//// activity layer share. Each conversion is total field-by-field code, so a
//// type edit that changes a wire shape breaks compilation HERE — the
//// drift is a compile error, never a runtime surprise. The two genuinely
//// partial conversions (a gate `fail` verdict without its report, an
//// `affected_closure` scope without its modules) return `Error` with the
//// reason, which `{{name}}/codecs_workflows` surfaces as a decode
//// failure.

import {{name}}_io as generated
import gleam/option
import {{name}}/types.{
  type DevFlowInput, type DevFlowResult,
  type GateInput, type GateResult, type GateScope, type GateVerdict,
  type Isolation, type PipelineInput, type PipelineResult, type Placement,
  type Workspace, AffectedClosure, BuildWarm, Copy, DevFlowInput,
  DevFlowResult, DevResult, GateFail, GateInput, GatePass, GateResult, Local,
  Overlay, PipelineInput, PipelineResult, Remote, Vm, Workspace,
  WorkspaceWide, Worktree,
}

// --- top-level pipeline input (schemas/input.json) ---------------------------

/// Domain view of a decoded top-level workflow input.
pub fn input_to_domain(input: generated.Input) -> PipelineInput {
  PipelineInput(
    repo_root: input.repo_root,
    brief_id: input.brief_id,
    reviewers: input.reviewers,
    base_ref: input.base_ref,
    placement: placement_from_input(input.placement),
    isolation: isolation_from_input(input.isolation),
    brief: input.brief,
    design: input.design,
    checklist: input.checklist,
    stories: input.stories,
    verify_fix_cap: input.verify_fix_cap,
    review_cap: input.review_cap,
    round_backoff_ms: input.round_backoff_ms,
    review_deadline_ms: input.review_deadline_ms,
  )
}

/// Wire view of a top-level workflow input.
pub fn input_from_domain(input: PipelineInput) -> generated.Input {
  generated.Input(
    repo_root: input.repo_root,
    brief_id: input.brief_id,
    reviewers: input.reviewers,
    base_ref: input.base_ref,
    placement: placement_to_input(input.placement),
    isolation: isolation_to_input(input.isolation),
    brief: input.brief,
    design: input.design,
    checklist: input.checklist,
    stories: input.stories,
    verify_fix_cap: input.verify_fix_cap,
    review_cap: input.review_cap,
    round_backoff_ms: input.round_backoff_ms,
    review_deadline_ms: input.review_deadline_ms,
  )
}

fn placement_from_input(placement: generated.InputPlacement) -> Placement {
  case placement {
    generated.InputPlacementLocal -> Local
    generated.InputPlacementRemote -> Remote
  }
}

fn placement_to_input(placement: Placement) -> generated.InputPlacement {
  case placement {
    Local -> generated.InputPlacementLocal
    Remote -> generated.InputPlacementRemote
  }
}

fn isolation_from_input(isolation: generated.InputIsolation) -> Isolation {
  case isolation {
    generated.InputIsolationWorktree -> Worktree
    generated.InputIsolationCopy -> Copy
    generated.InputIsolationOverlay -> Overlay
    generated.InputIsolationVm -> Vm
  }
}

fn isolation_to_input(isolation: Isolation) -> generated.InputIsolation {
  case isolation {
    Worktree -> generated.InputIsolationWorktree
    Copy -> generated.InputIsolationCopy
    Overlay -> generated.InputIsolationOverlay
    Vm -> generated.InputIsolationVm
  }
}

// --- top-level pipeline output (schemas/output.json) -------------------------

/// Domain view of a decoded top-level workflow output.
pub fn output_to_domain(output: generated.Output) -> PipelineResult {
  PipelineResult(
    branch: output.branch,
    merged_into: output.merged_into,
    session_id: output.session_id,
    build_warm: BuildWarm(
      ok: output.build_warm.ok,
      duration_ms: output.build_warm.duration_ms,
    ),
    verify_rounds: output.verify_rounds,
    review_rounds: output.review_rounds,
  )
}

/// Wire view of a top-level workflow output.
pub fn output_from_domain(result: PipelineResult) -> generated.Output {
  generated.Output(
    branch: result.branch,
    merged_into: result.merged_into,
    session_id: result.session_id,
    build_warm: generated.OutputBuildWarm(
      ok: result.build_warm.ok,
      duration_ms: result.build_warm.duration_ms,
    ),
    verify_rounds: result.verify_rounds,
    review_rounds: result.review_rounds,
  )
}

// --- dev child input (schemas/dev_input.json) --------------------------------

/// Domain view of a decoded dev child input.
pub fn dev_input_to_domain(input: generated.DevInput) -> DevFlowInput {
  DevFlowInput(
    workspace: workspace_from_dev(input.workspace),
    brief: input.brief,
    design: input.design,
    checklist: input.checklist,
    stories: input.stories,
    verify_fix_cap: input.verify_fix_cap,
    round_backoff_ms: input.round_backoff_ms,
  )
}

/// Wire view of a dev child input.
pub fn dev_input_from_domain(input: DevFlowInput) -> generated.DevInput {
  generated.DevInput(
    workspace: workspace_to_dev(input.workspace),
    brief: input.brief,
    design: input.design,
    checklist: input.checklist,
    stories: input.stories,
    verify_fix_cap: input.verify_fix_cap,
    round_backoff_ms: input.round_backoff_ms,
  )
}

fn workspace_from_dev(workspace: generated.DevInputWorkspace) -> Workspace {
  Workspace(
    path: workspace.path,
    branch: workspace.branch,
    placement: case workspace.placement {
      generated.DevInputWorkspacePlacementLocal -> Local
      generated.DevInputWorkspacePlacementRemote -> Remote
    },
    isolation: case workspace.isolation {
      generated.DevInputWorkspaceIsolationWorktree -> Worktree
      generated.DevInputWorkspaceIsolationCopy -> Copy
      generated.DevInputWorkspaceIsolationOverlay -> Overlay
      generated.DevInputWorkspaceIsolationVm -> Vm
    },
  )
}

fn workspace_to_dev(workspace: Workspace) -> generated.DevInputWorkspace {
  generated.DevInputWorkspace(
    path: workspace.path,
    branch: workspace.branch,
    placement: case workspace.placement {
      Local -> generated.DevInputWorkspacePlacementLocal
      Remote -> generated.DevInputWorkspacePlacementRemote
    },
    isolation: case workspace.isolation {
      Worktree -> generated.DevInputWorkspaceIsolationWorktree
      Copy -> generated.DevInputWorkspaceIsolationCopy
      Overlay -> generated.DevInputWorkspaceIsolationOverlay
      Vm -> generated.DevInputWorkspaceIsolationVm
    },
  )
}

// --- dev child output (schemas/dev_output.json) ------------------------------

/// Domain view of a decoded dev child output.
pub fn dev_output_to_domain(output: generated.DevOutput) -> DevFlowResult {
  DevFlowResult(
    dev_result: DevResult(
      session_id: output.dev_result.session_id,
      files_touched: output.dev_result.files_touched,
      summary: output.dev_result.summary,
    ),
    build_warm: BuildWarm(
      ok: output.build_warm.ok,
      duration_ms: output.build_warm.duration_ms,
    ),
    verify_rounds: output.verify_rounds,
  )
}

/// Wire view of a dev child output.
pub fn dev_output_from_domain(result: DevFlowResult) -> generated.DevOutput {
  generated.DevOutput(
    dev_result: generated.DevOutputDevResult(
      session_id: result.dev_result.session_id,
      files_touched: result.dev_result.files_touched,
      summary: result.dev_result.summary,
    ),
    build_warm: generated.DevOutputBuildWarm(
      ok: result.build_warm.ok,
      duration_ms: result.build_warm.duration_ms,
    ),
    verify_rounds: result.verify_rounds,
  )
}

// --- gate child input (schemas/gate_input.json) ------------------------------

/// Domain view of a decoded gate input. Partial: the schema cannot express
/// "`modules` is required exactly when `kind` is `affected_closure`", so the
/// invariant is enforced here and a violation is a decode failure upstream.
pub fn gate_input_to_domain(input: generated.GateInput) -> Result(
  GateInput,
  String,
) {
  case scope_to_domain(input.scope) {
    Ok(scope) ->
      Ok(GateInput(
        workspace: workspace_from_gate(input.workspace),
        files_touched: input.files_touched,
        scope: scope,
      ))
    Error(reason) -> Error(reason)
  }
}

/// Wire view of a gate input.
pub fn gate_input_from_domain(input: GateInput) -> generated.GateInput {
  generated.GateInput(
    workspace: workspace_to_gate(input.workspace),
    files_touched: input.files_touched,
    scope: scope_from_domain(input.scope),
  )
}

fn workspace_from_gate(workspace: generated.GateInputWorkspace) -> Workspace {
  Workspace(
    path: workspace.path,
    branch: workspace.branch,
    placement: case workspace.placement {
      generated.GateInputWorkspacePlacementLocal -> Local
      generated.GateInputWorkspacePlacementRemote -> Remote
    },
    isolation: case workspace.isolation {
      generated.GateInputWorkspaceIsolationWorktree -> Worktree
      generated.GateInputWorkspaceIsolationCopy -> Copy
      generated.GateInputWorkspaceIsolationOverlay -> Overlay
      generated.GateInputWorkspaceIsolationVm -> Vm
    },
  )
}

fn workspace_to_gate(workspace: Workspace) -> generated.GateInputWorkspace {
  generated.GateInputWorkspace(
    path: workspace.path,
    branch: workspace.branch,
    placement: case workspace.placement {
      Local -> generated.GateInputWorkspacePlacementLocal
      Remote -> generated.GateInputWorkspacePlacementRemote
    },
    isolation: case workspace.isolation {
      Worktree -> generated.GateInputWorkspaceIsolationWorktree
      Copy -> generated.GateInputWorkspaceIsolationCopy
      Overlay -> generated.GateInputWorkspaceIsolationOverlay
      Vm -> generated.GateInputWorkspaceIsolationVm
    },
  )
}

fn scope_to_domain(scope: generated.GateInputScope) -> Result(
  GateScope,
  String,
) {
  case scope.kind, scope.modules {
    generated.GateInputScopeKindWorkspaceWide, _ -> Ok(WorkspaceWide)
    generated.GateInputScopeKindAffectedClosure, option.Some(modules) ->
      Ok(AffectedClosure(modules: modules))
    generated.GateInputScopeKindAffectedClosure, option.None ->
      Error("an affected_closure gate scope requires its modules list")
  }
}

fn scope_from_domain(scope: GateScope) -> generated.GateInputScope {
  case scope {
    WorkspaceWide ->
      generated.GateInputScope(
        kind: generated.GateInputScopeKindWorkspaceWide,
        modules: option.None,
      )
    AffectedClosure(modules: modules) ->
      generated.GateInputScope(
        kind: generated.GateInputScopeKindAffectedClosure,
        modules: option.Some(modules),
      )
  }
}

// --- gate child output (schemas/gate_output.json) ----------------------------

/// Domain view of a decoded gate output. Partial: the schema cannot express
/// "`report` is required exactly when `outcome` is `fail`", so the invariant
/// is enforced here and a violation is a decode failure upstream.
pub fn gate_output_to_domain(output: generated.GateOutput) -> Result(
  GateResult,
  String,
) {
  case verdict_to_domain(output.verdict) {
    Ok(verdict) -> Ok(GateResult(verdict: verdict))
    Error(reason) -> Error(reason)
  }
}

/// Wire view of a gate output.
pub fn gate_output_from_domain(result: GateResult) -> generated.GateOutput {
  generated.GateOutput(verdict: verdict_from_domain(result.verdict))
}

fn verdict_to_domain(verdict: generated.GateOutputVerdict) -> Result(
  GateVerdict,
  String,
) {
  case verdict.outcome, verdict.report {
    generated.GateOutputVerdictOutcomePass, _ -> Ok(GatePass)
    generated.GateOutputVerdictOutcomeFail, option.Some(report) ->
      Ok(GateFail(report: report))
    generated.GateOutputVerdictOutcomeFail, option.None ->
      Error("a fail gate verdict requires its report")
  }
}

fn verdict_from_domain(verdict: GateVerdict) -> generated.GateOutputVerdict {
  case verdict {
    GatePass ->
      generated.GateOutputVerdict(
        outcome: generated.GateOutputVerdictOutcomePass,
        report: option.None,
      )
    GateFail(report: report) ->
      generated.GateOutputVerdict(
        outcome: generated.GateOutputVerdictOutcomeFail,
        report: option.Some(report),
      )
  }
}
