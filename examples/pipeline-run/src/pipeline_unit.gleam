//// The CHILD workflow: one unit of the stack, run as its own linked execution.
////
//// Why a child workflow per unit (not just activities in the parent): DRIVEN
//// Norn sessions are keyed by a session id the harness builds from `{workflow_id}`
//// — the only per-run identity a fixed harness arg can template. A unit's dev
//// and review agents must each keep ONE session resumed across rounds, and two
//// units must never share a session. A child workflow gives each unit its own
//// `{workflow_id}`, so `{workflow_id}-dev` / `{workflow_id}-review` are
//// automatically per-unit AND stable across that unit's rounds — the mission's
//// `{workflow_id}-{unit_id}-{role}` intent, realized with the unit id
//// materialized as the child's own workflow id. It also makes independent units
//// genuinely parallel: the parent spawns a stratum's children concurrently.
////
//// The body: provision an isolated workspace (branched on the prior unit's
//// landed branch), run the first dev round, then drive the bounded
//// dev<->review<->gate cap machine (`pipeline_run/cycle`) as a trampoline —
//// performing exactly the one effect the machine asks for and folding the
//// result back. Every terminal disposition (converged or cap-exhausted) is a
//// returned [`UnitResult`], never an error.

import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/string
import pipeline_run/activities
import pipeline_run/codecs
import pipeline_run/cycle
import pipeline_run/prompts
import pipeline_run/stack
import pipeline_run/types.{
  type DevReport, type Disposition, type GateOutcome, type PipelineError,
  type ReviewVerdict, type UnitInput, type UnitResult, type WorkspaceInfo,
  GateInput, GateOutcome, Passed, ProvisionInput, ReviewVerdict, StageFailed,
  UnitResult,
}

/// The parent and child agree on this base directory for unit workspaces: each
/// unit's worktree lives at `<base>/<child_workflow_id>`. The Rust worker's dev
/// and review harnesses point Norn's `--workspace-root` at the SAME
/// `<base>/{workflow_id}` template, so a driven agent operates in exactly the
/// worktree the `provision_workspace` activity created for its child. Keep this
/// in sync with `WORKSPACE_BASE` in `worker/src/main.rs`.
pub const workspace_base = "/tmp/aion-pipeline-run/ws"

/// Typed definition binding the codecs to the child execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  UnitInput,
  UnitResult,
  PipelineError,
) {
  workflow.define(
    "pipeline_unit",
    codecs.unit_input_codec(),
    codecs.unit_result_codec(),
    codecs.pipeline_error_codec(),
    execute,
  )
}

/// Engine entry point for the child workflow.
pub fn run(raw_input: Dynamic) -> Result(String, PipelineError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.unit_input_codec().decode(raw_json) {
        Ok(unit) ->
          case execute(unit) {
            Ok(result) -> Ok(codecs.unit_result_codec().encode(result))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(types.DecodeInputFailed(
            "failed to decode unit input: " <> reason,
          ))
      }
    Error(_) ->
      Error(types.DecodeInputFailed("unit input payload was not a string"))
  }
}

/// The child body: provision, first dev round, then the bounded cap machine.
pub fn execute(unit: UnitInput) -> Result(UnitResult, PipelineError) {
  let dev_review_cap =
    stack.resolve_cap(unit.dev_review_cap, types.default_dev_review_cap())
  let gate_cap = stack.resolve_cap(unit.gate_cap, types.default_gate_cap())

  use workspace <- try(provision(unit))
  use first_dev <- try(run_dev(prompts.dev_start(unit)))

  let machine = cycle.initial(dev_review_cap, gate_cap)
  let state =
    LoopState(
      workspace: workspace,
      last_dev: first_dev,
      last_review: no_review_yet(),
      last_gate: no_gate_yet(),
    )
  drive(unit, machine, state)
}

/// The carried artifacts alongside the pure cap machine: the workspace and the
/// most recent dev/review/gate results, used to compose the next prompt and to
/// build the terminal [`UnitResult`].
type LoopState {
  LoopState(
    workspace: WorkspaceInfo,
    last_dev: DevReport,
    last_review: ReviewVerdict,
    last_gate: GateOutcome,
  )
}

/// The trampoline: ask the machine for the next instruction, perform exactly
/// that one effect, fold the outcome back, recurse. Terminates when the machine
/// stops, building the unit's terminal result.
fn drive(
  unit: UnitInput,
  machine: cycle.Machine,
  state: LoopState,
) -> Result(UnitResult, PipelineError) {
  case cycle.plan(machine) {
    cycle.Stop(disposition) ->
      Ok(finish(unit, state, disposition, machine.rounds, machine.gate_rounds))
    cycle.Review(resume) -> {
      let prompt = case resume {
        False -> prompts.review_start(unit, state.last_dev.summary)
        True -> prompts.review_resume(state.last_dev.summary)
      }
      use verdict <- try(run_review(prompt))
      drive(
        unit,
        cycle.on_review(machine, verdict.pass),
        LoopState(..state, last_review: verdict),
      )
    }
    cycle.Gate -> {
      use outcome <- try(run_gate(state.workspace))
      drive(
        unit,
        cycle.on_gate(machine, outcome.pass),
        LoopState(..state, last_gate: outcome),
      )
    }
    cycle.DevReview -> {
      use report <- try(run_dev(prompts.dev_after_review(state.last_review)))
      drive(
        unit,
        cycle.on_dev_review(machine),
        LoopState(..state, last_dev: report),
      )
    }
    cycle.DevGate -> {
      use report <- try(run_dev(prompts.dev_after_gate(state.last_gate)))
      drive(
        unit,
        cycle.on_dev_gate(machine),
        LoopState(..state, last_dev: report),
      )
    }
  }
}

// --- effects ---------------------------------------------------------------

fn provision(unit: UnitInput) -> Result(WorkspaceInfo, PipelineError) {
  use child_id <- try(engine_id())
  let workspace_path = workspace_base <> "/" <> child_id
  case
    workflow.run(
      activities.provision(ProvisionInput(
        repo_root: unit.repo_root,
        base_branch: unit.base_branch,
        unit_branch: unit.unit_branch,
        workspace_path: workspace_path,
      )),
    )
  {
    Ok(info) -> Ok(info)
    Error(activity_error) -> stage_error("provision_workspace", activity_error)
  }
}

fn run_dev(prompt: String) -> Result(DevReport, PipelineError) {
  case workflow.run(activities.dev(prompt)) {
    Ok(report) -> Ok(report)
    Error(activity_error) -> stage_error("dev", activity_error)
  }
}

fn run_review(prompt: String) -> Result(ReviewVerdict, PipelineError) {
  case workflow.run(activities.review(prompt)) {
    Ok(verdict) -> Ok(verdict)
    Error(activity_error) -> stage_error("review", activity_error)
  }
}

fn run_gate(workspace: WorkspaceInfo) -> Result(GateOutcome, PipelineError) {
  case
    workflow.run(
      activities.gate(GateInput(workspace_path: workspace.workspace_path)),
    )
  {
    Ok(outcome) -> Ok(outcome)
    Error(activity_error) -> stage_error("gate", activity_error)
  }
}

/// The child's own workflow id — the per-unit scope the workspace path and the
/// Norn session ids are keyed on.
fn engine_id() -> Result(String, PipelineError) {
  case workflow.id() {
    Ok(id) -> Ok(id)
    Error(engine_error) ->
      Error(StageFailed(
        stage: "workflow_id",
        message: "could not read the child workflow id: "
          <> string.inspect(engine_error),
      ))
  }
}

// --- terminal result -------------------------------------------------------

fn finish(
  unit: UnitInput,
  state: LoopState,
  disposition: Disposition,
  dev_review_rounds: Int,
  gate_rounds: Int,
) -> UnitResult {
  UnitResult(
    unit_id: unit.unit_id,
    branch: state.workspace.branch,
    disposition: disposition,
    dev_review_rounds: dev_review_rounds,
    gate_rounds: gate_rounds,
    last_review_summary: state.last_review.summary,
    last_gate_diagnostics: gate_detail(state.last_gate),
    files_touched: state.last_dev.files_touched,
    summary: unit_summary(unit, disposition, dev_review_rounds, gate_rounds),
  )
}

fn unit_summary(
  unit: UnitInput,
  disposition: Disposition,
  dev_review_rounds: Int,
  gate_rounds: Int,
) -> String {
  let headline = case disposition {
    Passed -> "unit " <> unit.unit_id <> " passed review and gate"
    types.ReviewCapExhausted ->
      "unit " <> unit.unit_id <> " exhausted the dev<->review budget"
    types.GateCapExhausted ->
      "unit " <> unit.unit_id <> " exhausted the gate budget"
  }
  headline <> "; " <> prompts.rounds_phrase(dev_review_rounds, gate_rounds)
}

fn gate_detail(gate: GateOutcome) -> String {
  case gate.pass, gate.diagnostics {
    True, _ -> ""
    False, diagnostics -> diagnostics
  }
}

/// The placeholder review carried before any review has run. Never surfaced as
/// a verdict: the first machine instruction is always a real review.
fn no_review_yet() -> ReviewVerdict {
  ReviewVerdict(
    pass: False,
    blockers: [],
    should_fix: [],
    summary: "no review has run yet",
    not_covered: [],
  )
}

/// The placeholder gate carried before any gate has run.
fn no_gate_yet() -> GateOutcome {
  GateOutcome(pass: False, diagnostics: "")
}

fn stage_error(
  stage: String,
  activity_error: error.ActivityError,
) -> Result(value, PipelineError) {
  Error(StageFailed(stage: stage, message: activity_message(activity_error)))
}

fn activity_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}

/// `use`-friendly bind over `Result` with [`PipelineError`].
fn try(
  result: Result(a, PipelineError),
  next: fn(a) -> Result(b, PipelineError),
) -> Result(b, PipelineError) {
  case result {
    Ok(value) -> next(value)
    Error(pipeline_error) -> Error(pipeline_error)
  }
}
