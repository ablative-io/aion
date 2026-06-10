//// Durable AI agent orchestration workflow.
////
//// The workflow accepts a task brief, schedules a `develop` activity, schedules
//// a `review` activity with the brief plus development output, and loops back
//// through development when review asks for revisions. Activity results are
//// recorded by Aion, so a crash after development replays the cached dev result
//// and resumes at review instead of re-running the agent.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list

const max_iterations = 3

pub type TaskInput {
  TaskInput(title: String, description: String, requirements: List(String))
}

pub type DevInput {
  DevInput(brief: TaskInput, revision_findings: List(String), attempt: Int)
}

pub type DevOutput {
  DevOutput(code_diff: String, commit_message: String)
}

pub type ReviewInput {
  ReviewInput(brief: TaskInput, dev_output: DevOutput, attempt: Int)
}

pub type Verdict {
  Land
  Revise
}

pub type ReviewOutput {
  ReviewOutput(verdict: Verdict, findings: List(String))
}

pub type WorkflowError {
  ActivityFailed(message: String)
  ExhaustedIterations(findings: List(String))
}

pub fn definition() -> workflow.WorkflowDefinition(
  TaskInput,
  DevOutput,
  WorkflowError,
) {
  workflow.define(
    "agent-orchestration",
    task_input_codec(),
    dev_output_codec(),
    workflow_error_codec(),
    execute,
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
      let input_codec = task_input_codec()
      case input_codec.decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) -> {
              let output_codec = dev_output_codec()
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

pub fn execute(input: TaskInput) -> Result(DevOutput, WorkflowError) {
  run_iteration(input, [], 1)
}

fn run_iteration(
  brief: TaskInput,
  revision_findings: List(String),
  attempt: Int,
) -> Result(DevOutput, WorkflowError) {
  use dev_output <- result_try_activity(workflow.run(develop_activity(DevInput(
    brief: brief,
    revision_findings: revision_findings,
    attempt: attempt,
  ))))

  use review_output <- result_try_activity(workflow.run(review_activity(
    ReviewInput(brief: brief, dev_output: dev_output, attempt: attempt),
  )))

  case review_output.verdict {
    Land -> Ok(dev_output)
    Revise -> {
      let all_findings = list.append(revision_findings, review_output.findings)
      case attempt < max_iterations {
        True -> run_iteration(brief, all_findings, attempt + 1)
        False -> Error(ExhaustedIterations(findings: all_findings))
      }
    }
  }
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

fn develop_activity(input: DevInput) -> activity.Activity(DevInput, DevOutput) {
  activity.new("develop", input, dev_input_codec(), dev_output_codec(), local_develop)
}

fn review_activity(
  input: ReviewInput,
) -> activity.Activity(ReviewInput, ReviewOutput) {
  activity.new("review", input, review_input_codec(), review_output_codec(), local_review)
}

fn local_develop(input: DevInput) -> Result(DevOutput, error.ActivityError) {
  Ok(DevOutput(
    code_diff: "diff --git a/README.md b/README.md\n+Implemented: "
      <> input.brief.title
      <> "\n+Attempt: "
      <> int.to_string(input.attempt),
    commit_message: "feat: " <> input.brief.title,
  ))
}

fn local_review(input: ReviewInput) -> Result(ReviewOutput, error.ActivityError) {
  case input.attempt < 2 {
    True ->
      Ok(ReviewOutput(
        verdict: Revise,
        findings: ["Add reviewer feedback before landing the demo change."],
      ))
    False -> Ok(ReviewOutput(verdict: Land, findings: []))
  }
}

fn task_input_codec() -> codec.Codec(TaskInput) {
  codec.json_codec(task_input_to_json, task_input_decoder())
}

fn task_input_to_json(input: TaskInput) -> json.Json {
  json.object([
    #("title", json.string(input.title)),
    #("description", json.string(input.description)),
    #("requirements", json.array(input.requirements, json.string)),
  ])
}

fn task_input_decoder() -> decode.Decoder(TaskInput) {
  use title <- decode.field("title", decode.string)
  use description <- decode.field("description", decode.string)
  use requirements <- decode.field("requirements", decode.list(decode.string))
  decode.success(TaskInput(
    title: title,
    description: description,
    requirements: requirements,
  ))
}

fn dev_input_codec() -> codec.Codec(DevInput) {
  codec.json_codec(dev_input_to_json, dev_input_decoder())
}

fn dev_input_to_json(input: DevInput) -> json.Json {
  json.object([
    #("brief", task_input_to_json(input.brief)),
    #("revision_findings", json.array(input.revision_findings, json.string)),
    #("attempt", json.int(input.attempt)),
  ])
}

fn dev_input_decoder() -> decode.Decoder(DevInput) {
  use brief <- decode.field("brief", task_input_decoder())
  use revision_findings <- decode.field(
    "revision_findings",
    decode.list(decode.string),
  )
  use attempt <- decode.field("attempt", decode.int)
  decode.success(DevInput(
    brief: brief,
    revision_findings: revision_findings,
    attempt: attempt,
  ))
}

fn dev_output_codec() -> codec.Codec(DevOutput) {
  codec.json_codec(dev_output_to_json, dev_output_decoder())
}

fn dev_output_to_json(output: DevOutput) -> json.Json {
  json.object([
    #("code_diff", json.string(output.code_diff)),
    #("commit_message", json.string(output.commit_message)),
  ])
}

fn dev_output_decoder() -> decode.Decoder(DevOutput) {
  use code_diff <- decode.field("code_diff", decode.string)
  use commit_message <- decode.field("commit_message", decode.string)
  decode.success(DevOutput(code_diff: code_diff, commit_message: commit_message))
}

fn review_input_codec() -> codec.Codec(ReviewInput) {
  codec.json_codec(review_input_to_json, review_input_decoder())
}

fn review_input_to_json(input: ReviewInput) -> json.Json {
  json.object([
    #("brief", task_input_to_json(input.brief)),
    #("dev_output", dev_output_to_json(input.dev_output)),
    #("attempt", json.int(input.attempt)),
  ])
}

fn review_input_decoder() -> decode.Decoder(ReviewInput) {
  use brief <- decode.field("brief", task_input_decoder())
  use dev_output <- decode.field("dev_output", dev_output_decoder())
  use attempt <- decode.field("attempt", decode.int)
  decode.success(ReviewInput(
    brief: brief,
    dev_output: dev_output,
    attempt: attempt,
  ))
}

fn review_output_codec() -> codec.Codec(ReviewOutput) {
  codec.json_codec(review_output_to_json, review_output_decoder())
}

fn review_output_to_json(output: ReviewOutput) -> json.Json {
  json.object([
    #("verdict", verdict_to_json(output.verdict)),
    #("findings", json.array(output.findings, json.string)),
  ])
}

fn review_output_decoder() -> decode.Decoder(ReviewOutput) {
  use verdict_text <- decode.field("verdict", decode.string)
  use findings <- decode.field("findings", decode.list(decode.string))
  decode.success(ReviewOutput(
    verdict: verdict_from_string(verdict_text),
    findings: findings,
  ))
}

fn verdict_to_json(verdict: Verdict) -> json.Json {
  case verdict {
    Land -> json.string("land")
    Revise -> json.string("revise")
  }
}

fn verdict_from_string(verdict: String) -> Verdict {
  case verdict {
    "land" -> Land
    _ -> Revise
  }
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
    ExhaustedIterations(findings) ->
      json.object([
        #("type", json.string("exhausted_iterations")),
        #("findings", json.array(findings, json.string)),
      ])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use error_type <- decode.field("type", decode.string)
  case error_type {
    "exhausted_iterations" -> {
      use findings <- decode.field("findings", decode.list(decode.string))
      decode.success(ExhaustedIterations(findings: findings))
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
