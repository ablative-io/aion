//// Typed activity mock registry for `aion/testing`.
////
//// Tests register mocks against an `Activity(i, o)` value so the handler is
//// statically checked against the same input and output types that
//// `workflow.run` will use. The test-only FFI double stores a type-erased
//// wrapper in process-scoped state and intercepts activity dispatch by name.

import aion/activity.{type Activity, input_codec, name, output_codec}
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
