//// Self-spawning recursion probe for the `nested_chain` fixture: a workflow
//// that spawns a child of ITS OWN type with `depth + 1` until `max_depth`,
//// where it runs the `leaf_work` activity instead. Proves child-workflow
//// recursion has no artificial depth restriction and that a recursive chain
//// recovers correctly.

import aion/activity
import aion/child
import aion/codec
import aion/error
import aion/workflow
import chain_types.{LeafInput}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/json

/// A deployed workflow type is its entry module name.
pub const workflow_type = "level_self"

pub type SelfInput {
  SelfInput(depth: Int, max_depth: Int, job_id: String, note: String)
}

pub fn self_input_codec() -> codec.Codec(SelfInput) {
  codec.json_codec(self_input_to_json, self_input_decoder())
}

fn self_input_to_json(input: SelfInput) -> json.Json {
  json.object([
    #("depth", json.int(input.depth)),
    #("max_depth", json.int(input.max_depth)),
    #("job_id", json.string(input.job_id)),
    #("note", json.string(input.note)),
  ])
}

fn self_input_decoder() -> decode.Decoder(SelfInput) {
  use depth <- decode.field("depth", decode.int)
  use max_depth <- decode.field("max_depth", decode.int)
  use job_id <- decode.field("job_id", decode.string)
  use note <- decode.field("note", decode.string)
  decode.success(SelfInput(
    depth: depth,
    max_depth: max_depth,
    job_id: job_id,
    note: note,
  ))
}

/// Typed workflow logic, recursively its own `child.spawn` type anchor.
pub fn process(input: SelfInput) -> Result(String, String) {
  case input.depth >= input.max_depth {
    True -> leaf(input)
    False -> recurse(input)
  }
}

fn recurse(input: SelfInput) -> Result(String, String) {
  case
    child.spawn_and_wait(
      workflow_type,
      process,
      SelfInput(..input, depth: input.depth + 1),
      self_input_codec(),
      chain_types.text_codec(),
      chain_types.raw_error_codec(),
    )
  {
    Ok(output) -> Ok("d" <> int.to_string(input.depth) <> "<" <> output)
    Error(child_error) ->
      Error("level_self: " <> chain_types.describe_child_error(child_error))
  }
}

fn leaf(input: SelfInput) -> Result(String, String) {
  case workflow.run(leaf_activity(input)) {
    Ok(receipt) ->
      Ok("d" <> int.to_string(input.depth) <> ":" <> receipt.receipt)
    Error(activity_error) ->
      Error(
        "level_self: leaf_work failed: "
        <> chain_types.describe_activity_error(activity_error),
      )
  }
}

fn leaf_activity(
  input: SelfInput,
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
      case self_input_codec().decode(raw_json) {
        Ok(input) ->
          case process(input) {
            Ok(output) -> Ok(chain_types.text_codec().encode(output))
            Error(message) -> Error(message)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error("level_self: failed to decode input: " <> reason)
      }
    Error(_) -> Error("level_self: input payload was not a string")
  }
}
