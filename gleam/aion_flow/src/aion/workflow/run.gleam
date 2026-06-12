//// workflow.run (recorded activity dispatch) + now + random (determinism bindings)

import aion/activity.{type Activity}
import aion/duration
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/float
import gleam/int
import gleam/json
import gleam/option.{None, Some}
import gleam/string

/// A timestamp supplied by AD's determinism context.
///
/// The inner value is the canonical millisecond timestamp string returned by the
/// engine. Workflow code should treat it as recorded deterministic data, not as
/// a wall-clock reading.
pub opaque type Timestamp {
  Timestamp(milliseconds: Int)
}

/// Return the canonical millisecond representation of a deterministic timestamp.
pub fn timestamp_to_milliseconds(timestamp: Timestamp) -> Int {
  timestamp.milliseconds
}

/// Dispatch an activity through the single recorded side-effect boundary.
///
/// Plain Gleam workflow code is re-run on replay. The only recorded
/// side-effectful path exposed by this SDK is `run` (and later concurrency
/// combinators over activities); there is intentionally no generic
/// `side_effect(fn)` escape hatch. The activity input is encoded with the
/// activity's input `Codec`, the engine dispatches and records via AD, and the
/// returned payload is decoded with the output `Codec`.
pub fn run(activity_value: Activity(i, o)) -> Result(o, error.ActivityError) {
  let input_codec = activity.input_codec(activity_value)
  let output_codec = activity.output_codec(activity_value)
  let encoded_input = input_codec.encode(activity.input(activity_value))

  case
    ffi.dispatch_activity(
      activity.name(activity_value),
      encoded_input,
      activity_config(activity_value),
    )
  {
    Ok(correlation_id) -> {
      // The await is a yield point: pending workflow queries are serviced
      // by the query pump before the activity result resolves.
      case
        pump.run(fn() { pump.shield(ffi.await_activity_result(correlation_id)) })
      {
        Ok(payload) -> {
          case output_codec.decode(payload) {
            Ok(output) -> Ok(output)
            Error(decode_error) ->
              Error(error.ActivityDecodeFailed(decode_error))
          }
        }
        Error(raw_error) -> Error(activity_error(raw_error))
      }
    }
    Error(raw_error) -> Error(activity_error(raw_error))
  }
}

/// Return AD's recorded deterministic timestamp.
///
/// This is the only time source exposed to workflow code. Workflow authors must
/// not call wall-clock APIs such as Gleam/Erlang clocks from workflow logic, as
/// ambient time would desynchronise replay.
pub fn now() -> Result(Timestamp, error.EngineError) {
  case ffi.now() {
    Ok(raw_timestamp) -> parse_timestamp(raw_timestamp)
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

/// Draw a deterministic floating-point value from AD's seeded RNG.
///
/// The engine keys the RNG seed on WorkflowId + RunId so replay observes the
/// same sequence. Workflow authors must not call ambient entropy sources.
pub fn random() -> Result(Float, error.EngineError) {
  case ffi.random() {
    Ok(raw_random) -> parse_float(raw_random, "random")
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

/// Draw a deterministic integer in the engine-defined inclusive range.
///
/// Values come from AD's seeded RNG through the FFI boundary; no wall-clock or
/// ambient entropy binding is exposed by the SDK.
pub fn random_int(min: Int, max: Int) -> Result(Int, error.EngineError) {
  case min > max {
    True ->
      Error(error.EngineFailure(
        message: "Invalid deterministic random_int range: min is greater than max",
      ))
    False ->
      case ffi.random_int(int.to_string(min), int.to_string(max)) {
        Ok(raw_random) -> parse_int(raw_random, "random_int")
        Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
      }
  }
}

fn parse_timestamp(raw: String) -> Result(Timestamp, error.EngineError) {
  case int.parse(raw) {
    Ok(milliseconds) -> Ok(Timestamp(milliseconds: milliseconds))
    Error(_) ->
      Error(error.EngineFailure(
        message: "Invalid deterministic timestamp: " <> raw,
      ))
  }
}

fn parse_float(raw: String, label: String) -> Result(Float, error.EngineError) {
  case float.parse(raw) {
    Ok(value) -> Ok(value)
    Error(_) ->
      Error(error.EngineFailure(
        message: "Invalid deterministic " <> label <> ": " <> raw,
      ))
  }
}

fn parse_int(raw: String, label: String) -> Result(Int, error.EngineError) {
  case int.parse(raw) {
    Ok(value) -> Ok(value)
    Error(_) ->
      Error(error.EngineFailure(
        message: "Invalid deterministic " <> label <> ": " <> raw,
      ))
  }
}

fn activity_error(raw: String) -> error.ActivityError {
  case string.starts_with(raw, "retryable:") {
    True -> error.Retryable(message: string.drop_start(raw, 10), details: "")
    False ->
      case string.starts_with(raw, "terminal:") {
        True -> error.Terminal(message: string.drop_start(raw, 9), details: "")
        False ->
          case string.starts_with(raw, "timeout:") {
            True ->
              error.ActivityTimedOut(error.TimedOut(string.drop_start(raw, 8)))
            False ->
              case string.starts_with(raw, "cancelled:") {
                True ->
                  error.ActivityCancelled(
                    error.Cancelled(string.drop_start(raw, 10)),
                  )
                False ->
                  case string.starts_with(raw, "non_determinism:") {
                    True ->
                      error.ActivityNonDeterministic(
                        error.NonDeterminismViolation(string.drop_start(raw, 16)),
                      )
                    False -> error.ActivityEngineFailure(message: raw)
                  }
              }
          }
      }
  }
}

fn activity_config(activity_value: Activity(i, o)) -> String {
  json.object([
    #("retry", retry_config(activity.retry_policy(activity_value))),
    #(
      "timeout_ms",
      optional_duration(activity.timeout_duration(activity_value)),
    ),
    #(
      "heartbeat_ms",
      optional_duration(activity.heartbeat_interval(activity_value)),
    ),
  ])
  |> json.to_string
}

fn retry_config(policy) -> json.Json {
  case policy {
    None -> json.null()
    Some(activity.RetryPolicy(max_attempts: attempts, backoff: backoff)) ->
      json.object([
        #("max_attempts", json.int(attempts)),
        #("backoff", backoff_config(backoff)),
      ])
  }
}

fn backoff_config(backoff: activity.Backoff) -> json.Json {
  case backoff {
    activity.Exponential(initial: initial, multiplier: multiplier, max: max) ->
      json.object([
        #("kind", json.string("exponential")),
        #("initial_ms", duration_json(initial)),
        #("multiplier", json.float(multiplier)),
        #("max_ms", duration_json(max)),
      ])
    activity.Linear(initial: initial, increment: increment, max: max) ->
      json.object([
        #("kind", json.string("linear")),
        #("initial_ms", duration_json(initial)),
        #("increment_ms", duration_json(increment)),
        #("max_ms", duration_json(max)),
      ])
    activity.Fixed(delay: delay) ->
      json.object([
        #("kind", json.string("fixed")),
        #("delay_ms", duration_json(delay)),
      ])
  }
}

fn optional_duration(value) -> json.Json {
  case value {
    None -> json.null()
    Some(duration) -> duration_json(duration)
  }
}

fn duration_json(value: duration.Duration) -> json.Json {
  value
  |> duration.to_milliseconds
  |> json.int
}
