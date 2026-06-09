//// Fan-out/fan-in data-pipeline workflow.
////
//// The workflow accepts `{ "urls": List(String) }`, fans out `fetch_url`
//// activities with `workflow.all`, transforms the fetched content with
//// `workflow.map`, and fans in to one `aggregate_results` activity.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list
import gleam/string

pub type PipelineInput {
  PipelineInput(urls: List(String))
}

pub type FetchedContent {
  FetchedContent(url: String, content: String)
}

pub type ProcessedItem {
  ProcessedItem(url: String, word_count: Int, summary: String)
}

pub type AggregateOutput {
  AggregateOutput(total_urls: Int, total_words: Int, summaries: List(String))
}

pub type WorkflowError {
  ActivityFailed(message: String)
}

pub fn definition() -> workflow.WorkflowDefinition(
  PipelineInput,
  AggregateOutput,
  WorkflowError,
) {
  workflow.define(
    "data-pipeline",
    pipeline_input_codec(),
    aggregate_output_codec(),
    workflow_error_codec(),
    run,
  )
}

pub fn run(input: PipelineInput) -> Result(AggregateOutput, WorkflowError) {
  // Phase 1: fan out one fetch_url activity per URL, preserving input order.
  use fetched <- result_try_activity(fetch_all(input.urls))

  // Phase 2: transform each fetched item concurrently with process_item.
  use processed <- result_try_activity(process_all(fetched))

  // Phase 3: fan in all processed items through one aggregate_results activity.
  case aggregate(processed) {
    Ok(output) -> Ok(output)
    Error(activity_error) -> Error(ActivityFailed(activity_error_message(activity_error)))
  }
}

fn fetch_all(urls: List(String)) -> Result(List(FetchedContent), error.ActivityError) {
  urls
  |> list.map(fetch_url_activity)
  |> workflow.all
}

fn process_all(
  fetched: List(FetchedContent),
) -> Result(List(ProcessedItem), error.ActivityError) {
  workflow.map(fetched, process_item_activity)
}

fn aggregate(
  processed: List(ProcessedItem),
) -> Result(AggregateOutput, error.ActivityError) {
  workflow.run(aggregate_results_activity(processed))
}

fn result_try_activity(
  result: Result(output, error.ActivityError),
  next: fn(output) -> Result(final, WorkflowError),
) -> Result(final, WorkflowError) {
  case result {
    Ok(output) -> next(output)
    Error(activity_error) -> Error(ActivityFailed(activity_error_message(activity_error)))
  }
}

fn fetch_url_activity(url: String) -> activity.Activity(String, FetchedContent) {
  activity.new(
    "fetch_url",
    url,
    string_codec(),
    fetched_content_codec(),
    local_fetch_url,
  )
}

fn process_item_activity(
  fetched: FetchedContent,
) -> activity.Activity(FetchedContent, ProcessedItem) {
  activity.new(
    "process_item",
    fetched,
    fetched_content_codec(),
    processed_item_codec(),
    local_process_item,
  )
}

fn aggregate_results_activity(
  processed: List(ProcessedItem),
) -> activity.Activity(List(ProcessedItem), AggregateOutput) {
  activity.new(
    "aggregate_results",
    processed,
    processed_item_list_codec(),
    aggregate_output_codec(),
    local_aggregate_results,
  )
}

fn local_fetch_url(url: String) -> Result(FetchedContent, error.ActivityError) {
  Ok(FetchedContent(
    url: url,
    content: "Simulated content fetched from " <> url,
  ))
}

fn local_process_item(
  fetched: FetchedContent,
) -> Result(ProcessedItem, error.ActivityError) {
  let words = fetched.content |> string.split(" ") |> list.length

  Ok(ProcessedItem(
    url: fetched.url,
    word_count: words,
    summary: "Processed " <> fetched.url <> " with " <> int.to_string(words) <> " words",
  ))
}

fn local_aggregate_results(
  processed: List(ProcessedItem),
) -> Result(AggregateOutput, error.ActivityError) {
  Ok(AggregateOutput(
    total_urls: list.length(processed),
    total_words: sum_word_counts(processed),
    summaries: list.map(processed, fn(item) { item.summary }),
  ))
}

fn sum_word_counts(items: List(ProcessedItem)) -> Int {
  items
  |> list.fold(0, fn(total, item) { total + item.word_count })
}

fn pipeline_input_codec() -> codec.Codec(PipelineInput) {
  codec.json_codec(pipeline_input_to_json, pipeline_input_decoder())
}

fn pipeline_input_to_json(input: PipelineInput) -> json.Json {
  json.object([#("urls", json.array(input.urls, json.string))])
}

fn pipeline_input_decoder() -> decode.Decoder(PipelineInput) {
  use urls <- decode.field("urls", decode.list(decode.string))
  decode.success(PipelineInput(urls: urls))
}

fn fetched_content_codec() -> codec.Codec(FetchedContent) {
  codec.json_codec(fetched_content_to_json, fetched_content_decoder())
}

fn fetched_content_to_json(fetched: FetchedContent) -> json.Json {
  json.object([
    #("url", json.string(fetched.url)),
    #("content", json.string(fetched.content)),
  ])
}

fn fetched_content_decoder() -> decode.Decoder(FetchedContent) {
  use url <- decode.field("url", decode.string)
  use content <- decode.field("content", decode.string)
  decode.success(FetchedContent(url: url, content: content))
}

fn processed_item_codec() -> codec.Codec(ProcessedItem) {
  codec.json_codec(processed_item_to_json, processed_item_decoder())
}

fn processed_item_list_codec() -> codec.Codec(List(ProcessedItem)) {
  codec.json_codec(processed_item_list_to_json, decode.list(processed_item_decoder()))
}

fn processed_item_list_to_json(items: List(ProcessedItem)) -> json.Json {
  json.array(items, processed_item_to_json)
}

fn processed_item_to_json(item: ProcessedItem) -> json.Json {
  json.object([
    #("url", json.string(item.url)),
    #("word_count", json.int(item.word_count)),
    #("summary", json.string(item.summary)),
  ])
}

fn processed_item_decoder() -> decode.Decoder(ProcessedItem) {
  use url <- decode.field("url", decode.string)
  use word_count <- decode.field("word_count", decode.int)
  use summary <- decode.field("summary", decode.string)
  decode.success(ProcessedItem(
    url: url,
    word_count: word_count,
    summary: summary,
  ))
}

fn aggregate_output_codec() -> codec.Codec(AggregateOutput) {
  codec.json_codec(aggregate_output_to_json, aggregate_output_decoder())
}

fn aggregate_output_to_json(output: AggregateOutput) -> json.Json {
  json.object([
    #("total_urls", json.int(output.total_urls)),
    #("total_words", json.int(output.total_words)),
    #("summaries", json.array(output.summaries, json.string)),
  ])
}

fn aggregate_output_decoder() -> decode.Decoder(AggregateOutput) {
  use total_urls <- decode.field("total_urls", decode.int)
  use total_words <- decode.field("total_words", decode.int)
  use summaries <- decode.field("summaries", decode.list(decode.string))
  decode.success(AggregateOutput(
    total_urls: total_urls,
    total_words: total_words,
    summaries: summaries,
  ))
}

fn string_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn workflow_error_codec() -> codec.Codec(WorkflowError) {
  codec.json_codec(workflow_error_to_json, workflow_error_decoder())
}

fn workflow_error_to_json(error: WorkflowError) -> json.Json {
  case error {
    ActivityFailed(message) ->
      json.object([
        #("type", json.string("activity_failed")),
        #("message", json.string(message)),
      ])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use message <- decode.field("message", decode.string)
  decode.success(ActivityFailed(message: message))
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
