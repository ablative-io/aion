//// Greet a name, then shout it — the first workflow written in AWL and run for real.

import aion/activity
import aion/awl/codec as awlc
import aion/awl/error as awl_error
import aion/awl/runtime
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
import gleam/result

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

pub type AwlHelloInput {
  AwlHelloInput(
    name: String,
  )
}

pub type AwlHelloOutcome {
  ShoutedOutcome(Shouted)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(AwlHelloInput, AwlHelloOutcome, awl_error.AwlError) {
  workflow.define(
    "awl_hello",
    awl_hello_input_codec(),
    awl_hello_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {
  runtime.run(raw_input, awl_hello_input_codec(), awl_hello_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: AwlHelloInput) -> Result(AwlHelloOutcome, awl_error.AwlError) {
  let name = input.name
  step_greet_and_shout(name)
}

fn step_greet_and_shout(name: String) -> Result(AwlHelloOutcome, awl_error.AwlError) {
  use awl_piped_0 <- result.try(greet_activity(name) |> activity.task_queue("awl_hello") |> workflow.run |> awl_error.map_activity_error)
  let awl_piped_1 = awl_piped_0.greeting
  use awl_piped_2 <- result.try(shout_activity(awl_piped_1) |> activity.task_queue("awl_hello") |> workflow.run |> awl_error.map_activity_error)
  Ok(ShoutedOutcome(awl_piped_2))
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

fn awl_hello_input_codec() -> Codec(AwlHelloInput) {
  codec.json_codec(awl_hello_input_to_json, awl_hello_input_decoder())
}

fn awl_hello_input_to_json(value: AwlHelloInput) -> json.Json {
  json.object([
    #("name", awlc.string_to_json(value.name)),
  ])
}

fn awl_hello_input_decoder() -> decode.Decoder(AwlHelloInput) {
  use name <- decode.field("name", awlc.string_decoder())
  decode.success(AwlHelloInput(
    name: name,
  ))
}

fn awl_hello_outcome_codec() -> Codec(AwlHelloOutcome) {
  codec.json_codec(awl_hello_outcome_to_json, awl_hello_outcome_decoder())
}

fn awl_hello_outcome_to_json(value: AwlHelloOutcome) -> json.Json {
  case value {
    ShoutedOutcome(payload) -> json.object([#("outcome", json.string("shouted")), #("payload", shouted_to_json(payload))])
  }
}

fn awl_hello_outcome_decoder() -> decode.Decoder(AwlHelloOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "shouted" -> {
      use payload <- decode.field("payload", shouted_decoder())
      decode.success(ShoutedOutcome(payload))
    }
    _ -> decode.failure(ShoutedOutcome(Shouted(text: "")), "AwlHelloOutcome")
  }
}

fn greeting_codec() -> Codec(Greeting) {
  codec.json_codec(greeting_to_json, greeting_decoder())
}

fn greeting_to_json(value: Greeting) -> json.Json {
  json.object([
    #("greeting", awlc.string_to_json(value.greeting)),
  ])
}

fn greeting_decoder() -> decode.Decoder(Greeting) {
  use greeting <- decode.field("greeting", awlc.string_decoder())
  decode.success(Greeting(
    greeting: greeting,
  ))
}

fn shouted_codec() -> Codec(Shouted) {
  codec.json_codec(shouted_to_json, shouted_decoder())
}

fn shouted_to_json(value: Shouted) -> json.Json {
  json.object([
    #("text", awlc.string_to_json(value.text)),
  ])
}

fn shouted_decoder() -> decode.Decoder(Shouted) {
  use text <- decode.field("text", awlc.string_decoder())
  decode.success(Shouted(
    text: text,
  ))
}

fn greet_input_codec() -> Codec(GreetInput) {
  codec.json_codec(greet_input_to_json, greet_input_decoder())
}

fn greet_input_to_json(value: GreetInput) -> json.Json {
  json.object([
    #("name", awlc.string_to_json(value.name)),
  ])
}

fn greet_input_decoder() -> decode.Decoder(GreetInput) {
  use name <- decode.field("name", awlc.string_decoder())
  decode.success(GreetInput(
    name: name,
  ))
}

fn shout_input_codec() -> Codec(ShoutInput) {
  codec.json_codec(shout_input_to_json, shout_input_decoder())
}

fn shout_input_to_json(value: ShoutInput) -> json.Json {
  json.object([
    #("text", awlc.string_to_json(value.text)),
  ])
}

fn shout_input_decoder() -> decode.Decoder(ShoutInput) {
  use text <- decode.field("text", awlc.string_decoder())
  decode.success(ShoutInput(
    text: text,
  ))
}

