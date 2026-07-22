//// CLI structured artifact proof.

import aion/activity
import aion/awl/codec as awlc
import aion/awl/error as awl_error
import aion/awl/runtime
import aion/child
import aion/codec.{type Codec}
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result

pub type Done {
  Done(
    count: Int,
  )
}

pub type CliParallelInput {
  CliParallelInput(
    items: List(String),
  )
}

pub type CliParallelOutcome {
  DoneOutcome(Done)
}

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(CliParallelInput, CliParallelOutcome, awl_error.AwlError) {
  workflow.define(
    "cli_parallel",
    cli_parallel_input_codec(),
    cli_parallel_outcome_codec(),
    awl_error.codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  runtime.run(raw_input, cli_parallel_input_codec(), cli_parallel_outcome_codec(), execute)
}

/// Workflow body generated from the AWL steps.
pub fn execute(input: CliParallelInput) -> Result(CliParallelOutcome, awl_error.AwlError) {
  let items = input.items
  step_fan(items)
}

fn step_fan(items: List(String)) -> Result(CliParallelOutcome, awl_error.AwlError) {
  use awl_handles_reversed <- result.try(list.try_fold(items, [], fn(awl_acc, item) {
    use awl_handle <- result.try(workflow.spawn("aion_internal_awl_child_cli_parallel_fan_0", fn(_: json.Json) { Error(awl_error.AwlChildFailed("child workflow body runs in its own execution")) }, json.object([#("item", awlc.string_to_json(item))]), awlc.json_value(), awl_child_output_string_codec(), awl_error.codec()) |> awl_error.map_spawn_error)
    Ok([awl_handle, ..awl_acc])
  }))
  use awl_results_reversed <- result.try(list.try_fold(list.reverse(awl_handles_reversed), [], fn(awl_acc, awl_handle) {
    use awl_item <- result.try(child.await(awl_handle) |> awl_error.map_child_error)
    Ok([awl_item, ..awl_acc])
  }))
  let results = list.reverse(awl_results_reversed)
  let awl_piped_0 = list.length(results)
  let total = awl_piped_0
  Ok(DoneOutcome(Done(count: total)))
}

fn awl_r0_fan(item: String) -> Result(String, awl_error.AwlError) {
  awl_r0_fan_step_one(item)
}

fn awl_r0_fan_step_one(item: String) -> Result(String, awl_error.AwlError) {
  use prepared <- result.try(first_activity(item) |> activity.task_queue("proof") |> workflow.run |> awl_error.map_activity_error)
  use result <- result.try(second_activity(prepared) |> activity.task_queue("proof") |> workflow.run |> awl_error.map_activity_error)
  Ok(result)
}

pub type AionInternalAwlChildCliParallelFan0Input {
  AionInternalAwlChildCliParallelFan0Input(
    item: String,
  )
}

fn aion_internal_awl_child_cli_parallel_fan0_input_codec() -> Codec(AionInternalAwlChildCliParallelFan0Input) {
  codec.json_codec(aion_internal_awl_child_cli_parallel_fan0_input_to_json, aion_internal_awl_child_cli_parallel_fan0_input_decoder())
}

fn aion_internal_awl_child_cli_parallel_fan0_input_to_json(value: AionInternalAwlChildCliParallelFan0Input) -> json.Json {
  json.object([
    #("item", awlc.string_to_json(value.item)),
  ])
}

fn aion_internal_awl_child_cli_parallel_fan0_input_decoder() -> decode.Decoder(AionInternalAwlChildCliParallelFan0Input) {
  use item <- decode.field("item", awlc.string_decoder())
  decode.success(AionInternalAwlChildCliParallelFan0Input(
    item: item,
  ))
}

fn aion_internal_awl_child_cli_parallel_fan_0_execute(input: AionInternalAwlChildCliParallelFan0Input) -> Result(String, awl_error.AwlError) {
  awl_r0_fan(input.item)
}

/// Engine entry point for an implicit parallel region child.
pub fn aion_internal_awl_child_cli_parallel_fan_0_run(raw_input: Dynamic) -> Result(String, String) {
  runtime.run(raw_input, aion_internal_awl_child_cli_parallel_fan0_input_codec(), awl_child_output_string_codec(), aion_internal_awl_child_cli_parallel_fan_0_execute)
}

pub type FirstInput {
  FirstInput(
    item: String,
  )
}

fn first_activity(
  item: String,
) -> activity.Activity(FirstInput, String) {
  activity.new(
    "first",
    FirstInput(
      item: item,
    ),
    first_input_codec(),
    awlc.string_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

pub type SecondInput {
  SecondInput(
    item: String,
  )
}

fn second_activity(
  item: String,
) -> activity.Activity(SecondInput, String) {
  activity.new(
    "second",
    SecondInput(
      item: item,
    ),
    second_input_codec(),
    awlc.string_codec(),
    fn(_) { Error(error.terminal("activity body is provided by a worker")) },
  )
}

fn cli_parallel_input_codec() -> Codec(CliParallelInput) {
  codec.json_codec(cli_parallel_input_to_json, cli_parallel_input_decoder())
}

fn cli_parallel_input_to_json(value: CliParallelInput) -> json.Json {
  json.object([
    #("items", list_string_to_json(value.items)),
  ])
}

fn cli_parallel_input_decoder() -> decode.Decoder(CliParallelInput) {
  use items <- decode.field("items", list_string_decoder())
  decode.success(CliParallelInput(
    items: items,
  ))
}

fn cli_parallel_outcome_codec() -> Codec(CliParallelOutcome) {
  codec.json_codec(cli_parallel_outcome_to_json, cli_parallel_outcome_decoder())
}

fn cli_parallel_outcome_to_json(value: CliParallelOutcome) -> json.Json {
  case value {
    DoneOutcome(payload) -> json.object([#("outcome", json.string("done")), #("payload", done_to_json(payload))])
  }
}

fn cli_parallel_outcome_decoder() -> decode.Decoder(CliParallelOutcome) {
  use outcome <- decode.field("outcome", decode.string)
  case outcome {
    "done" -> {
      use payload <- decode.field("payload", done_decoder())
      decode.success(DoneOutcome(payload))
    }
    _ -> decode.failure(DoneOutcome(Done(count: 0)), "CliParallelOutcome")
  }
}

fn done_codec() -> Codec(Done) {
  codec.json_codec(done_to_json, done_decoder())
}

fn done_to_json(value: Done) -> json.Json {
  json.object([
    #("count", awlc.int_to_json(value.count)),
  ])
}

fn done_decoder() -> decode.Decoder(Done) {
  use count <- decode.field("count", awlc.int_decoder())
  decode.success(Done(
    count: count,
  ))
}

fn first_input_codec() -> Codec(FirstInput) {
  codec.json_codec(first_input_to_json, first_input_decoder())
}

fn first_input_to_json(value: FirstInput) -> json.Json {
  json.object([
    #("item", awlc.string_to_json(value.item)),
  ])
}

fn first_input_decoder() -> decode.Decoder(FirstInput) {
  use item <- decode.field("item", awlc.string_decoder())
  decode.success(FirstInput(
    item: item,
  ))
}

fn second_input_codec() -> Codec(SecondInput) {
  codec.json_codec(second_input_to_json, second_input_decoder())
}

fn second_input_to_json(value: SecondInput) -> json.Json {
  json.object([
    #("item", awlc.string_to_json(value.item)),
  ])
}

fn second_input_decoder() -> decode.Decoder(SecondInput) {
  use item <- decode.field("item", awlc.string_decoder())
  decode.success(SecondInput(
    item: item,
  ))
}

fn list_string_codec() -> Codec(List(String)) {
  codec.json_codec(list_string_to_json, list_string_decoder())
}
fn list_string_to_json(values: List(String)) -> json.Json { json.array(values, awlc.string_to_json) }
fn list_string_decoder() -> decode.Decoder(List(String)) { decode.list(awlc.string_decoder()) }

fn awl_child_output_string_codec() -> Codec(String) {
  codec.json_codec(awl_child_output_string_to_json, awl_child_output_string_decoder())
}

fn awl_child_output_string_to_json(payload: String) -> json.Json {
  json.object([#("outcome", json.string("child")), #("payload", awlc.string_to_json(payload))])
}

fn awl_child_output_string_decoder() -> decode.Decoder(String) {
  use _outcome <- decode.field("outcome", decode.string)
  use payload <- decode.field("payload", awlc.string_decoder())
  decode.success(payload)
}

