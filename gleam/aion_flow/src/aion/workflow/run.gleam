//// workflow.run (recorded activity dispatch) + now + random (determinism bindings)

import aion/activity.{type Activity}
import aion/error
import aion/internal/activity_dispatch
import aion/internal/ffi
import gleam/float
import gleam/int
import gleam/option.{None}
import gleam/result

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
  run_with_default(activity_value, None)
}

/// Dispatch an activity, supplying the workflow-level default task queue used
/// when the activity itself selects none.
///
/// Resolution precedence (activity override > workflow default > the named
/// `"default"` queue) is applied once at the engine schedule seam; this SDK
/// only carries both unresolved selections across the boundary. A `None`
/// workflow default behaves exactly as `run`.
pub fn run_with_default(
  activity_value: Activity(i, o),
  workflow_default_task_queue: option.Option(String),
) -> Result(o, error.ActivityError) {
  use dispatched <- result.try(activity_dispatch.dispatch(
    activity_value,
    workflow_default_task_queue,
  ))
  activity_dispatch.await(dispatched)
}

/// Return AD's recorded deterministic timestamp.
///
/// Return the current workflow execution's unique identifier.
///
/// The engine assigns a v4 UUID at workflow start and records it in the
/// `WorkflowStarted` event. This NIF reads the identifier from the
/// process's registered workflow handle, so it is stable across replay.
pub fn id() -> Result(String, error.EngineError) {
  case ffi.workflow_id() {
    Ok(workflow_id) -> Ok(workflow_id)
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

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
