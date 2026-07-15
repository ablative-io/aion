//// Shared activity dispatch boundary for `run` and settled fan-out.

import aion/activity.{type Activity}
import aion/codec
import aion/duration
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/json
import gleam/list
import gleam/option.{None, Some}
import gleam/string

pub opaque type Dispatched(o) {
  Dispatched(correlation_id: String, output_codec: codec.Codec(o))
}

/// Encode, configure, and dispatch one activity on its selected tier.
pub fn dispatch(
  activity_value: Activity(i, o),
  workflow_default_task_queue: option.Option(String),
) -> Result(Dispatched(o), error.ActivityError) {
  let input_codec = activity.input_codec(activity_value)
  let encoded_input = input_codec.encode(activity.input(activity_value))
  let config = config(activity_value, workflow_default_task_queue)
  let dispatched = case activity.selected_tier(activity_value) {
    Some(activity.InVm) ->
      ffi.dispatch_activity_in_vm(
        activity.name(activity_value),
        encoded_input,
        config,
        in_vm_thunk(activity_value),
      )
    Some(activity.RemotePython) | Some(activity.RemoteRust) | None ->
      ffi.dispatch_activity(
        activity.name(activity_value),
        encoded_input,
        config,
      )
  }
  case dispatched {
    Ok(correlation_id) ->
      Ok(Dispatched(correlation_id, activity.output_codec(activity_value)))
    Error(raw_error) -> Error(parse_error(raw_error))
  }
}

/// Await and decode one previously dispatched activity.
pub fn await(dispatched: Dispatched(o)) -> Result(o, error.ActivityError) {
  let Dispatched(correlation_id, output_codec) = dispatched
  case
    pump.run(fn() { pump.shield(ffi.await_activity_result(correlation_id)) })
  {
    Ok(payload) ->
      case output_codec.decode(payload) {
        Ok(output) -> Ok(output)
        Error(decode_error) -> Error(error.ActivityDecodeFailed(decode_error))
      }
    Error(raw_error) -> Error(parse_error(raw_error))
  }
}

/// Whether settled fan-out can honor an in-VM member's policies.
///
/// The current arity-4 engine wire runs one attempt and owns no timeout. A
/// settled combinator must refuse policy-bearing in-VM members rather than
/// silently discard their retry or timeout contract.
pub fn settled_policy_supported(activity_value: Activity(i, o)) -> Bool {
  case activity.selected_tier(activity_value) {
    Some(activity.InVm) ->
      case
        activity.retry_policy(activity_value),
        activity.timeout_duration(activity_value)
      {
        None, None -> True
        _, _ -> False
      }
    Some(activity.RemotePython) | Some(activity.RemoteRust) | None -> True
  }
}

pub fn settled_policy_error() -> error.ActivityError {
  error.ActivityEngineFailure(
    message: "settled in-VM activities do not support retry or timeout policies on the current arity-4 wire",
  )
}

pub fn parse_error(raw: String) -> error.ActivityError {
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

fn in_vm_thunk(
  activity_value: Activity(i, o),
) -> fn() -> Result(String, String) {
  let runner = activity.runner(activity_value)
  let input = activity.input(activity_value)
  let output_codec = activity.output_codec(activity_value)
  fn() {
    case runner(input) {
      Ok(output) -> Ok(output_codec.encode(output))
      Error(runner_error) -> Error(encode_error(runner_error))
    }
  }
}

fn encode_error(runner_error: error.ActivityError) -> String {
  case runner_error {
    error.Retryable(message: message, details: _) -> "retryable:" <> message
    error.Terminal(message: message, details: _) -> "terminal:" <> message
    error.ActivityTimedOut(error.TimedOut(message: message)) ->
      "timeout:" <> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) ->
      "cancelled:" <> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> "non_determinism:" <> message
    error.ActivityDecodeFailed(codec.DecodeError(reason: reason, path: path)) ->
      "terminal:activity output decode failed at "
      <> string.join(path, ".")
      <> ": "
      <> reason
    error.ActivityEngineFailure(message: message) -> message
  }
}

pub fn config(
  activity_value: Activity(i, o),
  workflow_default_task_queue: option.Option(String),
) -> String {
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
    #("labels", labels_config(activity.labels(activity_value))),
    #(
      "task_queue",
      optional_string(activity.selected_task_queue(activity_value)),
    ),
    #("workflow_task_queue", optional_string(workflow_default_task_queue)),
    #("node", optional_string(activity.selected_node(activity_value))),
    #(
      "tier",
      optional_string(option.map(
        activity.selected_tier(activity_value),
        activity.tier_to_string,
      )),
    ),
  ])
  |> json.to_string
}

fn optional_string(value: option.Option(String)) -> json.Json {
  case value {
    None -> json.null()
    Some(text) -> json.string(text)
  }
}

fn labels_config(labels: List(#(String, String))) -> json.Json {
  json.object(list.map(labels, fn(pair) { #(pair.0, json.string(pair.1)) }))
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
