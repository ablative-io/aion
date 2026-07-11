//// Probe: spec §Worker blocks — "a step may override `node`/`timeout` at the call
//// site when it must pin." No spelling is shown anywhere. Second guess: the action
//// declaration's indented config line, transplanted under the call.

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

pub type Done {
  Done(
    got: String,
  )
}

pub type Thing {
  Thing(
    value: String,
  )
}

pub type ProbeCallsiteOverrideInput {
  ProbeCallsiteOverrideInput(
    id: String,
  )
}

pub type ProbeCallsiteOverrideOutcome {
  DoneOutcome(Done)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(ProbeCallsiteOverrideInput, ProbeCallsiteOverrideOutcome, awl_error.AwlError) {
  workflow.define(
    "probe_callsite_override",
    probe_callsite_override_input_codec(),
    probe_callsite_override_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {
  runtime.run(raw_input, probe_callsite_override_input_codec(), probe_callsite_override_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: ProbeCallsiteOverrideInput) -> Result(ProbeCallsiteOverrideOutcome, awl_error.AwlError) {
  let id = input.id
  step_pin_it(id)
}

fn step_pin_it(id: String) -> Result(ProbeCallsiteOverrideOutcome, awl_error.AwlError) {
  use thing <- result.try(fetch_activity(id) |> activity.timeout(duration.milliseconds(30000)) |> activity.task_queue("probe") |> activity.node("shell") |> workflow.run |> awl_error.map_activity_error)
  Ok(DoneOutcome(Done(got: thing.value)))
}

pub type FetchInput {
  FetchInput(
    id: String,
  )
}

fn fetch_activity(
  id: String,
) -> activity.Activity(FetchInput, Thing) {
  activity.new(
    "fetch",
    FetchInput(
      id: id,
    ),
    fetch_input_codec(),
    thing_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn probe_callsite_override_input_codec() -> Codec(ProbeCallsiteOverrideInput) {
  codec.json_codec(probe_callsite_override_input_to_json, probe_callsite_override_input_decoder())
}

fn probe_callsite_override_input_to_json(value: ProbeCallsiteOverrideInput) -> json.Json {
  json.object([
    #("id", awlc.string_to_json(value.id)),
  ])
}

fn probe_callsite_override_input_decoder() -> decode.Decoder(ProbeCallsiteOverrideInput) {
  use id <- decode.field("id", awlc.string_decoder())
  decode.success(ProbeCallsiteOverrideInput(
    id: id,
  ))
}

fn probe_callsite_override_outcome_codec() -> Codec(ProbeCallsiteOverrideOutcome) {
  codec.json_codec(probe_callsite_override_outcome_to_json, probe_callsite_override_outcome_decoder())
}

fn probe_callsite_override_outcome_to_json(value: ProbeCallsiteOverrideOutcome) -> json.Json {
  case value {
    DoneOutcome(payload) -> json.object([#("outcome", json.string("done")), #("payload", done_to_json(payload))])
  }
}

fn probe_callsite_override_outcome_decoder() -> decode.Decoder(ProbeCallsiteOverrideOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "done" -> {
      use payload <- decode.field("payload", done_decoder())
      decode.success(DoneOutcome(payload))
    }
    _ -> decode.failure(DoneOutcome(Done(got: "")), "ProbeCallsiteOverrideOutcome")
  }
}

fn done_codec() -> Codec(Done) {
  codec.json_codec(done_to_json, done_decoder())
}

fn done_to_json(value: Done) -> json.Json {
  json.object([
    #("got", awlc.string_to_json(value.got)),
  ])
}

fn done_decoder() -> decode.Decoder(Done) {
  use got <- decode.field("got", awlc.string_decoder())
  decode.success(Done(
    got: got,
  ))
}

fn thing_codec() -> Codec(Thing) {
  codec.json_codec(thing_to_json, thing_decoder())
}

fn thing_to_json(value: Thing) -> json.Json {
  json.object([
    #("value", awlc.string_to_json(value.value)),
  ])
}

fn thing_decoder() -> decode.Decoder(Thing) {
  use value <- decode.field("value", awlc.string_decoder())
  decode.success(Thing(
    value: value,
  ))
}

fn fetch_input_codec() -> Codec(FetchInput) {
  codec.json_codec(fetch_input_to_json, fetch_input_decoder())
}

fn fetch_input_to_json(value: FetchInput) -> json.Json {
  json.object([
    #("id", awlc.string_to_json(value.id)),
  ])
}

fn fetch_input_decoder() -> decode.Decoder(FetchInput) {
  use id <- decode.field("id", awlc.string_decoder())
  decode.success(FetchInput(
    id: id,
  ))
}

