//// Minimal Aion workflow used by the getting-started guide.
////
//// The workflow accepts `{ "name": String }`, schedules one remote `greet`
//// activity through the public `aion_flow` SDK, and returns the greeting string
//// produced by the worker.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

pub type HelloInput {
  HelloInput(name: String)
}

pub type GreetingOutput {
  GreetingOutput(greeting: String)
}

pub type WorkflowError {
  ActivityFailed(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) -> {
      let input_codec = hello_input_codec()
      case input_codec.decode(raw_json) {
        Ok(input) ->
          case workflow.run(greet_activity(input)) {
            Ok(greeting) -> Ok(greeting.greeting)
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

fn greet_activity(input: HelloInput) -> activity.Activity(HelloInput, GreetingOutput) {
  activity.new(
    "greet",
    input,
    hello_input_codec(),
    greeting_output_codec(),
    local_greet,
  )
}

fn local_greet(input: HelloInput) -> Result(GreetingOutput, error.ActivityError) {
  Ok(GreetingOutput(
    greeting: "Hello, " <> input.name <> "! Welcome to Aion.",
  ))
}

fn hello_input_codec() -> codec.Codec(HelloInput) {
  codec.json_codec(hello_input_to_json, hello_input_decoder())
}

fn hello_input_to_json(input: HelloInput) -> json.Json {
  json.object([#("name", json.string(input.name))])
}

fn hello_input_decoder() -> decode.Decoder(HelloInput) {
  use name <- decode.field("name", decode.string)
  decode.success(HelloInput(name: name))
}

fn greeting_output_codec() -> codec.Codec(GreetingOutput) {
  codec.json_codec(greeting_output_to_json, greeting_output_decoder())
}

fn greeting_output_to_json(output: GreetingOutput) -> json.Json {
  json.object([#("greeting", json.string(output.greeting))])
}

fn greeting_output_decoder() -> decode.Decoder(GreetingOutput) {
  use greeting <- decode.field("greeting", decode.string)
  decode.success(GreetingOutput(greeting: greeting))
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
