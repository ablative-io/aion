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

import aion/workflow
import gleam/dynamic.{type Dynamic}
import stacked_dev/activities
import stacked_dev/codecs_flow
import stacked_dev/errors
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
/// The runtime delivers the start input as a raw JSON string;
/// `workflow.entrypoint` decodes it with the definition's input codec, drives
/// `execute`, and encodes the outcome back to JSON text. The engine records
/// those exact payloads as the child terminal, and the awaiting parent decodes
/// them with the same codecs `stacked_dev/codecs_flow` exports. An undecodable
/// input records the SDK's documented `{"aion_error":"input_decode",...}`
/// envelope as the failure payload.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  workflow.entrypoint(definition(), raw_input)
}

/// Dispatch the `full_checks` activity and return its recorded verdict.
pub fn execute(input: GateInput) -> Result(GateResult, GateError) {
  case workflow.run(activities.full_checks(input)) {
    Ok(result) -> Ok(result)
    Error(activity_error) ->
      Error(GateStageFailed(
        stage: "full_checks",
        message: errors.activity_message(activity_error),
      ))
  }
}
