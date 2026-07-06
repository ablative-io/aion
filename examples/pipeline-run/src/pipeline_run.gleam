//// The PARENT workflow: a prospekt dev-cycle brief -> a landed stack.
////
////   scout (agent, ground the tree)
////   -> plan (agent, decompose into a dependency-ordered stack of units)
////   -> stratify the units into dependency LAYERS (pure; cycle-rejecting)
////   -> for each stratum in order:
////        spawn one `pipeline_unit` CHILD per unit CONCURRENTLY (independent
////        units run in parallel), await them all, then LAND the passing units
////        onto the integration branch so the next stratum branches on landed
////        work
////   -> notify.
////
//// This module is the determinism boundary: it issues only recorded activity
//// dispatches / child spawns and branches on their recorded outputs. The stack
//// ordering and cap accounting are pure (`pipeline_run/stack`, `.../cycle`) and
//// unit-tested; the agents propose and the command activities verify
//// (PIPELINE.md's core principle).

import aion/child
import aion/codec
import aion/error
import aion/workflow
import gleam/dict.{type Dict}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/list
import gleam/string
import pipeline_run/activities
import pipeline_run/codecs
import pipeline_run/prompts
import pipeline_run/stack
import pipeline_run/types.{
  type Disposition, type LandUnit, type PipelineBrief, type PipelineError,
  type PipelineResult, type PlanUnit, type ScoutFindings, type StackPlan,
  type UnitInput, type UnitResult, GateCapExhausted, LandInput, LandUnit,
  NotifyInput, Passed, PipelineResult, ReviewCapExhausted, StackInvalid,
  StageFailed, UnitInput,
}
import pipeline_unit

/// Typed definition binding the codecs to the parent execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  PipelineBrief,
  PipelineResult,
  PipelineError,
) {
  workflow.define(
    "pipeline_run",
    codecs.brief_codec(),
    codecs.pipeline_result_codec(),
    codecs.pipeline_error_codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, PipelineError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.brief_codec().decode(raw_json) {
        Ok(brief) ->
          case execute(brief) {
            Ok(result) -> Ok(codecs.pipeline_result_codec().encode(result))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(types.DecodeInputFailed(
            "failed to decode brief input: " <> reason,
          ))
      }
    Error(_) ->
      Error(types.DecodeInputFailed("brief input payload was not a string"))
  }
}

/// The parent body.
pub fn execute(brief: PipelineBrief) -> Result(PipelineResult, PipelineError) {
  use findings <- try(run_scout(brief))
  use plan <- try(run_plan(brief, findings))
  use strata <- try(order_stack(plan))

  let unit_index = index_units(plan.units)
  let integration_branch = integration_branch_name(brief)

  use run_state <- try(process_strata(
    brief,
    findings,
    strata,
    unit_index,
    integration_branch,
    RunState(base_branch: brief.base_branch, results: [], landed: []),
  ))

  let results = list.reverse(run_state.results)
  let disposition = overall_disposition(results)
  let summary = pipeline_summary(brief, strata, disposition, run_state.landed)
  use _ <- try(run_notify(brief, summary))

  Ok(PipelineResult(
    disposition: disposition,
    strata: strata,
    units: results,
    landed: list.reverse(run_state.landed),
    summary: summary,
  ))
}

// --- scout / plan / ordering -----------------------------------------------

fn run_scout(brief: PipelineBrief) -> Result(ScoutFindings, PipelineError) {
  case workflow.run(activities.scout(prompts.scout(brief))) {
    Ok(findings) -> Ok(findings)
    Error(activity_error) -> stage_error("scout", activity_error)
  }
}

fn run_plan(
  brief: PipelineBrief,
  findings: ScoutFindings,
) -> Result(StackPlan, PipelineError) {
  case workflow.run(activities.plan(prompts.plan(brief, findings))) {
    Ok(plan) -> Ok(plan)
    Error(activity_error) -> stage_error("plan", activity_error)
  }
}

/// Turn the plan's `depends_on` graph into ordered strata, or reject a
/// non-DAG plan with a pointed [`StackInvalid`] carrying the reason.
fn order_stack(plan: StackPlan) -> Result(List(List(String)), PipelineError) {
  case stack.stratify(plan.units) {
    Ok(strata) -> Ok(strata)
    Error(stack_error) ->
      Error(StackInvalid(reason: stack.stack_error_message(stack_error)))
  }
}

fn index_units(units: List(PlanUnit)) -> Dict(String, PlanUnit) {
  list.fold(units, dict.new(), fn(acc, unit) {
    dict.insert(acc, unit.unit_id, unit)
  })
}

// --- stratum processing ----------------------------------------------------

/// The accumulator threaded across strata: the branch the NEXT stratum's units
/// branch on (advanced to the integration branch after the first land), the
/// unit results so far (newest first), and the unit ids landed so far (newest
/// first).
type RunState {
  RunState(base_branch: String, results: List(UnitResult), landed: List(String))
}

fn process_strata(
  brief: PipelineBrief,
  findings: ScoutFindings,
  strata: List(List(String)),
  unit_index: Dict(String, PlanUnit),
  integration_branch: String,
  state: RunState,
) -> Result(RunState, PipelineError) {
  case strata {
    [] -> Ok(state)
    [stratum, ..rest] -> {
      use stratum_results <- try(run_stratum(
        brief,
        findings,
        stratum,
        unit_index,
        state.base_branch,
      ))
      // Land the units that passed, in the stratum's order, onto the
      // integration branch (created from the current base on the first land).
      let landable =
        stratum_results
        |> list.filter(fn(result) { result.disposition == Passed })
        |> list.map(fn(result) {
          LandUnit(unit_id: result.unit_id, branch: result.branch)
        })
      use landed_now <- try(run_land(
        brief,
        state.base_branch,
        integration_branch,
        landable,
      ))
      let next_state =
        RunState(
          // Once anything has landed, later strata branch on the integration
          // branch so they build on prior landed work.
          base_branch: case landed_now {
            [] -> state.base_branch
            _ -> integration_branch
          },
          results: prepend_all(stratum_results, state.results),
          landed: prepend_all(landed_now, state.landed),
        )
      process_strata(
        brief,
        findings,
        rest,
        unit_index,
        integration_branch,
        next_state,
      )
    }
  }
}

/// Spawn every unit in a stratum as a CHILD `pipeline_unit`, concurrently, then
/// await them all. Independent units in the same stratum thus develop in
/// parallel (fan-out); the awaits collect their terminal results in order.
fn run_stratum(
  brief: PipelineBrief,
  findings: ScoutFindings,
  stratum: List(String),
  unit_index: Dict(String, PlanUnit),
  base_branch: String,
) -> Result(List(UnitResult), PipelineError) {
  use handles <- try(spawn_all(
    brief,
    findings,
    stratum,
    unit_index,
    base_branch,
  ))
  await_all(handles, [])
}

fn spawn_all(
  brief: PipelineBrief,
  findings: ScoutFindings,
  stratum: List(String),
  unit_index: Dict(String, PlanUnit),
  base_branch: String,
) -> Result(List(child.ChildHandle(UnitResult, PipelineError)), PipelineError) {
  case stratum {
    [] -> Ok([])
    [unit_id, ..rest] -> {
      use unit <- try(lookup_unit(unit_index, unit_id))
      let input = unit_input(brief, findings, unit, base_branch)
      case
        child.spawn(
          "pipeline_unit",
          pipeline_unit.execute,
          input,
          codecs.unit_input_codec(),
          codecs.unit_result_codec(),
          codecs.pipeline_error_codec(),
        )
      {
        Ok(handle) -> {
          use rest_handles <- try(spawn_all(
            brief,
            findings,
            rest,
            unit_index,
            base_branch,
          ))
          Ok([handle, ..rest_handles])
        }
        Error(engine_error) ->
          Error(StageFailed(
            stage: "spawn_unit",
            message: "could not spawn unit "
              <> unit_id
              <> ": "
              <> string.inspect(engine_error),
          ))
      }
    }
  }
}

fn await_all(
  handles: List(child.ChildHandle(UnitResult, PipelineError)),
  acc: List(UnitResult),
) -> Result(List(UnitResult), PipelineError) {
  case handles {
    [] -> Ok(list.reverse(acc))
    [handle, ..rest] ->
      case child.await(handle) {
        Ok(result) -> await_all(rest, [result, ..acc])
        Error(child_error) -> Error(child_error_to_pipeline(child_error))
      }
  }
}

fn unit_input(
  brief: PipelineBrief,
  findings: ScoutFindings,
  unit: PlanUnit,
  base_branch: String,
) -> UnitInput {
  UnitInput(
    repo_root: brief.repo_root,
    base_branch: base_branch,
    unit_branch: unit_branch_name(brief, unit.unit_id),
    unit_id: unit.unit_id,
    goal: unit.goal,
    files_hint: unit.files_hint,
    brief_title: brief.title,
    brief_intent: brief.intent,
    acceptance_criteria: brief.acceptance_criteria,
    constraints: brief.constraints,
    scout_summary: findings.summary,
    dev_review_cap: brief.dev_review_cap,
    gate_cap: brief.gate_cap,
  )
}

// --- land / notify ---------------------------------------------------------

fn run_land(
  brief: PipelineBrief,
  base_branch: String,
  integration_branch: String,
  units: List(LandUnit),
) -> Result(List(String), PipelineError) {
  case units {
    // Nothing passed in this stratum: skip the land activity entirely, and
    // report an empty landed set honestly.
    [] -> Ok([])
    _ ->
      case
        workflow.run(
          activities.land(LandInput(
            repo_root: brief.repo_root,
            base_branch: base_branch,
            integration_branch: integration_branch,
            units: units,
          )),
        )
      {
        Ok(outcome) -> Ok(outcome.landed)
        Error(activity_error) -> stage_error("land", activity_error)
      }
  }
}

fn run_notify(
  brief: PipelineBrief,
  summary: String,
) -> Result(Nil, PipelineError) {
  case
    workflow.run(
      activities.notify(NotifyInput(brief_id: brief.id, summary: summary)),
    )
  {
    Ok(_outcome) -> Ok(Nil)
    Error(activity_error) -> stage_error("notify", activity_error)
  }
}

// --- summaries / dispositions ----------------------------------------------

/// The overall disposition: `Passed` only if every unit passed; otherwise the
/// most severe failure surfaced (a gate exhaustion outranks a review one, since
/// it means code that reviewers accepted still would not build).
fn overall_disposition(results: List(UnitResult)) -> Disposition {
  list.fold(results, Passed, fn(worst, result) {
    case worst, result.disposition {
      GateCapExhausted, _ -> GateCapExhausted
      _, GateCapExhausted -> GateCapExhausted
      ReviewCapExhausted, _ -> ReviewCapExhausted
      _, ReviewCapExhausted -> ReviewCapExhausted
      Passed, Passed -> Passed
    }
  })
}

fn pipeline_summary(
  brief: PipelineBrief,
  strata: List(List(String)),
  disposition: Disposition,
  landed_reversed: List(String),
) -> String {
  let landed = list.reverse(landed_reversed)
  "Brief "
  <> brief.id
  <> " ("
  <> brief.title
  <> "): "
  <> types.disposition_to_string(disposition)
  <> ". Strata: "
  <> string.inspect(strata)
  <> ". Landed in order: "
  <> string.inspect(landed)
  <> "."
}

fn integration_branch_name(brief: PipelineBrief) -> String {
  "pipeline/" <> safe(brief.id) <> "/integration"
}

fn unit_branch_name(brief: PipelineBrief, unit_id: String) -> String {
  "pipeline/" <> safe(brief.id) <> "/" <> safe(unit_id)
}

/// Reduce an id to branch-safe characters (letters, digits, `-`, `_`), so a
/// document id or unit id can never mint an invalid ref name.
fn safe(id: String) -> String {
  id
  |> string.to_graphemes
  |> list.map(fn(character) {
    case is_branch_safe(character) {
      True -> character
      False -> "-"
    }
  })
  |> string.join("")
}

fn is_branch_safe(character: String) -> Bool {
  case character {
    "-" | "_" -> True
    _ -> is_alphanumeric(character)
  }
}

fn is_alphanumeric(character: String) -> Bool {
  let lower = string.lowercase(character)
  string.contains("abcdefghijklmnopqrstuvwxyz0123456789", lower)
  && string.length(character) == 1
}

// --- helpers ---------------------------------------------------------------

fn lookup_unit(
  unit_index: Dict(String, PlanUnit),
  unit_id: String,
) -> Result(PlanUnit, PipelineError) {
  case dict.get(unit_index, unit_id) {
    Ok(unit) -> Ok(unit)
    Error(_) ->
      Error(StageFailed(
        stage: "stratify",
        message: "stratum names unknown unit " <> unit_id,
      ))
  }
}

fn prepend_all(items: List(a), onto: List(a)) -> List(a) {
  list.fold(items, onto, fn(acc, item) { [item, ..acc] })
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

fn child_error_to_pipeline(
  child_error: error.ChildError(PipelineError),
) -> PipelineError {
  case child_error {
    error.ChildWorkflowFailed(pipeline_error) -> pipeline_error
    error.ChildOutputDecodeFailed(_) ->
      types.StackFailed("child unit result could not be decoded")
    error.ChildErrorDecodeFailed(_) ->
      types.StackFailed("child unit error could not be decoded")
    error.ChildEngineFailure(message: message) ->
      types.StackFailed("child unit engine failure: " <> message)
  }
}

fn try(
  result: Result(a, PipelineError),
  next: fn(a) -> Result(b, PipelineError),
) -> Result(b, PipelineError) {
  case result {
    Ok(value) -> next(value)
    Error(pipeline_error) -> Error(pipeline_error)
  }
}
