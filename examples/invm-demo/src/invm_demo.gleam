//// Minimal Aion in-VM activity tier demo.
////
//// The workflow accepts `{ "name": String }` and schedules one `shout`
//// activity decorated with `activity.execution_tier(activity.InVm)`: the
//// runner (a pure-Gleam transform) executes ONCE inside a linked child
//// process of the workflow process — no remote worker, no task queue
//// subscription, nothing to deploy besides this package. The recorded
//// history and replay semantics are identical to a remote activity: kill
//// the server mid-activity and the recovered engine replays the recording
//// (or reopens and re-runs the thunk if no terminal was recorded).

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/string

pub type ShoutInput {
  ShoutInput(name: String)
}

pub type WorkflowError {
  ActivityFailed(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) -> {
      let input_codec = shout_input_codec()
      case input_codec.decode(raw_json) {
        Ok(input) ->
          case workflow.run(shout_activity(input)) {
            Ok(shouted) -> Ok(shouted)
            Error(activity_error) ->
              Error(ActivityFailed(activity_error_message(activity_error)))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(ActivityFailed("failed to decode workflow input: " <> reason))
      }
    }
    Error(_) ->
      Error(ActivityFailed("workflow input payload was not a string"))
  }
}

/// The in-VM activity: `execution_tier(InVm)` routes the dispatch through the
/// engine's linked child-process path, so `local_shout` IS the production
/// runner — not a placeholder for a remote worker.
fn shout_activity(input: ShoutInput) -> activity.Activity(ShoutInput, String) {
  activity.new(
    "shout",
    input,
    shout_input_codec(),
    shout_output_codec(),
    local_shout,
  )
  |> activity.execution_tier(activity.InVm)
}

fn shout_output_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn local_shout(input: ShoutInput) -> Result(String, error.ActivityError) {
  case input.name {
    "" -> Error(error.terminal("cannot shout at nobody"))
    name -> Ok(string.uppercase(name) <> "!!!")
  }
}

fn shout_input_codec() -> codec.Codec(ShoutInput) {
  codec.json_codec(shout_input_to_json, shout_input_decoder())
}

fn shout_input_to_json(input: ShoutInput) -> json.Json {
  json.object([#("name", json.string(input.name))])
}

fn shout_input_decoder() -> decode.Decoder(ShoutInput) {
  use name <- decode.field("name", decode.string)
  decode.success(ShoutInput(name: name))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.ActivityEngineFailure(message: message) -> message
  }
}
