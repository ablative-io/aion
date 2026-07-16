//// The generic run shell and literal-list indexing hoisted out of every
//// generated AWL module (AWL-BC-0, hoist-only).
////
//// `run` is the engine entry point's body: it reproduces the three-stage
//// decode/execute/encode case tree with the exact failure strings the emitter
//// used to inline. Each generated module keeps a three-line `run/1` wrapper
//// (the engine invokes it by name) that forwards to this function with the
//// module's own codecs and `execute`.

import aion/awl/error.{type AwlError, AwlDecodeInputFailed, AwlIndexOutOfRange}
import aion/codec.{type Codec}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/list

/// Decode the raw engine input to a string, decode it with `input_codec`, run
/// `execute`, and encode the outcome with `output_codec`. Failure strings are
/// byte-identical to the code the emitter used to inline.
///
/// A failure is returned as the `AwlError` codec's OWN encoding (the
/// `{"tag":…}` JSON object), exactly like `workflow.entrypoint` encodes a
/// definition's typed error. The engine records that text as the run's
/// failure details, and an awaiting parent decodes it back with the same
/// `aion/awl/error` codec it already holds — so typed kinds such as
/// `AwlVisitsExceeded` survive the parent-child boundary instead of leaking
/// the raw error record (whose term-to-JSON image is a *list*, which the
/// codec's object decoder can never accept).
pub fn run(
  raw_input: Dynamic,
  input_codec: Codec(input),
  output_codec: Codec(output),
  execute: fn(input) -> Result(output, AwlError),
) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case input_codec.decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(result) -> Ok(output_codec.encode(result))
            Error(workflow_error) -> Error(encode_error(workflow_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            encode_error(AwlDecodeInputFailed(
              "failed to decode workflow input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        encode_error(AwlDecodeInputFailed(
          "workflow input payload was not a string",
        )),
      )
  }
}

/// Encode a typed AWL failure with the fixed `AwlError` codec, the one wire
/// shape `aion/awl/error`'s decoder round-trips.
fn encode_error(workflow_error: AwlError) -> String {
  let error_codec = error.codec()
  error_codec.encode(workflow_error)
}

/// Literal-only list indexing; out of range is a step failure carrying the
/// source-anchored label the emitter built.
pub fn index(items: List(a), index: Int, label: String) -> Result(a, AwlError) {
  case list.drop(items, index) |> list.first {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlIndexOutOfRange(label))
  }
}
