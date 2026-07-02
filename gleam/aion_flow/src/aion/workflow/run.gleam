//// workflow.run (recorded activity dispatch) + now + random (determinism bindings)

import aion/activity.{type Activity}
import aion/codec
import aion/duration
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/float
import gleam/int
import gleam/json
import gleam/list
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
  let input_codec = activity.input_codec(activity_value)
  let output_codec = activity.output_codec(activity_value)
  let encoded_input = input_codec.encode(activity.input(activity_value))

  case
    dispatch(
      activity_value,
      encoded_input,
      activity_config(activity_value, workflow_default_task_queue),
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

/// Route one dispatch on the activity's selected execution tier.
///
/// `Some(InVm)` crosses the arity-4 in-VM wire, carrying a thunk that composes
/// the captured input, the runner, and the output codec — the engine records
/// the same `ActivityScheduled`/`ActivityStarted` pair as a remote dispatch
/// and runs the thunk in a linked child process. Every other selection
/// (absence, or a remote tier) keeps today's arity-3 remote wire untouched.
/// Both paths return the same correlation id and converge on the same
/// `await_activity_result`, so replay is byte-identical across tiers.
fn dispatch(
  activity_value: Activity(i, o),
  encoded_input: String,
  config: String,
) -> Result(String, String) {
  case activity.selected_tier(activity_value) {
    Some(activity.InVm) ->
      ffi.dispatch_activity_in_vm(
        activity.name(activity_value),
        encoded_input,
        config,
        in_vm_thunk(activity_value),
      )
    // Deliberately exhaustive (no `_` arm): a future `Tier` variant must make
    // an explicit routing decision here instead of silently defaulting to the
    // remote wire.
    Some(activity.RemotePython) | Some(activity.RemoteRust) | None ->
      ffi.dispatch_activity(
        activity.name(activity_value),
        encoded_input,
        config,
      )
  }
}

/// Build the zero-argument thunk an in-VM dispatch hands the engine.
///
/// The thunk closes over the typed input, the runner, and the output codec, so
/// the child process needs no input decode: it runs the runner and encodes the
/// outcome. Errors are encoded to the EXACT prefixed reason strings the
/// `activity_error` parser below consumes, preserving `ActivityError` kind
/// fidelity across the process boundary with zero new conventions.
fn in_vm_thunk(
  activity_value: Activity(i, o),
) -> fn() -> Result(String, String) {
  let runner = activity.runner(activity_value)
  let input = activity.input(activity_value)
  let output_codec = activity.output_codec(activity_value)
  fn() {
    case runner(input) {
      Ok(output) -> Ok(output_codec.encode(output))
      Error(runner_error) -> Error(encode_activity_error(runner_error))
    }
  }
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

/// Encode a runner's `ActivityError` to the prefixed reason vocabulary that
/// `activity_error` below parses — the exact inverse, so kind fidelity
/// (`retryable:` vs `terminal:` and friends) survives the in-VM child-process
/// boundary. Detail payloads are dropped, matching what the parser
/// reconstructs (`details: ""`) for remote failures on the same wire. An
/// `ActivityEngineFailure` crosses unprefixed: the parser's fallthrough arm
/// maps any unprefixed reason back to `ActivityEngineFailure`.
fn encode_activity_error(runner_error: error.ActivityError) -> String {
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
    // Optional execution-tier selection (canonical `tier_to_string` values).
    // `null` = no selection = the remote wire. The engine's remote dispatch
    // rejects `"in_vm"` arriving on the arity-3 wire as a defense: an in-VM
    // selection must cross the arity-4 wire that carries the thunk.
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
