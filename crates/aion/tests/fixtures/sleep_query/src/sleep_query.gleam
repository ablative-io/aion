//// Engine e2e fixture built from committed SDK source.
////
//// The workflow registers two query handlers — `status`, which answers
//// normally, and `boom`, which panics — then parks in a durable sleep and
//// completes. The engine suite uses it to prove three release-integrity
//// invariants on the real SDK→engine contract:
////
//// 1. a suspending await resumes correctly on wake (the minimal sleep
////    baseline for the two-phase suspension protocol);
//// 2. a query delivered while the workflow is suspended is answered at the
////    sleep yield point without disturbing the run;
//// 3. a raising query handler is converted into a typed query failure and
////    never kills the run.

import aion/codec
import aion/duration
import aion/error
import aion/query
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

pub type SleepInput {
  SleepInput(sleep_ms: Int)
}

pub type WorkflowError {
  SleepFailed(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case sleep_input_codec().decode(raw_json) {
        Ok(input) -> sleep_with_queries(input)
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(SleepFailed("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(SleepFailed("workflow input payload was not a string"))
  }
}

fn sleep_with_queries(input: SleepInput) -> Result(String, WorkflowError) {
  let status_registered =
    query.handler("status", status_codec(), fn() { "sleeping" })
  let boom_registered =
    query.handler("boom", status_codec(), fn() {
      panic as "deliberate handler failure"
    })
  case status_registered, boom_registered {
    Ok(_), Ok(_) ->
      case workflow.sleep(duration.milliseconds(input.sleep_ms)) {
        Ok(_) -> Ok("slept")
        Error(error.EngineFailure(message: message)) ->
          Error(SleepFailed("sleep failed: " <> message))
      }
    _, _ -> Error(SleepFailed("query handler registration failed"))
  }
}

fn sleep_input_codec() -> codec.Codec(SleepInput) {
  codec.json_codec(sleep_input_to_json, sleep_input_decoder())
}

fn sleep_input_to_json(input: SleepInput) -> json.Json {
  json.object([#("sleep_ms", json.int(input.sleep_ms))])
}

fn sleep_input_decoder() -> decode.Decoder(SleepInput) {
  use sleep_ms <- decode.field("sleep_ms", decode.int)
  decode.success(SleepInput(sleep_ms: sleep_ms))
}

fn status_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}
