//// The fixed AWL workflow-error taxonomy: the `AwlError` type every generated
//// AWL module returns, its wire codec, and the six runtime-error mappers.
////
//// Hoisted out of the emitted surface by AWL-BC-0 (hoist-only): the codec and
//// the mappers are workflow-independent fixed glue, identical across every
//// generated module. The variant atoms and the JSON shape are byte-identical
//// to the code the emitter used to inline, so durable trails and wire payloads
//// are unchanged by the move.

import aion/codec.{type Codec}
import aion/error
import gleam/dynamic/decode
import gleam/json

/// The error type every generated AWL workflow returns. One variant per
/// failure class the language surfaces; `AwlOutcomeFailure` carries the routed
/// failure outcome name and its encoded payload.
pub type AwlError {
  AwlDecodeInputFailed(String)
  AwlActivityFailed(String)
  AwlSignalFailed(String)
  AwlChildFailed(String)
  AwlTimerFailed(String)
  AwlTimedOut(String)
  AwlIndexOutOfRange(String)
  AwlVisitsExceeded(String)
  AwlOutcomeFailure(outcome: String, payload: String)
  AwlFailed
}

/// The workflow-error codec bound into every generated `definition()`.
pub fn codec() -> Codec(AwlError) {
  codec.json_codec(to_json, decoder())
}

fn to_json(error_value: AwlError) -> json.Json {
  case error_value {
    AwlDecodeInputFailed(message) ->
      json.object([
        #("tag", json.string("AwlDecodeInputFailed")),
        #("message", json.string(message)),
      ])
    AwlActivityFailed(message) ->
      json.object([
        #("tag", json.string("AwlActivityFailed")),
        #("message", json.string(message)),
      ])
    AwlSignalFailed(message) ->
      json.object([
        #("tag", json.string("AwlSignalFailed")),
        #("message", json.string(message)),
      ])
    AwlChildFailed(message) ->
      json.object([
        #("tag", json.string("AwlChildFailed")),
        #("message", json.string(message)),
      ])
    AwlTimerFailed(message) ->
      json.object([
        #("tag", json.string("AwlTimerFailed")),
        #("message", json.string(message)),
      ])
    AwlTimedOut(message) ->
      json.object([
        #("tag", json.string("AwlTimedOut")),
        #("message", json.string(message)),
      ])
    AwlIndexOutOfRange(message) ->
      json.object([
        #("tag", json.string("AwlIndexOutOfRange")),
        #("message", json.string(message)),
      ])
    AwlVisitsExceeded(message) ->
      json.object([
        #("tag", json.string("AwlVisitsExceeded")),
        #("message", json.string(message)),
      ])
    AwlOutcomeFailure(outcome, payload) ->
      json.object([
        #("tag", json.string("AwlOutcomeFailure")),
        #("outcome", json.string(outcome)),
        #("payload", json.string(payload)),
      ])
    AwlFailed -> json.object([#("tag", json.string("AwlFailed"))])
  }
}

fn decoder() -> decode.Decoder(AwlError) {
  use tag <- decode.field("tag", decode.string)
  case tag {
    "AwlDecodeInputFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlDecodeInputFailed(message))
    }
    "AwlActivityFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlActivityFailed(message))
    }
    "AwlSignalFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlSignalFailed(message))
    }
    "AwlChildFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlChildFailed(message))
    }
    "AwlTimerFailed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlTimerFailed(message))
    }
    "AwlTimedOut" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlTimedOut(message))
    }
    "AwlIndexOutOfRange" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlIndexOutOfRange(message))
    }
    "AwlVisitsExceeded" -> {
      use message <- decode.field("message", decode.string)
      decode.success(AwlVisitsExceeded(message))
    }
    "AwlOutcomeFailure" -> {
      use outcome <- decode.field("outcome", decode.string)
      use payload <- decode.field("payload", decode.string)
      decode.success(AwlOutcomeFailure(outcome: outcome, payload: payload))
    }
    "AwlFailed" -> decode.success(AwlFailed)
    _ -> decode.failure(AwlFailed, "AwlError")
  }
}

/// Collapse an activity dispatch failure to a step failure.
pub fn map_activity_error(
  result: Result(a, error.ActivityError),
) -> Result(a, AwlError) {
  case result {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlActivityFailed("activity failed"))
  }
}

/// Collapse a signal receive failure to a step failure.
pub fn map_receive_error(
  result: Result(a, error.ReceiveError),
) -> Result(a, AwlError) {
  case result {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlSignalFailed("signal receive failed"))
  }
}

/// Collapse an awaited child-workflow failure to a step failure.
pub fn map_child_error(
  result: Result(a, error.ChildError(AwlError)),
) -> Result(a, AwlError) {
  case result {
    Ok(value) -> Ok(value)
    Error(error.ChildWorkflowFailed(child_error)) -> Error(child_error)
    Error(error.ChildOutputDecodeFailed(decode_error)) ->
      Error(AwlChildFailed(
        "child output decode failed: " <> decode_error.reason,
      ))
    Error(error.ChildErrorDecodeFailed(decode_error)) ->
      Error(AwlChildFailed("child error decode failed: " <> decode_error.reason))
    Error(error.ChildEngineFailure(message)) ->
      Error(AwlChildFailed("child engine failure: " <> message))
  }
}

/// Collapse a detached-spawn failure to a step failure.
pub fn map_spawn_error(
  result: Result(a, error.EngineError),
) -> Result(a, AwlError) {
  case result {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlChildFailed("detached spawn failed"))
  }
}

/// Collapse a timer/sleep failure to a step failure.
pub fn map_timer_error(
  result: Result(a, error.EngineError),
) -> Result(a, AwlError) {
  case result {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlTimerFailed("timer failed"))
  }
}

/// Collapse a workflow-context lookup failure to the language's generic
/// workflow failure. The context lookup is normally infallible for registered
/// workflow code; retaining the `Result` keeps misuse outside that context loud.
pub fn map_engine_error(
  result: Result(a, error.EngineError),
) -> Result(a, AwlError) {
  case result {
    Ok(value) -> Ok(value)
    Error(_) -> Error(AwlFailed)
  }
}
