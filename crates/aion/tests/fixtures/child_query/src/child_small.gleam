//// Child half of the `child_query` engine e2e fixture: sleeps for the
//// requested duration and completes with a deliberately tiny output, so the
//// parent's child-terminal envelope stays within beamr 0.5.0's inline-binary
//// limits and the suite isolates child-await + query semantics.

import aion/codec
import aion/duration
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

pub type ChildInput {
  ChildInput(sleep_ms: Int)
}

pub type ChildError {
  ChildFailed(message: String)
}

/// The child workflow's typed logic, also used by the parent's
/// `child.spawn` call as the type anchor for the handle codecs.
pub fn process(input: ChildInput) -> Result(String, ChildError) {
  case workflow.sleep(duration.milliseconds(input.sleep_ms)) {
    Ok(_) -> Ok("done")
    Error(error.EngineFailure(message: message)) ->
      Error(ChildFailed("sleep failed: " <> message))
  }
}

pub fn run(raw_input: Dynamic) -> Result(String, ChildError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case child_input_codec().decode(raw_json) {
        Ok(input) -> process(input)
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(ChildFailed("failed to decode child input: " <> reason))
      }
    Error(_) -> Error(ChildFailed("child input payload was not a string"))
  }
}

pub fn child_input_codec() -> codec.Codec(ChildInput) {
  codec.json_codec(child_input_to_json, child_input_decoder())
}

fn child_input_to_json(input: ChildInput) -> json.Json {
  json.object([#("sleep_ms", json.int(input.sleep_ms))])
}

fn child_input_decoder() -> decode.Decoder(ChildInput) {
  use sleep_ms <- decode.field("sleep_ms", decode.int)
  decode.success(ChildInput(sleep_ms: sleep_ms))
}

pub fn child_output_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

pub fn child_error_codec() -> codec.Codec(ChildError) {
  codec.json_codec(child_error_to_json, child_error_decoder())
}

fn child_error_to_json(child_error: ChildError) -> json.Json {
  case child_error {
    ChildFailed(message: message) ->
      json.object([#("message", json.string(message))])
  }
}

fn child_error_decoder() -> decode.Decoder(ChildError) {
  use message <- decode.field("message", decode.string)
  decode.success(ChildFailed(message: message))
}
