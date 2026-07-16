//// Typed child-workflow handles and await wrappers.

import aion/codec.{type Codec}
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/json
import gleam/string

/// A typed handle for a linked child-workflow execution.
///
/// `output` and `workflow_error` are the child workflow's statically-known
/// result and error types. The handle carries the engine correlation id plus the
/// codecs required to decode the recorded child completion or failure payload
/// returned by AT/AD.
pub opaque type ChildHandle(output, workflow_error) {
  ChildHandle(
    child_id: String,
    output_codec: Codec(output),
    error_codec: Codec(workflow_error),
  )
}

/// Start a linked child workflow and return its typed handle.
///
/// The `workflow_fn` is accepted as a type anchor for the child workflow's
/// `fn(input) -> Result(output, workflow_error)` contract. The SDK does not call
/// it here; lifecycle, linking, recording, and replay/no-respawn behavior are
/// owned by AT/AD behind the FFI boundary.
pub fn spawn(
  name: String,
  workflow_fn: fn(input) -> Result(output, workflow_error),
  input: input,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
) -> Result(ChildHandle(output, workflow_error), error.EngineError) {
  let _workflow_fn = workflow_fn
  let encoded_input = input_codec.encode(input)

  case ffi.spawn_child(name, encoded_input, spawn_config()) {
    Ok(raw_child_id) ->
      Ok(ChildHandle(
        child_id: raw_child_id,
        output_codec: output_codec,
        error_codec: error_codec,
      ))
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

/// Await a child workflow's recorded completion or failure.
///
/// AT/AD own blocking, replay resolution, and event recording. This wrapper
/// decodes the raw recorded envelope with the codecs carried on the handle and
/// returns decode/engine failures as typed data.
///
/// The await is a yield point: pending workflow queries are serviced by the
/// query pump before the child terminal resolves, exactly as activity awaits,
/// signal receives, and timers do. Without the pump, a query arriving while
/// the workflow is parked here would surface its sentinel as a bogus child
/// failure and leave the engine refusing every later await in the run.
pub fn await(
  handle: ChildHandle(output, workflow_error),
) -> Result(output, error.ChildError(workflow_error)) {
  // The engine reserves `{error, _}` from `await_child` for engine faults
  // (`await_child:`-prefixed messages) and the `with_timeout` scope-expiry
  // sentinel that the enclosing scope consumes. Child success arrives as its
  // exact durable payload bytes; child failure — including engine-side
  // cancellation/timeout terminals — arrives as `{ok, "error:"}` data.
  //
  // The child id is precomputed so the pump thunk's body is exactly one
  // shielded FFI call on a captured value — the re-execution-safety contract
  // for suspending awaits (see `aion/internal/pump`).
  let awaited_child_id = child_id(handle)
  case pump.run(fn() { pump.shield(ffi.await_child(awaited_child_id)) }) {
    Ok(raw_result) -> decode_child_result(raw_result, handle)
    Error(raw_error) -> Error(error.ChildEngineFailure(message: raw_error))
  }
}

/// Start a linked child workflow and await its recorded result.
///
/// This is the spawn-then-await convenience kept in the child logic module so
/// `aion/workflow` can remain a forwarding authoring surface.
pub fn spawn_and_wait(
  name: String,
  workflow_fn: fn(input) -> Result(output, workflow_error),
  input: input,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
) -> Result(output, error.ChildError(workflow_error)) {
  case spawn(name, workflow_fn, input, input_codec, output_codec, error_codec) {
    Ok(handle) -> await(handle)
    Error(error.EngineFailure(message: message)) ->
      Error(error.ChildEngineFailure(message: message))
  }
}

/// Return the engine child/correlation id carried by this handle.
pub fn child_id(handle: ChildHandle(output, workflow_error)) -> String {
  handle.child_id
}

/// Return the output codec carried by this child handle.
pub fn output_codec(
  handle: ChildHandle(output, workflow_error),
) -> Codec(output) {
  handle.output_codec
}

/// Return the workflow-error codec carried by this child handle.
pub fn error_codec(
  handle: ChildHandle(output, workflow_error),
) -> Codec(workflow_error) {
  handle.error_codec
}

fn decode_child_result(
  raw_result: String,
  handle: ChildHandle(output, workflow_error),
) -> Result(output, error.ChildError(workflow_error)) {
  case string.starts_with(raw_result, "error:") {
    True -> decode_error_payload(string.drop_start(raw_result, 6), handle)
    False -> decode_output(raw_result, handle)
  }
}

fn decode_output(
  payload: String,
  handle: ChildHandle(output, workflow_error),
) -> Result(output, error.ChildError(workflow_error)) {
  let codec = output_codec(handle)
  case codec.decode(payload) {
    Ok(output) -> Ok(output)
    Error(decode_error) -> Error(error.ChildOutputDecodeFailed(decode_error))
  }
}

fn decode_error_payload(
  payload: String,
  handle: ChildHandle(output, workflow_error),
) -> Result(output, error.ChildError(workflow_error)) {
  let codec = error_codec(handle)
  case codec.decode(payload) {
    Ok(workflow_error) -> Error(error.ChildWorkflowFailed(workflow_error))
    Error(decode_error) -> Error(error.ChildErrorDecodeFailed(decode_error))
  }
}

fn spawn_config() -> String {
  json.object([]) |> json.to_string
}
