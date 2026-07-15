//// Typed workflow concurrency combinators over homogeneous activity lists.

import aion/activity.{type Activity}
import aion/codec
import aion/error
import aion/internal/activity_dispatch
import aion/internal/ffi
import aion/internal/pump
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{None}

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
        Error(raw_error) -> Error(activity_dispatch.parse_error(raw_error))
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
        Error(raw_error) -> Error(activity_dispatch.parse_error(raw_error))
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
/// each id is then awaited in input order. Remote members run retry and timeout
/// policies to their own final outcome. The current arity-4 in-VM wire applies
/// neither policy, so an in-VM member declaring retry or timeout is not
/// dispatched and its slot is an explicit `ActivityEngineFailure`. Policy-free
/// in-VM members remain supported. A terminal failure arrives as `Error(...)`
/// in that member's slot while its siblings keep their own results. Empty list
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
  activity_dispatch.all_settled(activities, workflow_default_task_queue)
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
      json.string(activity_dispatch.config(
        activity_value,
        workflow_default_task_queue,
      )),
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
