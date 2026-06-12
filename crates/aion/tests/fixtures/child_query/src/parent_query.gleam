//// Parent half of the `child_query` engine e2e fixture: registers a query
//// handler, spawns one `child_small` child, and parks in `child.await`. The
//// engine suite queries the parent while it is parked at that yield point
//// and asserts the run still completes with the child's output.

import aion/child
import aion/codec
import aion/query
import child_small
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json

pub type ParentError {
  ParentFailed(message: String)
}

pub fn run(raw_input: Dynamic) -> Result(String, ParentError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case child_small.child_input_codec().decode(raw_json) {
        Ok(input) -> spawn_and_await(input)
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(ParentFailed("failed to decode parent input: " <> reason))
      }
    Error(_) -> Error(ParentFailed("parent input payload was not a string"))
  }
}

fn spawn_and_await(
  input: child_small.ChildInput,
) -> Result(String, ParentError) {
  case query.handler("phase", phase_codec(), fn() { "awaiting-child" }) {
    Ok(_) ->
      case
        child.spawn(
          "child_small",
          child_small.process,
          input,
          child_small.child_input_codec(),
          child_small.child_output_codec(),
          child_small.child_error_codec(),
        )
      {
        Ok(handle) ->
          case child.await(handle) {
            Ok(output) -> Ok("child:" <> output)
            Error(_) -> Error(ParentFailed("child await failed"))
          }
        Error(_) -> Error(ParentFailed("child spawn failed"))
      }
    Error(_) -> Error(ParentFailed("query registration failed"))
  }
}

fn phase_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}
