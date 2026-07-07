//// PLACEHOLDER — this module will be REPLACED by AWL-generated code.
////
//// It exists so the project shell compiles (and packages) before the AWL
//// emitter exists: a trivial valid workflow with the same module name,
//// entry function, and activity contract the generated module will carry.
//// Do not grow logic here; the emitter overwrites this file wholesale.
////
//// The workflow accepts `{ "name": String }`, chains the two remote
//// activities the awl-hello worker serves — `greet` then `shout` — and
//// returns the shouted greeting string. Both activities are pinned to the
//// `awl_hello` task queue and the `hello` node: the server routes a pushed
//// activity by (namespace, task_queue, node) ONLY, never by activity type,
//// and the worker's single connection registers exactly that node.

import aion/activity.{type Activity}
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

/// The one task queue every awl-hello activity is dispatched on. MUST match
/// `TASK_QUEUE` in `worker/src/main.rs`.
pub const task_queue = "awl_hello"

/// The node the worker's single connection registers; both activities pin
/// to it. MUST match `NODE` in `worker/src/main.rs`.
pub const hello_node = "hello"

pub type HelloInput {
  HelloInput(name: String)
}

pub type GreetOutput {
  GreetOutput(greeting: String)
}

pub type ShoutInput {
  ShoutInput(text: String)
}

pub type ShoutOutput {
  ShoutOutput(text: String)
}

pub type WorkflowError {
  ActivityFailed(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case hello_input_codec().decode(raw_json) {
        Ok(input) ->
          case workflow.run(greet_activity(input)) {
            Ok(greeted) ->
              case
                workflow.run(shout_activity(ShoutInput(text: greeted.greeting)))
              {
                Ok(shouted) -> Ok(shouted.text)
                Error(activity_error) ->
                  Error(ActivityFailed(activity_error_message(activity_error)))
              }
            Error(activity_error) ->
              Error(ActivityFailed(activity_error_message(activity_error)))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(ActivityFailed("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(ActivityFailed("workflow input payload was not a string"))
  }
}

/// Pin an activity to the awl-hello queue AND to the worker's one
/// connection. The local runner is a guard that never runs in production and
/// fails loudly if a misconfiguration ever routed one in-VM.
fn route(activity: Activity(i, o)) -> Activity(i, o) {
  activity.node(activity.task_queue(activity, task_queue), hello_node)
}

fn remote_only(
  name: String,
) -> fn(input) -> Result(output, error.ActivityError) {
  fn(_input) {
    Error(error.Terminal(
      message: name
        <> " is a remote-only awl-hello activity and has no in-VM runner",
      details: "",
    ))
  }
}

fn greet_activity(input: HelloInput) -> Activity(HelloInput, GreetOutput) {
  activity.new(
    "greet",
    input,
    hello_input_codec(),
    greet_output_codec(),
    remote_only("greet"),
  )
  |> route
}

fn shout_activity(input: ShoutInput) -> Activity(ShoutInput, ShoutOutput) {
  activity.new(
    "shout",
    input,
    shout_input_codec(),
    shout_output_codec(),
    remote_only("shout"),
  )
  |> route
}

fn hello_input_codec() -> codec.Codec(HelloInput) {
  codec.json_codec(
    fn(input: HelloInput) { json.object([#("name", json.string(input.name))]) },
    {
      use name <- decode.field("name", decode.string)
      decode.success(HelloInput(name: name))
    },
  )
}

fn greet_output_codec() -> codec.Codec(GreetOutput) {
  codec.json_codec(
    fn(output: GreetOutput) {
      json.object([#("greeting", json.string(output.greeting))])
    },
    {
      use greeting <- decode.field("greeting", decode.string)
      decode.success(GreetOutput(greeting: greeting))
    },
  )
}

fn shout_input_codec() -> codec.Codec(ShoutInput) {
  codec.json_codec(
    fn(input: ShoutInput) { json.object([#("text", json.string(input.text))]) },
    {
      use text <- decode.field("text", decode.string)
      decode.success(ShoutInput(text: text))
    },
  )
}

fn shout_output_codec() -> codec.Codec(ShoutOutput) {
  codec.json_codec(
    fn(output: ShoutOutput) {
      json.object([#("text", json.string(output.text))])
    },
    {
      use text <- decode.field("text", decode.string)
      decode.success(ShoutOutput(text: text))
    },
  )
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
