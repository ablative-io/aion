//// workflow.entrypoint: the engine-facing run adapter, assembled from a
//// `WorkflowDefinition`'s codecs and typed entry function.
////
//// The engine spawns `entry_module:entry_function/1` with one argument: a
//// BEAM binary holding the start input's JSON text. Every workflow used to
//// hand-write the same adapter — decode the binary to a string, decode the
//// string with the input codec, call the typed entry function, encode the
//// output or error back to JSON text. `entrypoint` is that adapter, built
//// purely from the definition's own accessors, so a workflow module's engine
//// entry collapses to one line:
////
//// ```gleam
//// pub fn run(raw_input: Dynamic) -> Result(String, String) {
////   workflow.entrypoint(definition(), raw_input)
//// }
//// ```
////
//// Success and typed failure are byte-identical to the hand-written adapter:
//// `Ok` encodes with the definition's output codec, `Error` with its error
//// codec, so an awaiting parent decodes both with the same codecs it already
//// holds. Only the garbage-input edge has a fixed shape: when the raw payload
//// is not a string or the input codec rejects it, the failure payload is the
//// documented JSON envelope
//// `{"aion_error":"input_decode","reason":...,"path":[...]}` — the engine
//// records it as failure details, and a parent awaiting the child surfaces it
//// as typed error-decode data, never a crash.

import aion/codec
import aion/workflow/define.{type WorkflowDefinition}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

/// Drive a workflow's typed entry from the engine's raw spawn argument.
///
/// Decodes `raw_input` (a BEAM binary of JSON text) with the definition's
/// input codec, invokes the typed entry function, and encodes the outcome
/// with the definition's output or error codec. An undecodable input yields
/// `Error` of the documented `aion_error: input_decode` JSON envelope.
pub fn entrypoint(
  definition: WorkflowDefinition(input, output, workflow_error),
  raw_input: Dynamic,
) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) -> run_typed(definition, raw_json)
    Error(_) ->
      Error(
        input_decode_envelope("workflow input payload was not a string", []),
      )
  }
}

/// Decode the JSON text, run the typed entry, and encode the outcome.
fn run_typed(
  definition: WorkflowDefinition(input, output, workflow_error),
  raw_json: String,
) -> Result(String, String) {
  let input_codec = define.input_codec(definition)
  case input_codec.decode(raw_json) {
    Ok(input) -> {
      let entry = define.entry_fn(definition)
      case entry(input) {
        Ok(output) -> {
          let output_codec = define.output_codec(definition)
          Ok(output_codec.encode(output))
        }
        Error(workflow_error) -> {
          let error_codec = define.error_codec(definition)
          Error(error_codec.encode(workflow_error))
        }
      }
    }
    Error(codec.DecodeError(reason: reason, path: path)) ->
      Error(input_decode_envelope(reason, path))
  }
}

/// The documented input-decode failure envelope:
/// `{"aion_error":"input_decode","reason":...,"path":[...]}`.
fn input_decode_envelope(reason: String, path: List(String)) -> String {
  json.object([
    #("aion_error", json.string("input_decode")),
    #("reason", json.string(reason)),
    #("path", json.array(path, json.string)),
  ])
  |> json.to_string
}
