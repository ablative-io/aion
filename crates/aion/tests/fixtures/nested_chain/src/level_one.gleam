//// Top level of the `nested_chain` fixture: registers a query handler,
//// spawns one `level_two` child (which spawns `level_three`), parks in
//// `child.await`, and prefixes the propagated output.
////
//// A child-await failure deliberately COMPLETES this workflow with a
//// descriptive payload instead of failing it: the cancellation-semantics
//// test pins the exact `child-failed:cancelled:<reason>` text a parent's
//// await observes when its child is cancelled mid-flight (the engine
//// records the child terminal as a parent-side `ChildWorkflowFailed` whose
//// message is the non-JSON `cancelled:<reason>` marker, decoded verbatim
//// here through the fixture's passthrough error codec).

import aion/child
import aion/codec
import aion/error
import aion/query
import chain_types.{type ChainInput}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import level_two

/// A deployed workflow type is its entry module name.
pub const workflow_type = "level_one"

pub const status_query_name = "level_one_status"

/// Typed workflow logic.
pub fn process(input: ChainInput) -> Result(String, String) {
  case
    query.handler(status_query_name, chain_types.text_codec(), fn() {
      "awaiting-level-two:" <> input.job_id
    })
  {
    Ok(Nil) -> spawn_level_two(input)
    Error(_) -> Error("level_one: query registration failed")
  }
}

fn spawn_level_two(input: ChainInput) -> Result(String, String) {
  case
    child.spawn(
      level_two.workflow_type,
      level_two.process,
      input,
      chain_types.chain_input_codec(),
      chain_types.text_codec(),
      chain_types.raw_error_codec(),
    )
  {
    Ok(handle) ->
      case child.await(handle) {
        Ok(output) -> Ok("l1:" <> output)
        Error(child_error) ->
          Ok("l1-" <> chain_types.describe_child_error(child_error))
      }
    Error(error.EngineFailure(message: message)) ->
      Error("level_one: spawn failed: " <> message)
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
          Error("level_one: failed to decode input: " <> reason)
      }
    Error(_) -> Error("level_one: input payload was not a string")
  }
}
