//// The CHILD workflow: ONE adversarial review lens over one developer round,
//// run as its own linked execution.
////
//// Why a child workflow per lens (the remediation-wave pattern applied
//// INSIDE the brief): DRIVEN Norn sessions are keyed by a session id the
//// harness builds from `{workflow_id}`. Concurrent lenses must never share
//// a session, and the parent must be able to run them SIMULTANEOUSLY — a
//// child workflow gives each lens its own `{workflow_id}` (its own session,
//// its own transcript in the ops console) and `child.spawn` gives the
//// parent real concurrency. This is the intra-brief fan-out: while one
//// brief runs, several agents are visibly working at once.
////
//// The body is deliberately one activity: project the lens input to the
//// reviewer agent, return its schema-constrained verdict. All
//// derive-and-check judgment over the verdict lives in the PARENT
//// (`dev_brief`), which never trusts an asserted overall.

import aion/codec
import aion/error
import aion/workflow
import dev_brief/activities
import dev_brief/codecs
import dev_brief/types.{
  type DevBriefError, type LensInput, type LensVerdict, StageFailed,
}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

/// Typed definition binding the codecs to the child execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  LensInput,
  LensVerdict,
  DevBriefError,
) {
  workflow.define(
    "review_lens",
    codecs.lens_input_codec(),
    codecs.lens_verdict_codec(),
    codecs.dev_brief_error_codec(),
    execute,
  )
}

/// Engine entry point for the child workflow.
pub fn run(raw_input: Dynamic) -> Result(String, DevBriefError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.lens_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(verdict) -> Ok(codecs.lens_verdict_codec().encode(verdict))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(types.DecodeInputFailed(
            "failed to decode lens input: " <> reason,
          ))
      }
    Error(_) ->
      Error(types.DecodeInputFailed("lens input payload was not a string"))
  }
}

/// The child body: one driven adversarial review through this lens.
pub fn execute(input: LensInput) -> Result(LensVerdict, DevBriefError) {
  case workflow.run(activities.review_lens(input)) {
    Ok(verdict) -> Ok(verdict)
    Error(activity_error) ->
      Error(StageFailed(
        stage: "review_lens",
        message: activity_message(activity_error),
      ))
  }
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
