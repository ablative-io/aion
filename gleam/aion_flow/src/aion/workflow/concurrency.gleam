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
  all_with_default(activities, None)
}

/// `all`, supplying the workflow-level default task queue used for any member
/// that selects none. Precedence (member override > workflow default > the
/// named `"default"` queue) is resolved once at the engine schedule seam.
pub fn all_with_default(
  activities: List(Activity(i, o)),
  workflow_default_task_queue: option.Option(String),
) -> Result(List(o), error.ActivityError) {
  case activities {
    [] -> Ok([])
    [first, ..] -> {
      let output_codec = activity.output_codec(first)
      let specs = activity_specs(activities, workflow_default_task_queue)
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
  race_with_default(activities, None)
}

/// `race`, supplying the workflow-level default task queue used for any member
/// that selects none. Precedence (member override > workflow default > the
/// named `"default"` queue) is resolved once at the engine schedule seam.
pub fn race_with_default(
  activities: List(Activity(i, o)),
  workflow_default_task_queue: option.Option(String),
) -> Result(o, error.ActivityError) {
  case activities {
    [] ->
      Error(error.ActivityEngineFailure(
        message: "race requires at least one activity",
      ))
    [first, ..] -> {
      let output_codec = activity.output_codec(first)
      let specs = activity_specs(activities, workflow_default_task_queue)
      let id = collection_id("race", specs)

      case pump.run(fn() { pump.shield(ffi.collect_race(id, specs)) }) {
        Ok(payload) -> decode_one(payload, output_codec)
        Error(raw_error) -> Error(activity_error(raw_error))
      }
    }
  }
}

/// Spawn all activities concurrently and settle every member independently:
/// one `Result` slot per activity, in input order, with NO fail-fast and NO
/// sibling cancellation.
///
/// Each member dispatches through the same single-dispatch wire `run` uses
/// (tier-aware: an in-VM selection crosses the arity-4 thunk wire, everything
/// else the arity-3 remote wire), collecting correlation ids in input order;
/// each id is then awaited in input order. Every member's retry policy runs
/// to its own final outcome — a terminal failure arrives as `Error(...)` in
/// that member's slot while its siblings keep their own results. Empty list
/// settles to `[]`.
///
/// Every await is a query-pump yield point, exactly like `run`.
///
/// CAUTION for hand-authored workflows: do not wrap a settled fan-out in a
/// `with_timeout` scope. Scope expiry cancels only the operation being
/// awaited; members dispatched but not yet awaited when the scope expires
/// have no cancellation story on this wire.
pub fn all_settled(
  activities: List(Activity(i, o)),
) -> List(Result(o, error.ActivityError)) {
  all_settled_with_default(activities, None)
}

/// `all_settled`, supplying the workflow-level default task queue used for
/// any member that selects none. Precedence (member override > workflow
/// default > the named `"default"` queue) is resolved once at the engine
/// schedule seam.
pub fn all_settled_with_default(
  activities: List(Activity(i, o)),
  workflow_default_task_queue: option.Option(String),
) -> List(Result(o, error.ActivityError)) {
  let dispatched =
    list.map(activities, fn(activity_value) {
      settled_dispatch(activity_value, workflow_default_task_queue)
    })
  list.map(dispatched, settled_await)
}

/// Dynamically produce one activity per input element, then settle like
/// `all_settled`: one `Result` slot per item, item order, no fail-fast.
pub fn map_settled(
  items: List(a),
  to_activity: fn(a) -> Activity(i, o),
) -> List(Result(o, error.ActivityError)) {
  map_settled_with_default(items, to_activity, None)
}

/// `map_settled`, supplying the workflow-level default task queue used for
/// any produced activity that selects none. See `all_settled_with_default`
/// for the resolution precedence.
pub fn map_settled_with_default(
  items: List(a),
  to_activity: fn(a) -> Activity(i, o),
  workflow_default_task_queue: option.Option(String),
) -> List(Result(o, error.ActivityError)) {
  items
  |> list.map(to_activity)
  |> all_settled_with_default(workflow_default_task_queue)
}

/// One settled member mid-flight: its correlation id and output codec when
/// the dispatch was accepted, or the dispatch failure held for its slot.
type SettledDispatch(o) {
  SettledDispatch(correlation_id: String, output_codec: codec.Codec(o))
  SettledRefused(failure: error.ActivityError)
}

/// Dispatch one settled member on its selected execution tier — the exact
/// routing `aion/workflow/run` applies: `Some(InVm)` crosses the arity-4
/// in-VM wire carrying the runner thunk; absence or a remote tier keeps the
/// arity-3 remote wire (mirrors `ActivitySpec::selects_in_vm` engine-side).
fn settled_dispatch(
  activity_value: Activity(i, o),
  workflow_default_task_queue: option.Option(String),
) -> SettledDispatch(o) {
  let input_codec = activity.input_codec(activity_value)
  let output_codec = activity.output_codec(activity_value)
  let encoded_input = input_codec.encode(activity.input(activity_value))
  let config =
    activity_config(activity_value, workflow_default_task_queue)
  let dispatched = case activity.selected_tier(activity_value) {
    Some(activity.InVm) ->
      ffi.dispatch_activity_in_vm(
        activity.name(activity_value),
        encoded_input,
        config,
        settled_in_vm_thunk(activity_value),
      )
    // Deliberately exhaustive (no `_` arm), mirroring `run.gleam::dispatch`:
    // a future `Tier` variant must make an explicit routing decision here.
    Some(activity.RemotePython) | Some(activity.RemoteRust) | None ->
      ffi.dispatch_activity(
        activity.name(activity_value),
        encoded_input,
        config,
      )
  }
  case dispatched {
    Ok(correlation_id) ->
      SettledDispatch(correlation_id: correlation_id, output_codec: output_codec)
    Error(raw_error) -> SettledRefused(failure: activity_error(raw_error))
  }
}

/// Await one settled member's final outcome and decode it into its slot.
fn settled_await(
  dispatched: SettledDispatch(o),
) -> Result(o, error.ActivityError) {
  case dispatched {
    SettledRefused(failure) -> Error(failure)
    SettledDispatch(correlation_id, output_codec) ->
      case
        pump.run(fn() { pump.shield(ffi.await_activity_result(correlation_id)) })
      {
        Ok(payload) ->
          case output_codec.decode(payload) {
            Ok(output) -> Ok(output)
            Error(decode_error) ->
              Error(error.ActivityDecodeFailed(decode_error))
          }
        Error(raw_error) -> Error(activity_error(raw_error))
      }
  }
}

/// The zero-argument thunk an in-VM settled dispatch hands the engine —
/// byte-identical behavior to `run.gleam::in_vm_thunk`: run the runner,
/// encode the outcome, and encode errors to the exact prefixed reason
/// vocabulary `activity_error` parses back.
fn settled_in_vm_thunk(
  activity_value: Activity(i, o),
) -> fn() -> Result(String, String) {
  let runner = activity.runner(activity_value)
  let input = activity.input(activity_value)
  let output_codec = activity.output_codec(activity_value)
  fn() {
    case runner(input) {
      Ok(output) -> Ok(output_codec.encode(output))
      Error(runner_error) -> Error(encode_settled_error(runner_error))
    }
  }
}

/// Encode a runner's `ActivityError` to the prefixed reason vocabulary
/// `activity_error` parses — the same inverse `run.gleam` keeps for its
/// in-VM thunk, so kind fidelity survives the child-process boundary.
fn encode_settled_error(runner_error: error.ActivityError) -> String {
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

/// Dynamically produce one activity per input element, then collect like `all`.
///
/// The v1 concurrency surface intentionally covers homogeneous-output list
/// fan-out. Typed tuple variants such as `all2`/`all3` are deferred additions.
pub fn map(
  items: List(a),
  to_activity: fn(a) -> Activity(i, o),
) -> Result(List(o), error.ActivityError) {
  map_with_default(items, to_activity, None)
}

/// `map`, supplying the workflow-level default task queue used for any produced
/// activity that selects none. Precedence (activity override > workflow default
/// > the named `"default"` queue) is resolved once at the engine schedule seam.
pub fn map_with_default(
  items: List(a),
  to_activity: fn(a) -> Activity(i, o),
  workflow_default_task_queue: option.Option(String),
) -> Result(List(o), error.ActivityError) {
  items
  |> list.map(to_activity)
  |> all_with_default(workflow_default_task_queue)
}

fn activity_specs(
  activities: List(Activity(i, o)),
  workflow_default_task_queue: option.Option(String),
) -> List(String) {
  activities
  |> list.index_map(fn(activity_value, index) {
    activity_spec(activity_value, index, workflow_default_task_queue)
  })
}

fn activity_spec(
  activity_value: Activity(i, o),
  index: Int,
  workflow_default_task_queue: option.Option(String),
) -> String {
  let input_codec = activity.input_codec(activity_value)
  let encoded_input = input_codec.encode(activity.input(activity_value))

  json.object([
    #("correlation", json.string("activity-" <> int.to_string(index))),
    #("name", json.string(activity.name(activity_value))),
    #("input", json.string(encoded_input)),
    #(
      "config",
      json.string(activity_config(activity_value, workflow_default_task_queue)),
    ),
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

fn activity_config(
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
    // Both task-queue selections cross unresolved: the activity override and the
    // workflow-level default. The engine schedule seam applies the precedence
    // (override > default > the named "default" queue) exactly once.
    #(
      "task_queue",
      optional_string(activity.selected_task_queue(activity_value)),
    ),
    #("workflow_task_queue", optional_string(workflow_default_task_queue)),
    // Optional per-activity node affinity (NODE-4). `null` = no pin (dispatch to
    // any worker in the pool); there is no workflow-level node default to carry.
    #("node", optional_string(activity.selected_node(activity_value))),
  ])
  |> json.to_string
}

fn optional_string(value: option.Option(String)) -> json.Json {
  case value {
    None -> json.null()
    Some(text) -> json.string(text)
  }
}

/// Encode the activity's display labels as a JSON object of string values.
/// The engine carries these to the worker for log and dashboard display; it
/// never interprets them.
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
