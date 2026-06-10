//// Parent-child batch orchestration example for Aion.
////
//// The parent workflow starts one child workflow per work item, exposes a
//// read-only progress query, and aggregates every child outcome into a final
//// summary. Child workflow failures are collected as per-item failed outcomes so
//// one bad item does not fail the parent or its siblings.

import aion/activity
import aion/child
import aion/codec
import aion/error
import aion/query
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/string

pub type BatchInput {
  BatchInput(items: List(WorkItem))
}

pub type WorkItem {
  WorkItem(id: String, payload: String)
}

pub type ItemResult {
  ItemResult(item_id: String, processed_payload: String, detail: String)
}

pub type ItemOutcome {
  ItemOutcome(item_id: String, status: String, detail: String)
}

pub type BatchProgress {
  BatchProgress(total: Int, completed: Int, failed: Int, pending: Int)
}

pub type BatchSummary {
  BatchSummary(
    total_processed: Int,
    success_count: Int,
    failure_count: Int,
    items: List(ItemOutcome),
  )
}

pub type WorkflowError {
  ActivityFailed(message: String)
  QueryRegistrationFailed(message: String)
}

type SpawnedChild {
  SpawnedChild(
    item: WorkItem,
    handle: child.ChildHandle(ItemResult, WorkflowError),
  )
}

type CollectionState {
  CollectionState(
    progress: BatchProgress,
    outcomes: List(ItemOutcome),
    success_count: Int,
    failure_count: Int,
  )
}

pub fn definition() -> workflow.WorkflowDefinition(BatchInput, BatchSummary, WorkflowError) {
  workflow.define(
    "batch-orchestrator",
    batch_input_codec(),
    batch_summary_codec(),
    workflow_error_codec(),
    execute,
  )
}

pub fn child_definition() -> workflow.WorkflowDefinition(WorkItem, ItemResult, WorkflowError) {
  workflow.define(
    "batch-orchestrator-item",
    work_item_codec(),
    item_result_codec(),
    workflow_error_codec(),
    process_item,
  )
}

/// Engine entry point.
///
/// The runtime delivers the start input as a raw JSON string: decode it with
/// the input codec, run the typed workflow, and encode the success value back
/// to its JSON string for the recorded result payload.
pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) -> {
      let input_codec = batch_input_codec()
      case input_codec.decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) -> {
              let output_codec = batch_summary_codec()
              Ok(output_codec.encode(output))
            }
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(ActivityFailed("failed to decode workflow input: " <> reason))
      }
    }
    Error(_) -> Error(ActivityFailed("workflow input payload was not a string"))
  }
}

pub fn execute(input: BatchInput) -> Result(BatchSummary, WorkflowError) {
  let total = list.length(input.items)
  let initial_progress = BatchProgress(
    total: total,
    completed: 0,
    failed: 0,
    pending: total,
  )

  use _registered <- result_try(register_progress(initial_progress))

  let spawned = spawn_children(input.items, [], [])
  let progress_after_spawns = apply_spawn_failures(
    initial_progress,
    list.length(spawned.spawn_failures),
  )
  use _spawn_progress_registered <- result_try(register_progress(progress_after_spawns))

  let initial_state = CollectionState(
    progress: progress_after_spawns,
    outcomes: spawned.spawn_failures,
    success_count: 0,
    failure_count: list.length(spawned.spawn_failures),
  )
  use final_state <- result_try(await_children(spawned.children, initial_state))

  Ok(BatchSummary(
    total_processed: final_state.success_count + final_state.failure_count,
    success_count: final_state.success_count,
    failure_count: final_state.failure_count,
    items: list.reverse(final_state.outcomes),
  ))
}

pub fn process_item(item: WorkItem) -> Result(ItemResult, WorkflowError) {
  case workflow.run(process_item_activity(item)) {
    Ok(result) -> Ok(result)
    Error(activity_error) -> Error(ActivityFailed(activity_error_message(activity_error)))
  }
}

type SpawnResult {
  SpawnResult(children: List(SpawnedChild), spawn_failures: List(ItemOutcome))
}

fn spawn_children(
  items: List(WorkItem),
  children: List(SpawnedChild),
  spawn_failures: List(ItemOutcome),
) -> SpawnResult {
  case items {
    [] -> SpawnResult(
      children: list.reverse(children),
      spawn_failures: spawn_failures,
    )
    [item, ..rest] -> {
      case
        child.spawn(
          "batch-orchestrator-item",
          process_item,
          item,
          work_item_codec(),
          item_result_codec(),
          workflow_error_codec(),
        )
      {
        Ok(handle) ->
          spawn_children(rest, [SpawnedChild(item: item, handle: handle), ..children], spawn_failures)
        Error(spawn_error) -> {
          let failure = ItemOutcome(
            item_id: item.id,
            status: "failed",
            detail: "child spawn failed: " <> engine_error_message(spawn_error),
          )
          spawn_children(rest, children, [failure, ..spawn_failures])
        }
      }
    }
  }
}

fn await_children(
  children: List(SpawnedChild),
  state: CollectionState,
) -> Result(CollectionState, WorkflowError) {
  case children {
    [] -> Ok(state)
    [spawned, ..rest] -> {
      let next_state = case child.await(spawned.handle) {
        Ok(item_result) -> record_success(state, item_result)
        Error(child_error) -> record_failure(state, spawned.item, child_error)
      }
      use _registered <- result_try(register_progress(next_state.progress))
      await_children(rest, next_state)
    }
  }
}

fn register_progress(progress: BatchProgress) -> Result(Nil, WorkflowError) {
  query.handler("batch_progress", batch_progress_codec(), fn() {
    progress
  })
  |> result_map_query_error
}

fn apply_spawn_failures(progress: BatchProgress, failure_count: Int) -> BatchProgress {
  BatchProgress(
    total: progress.total,
    completed: progress.completed,
    failed: progress.failed + failure_count,
    pending: progress.pending - failure_count,
  )
}

fn record_success(state: CollectionState, result: ItemResult) -> CollectionState {
  let progress = mark_completed(state.progress)
  let outcome = ItemOutcome(
    item_id: result.item_id,
    status: "succeeded",
    detail: result.detail,
  )

  CollectionState(
    progress: progress,
    outcomes: [outcome, ..state.outcomes],
    success_count: state.success_count + 1,
    failure_count: state.failure_count,
  )
}

fn record_failure(
  state: CollectionState,
  item: WorkItem,
  child_error: error.ChildError(WorkflowError),
) -> CollectionState {
  let progress = mark_failed(state.progress)
  let outcome = ItemOutcome(
    item_id: item.id,
    status: "failed",
    detail: child_error_message(child_error),
  )

  CollectionState(
    progress: progress,
    outcomes: [outcome, ..state.outcomes],
    success_count: state.success_count,
    failure_count: state.failure_count + 1,
  )
}

fn mark_completed(progress: BatchProgress) -> BatchProgress {
  BatchProgress(
    total: progress.total,
    completed: progress.completed + 1,
    failed: progress.failed,
    pending: progress.pending - 1,
  )
}

fn mark_failed(progress: BatchProgress) -> BatchProgress {
  BatchProgress(
    total: progress.total,
    completed: progress.completed,
    failed: progress.failed + 1,
    pending: progress.pending - 1,
  )
}

fn process_item_activity(item: WorkItem) -> activity.Activity(WorkItem, ItemResult) {
  activity.new(
    "process-batch-item",
    item,
    work_item_codec(),
    item_result_codec(),
    local_process_item,
  )
}

fn local_process_item(item: WorkItem) -> Result(ItemResult, error.ActivityError) {
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

fn batch_input_codec() -> codec.Codec(BatchInput) {
  codec.json_codec(batch_input_to_json, batch_input_decoder())
}

fn batch_input_to_json(input: BatchInput) -> json.Json {
  json.object([#("items", json.array(input.items, work_item_to_json))])
}

fn batch_input_decoder() -> decode.Decoder(BatchInput) {
  use items <- decode.field("items", decode.list(work_item_decoder()))
  decode.success(BatchInput(items: items))
}

fn work_item_codec() -> codec.Codec(WorkItem) {
  codec.json_codec(work_item_to_json, work_item_decoder())
}

fn work_item_to_json(item: WorkItem) -> json.Json {
  json.object([
    #("id", json.string(item.id)),
    #("payload", json.string(item.payload)),
  ])
}

fn work_item_decoder() -> decode.Decoder(WorkItem) {
  use id <- decode.field("id", decode.string)
  use payload <- decode.field("payload", decode.string)
  decode.success(WorkItem(id: id, payload: payload))
}

fn item_result_codec() -> codec.Codec(ItemResult) {
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

fn item_outcome_to_json(outcome: ItemOutcome) -> json.Json {
  json.object([
    #("item_id", json.string(outcome.item_id)),
    #("status", json.string(outcome.status)),
    #("detail", json.string(outcome.detail)),
  ])
}

fn item_outcome_decoder() -> decode.Decoder(ItemOutcome) {
  use item_id <- decode.field("item_id", decode.string)
  use status <- decode.field("status", decode.string)
  use detail <- decode.field("detail", decode.string)
  decode.success(ItemOutcome(item_id: item_id, status: status, detail: detail))
}

fn batch_progress_codec() -> codec.Codec(BatchProgress) {
  codec.json_codec(batch_progress_to_json, batch_progress_decoder())
}

fn batch_progress_to_json(progress: BatchProgress) -> json.Json {
  json.object([
    #("total", json.int(progress.total)),
    #("completed", json.int(progress.completed)),
    #("failed", json.int(progress.failed)),
    #("pending", json.int(progress.pending)),
  ])
}

fn batch_progress_decoder() -> decode.Decoder(BatchProgress) {
  use total <- decode.field("total", decode.int)
  use completed <- decode.field("completed", decode.int)
  use failed <- decode.field("failed", decode.int)
  use pending <- decode.field("pending", decode.int)
  decode.success(BatchProgress(
    total: total,
    completed: completed,
    failed: failed,
    pending: pending,
  ))
}

fn batch_summary_codec() -> codec.Codec(BatchSummary) {
  codec.json_codec(batch_summary_to_json, batch_summary_decoder())
}

fn batch_summary_to_json(summary: BatchSummary) -> json.Json {
  json.object([
    #("total_processed", json.int(summary.total_processed)),
    #("success_count", json.int(summary.success_count)),
    #("failure_count", json.int(summary.failure_count)),
    #("items", json.array(summary.items, item_outcome_to_json)),
  ])
}

fn batch_summary_decoder() -> decode.Decoder(BatchSummary) {
  use total_processed <- decode.field("total_processed", decode.int)
  use success_count <- decode.field("success_count", decode.int)
  use failure_count <- decode.field("failure_count", decode.int)
  use items <- decode.field("items", decode.list(item_outcome_decoder()))
  decode.success(BatchSummary(
    total_processed: total_processed,
    success_count: success_count,
    failure_count: failure_count,
    items: items,
  ))
}

fn workflow_error_codec() -> codec.Codec(WorkflowError) {
  codec.json_codec(workflow_error_to_json, workflow_error_decoder())
}

fn workflow_error_to_json(workflow_error: WorkflowError) -> json.Json {
  case workflow_error {
    ActivityFailed(message) ->
      json.object([
        #("type", json.string("activity_failed")),
        #("message", json.string(message)),
      ])
    QueryRegistrationFailed(message) ->
      json.object([
        #("type", json.string("query_registration_failed")),
        #("message", json.string(message)),
      ])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use error_type <- decode.field("type", decode.string)
  case error_type {
    "activity_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(ActivityFailed(message: message))
    }
    "query_registration_failed" -> {
      use message <- decode.field("message", decode.string)
      decode.success(QueryRegistrationFailed(message: message))
    }
    _ -> {
      use message <- decode.field("message", decode.string)
      decode.success(ActivityFailed(message: message))
    }
  }
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.ActivityEngineFailure(message: message) -> message
  }
}

fn child_error_message(child_error: error.ChildError(WorkflowError)) -> String {
  case child_error {
    error.ChildWorkflowFailed(workflow_error) -> workflow_error_message(workflow_error)
    error.ChildOutputDecodeFailed(_) -> "child output could not be decoded"
    error.ChildErrorDecodeFailed(_) -> "child error could not be decoded"
    error.ChildCancelled(error.Cancelled(reason: reason)) -> reason
    error.ChildNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.ChildEngineFailure(message: message) -> message
  }
}

fn workflow_error_message(workflow_error: WorkflowError) -> String {
  case workflow_error {
    ActivityFailed(message) -> message
    QueryRegistrationFailed(message) -> message
  }
}

fn query_error_message(query_error: error.QueryError) -> String {
  case query_error {
    error.QueryDecodeFailed(_) -> "query payload could not be decoded"
    error.UnknownQuery(name: name) -> "unknown query: " <> name
    error.QueryCancelled(error.Cancelled(reason: reason)) -> reason
    error.QueryNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.QueryEngineFailure(message: message) -> message
  }
}

fn engine_error_message(engine_error: error.EngineError) -> String {
  case engine_error {
    error.EngineFailure(message: message) -> message
  }
}

fn result_map_query_error(result: Result(Nil, error.QueryError)) -> Result(Nil, WorkflowError) {
  case result {
    Ok(value) -> Ok(value)
    Error(query_error) ->
      Error(QueryRegistrationFailed(query_error_message(query_error)))
  }
}

fn result_try(
  result: Result(value, WorkflowError),
  next: fn(value) -> Result(output, WorkflowError),
) -> Result(output, WorkflowError) {
  case result {
    Ok(value) -> next(value)
    Error(workflow_error) -> Error(workflow_error)
  }
}
