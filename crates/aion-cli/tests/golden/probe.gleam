//// Probe: make a note of the token and hand it back.

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
import gleam/option.{type Option, None, Some}
import gleam/result

pub type ProbeInput {
  ProbeInput(
    token: String,
  )
}

pub type ProbeOutcome {
  DoneOutcome(String)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(ProbeInput, ProbeOutcome, awl_error.AwlError) {
  workflow.define(
    "probe",
    probe_input_codec(),
    probe_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  runtime.run(raw_input, probe_input_codec(), probe_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: ProbeInput) -> Result(ProbeOutcome, awl_error.AwlError) {
  let token = input.token
  step_one(token)
}

fn step_one(token: String) -> Result(ProbeOutcome, awl_error.AwlError) {
  use awl_piped_0 <- result.try(make_activity(token) |> activity.task_queue("probe") |> workflow.run |> awl_error.map_activity_error)
  Ok(DoneOutcome(awl_piped_0))
}

pub type MakeInput {
  MakeInput(
    token: String,
  )
}

fn make_activity(
  token: String,
) -> activity.Activity(MakeInput, String) {
  activity.new(
    "make",
    MakeInput(
      token: token,
    ),
    make_input_codec(),
    awlc.string_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn probe_input_codec() -> Codec(ProbeInput) {
  codec.json_codec(probe_input_to_json, probe_input_decoder())
}

fn probe_input_to_json(value: ProbeInput) -> json.Json {
  json.object([
    #("token", awlc.string_to_json(value.token)),
  ])
}

fn probe_input_decoder() -> decode.Decoder(ProbeInput) {
  use token <- decode.field("token", awlc.string_decoder())
  decode.success(ProbeInput(
    token: token,
  ))
}

fn probe_outcome_codec() -> Codec(ProbeOutcome) {
  codec.json_codec(probe_outcome_to_json, probe_outcome_decoder())
}

fn probe_outcome_to_json(value: ProbeOutcome) -> json.Json {
  case value {
    DoneOutcome(payload) -> json.object([#("outcome", json.string("done")), #("payload", awlc.string_to_json(payload))])
  }
}

fn probe_outcome_decoder() -> decode.Decoder(ProbeOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "done" -> {
      use payload <- decode.field("payload", awlc.string_decoder())
      decode.success(DoneOutcome(payload))
    }
    _ -> decode.failure(DoneOutcome(""), "ProbeOutcome")
  }
}

fn make_input_codec() -> Codec(MakeInput) {
  codec.json_codec(make_input_to_json, make_input_decoder())
}

fn make_input_to_json(value: MakeInput) -> json.Json {
  json.object([
    #("token", awlc.string_to_json(value.token)),
  ])
}

fn make_input_decoder() -> decode.Decoder(MakeInput) {
  use token <- decode.field("token", awlc.string_decoder())
  decode.success(MakeInput(
    token: token,
  ))
}

