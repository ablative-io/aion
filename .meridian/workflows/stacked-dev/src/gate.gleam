//// The authoritative gate child workflow.
////
//// One recorded `full_checks` activity: `cargo fmt --check`, workspace-wide
//// `cargo clippy -- -D warnings`, and `cargo test`. The fast scoped loop in
//// `brief_dev` is the inner iteration aid; this gate is the trustworthy
//// outer judgment, run as its own child workflow so it composes, versions,
//// and tests in isolation — and stays independently dispatchable for
//// partial runs (open question Q6).
////
//// A failing gate is recorded data (`GateFail(report)`), not a workflow
//// error: the parent decides what a failed gate means for the run. The
//// typed `GateError` is reserved for checks that could not execute at all.

import aion/codec
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import stacked_dev/codecs_flow
import stacked_dev/errors
import stacked_dev/meridian_dispatch
import stacked_dev/types.{
  type GateError, type GateInput, type GateResult, GateStageFailed,
}

/// The child workflow type the parent passes to `workflow.spawn_and_wait`.
/// A deployed workflow type is its entry module name, so this is exactly
/// this module's name.
pub const workflow_type = "gate"

/// Typed definition binding the gate's codecs to its execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  GateInput,
  GateResult,
  GateError,
) {
  workflow.define(
    "gate",
    codecs_flow.gate_input_codec(),
    codecs_flow.gate_result_codec(),
    codecs_flow.gate_error_codec(),
    execute,
  )
}

/// Engine entry point for one gate execution.
///
/// The runtime delivers the start input as a raw JSON string. Success and
/// failure are both encoded back to JSON text here: the engine records these
/// exact payloads as the child terminal, and the awaiting parent decodes
/// them with the same codecs `stacked_dev/codecs_flow` exports.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs_flow.gate_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) -> Ok(codecs_flow.gate_result_codec().encode(output))
            Error(gate_error) ->
              Error(codecs_flow.gate_error_codec().encode(gate_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            codecs_flow.gate_error_codec().encode(GateStageFailed(
              stage: "decode_input",
              message: "failed to decode gate input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        codecs_flow.gate_error_codec().encode(GateStageFailed(
          stage: "decode_input",
          message: "gate input payload was not a string",
        )),
      )
  }
}

/// Dispatch fmt + clippy + test via Meridian's check activity workers.
pub fn execute(input: GateInput) -> Result(GateResult, GateError) {
  case meridian_dispatch.run_full_checks(input.workspace) {
    Ok(result) -> Ok(result)
    Error(activity_error) ->
      Error(GateStageFailed(
        stage: "full_checks",
        message: errors.activity_message(activity_error),
      ))
  }
}
