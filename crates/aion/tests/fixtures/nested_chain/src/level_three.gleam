//// Deepest level of the `nested_chain` fixture: registers a query handler,
//// runs the `leaf_work` activity (the Rust test dispatcher optionally holds
//// it open so tests can observe the parked chain), and — when the input's
//// `gate` flag is set — parks on a `leaf_release` signal after the activity
//// terminal records, so recovery tests can kill the engine at a byte-stable
//// yield point.

import aion/activity
import aion/codec
import aion/error
import aion/query
import aion/signal
import aion/workflow
import chain_types.{type ChainInput, LeafInput}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

/// A deployed workflow type is its entry module name.
pub const workflow_type = "level_three"

pub const status_query_name = "level_three_status"

pub const release_signal_name = "leaf_release"

/// Typed workflow logic, also the parent's `child.spawn` type anchor.
pub fn process(input: ChainInput) -> Result(String, String) {
  case
    query.handler(status_query_name, chain_types.text_codec(), fn() {
      "processing:" <> input.job_id
    })
  {
    Ok(Nil) -> run_leaf(input)
    Error(_) -> Error("level_three: query registration failed")
  }
}

fn run_leaf(input: ChainInput) -> Result(String, String) {
  case workflow.run(leaf_activity(input)) {
    Ok(receipt) -> gate_completion(input, receipt.receipt)
    Error(activity_error) ->
      Error(
        "level_three: leaf_work failed: "
        <> chain_types.describe_activity_error(activity_error),
      )
  }
}

fn gate_completion(input: ChainInput, receipt: String) -> Result(String, String) {
  case input.gate {
    False -> Ok("l3:" <> receipt)
    True ->
      case
        workflow.receive(signal.new(release_signal_name, chain_types.text_codec()))
      {
        Ok(token) -> Ok("l3:" <> receipt <> ":" <> token)
        Error(_) -> Error("level_three: release receive failed")
      }
  }
}

fn leaf_activity(
  input: ChainInput,
) -> activity.Activity(chain_types.LeafInput, chain_types.LeafReceipt) {
  activity.new(
    "leaf_work",
    LeafInput(job_id: input.job_id, note: input.note),
    chain_types.leaf_input_codec(),
    chain_types.leaf_receipt_codec(),
    local_leaf_work,
  )
}

/// Local stub used by the pure-Gleam test double; the engine e2e tests
/// install a Rust `ActivityDispatcher` mirroring this contract.
fn local_leaf_work(
  input: chain_types.LeafInput,
) -> Result(chain_types.LeafReceipt, error.ActivityError) {
  Ok(chain_types.LeafReceipt(receipt: "done:" <> input.job_id, audit: input.note))
}

/// Engine entry point: the runtime delivers the start input as a raw JSON
/// string; the recorded result payload is the JSON-encoded output string.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case chain_types.chain_input_codec().decode(raw_json) {
        Ok(input) ->
          case process(input) {
            Ok(output) -> Ok(chain_types.text_codec().encode(output))
            Error(message) -> Error(message)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error("level_three: failed to decode input: " <> reason)
      }
    Error(_) -> Error("level_three: input payload was not a string")
  }
}
