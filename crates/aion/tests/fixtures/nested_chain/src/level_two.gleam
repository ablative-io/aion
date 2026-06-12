//// Middle level of the `nested_chain` fixture: spawns one `level_three`
//// child, parks in `child.await`, and prefixes the child's output. The
//// cancellation-semantics test cancels THIS workflow mid-flight to pin what
//// happens to its still-running `level_three` child (orphaned today) and
//// what the awaiting `level_one` parent observes.

import aion/child
import aion/codec
import aion/error
import chain_types.{type ChainInput}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import level_three

/// A deployed workflow type is its entry module name.
pub const workflow_type = "level_two"

/// Typed workflow logic, also the parent's `child.spawn` type anchor.
pub fn process(input: ChainInput) -> Result(String, String) {
  case
    child.spawn(
      level_three.workflow_type,
      level_three.process,
      input,
      chain_types.chain_input_codec(),
      chain_types.text_codec(),
      chain_types.raw_error_codec(),
    )
  {
    Ok(handle) ->
      case child.await(handle) {
        Ok(output) -> Ok("l2:" <> output)
        Error(child_error) ->
          Error("level_two: " <> chain_types.describe_child_error(child_error))
      }
    Error(error.EngineFailure(message: message)) ->
      Error("level_two: spawn failed: " <> message)
  }
}

/// Engine entry point: the runtime delivers the start input as a raw JSON
/// string; the recorded result payload is the JSON-encoded output string.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case chain_types.chain_input_codec().decode(raw_json) {
        Ok(input) ->
          case process(input) {
            Ok(output) -> Ok(chain_types.text_codec().encode(output))
            Error(message) -> Error(message)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error("level_two: failed to decode input: " <> reason)
      }
    Error(_) -> Error("level_two: input payload was not a string")
  }
}
