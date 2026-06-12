//// Typed workflow concurrency combinators over homogeneous activity lists.

import aion/activity.{type Activity}
import aion/codec
import aion/duration
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{None, Some}
import gleam/string

/// Spawn all activities concurrently and collect their typed outputs in input
/// order.
///
/// Activity inputs are encoded with each activity's input `Codec`; returned
/// payloads are decoded one-by-one with the homogeneous output `Codec`. AT owns
/// selective receive, fail-fast behaviour, correlation, and cancellation of
/// remaining activities when any activity fails.
///
/// The collect is a yield point: pending workflow queries are serviced by the
/// query pump before the fan-out settles, exactly as activity awaits, signal
/// receives, timers, and child awaits do.
pub fn all(
  activities: List(Activity(i, o)),
) -> Result(List(o), error.ActivityError) {
  case activities {
    [] -> Ok([])
    [first, ..] -> {
      let output_codec = activity.output_codec(first)
      let specs = activity_specs(activities)
      let id = collection_id("all", specs)

      case pump.run(fn() { pump.shield(ffi.collect_all(id, specs)) }) {
        Ok(payloads) -> decode_many(payloads, output_codec)
        Error(raw_error) -> Error(activity_error(raw_error))
      }
    }
  }
}

/// Race activities and return the first settled typed result.
///
/// This is FIRST SETTLE semantics, not first-success-wins: the first activity to
/// finish wins whether it completes successfully or returns an `ActivityError`.
/// AT records that winner and cancels the losers.
///
/// Like `all`, the race is a query-pump yield point: pending workflow queries
/// are serviced while the race is parked.
pub fn race(
  activities: List(Activity(i, o)),
) -> Result(o, error.ActivityError) {
  case activities {
    [] ->
      Error(error.ActivityEngineFailure(
        message: "race requires at least one activity",
      ))
    [first, ..] -> {
      let output_codec = activity.output_codec(first)
      let specs = activity_specs(activities)
      let id = collection_id("race", specs)

      case pump.run(fn() { pump.shield(ffi.collect_race(id, specs)) }) {
        Ok(payload) -> decode_one(payload, output_codec)
        Error(raw_error) -> Error(activity_error(raw_error))
      }
    }
  }
}

/// Dynamically produce one activity per input element, then collect like `all`.
///
/// The v1 concurrency surface intentionally covers homogeneous-output list
/// fan-out. Typed tuple variants such as `all2`/`all3` are deferred additions.
pub fn map(
  items: List(a),
  to_activity: fn(a) -> Activity(i, o),
) -> Result(List(o), error.ActivityError) {
  items
  |> list.map(to_activity)
  |> all
}

fn activity_specs(activities: List(Activity(i, o))) -> List(String) {
  activities
  |> list.index_map(fn(activity_value, index) {
    activity_spec(activity_value, index)
  })
}

fn activity_spec(activity_value: Activity(i, o), index: Int) -> String {
  let input_codec = activity.input_codec(activity_value)
  let encoded_input = input_codec.encode(activity.input(activity_value))

  json.object([
    #("correlation", json.string("activity-" <> int.to_string(index))),
    #("name", json.string(activity.name(activity_value))),
    #("input", json.string(encoded_input)),
    #("config", json.string(activity_config(activity_value))),
  ])
  |> json.to_string
}

fn decode_many(
  payloads: String,
  output_codec: codec.Codec(o),
) -> Result(List(o), error.ActivityError) {
  case json.parse(payloads, decode.list(decode.string)) {
    Ok(encoded_payloads) ->
      list.try_map(encoded_payloads, fn(payload) {
        decode_one(payload, output_codec)
      })
    Error(_) ->
      Error(error.ActivityEngineFailure(
        message: "Invalid collect_all result envelope: " <> payloads,
      ))
  }
}

fn decode_one(
  payload: String,
  output_codec: codec.Codec(o),
) -> Result(o, error.ActivityError) {
  case output_codec.decode(payload) {
    Ok(output) -> Ok(output)
    Error(decode_error) -> Error(error.ActivityDecodeFailed(decode_error))
  }
}

fn collection_id(prefix: String, specs: List(String)) -> String {
  prefix <> ":" <> int.to_string(list.length(specs))
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
