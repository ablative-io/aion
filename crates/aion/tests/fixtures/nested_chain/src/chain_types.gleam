//// Shared types and codecs for the `nested_chain` engine e2e fixture: a
//// three-level workflow chain (`level_one` -> `level_two` -> `level_three`)
//// plus a self-spawning recursion probe (`level_self`), driven by
//// `crates/aion/tests/nested_workflows_e2e.rs`.

import aion/codec
import aion/error
import gleam/dynamic/decode
import gleam/json

/// Input passed unchanged down the chain. `note` is deliberately large
/// (>64 bytes in the tests) so every spawn input and activity payload keeps
/// the large refc-binary path exercised end-to-end. When `gate` is set,
/// `level_three` parks on a `leaf_release` signal after its activity
/// terminal records, giving the recovery tests a byte-stable park point.
pub type ChainInput {
  ChainInput(job_id: String, note: String, gate: Bool)
}

pub fn chain_input_codec() -> codec.Codec(ChainInput) {
  codec.json_codec(chain_input_to_json, chain_input_decoder())
}

fn chain_input_to_json(input: ChainInput) -> json.Json {
  json.object([
    #("job_id", json.string(input.job_id)),
    #("note", json.string(input.note)),
    #("gate", json.bool(input.gate)),
  ])
}

fn chain_input_decoder() -> decode.Decoder(ChainInput) {
  use job_id <- decode.field("job_id", decode.string)
  use note <- decode.field("note", decode.string)
  use gate <- decode.field("gate", decode.bool)
  decode.success(ChainInput(job_id: job_id, note: note, gate: gate))
}

/// Result, query-reply, and signal payloads are all JSON strings.
pub fn text_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

/// Verbatim passthrough codec for child-error payloads.
///
/// The engine maps a cancelled or timed-out child to a parent-side
/// `ChildWorkflowFailed` whose message is the non-JSON marker
/// `cancelled:<reason>` / `timed_out:<timeout>`, so the parents in this
/// fixture decode error payloads verbatim instead of through JSON — the
/// cancellation-semantics test pins those exact bytes.
pub fn raw_error_codec() -> codec.Codec(String) {
  codec.Codec(encode: fn(raw) { raw }, decode: fn(raw) { Ok(raw) })
}

/// Input for the `leaf_work` activity run at the deepest level.
pub type LeafInput {
  LeafInput(job_id: String, note: String)
}

/// Output of the `leaf_work` activity: a receipt plus an audit echo large
/// enough to keep the >64-byte result path exercised.
pub type LeafReceipt {
  LeafReceipt(receipt: String, audit: String)
}

pub fn leaf_input_codec() -> codec.Codec(LeafInput) {
  codec.json_codec(leaf_input_to_json, leaf_input_decoder())
}

fn leaf_input_to_json(input: LeafInput) -> json.Json {
  json.object([
    #("job_id", json.string(input.job_id)),
    #("note", json.string(input.note)),
  ])
}

fn leaf_input_decoder() -> decode.Decoder(LeafInput) {
  use job_id <- decode.field("job_id", decode.string)
  use note <- decode.field("note", decode.string)
  decode.success(LeafInput(job_id: job_id, note: note))
}

pub fn leaf_receipt_codec() -> codec.Codec(LeafReceipt) {
  codec.json_codec(leaf_receipt_to_json, leaf_receipt_decoder())
}

fn leaf_receipt_to_json(receipt: LeafReceipt) -> json.Json {
  json.object([
    #("receipt", json.string(receipt.receipt)),
    #("audit", json.string(receipt.audit)),
  ])
}

fn leaf_receipt_decoder() -> decode.Decoder(LeafReceipt) {
  use receipt <- decode.field("receipt", decode.string)
  use audit <- decode.field("audit", decode.string)
  decode.success(LeafReceipt(receipt: receipt, audit: audit))
}

/// Render a child-await failure as a stable, test-pinnable string.
pub fn describe_child_error(child_error: error.ChildError(String)) -> String {
  case child_error {
    error.ChildWorkflowFailed(raw) -> "child-failed:" <> raw
    error.ChildOutputDecodeFailed(codec.DecodeError(reason: reason, path: _)) ->
      "child-output-decode-failed:" <> reason
    error.ChildErrorDecodeFailed(codec.DecodeError(reason: reason, path: _)) ->
      "child-error-decode-failed:" <> reason
    error.ChildEngineFailure(message: message) ->
      "child-engine-failure:" <> message
  }
}

/// Render an activity failure as a stable, test-pinnable string.
pub fn describe_activity_error(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}
