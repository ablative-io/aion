//// The dev child workflow: concurrent warm-build + dev start-up,
//// then the bounded scoped verify-fix loop.
////
//// Stage shape (brief sections 2 and 5):
////
//// 1. `workflow.all([warm_build, dev])` — the build cache warms while the
////    dev agent works. The warm build is advisory: its activity reports a
////    failed build as `BuildWarm(ok: False, ..)` data, so a forfeited cache
////    can never fail the run (open question Q4).
//// 2. A bounded verify-fix loop: `scoped_checks` limited to the affected
////    modules; on `CheckFail` the same agent session is resumed with the
////    diagnostics (`dev_resume`) after a durable `workflow.sleep` backoff,
////    and the loop recurses with its attempt counter. Spending the budget
////    surfaces the typed `VerifyFixExhausted` error carrying the last
////    diagnostics — never landed, never infinite.
////
//// The loop cap and backoff are required workflow inputs (open question
//// Q5); this module bakes no defaults. The module is spawned as a child by
//// the parent workflow and is independently dispatchable as its own `[[workflow]]`
//// entry (open question Q6).

import aion/codec
import aion/duration
import aion/query
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import {{name}}/activities
import {{name}}/codecs_workflows
import {{name}}/errors
import {{name}}/types.{
  type BuildWarm, type DevResult, type DevFlowError, type DevFlowInput,
  type DevFlowResult, CheckFail, CheckPass, CheckResult, DevInput, Developed,
  DevFlowResult, DevFlowStageFailed, DevFlowStatus, ResumeInput, ScopedInput,
  StartupFailed, VerifyFixExhausted, Warmed,
}

/// The child workflow type the parent passes to `workflow.spawn_and_wait`.
/// A deployed workflow type is its entry module name, so this is exactly
/// this module's name.
pub const workflow_type = "{{name}}_dev"

/// Name of the live `{phase, round}` status query this workflow answers.
pub const status_query_name = "{{name}}_dev_status"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  DevFlowInput,
  DevFlowResult,
  DevFlowError,
) {
  workflow.define(
    "{{name}}_dev",
    codecs_workflows.dev_flow_input_codec(),
    codecs_workflows.dev_flow_result_codec(),
    codecs_workflows.dev_flow_error_codec(),
    execute,
  )
}

/// Engine entry point for one child execution.
///
/// The runtime delivers the start input as a raw JSON string. Success and
/// failure are both encoded back to JSON text here: the engine records these
/// exact payloads as the child terminal, and the awaiting parent decodes
/// them with the same codecs `{{name}}/codecs_workflows` exports.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs_workflows.dev_flow_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) ->
              Ok(codecs_workflows.dev_flow_result_codec().encode(output))
            Error(dev_flow_error) ->
              Error(codecs_workflows.dev_flow_error_codec().encode(dev_flow_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            codecs_workflows.dev_flow_error_codec().encode(DevFlowStageFailed(
              stage: "decode_input",
              message: "failed to decode dev child input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        codecs_workflows.dev_flow_error_codec().encode(DevFlowStageFailed(
          stage: "decode_input",
          message: "dev child input payload was not a string",
        )),
      )
  }
}

/// Typed workflow body: concurrent start-up, then the bounded verify-fix
/// loop.
pub fn execute(input: DevFlowInput) -> Result(DevFlowResult, DevFlowError) {
  use _ <- result_try(set_status("starting", 0))
  // Brief section 5, "parallel start": warm the build cache while the dev
  // agent works. `workflow.all` collects a homogeneous activity list, so
  // both activities share the StartupTask/StartupResult envelope and the
  // results come back in input order.
  case
    workflow.all([
      activities.warm_build(input.workspace),
      activities.dev(DevInput(
        workspace: input.workspace,
        brief: input.brief,
        design: input.design,
        checklist: input.checklist,
        stories: input.stories,
      )),
    ])
  {
    Ok([Warmed(build_warm: build_warm), Developed(dev_result: dev_result)]) ->
      verify_loop(input, build_warm, dev_result, 1)
    Ok(_) ->
      Error(StartupFailed(
        message: "startup fan-out settled with a result shape other than"
        <> " [warm_build, dev] — envelope contract violation",
      ))
    Error(activity_error) ->
      Error(StartupFailed(message: errors.activity_message(activity_error)))
  }
}

/// One bounded verify-fix round: scoped checks, then on failure a durable
/// backoff, a session resume carrying the diagnostics, and recursion with
/// the attempt counter.
fn verify_loop(
  input: DevFlowInput,
  build_warm: BuildWarm,
  dev_result: DevResult,
  round: Int,
) -> Result(DevFlowResult, DevFlowError) {
  use _ <- result_try(set_status("verifying", round))
  case
    workflow.run(
      activities.scoped_checks(ScopedInput(
        workspace: input.workspace,
        files_touched: dev_result.files_touched,
      )),
    )
  {
    Ok(CheckResult(verdict: CheckPass, affected_modules: _, checked_scope: _)) -> {
      use _ <- result_try(set_status("converged", round))
      Ok(DevFlowResult(
        dev_result: dev_result,
        build_warm: build_warm,
        verify_rounds: round,
      ))
    }
    Ok(CheckResult(
      verdict: CheckFail(diagnostics: diagnostics),
      affected_modules: _,
      checked_scope: _,
    )) ->
      case round >= input.verify_fix_cap {
        True ->
          // Typed exhaustion carrying the last diagnostics: the budget is
          // spent and the run fails loudly instead of looping forever.
          Error(VerifyFixExhausted(rounds: round, diagnostics: diagnostics))
        False -> fix_round(input, build_warm, dev_result, round, diagnostics)
      }
    Error(activity_error) ->
      Error(DevFlowStageFailed(
        stage: "scoped_checks round " <> int.to_string(round),
        message: errors.activity_message(activity_error),
      ))
  }
}

fn fix_round(
  input: DevFlowInput,
  build_warm: BuildWarm,
  dev_result: DevResult,
  round: Int,
  diagnostics: String,
) -> Result(DevFlowResult, DevFlowError) {
  use _ <- result_try(set_status("fixing", round))
  // Durable backoff between rounds — required input, no baked default.
  case workflow.sleep(duration.milliseconds(input.round_backoff_ms)) {
    Ok(Nil) ->
      case
        workflow.run(
          activities.dev_resume(ResumeInput(
            session_id: dev_result.session_id,
            feedback: diagnostics,
          )),
        )
      {
        Ok(resumed) -> verify_loop(input, build_warm, resumed, round + 1)
        Error(activity_error) ->
          Error(DevFlowStageFailed(
            stage: "dev_resume round " <> int.to_string(round),
            message: errors.activity_message(activity_error),
          ))
      }
    Error(engine_error) ->
      Error(DevFlowStageFailed(
        stage: "round_backoff round " <> int.to_string(round),
        message: errors.engine_message(engine_error),
      ))
  }
}

/// Re-register the status handler with the current phase and round, so
/// `{{name}}_dev_status` queries answer live state at every yield point
/// (re-registration per stage, per docs/guides/workflows.md).
fn set_status(phase: String, round: Int) -> Result(Nil, DevFlowError) {
  let status = DevFlowStatus(phase: phase, round: round)
  case
    query.handler(
      status_query_name,
      codecs_workflows.dev_flow_status_codec(),
      fn() { status },
    )
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(DevFlowStageFailed(
        stage: "register_status",
        message: errors.query_message(query_error),
      ))
  }
}

fn result_try(
  result: Result(value, DevFlowError),
  next: fn(value) -> Result(output, DevFlowError),
) -> Result(output, DevFlowError) {
  case result {
    Ok(value) -> next(value)
    Error(dev_flow_error) -> Error(dev_flow_error)
  }
}
