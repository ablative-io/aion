//// Minimal workflow exercising the ENGINE-honored per-activity retry policy
//// (#197).
////
//// The workflow accepts `{ "name": String }` and schedules one remote
//// `flaky_call` activity that declares an explicit `RetryPolicy` (three total
//// attempts, fixed 25ms backoff). The workflow body contains NO retry logic:
//// when the worker fails an attempt with a retryable error, the engine's
//// dispatch seam records the non-terminal failure and re-dispatches the same
//// activity at the incremented attempt, transparently to this code.

import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

pub type FlakyInput {
  FlakyInput(name: String)
}

pub type FlakyOutput {
  FlakyOutput(reply: String)
}

pub type WorkflowError {
  ActivityFailed(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) -> {
      let input_codec = flaky_input_codec()
      case input_codec.decode(raw_json) {
        Ok(input) ->
          case workflow.run(flaky_activity(input)) {
            Ok(output) -> Ok(output.reply)
            Error(activity_error) ->
              Error(ActivityFailed(activity_error_message(activity_error)))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(ActivityFailed("failed to decode workflow input: " <> reason))
      }
    }
    Error(_) -> Error(ActivityFailed("workflow input payload was not a string"))
  }
}

/// Total attempt budget the declared policy grants the engine.
pub const max_attempts = 3

fn flaky_activity(
  input: FlakyInput,
) -> activity.Activity(FlakyInput, FlakyOutput) {
  activity.new(
    "flaky_call",
    input,
    flaky_input_codec(),
    flaky_output_codec(),
    local_flaky_call,
  )
  |> activity.retry(activity.RetryPolicy(
    max_attempts: max_attempts,
    backoff: activity.Fixed(delay: duration.milliseconds(25)),
  ))
}

fn local_flaky_call(
  input: FlakyInput,
) -> Result(FlakyOutput, error.ActivityError) {
  Ok(FlakyOutput(reply: "steady hello, " <> input.name))
}

fn flaky_input_codec() -> codec.Codec(FlakyInput) {
  codec.json_codec(flaky_input_to_json, flaky_input_decoder())
}

fn flaky_input_to_json(input: FlakyInput) -> json.Json {
  json.object([#("name", json.string(input.name))])
}

fn flaky_input_decoder() -> decode.Decoder(FlakyInput) {
  use name <- decode.field("name", decode.string)
  decode.success(FlakyInput(name: name))
}

fn flaky_output_codec() -> codec.Codec(FlakyOutput) {
  codec.json_codec(flaky_output_to_json, flaky_output_decoder())
}

fn flaky_output_to_json(output: FlakyOutput) -> json.Json {
  json.object([#("reply", json.string(output.reply))])
}

fn flaky_output_decoder() -> decode.Decoder(FlakyOutput) {
  use reply <- decode.field("reply", decode.string)
  decode.success(FlakyOutput(reply: reply))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
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
