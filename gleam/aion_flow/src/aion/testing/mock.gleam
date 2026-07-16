//// Typed activity and child-workflow mock registries for `aion/testing`.
////
//// Tests register activity mocks against an `Activity(i, o)` value so the
//// handler is statically checked against the same input and output types that
//// `workflow.run` will use, and child doubles against the input/output/error
//// codecs that `workflow.spawn_and_wait` will use. The test-only FFI double
//// stores a type-erased wrapper in process-scoped state and intercepts
//// activity dispatch and child spawn by name.

import aion/activity.{type Activity, input_codec, name, output_codec}
import aion/codec.{type Codec}
import aion/error
import aion/internal/ffi

/// Register a typed activity mock for the current process.
///
/// A handler whose input or output type does not match the supplied activity will
/// fail at `gleam build`, before the workflow test can run.
pub fn activity(
  env: env,
  activity_value: Activity(input, output),
  handler: fn(input) -> Result(output, error.ActivityError),
) -> Result(env, error.EngineError) {
  let input_codec = input_codec(activity_value)
  let output_codec = output_codec(activity_value)
  let name = name(activity_value)
  let raw_handler = fn(raw_input: String) {
    case input_codec.decode(raw_input) {
      Ok(typed_input) ->
        case handler(typed_input) {
          Ok(typed_output) -> Ok(output_codec.encode(typed_output))
          Error(activity_error) -> Error(activity_error_to_raw(activity_error))
        }
      Error(decode_error) ->
        Error("terminal:mock input decode failed: " <> decode_error.reason)
    }
  }

  case ffi.testing_register_activity_mock(name, raw_handler) {
    Ok(_) -> Ok(env)
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

/// Register a typed child-workflow double for the current test process.
///
/// `workflow.spawn_and_wait(name, ...)` calls with the same `name` execute
/// `handler` synchronously and record its typed result as the child terminal:
/// `Ok` is decoded by the parent's output codec and a typed `Error` surfaces
/// as `error.ChildWorkflowFailed`. Registering the child module's real
/// `execute` function as the handler runs the full child workflow body —
/// including its own activity dispatches against this process's activity
/// mocks — inside the parent test.
pub fn child(
  env: env,
  name: String,
  child_input_codec: Codec(input),
  child_output_codec: Codec(output),
  child_error_codec: Codec(workflow_error),
  handler: fn(input) -> Result(output, workflow_error),
) -> Result(env, error.EngineError) {
  let raw_handler = fn(raw_input: String) {
    case child_input_codec.decode(raw_input) {
      Ok(typed_input) ->
        case handler(typed_input) {
          Ok(typed_output) ->
            Ok("ok:" <> child_output_codec.encode(typed_output))
          Error(workflow_error) ->
            Ok("error:" <> child_error_codec.encode(workflow_error))
        }
      Error(decode_error) ->
        Error("child mock input decode failed: " <> decode_error.reason)
    }
  }

  case ffi.testing_register_child_mock(name, raw_handler) {
    Ok(_) -> Ok(env)
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

fn activity_error_to_raw(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> "retryable:" <> message
    error.Terminal(message: message, details: _) -> "terminal:" <> message
    error.ActivityDecodeFailed(decode_error) ->
      "terminal:activity decode failed: " <> decode_error.reason
    error.ActivityTimedOut(error.TimedOut(message: message)) ->
      "timeout:" <> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) ->
      "cancelled:" <> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> "non_determinism:" <> message
    error.ActivityEngineFailure(message: message) -> message
  }
}
