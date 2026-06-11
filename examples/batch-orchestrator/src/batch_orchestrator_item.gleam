//// Single-item child workflow for the batch orchestrator example.
////
//// The engine resolves a spawned child's workflow type against its loaded
//// packages by entry module name — exactly the way `start` resolves a
//// top-level workflow type. This module is therefore its own `[[workflow]]`
//// entry in `workflow.toml`: the parent spawns children of the type named by
//// `workflow_type` below, and the child archive must be loaded into the
//// engine alongside the parent archive. Loading only the parent archive
//// leaves every spawn failing with an unknown child workflow type.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/string

/// The child workflow type the parent passes to `child.spawn`. A deployed
/// workflow type is its entry module name, so this is exactly this module's
/// name.
pub const workflow_type = "batch_orchestrator_item"

pub type WorkItem {
  WorkItem(id: String, payload: String)
}

pub type ItemResult {
  ItemResult(item_id: String, processed_payload: String, detail: String)
}

/// Child-side failure surfaced to the awaiting parent.
pub type ItemError {
  ItemFailed(message: String)
}

pub fn definition() -> workflow.WorkflowDefinition(
  WorkItem,
  ItemResult,
  ItemError,
) {
  workflow.define(
    "batch-orchestrator-item",
    work_item_codec(),
    item_result_codec(),
    item_error_codec(),
    process_item,
  )
}

/// Engine entry point for one child execution.
///
/// The runtime delivers the start input as a raw JSON string. Success and
/// failure are both encoded back to JSON text here: the engine records these
/// exact payloads as the child terminal, and the awaiting parent decodes them
/// with the same codecs this module exports.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case work_item_codec().decode(raw_json) {
        Ok(item) ->
          case process_item(item) {
            Ok(item_result) -> Ok(item_result_codec().encode(item_result))
            Error(item_error) -> Error(item_error_codec().encode(item_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            item_error_codec().encode(ItemFailed(
              "failed to decode child input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        item_error_codec().encode(ItemFailed(
          "child input payload was not a string",
        )),
      )
  }
}

/// Process one work item through the typed `process-batch-item` activity.
pub fn process_item(item: WorkItem) -> Result(ItemResult, ItemError) {
  case workflow.run(process_item_activity(item)) {
    Ok(result) -> Ok(result)
    Error(activity_error) ->
      Error(ItemFailed(activity_error_message(activity_error)))
  }
}

fn process_item_activity(
  item: WorkItem,
) -> activity.Activity(WorkItem, ItemResult) {
  activity.new(
    "process-batch-item",
    item,
    work_item_codec(),
    item_result_codec(),
    local_process_item,
  )
}

/// Local stub used by the pure-Gleam test double. On a real server the
/// connected activity worker executes `process-batch-item`
/// (`worker/worker.py` in this example) and must mirror this deterministic
/// contract: ids or payloads containing `fail` are terminal failures.
fn local_process_item(
  item: WorkItem,
) -> Result(ItemResult, error.ActivityError) {
  case should_fail(item) {
    True -> Error(error.terminal("deterministic failure for item " <> item.id))
    False ->
      Ok(ItemResult(
        item_id: item.id,
        processed_payload: "processed:" <> item.payload,
        detail: "processed item " <> item.id,
      ))
  }
}

fn should_fail(item: WorkItem) -> Bool {
  string.contains(item.id, "fail") || string.contains(item.payload, "fail")
}

pub fn work_item_codec() -> codec.Codec(WorkItem) {
  codec.json_codec(work_item_to_json, work_item_decoder())
}

pub fn work_item_to_json(item: WorkItem) -> json.Json {
  json.object([
    #("id", json.string(item.id)),
    #("payload", json.string(item.payload)),
  ])
}

pub fn work_item_decoder() -> decode.Decoder(WorkItem) {
  use id <- decode.field("id", decode.string)
  use payload <- decode.field("payload", decode.string)
  decode.success(WorkItem(id: id, payload: payload))
}

pub fn item_result_codec() -> codec.Codec(ItemResult) {
  codec.json_codec(item_result_to_json, item_result_decoder())
}

fn item_result_to_json(result: ItemResult) -> json.Json {
  json.object([
    #("item_id", json.string(result.item_id)),
    #("processed_payload", json.string(result.processed_payload)),
    #("detail", json.string(result.detail)),
  ])
}

fn item_result_decoder() -> decode.Decoder(ItemResult) {
  use item_id <- decode.field("item_id", decode.string)
  use processed_payload <- decode.field("processed_payload", decode.string)
  use detail <- decode.field("detail", decode.string)
  decode.success(ItemResult(
    item_id: item_id,
    processed_payload: processed_payload,
    detail: detail,
  ))
}

pub fn item_error_codec() -> codec.Codec(ItemError) {
  codec.json_codec(item_error_to_json, item_error_decoder())
}

fn item_error_to_json(item_error: ItemError) -> json.Json {
  let ItemFailed(message) = item_error
  json.object([#("message", json.string(message))])
}

fn item_error_decoder() -> decode.Decoder(ItemError) {
  use message <- decode.field("message", decode.string)
  decode.success(ItemFailed(message: message))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
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
