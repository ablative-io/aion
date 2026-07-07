//// Greet a name, then shout it — the first workflow written in AWL and run for real.

import aion/activity
import aion/codec.{type Codec}
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}

pub type AwlError {
  AwlDecodeInputFailed(String)
  AwlActivityFailed(String)
  AwlSignalFailed(String)
  AwlChildFailed(String)
  AwlTimerFailed(String)
  AwlTimedOut(String)
  AwlFailed
}

pub type AwlHelloInput {
  AwlHelloInput(
    name: String,
  )
}

pub type Greeting {
  Greeting(
    greeting: String,
  )
}

pub type Shouted {
  Shouted(
    text: String,
  )
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(AwlHelloInput, Shouted, AwlError) {
  workflow.define(
    "awl_hello",
    awl_hello_input_codec(),
    shouted_codec(),
    awl_error_codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, AwlError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case awl_hello_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(result) -> Ok(shouted_codec().encode(result))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(AwlDecodeInputFailed("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(AwlDecodeInputFailed("workflow input payload was not a string"))
  }
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: AwlHelloInput) -> Result(Shouted, AwlError) {
  let name = input.name

  // One activity call: compose the greeting.
  let assert Ok(greeting) =
    greet_activity(name) |> activity.task_queue("awl_hello") |> activity.node("hello") |> workflow.run |> map_activity_error

  // Shout it back.
  let assert Ok(shouted) =
    shout_activity(greeting.greeting) |> activity.task_queue("awl_hello") |> activity.node("hello") |> workflow.run |> map_activity_error

  Ok(shouted)
}

pub type GreetInput {
  GreetInput(
    name: String,
  )
}

fn greet_activity(
  name: String,
) -> activity.Activity(GreetInput, Greeting) {
  activity.new(
    "greet",
    GreetInput(
      name: name,
    ),
    greet_input_codec(),
    greeting_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type ShoutInput {
  ShoutInput(
    text: String,
  )
}

fn shout_activity(
  text: String,
) -> activity.Activity(ShoutInput, Shouted) {
  activity.new(
    "shout",
    ShoutInput(
      text: text,
    ),
    shout_input_codec(),
    shouted_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn awl_error_codec() -> Codec(AwlError) {
  codec.json_codec(awl_error_to_json, awl_error_decoder())
}

fn awl_error_to_json(error_value: AwlError) -> json.Json {
  case error_value {
    AwlDecodeInputFailed(message) -> json.object([#("tag", json.string("AwlDecodeInputFailed")), #("message", json.string(message))])
    AwlActivityFailed(message) -> json.object([#("tag", json.string("AwlActivityFailed")), #("message", json.string(message))])
    AwlSignalFailed(message) -> json.object([#("tag", json.string("AwlSignalFailed")), #("message", json.string(message))])
    AwlChildFailed(message) -> json.object([#("tag", json.string("AwlChildFailed")), #("message", json.string(message))])
    AwlTimerFailed(message) -> json.object([#("tag", json.string("AwlTimerFailed")), #("message", json.string(message))])
    AwlTimedOut(message) -> json.object([#("tag", json.string("AwlTimedOut")), #("message", json.string(message))])
    AwlFailed -> json.object([#("tag", json.string("AwlFailed"))])
  }
}

fn awl_error_decoder() -> decode.Decoder(AwlError) {
  use tag <- decode.field("tag", decode.string)
  case tag {
    "AwlDecodeInputFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlDecodeInputFailed(message))
    }
    "AwlActivityFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlActivityFailed(message))
    }
    "AwlSignalFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlSignalFailed(message))
    }
    "AwlChildFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlChildFailed(message))
    }
    "AwlTimerFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlTimerFailed(message))
    }
    "AwlTimedOut" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlTimedOut(message))
    }
    "AwlFailed" -> decode.success(AwlFailed)
    _ -> decode.failure(AwlFailed, "AwlError")
  }
}

fn awl_hello_input_codec() -> Codec(AwlHelloInput) {
  codec.json_codec(awl_hello_input_to_json, awl_hello_input_decoder())
}

fn awl_hello_input_to_json(awl_hello_input: AwlHelloInput) -> json.Json {
  json.object([
    #("name", string_to_json(awl_hello_input.name)),
  ])
}

fn awl_hello_input_decoder() -> decode.Decoder(AwlHelloInput) {
  use name <- decode.field("name", string_decoder())
  decode.success(AwlHelloInput(
    name: name,
  ))
}

fn greeting_codec() -> Codec(Greeting) {
  codec.json_codec(greeting_to_json, greeting_decoder())
}

fn greeting_to_json(greeting: Greeting) -> json.Json {
  json.object([
    #("greeting", string_to_json(greeting.greeting)),
  ])
}

fn greeting_decoder() -> decode.Decoder(Greeting) {
  use greeting <- decode.field("greeting", string_decoder())
  decode.success(Greeting(
    greeting: greeting,
  ))
}

fn shouted_codec() -> Codec(Shouted) {
  codec.json_codec(shouted_to_json, shouted_decoder())
}

fn shouted_to_json(shouted: Shouted) -> json.Json {
  json.object([
    #("text", string_to_json(shouted.text)),
  ])
}

fn shouted_decoder() -> decode.Decoder(Shouted) {
  use text <- decode.field("text", string_decoder())
  decode.success(Shouted(
    text: text,
  ))
}

fn greet_input_codec() -> Codec(GreetInput) {
  codec.json_codec(greet_input_to_json, greet_input_decoder())
}

fn greet_input_to_json(greet_input: GreetInput) -> json.Json {
  json.object([
    #("name", string_to_json(greet_input.name)),
  ])
}

fn greet_input_decoder() -> decode.Decoder(GreetInput) {
  use name <- decode.field("name", string_decoder())
  decode.success(GreetInput(
    name: name,
  ))
}

fn shout_input_codec() -> Codec(ShoutInput) {
  codec.json_codec(shout_input_to_json, shout_input_decoder())
}

fn shout_input_to_json(shout_input: ShoutInput) -> json.Json {
  json.object([
    #("text", string_to_json(shout_input.text)),
  ])
}

fn shout_input_decoder() -> decode.Decoder(ShoutInput) {
  use text <- decode.field("text", string_decoder())
  decode.success(ShoutInput(
    text: text,
  ))
}

fn string_codec() -> Codec(String) { codec.json_codec(json.string, decode.string) }
fn int_codec() -> Codec(Int) { codec.json_codec(json.int, decode.int) }
fn float_codec() -> Codec(Float) { codec.json_codec(json.float, decode.float) }
fn bool_codec() -> Codec(Bool) { codec.json_codec(json.bool, decode.bool) }
fn nil_codec() -> Codec(Nil) { codec.json_codec(fn(_) { json.object([]) }, decode.success(Nil)) }

fn string_to_json(value: String) -> json.Json { json.string(value) }
fn int_to_json(value: Int) -> json.Json { json.int(value) }
fn float_to_json(value: Float) -> json.Json { json.float(value) }
fn bool_to_json(value: Bool) -> json.Json { json.bool(value) }
fn nil_to_json(_: Nil) -> json.Json { json.object([]) }

fn string_decoder() -> decode.Decoder(String) { decode.string }
fn int_decoder() -> decode.Decoder(Int) { decode.int }
fn float_decoder() -> decode.Decoder(Float) { decode.float }
fn bool_decoder() -> decode.Decoder(Bool) { decode.bool }
fn nil_decoder() -> decode.Decoder(Nil) { decode.success(Nil) }

fn list_to_json(values: List(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.array(values, item_to_json) }
fn list_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(List(a)) { decode.list(item_decoder) }
fn option_to_json(value: Option(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.nullable(value, item_to_json) }
fn option_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(Option(a)) { decode.optional(item_decoder) }

fn try(result: Result(a, AwlError), next: fn(a) -> Result(b, AwlError)) -> Result(b, AwlError) {
  case result { Ok(value) -> next(value) Error(awl_error) -> Error(awl_error) }
}

fn map_activity_error(result: Result(a, error.ActivityError)) -> Result(a, AwlError) {
  case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlActivityFailed("activity failed")) }
}

fn map_receive_error(result: Result(a, error.ReceiveError)) -> Result(a, AwlError) {
  case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlSignalFailed("signal receive failed")) }
}

fn map_child_error(result: Result(a, error.ChildError(AwlError))) -> Result(a, AwlError) {
  case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlChildFailed("child workflow failed")) }
}

fn map_timer_error(result: Result(a, error.EngineError)) -> Result(a, AwlError) {
  case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlTimerFailed("timer failed")) }
}

