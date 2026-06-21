//// {{name}} — durable agent loop (agent template).
////
//// A configuration-driven agentic family: scout -> act -> verify ->
//// signal-gated human review. Each of the three agent steps is a
//// worker-served activity parameterised by a prompt carried in the start
//// input — the scaffold bundles no agent runtime of its own. The worker
//// (`worker/`) decides what "scout", "act", and "verify" actually do; swap
//// its handlers for your own agent driver (LLM call, tool loop, `norn`, …)
//// without touching this workflow.
////
//// The human approval pause is a durable `workflow.receive` raced against a
//// caller-chosen deadline with `workflow.with_timeout` — not a polling loop.
//// The run suspends, for seconds or for weeks, and survives server restarts
//// while it waits. No deadline is invented here: `review_timeout_ms` is a
//// required field of the start input, and the per-step agent activities run
//// unbounded until the worker answers (the engine imposes no step timeout).
////
//// Outcomes:
////   * an `agent_review` signal carrying `approve` applies the artifact,
////   * `reject` or the deadline lapsing holds it.
//// Either way the run completes — a held artifact is a successful, fully
//// recorded run, ready for a human follow-up.
////
//// An `agent_status` query reports the live stage at every point, answered
//// after replay with no extra author code.
////
//// Edit `handle` and the helpers above the generated-code marker; the raw
//// engine plumbing and JSON codecs live below it.

import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/query
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/result

pub type AgentInput {
  AgentInput(
    task_id: String,
    scout_prompt: String,
    act_prompt: String,
    verify_prompt: String,
    review_timeout_ms: Int,
  )
}

/// Input handed to each parameterised agent step. `prompt` is the step's
/// instruction; `context` carries the prior step's output so the worker-side
/// agent can build on it. The scaffold treats the body as opaque.
pub type StepInput {
  StepInput(task_id: String, prompt: String, context: String)
}

/// Output of an agent step: the worker's textual result for that step.
pub type StepOutput {
  StepOutput(result: String)
}

pub type Decision {
  Approve
  Reject
}

/// The `agent_review` signal: a human's gate decision plus their identity.
pub type ReviewSignal {
  ReviewSignal(decision: Decision, reviewer: String)
}

pub type AgentStatus {
  AgentStatus(stage: String, task_id: String)
}

pub type AgentResult {
  AgentResult(
    task_id: String,
    disposition: String,
    scout_finding: String,
    act_artifact: String,
    verify_verdict: String,
    reviewed_by: String,
    reason: String,
  )
}

pub type AgentError {
  InvalidInput(message: String)
  StepFailed(stage: String, message: String)
  GateFailed(message: String)
  QueryFailed(message: String)
}

/// Name of the human-review signal this agent waits on.
pub const review_signal_name = "agent_review"

/// Name of the read-only stage query this agent answers at every step.
pub const status_query_name = "agent_status"

/// Your typed workflow: drive the agent loop scout -> act -> verify, then
/// suspend on a durable, deadline-bounded human review before applying.
pub fn handle(input: AgentInput) -> Result(AgentResult, AgentError) {
  use _ <- result.try(set_status("scouting", input.task_id))
  use scouted <- result.try(run_step(
    "scout",
    input.task_id,
    input.scout_prompt,
    "",
  ))

  use _ <- result.try(set_status("acting", input.task_id))
  use acted <- result.try(run_step(
    "act",
    input.task_id,
    input.act_prompt,
    scouted.result,
  ))

  use _ <- result.try(set_status("verifying", input.task_id))
  use verified <- result.try(run_step(
    "verify",
    input.task_id,
    input.verify_prompt,
    acted.result,
  ))

  use _ <- result.try(set_status("awaiting_review", input.task_id))
  await_review(input, scouted, acted, verified)
}

/// Dispatch one parameterised agent step to the worker. The step is opaque to
/// this workflow: the worker decides what scouting, acting, or verifying
/// means. Each dispatch is a durably recorded activity; replay resolves it
/// from history without re-running the worker. No step deadline is imposed —
/// agentic work legitimately runs long, and the engine waits for the worker.
fn run_step(
  stage: String,
  task_id: String,
  prompt: String,
  context: String,
) -> Result(StepOutput, AgentError) {
  let step = StepInput(task_id: task_id, prompt: prompt, context: context)
  case workflow.run(step_activity(stage, step)) {
    Ok(output) -> Ok(output)
    Error(activity_error) ->
      Error(StepFailed(
        stage: stage,
        message: activity_error_message(activity_error),
      ))
  }
}

/// Race the human review decision against the caller-chosen deadline. The
/// wait is a durable `workflow.receive`; the run suspends until the signal
/// arrives or the deadline lapses. There is no default — `review_timeout_ms`
/// is supplied in the start input.
fn await_review(
  input: AgentInput,
  scouted: StepOutput,
  acted: StepOutput,
  verified: StepOutput,
) -> Result(AgentResult, AgentError) {
  case
    workflow.with_timeout(
      fn() { workflow.receive(review_signal()) },
      duration.milliseconds(input.review_timeout_ms),
    )
  {
    Ok(ReviewSignal(decision: Approve, reviewer: reviewer)) -> {
      use _ <- result.try(set_status("applied", input.task_id))
      Ok(result_for(
        input,
        scouted,
        acted,
        verified,
        "applied",
        reviewer,
        "approved by " <> reviewer,
      ))
    }
    Ok(ReviewSignal(decision: Reject, reviewer: reviewer)) -> {
      use _ <- result.try(set_status("held", input.task_id))
      Ok(result_for(
        input,
        scouted,
        acted,
        verified,
        "held",
        reviewer,
        "rejected by " <> reviewer,
      ))
    }
    Error(error.TimedOutError(error.TimedOut(message: _))) -> {
      use _ <- result.try(set_status("held", input.task_id))
      Ok(result_for(
        input,
        scouted,
        acted,
        verified,
        "held",
        "",
        "review timed out after "
          <> int.to_string(input.review_timeout_ms)
          <> "ms",
      ))
    }
    Error(error.InnerError(receive_error)) ->
      Error(GateFailed(receive_error_message(receive_error)))
    Error(error.TimeoutEngineFailure(message: message)) ->
      Error(GateFailed(message))
  }
}

fn result_for(
  input: AgentInput,
  scouted: StepOutput,
  acted: StepOutput,
  verified: StepOutput,
  disposition: String,
  reviewer: String,
  reason: String,
) -> AgentResult {
  AgentResult(
    task_id: input.task_id,
    disposition: disposition,
    scout_finding: scouted.result,
    act_artifact: acted.result,
    verify_verdict: verified.result,
    reviewed_by: reviewer,
    reason: reason,
  )
}

/// Re-register the `agent_status` handler with the current stage. Queries are
/// answered at yield points and never touch history, so a recovered run
/// answers them after replay with no extra author code.
fn set_status(stage: String, task_id: String) -> Result(Nil, AgentError) {
  let status = AgentStatus(stage: stage, task_id: task_id)
  case query.handler(status_query_name, agent_status_codec(), fn() { status }) {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) -> Error(QueryFailed(query_error_message(query_error)))
  }
}

fn review_signal() -> workflow.SignalRef(ReviewSignal) {
  signal.new(review_signal_name, review_signal_codec())
}

fn step_activity(
  stage: String,
  step: StepInput,
) -> activity.Activity(StepInput, StepOutput) {
  activity.new(stage, step, step_input_codec(), step_output_codec(), local_step)
}

/// Local stub used only by the `aion/testing` harness; a deployed workflow
/// always dispatches each step to a connected worker (`worker/`), which is
/// where the real agent runtime lives.
fn local_step(input: StepInput) -> Result(StepOutput, error.ActivityError) {
  Ok(StepOutput(result: input.prompt <> " :: " <> input.context))
}

// ---------------------------------------------------------------------------
// Generated plumbing — written by `aion new`. You normally never edit this.
//
// `run` is the engine entry point named by `workflow.toml`. The runtime
// delivers the start input as a raw JSON string inside a `Dynamic`: decode
// it, parse it with the input codec, run the typed `handle`, and encode the
// success value back to a JSON string for the recorded result payload. The
// codecs mirror the JSON Schemas in `schemas/` and the worker's step
// input/output types.
// ---------------------------------------------------------------------------

pub fn run(raw_input: Dynamic) -> Result(String, AgentError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case input_codec().decode(raw_json) {
        Ok(input) ->
          case handle(input) {
            Ok(output) -> Ok(output_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(InvalidInput("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(InvalidInput("workflow input payload was not a string"))
  }
}

fn input_codec() -> codec.Codec(AgentInput) {
  codec.json_codec(agent_input_to_json, agent_input_decoder())
}

fn agent_input_to_json(input: AgentInput) -> json.Json {
  json.object([
    #("task_id", json.string(input.task_id)),
    #("scout_prompt", json.string(input.scout_prompt)),
    #("act_prompt", json.string(input.act_prompt)),
    #("verify_prompt", json.string(input.verify_prompt)),
    #("review_timeout_ms", json.int(input.review_timeout_ms)),
  ])
}

fn agent_input_decoder() -> decode.Decoder(AgentInput) {
  use task_id <- decode.field("task_id", decode.string)
  use scout_prompt <- decode.field("scout_prompt", decode.string)
  use act_prompt <- decode.field("act_prompt", decode.string)
  use verify_prompt <- decode.field("verify_prompt", decode.string)
  use review_timeout_ms <- decode.field("review_timeout_ms", decode.int)
  decode.success(AgentInput(
    task_id: task_id,
    scout_prompt: scout_prompt,
    act_prompt: act_prompt,
    verify_prompt: verify_prompt,
    review_timeout_ms: review_timeout_ms,
  ))
}

fn output_codec() -> codec.Codec(AgentResult) {
  codec.json_codec(agent_result_to_json, agent_result_decoder())
}

fn agent_result_to_json(agent_result: AgentResult) -> json.Json {
  json.object([
    #("task_id", json.string(agent_result.task_id)),
    #("disposition", json.string(agent_result.disposition)),
    #("scout_finding", json.string(agent_result.scout_finding)),
    #("act_artifact", json.string(agent_result.act_artifact)),
    #("verify_verdict", json.string(agent_result.verify_verdict)),
    #("reviewed_by", json.string(agent_result.reviewed_by)),
    #("reason", json.string(agent_result.reason)),
  ])
}

fn agent_result_decoder() -> decode.Decoder(AgentResult) {
  use task_id <- decode.field("task_id", decode.string)
  use disposition <- decode.field("disposition", decode.string)
  use scout_finding <- decode.field("scout_finding", decode.string)
  use act_artifact <- decode.field("act_artifact", decode.string)
  use verify_verdict <- decode.field("verify_verdict", decode.string)
  use reviewed_by <- decode.field("reviewed_by", decode.string)
  use reason <- decode.field("reason", decode.string)
  decode.success(AgentResult(
    task_id: task_id,
    disposition: disposition,
    scout_finding: scout_finding,
    act_artifact: act_artifact,
    verify_verdict: verify_verdict,
    reviewed_by: reviewed_by,
    reason: reason,
  ))
}

fn step_input_codec() -> codec.Codec(StepInput) {
  codec.json_codec(step_input_to_json, step_input_decoder())
}

fn step_input_to_json(input: StepInput) -> json.Json {
  json.object([
    #("task_id", json.string(input.task_id)),
    #("prompt", json.string(input.prompt)),
    #("context", json.string(input.context)),
  ])
}

fn step_input_decoder() -> decode.Decoder(StepInput) {
  use task_id <- decode.field("task_id", decode.string)
  use prompt <- decode.field("prompt", decode.string)
  use context <- decode.field("context", decode.string)
  decode.success(StepInput(task_id: task_id, prompt: prompt, context: context))
}

fn step_output_codec() -> codec.Codec(StepOutput) {
  codec.json_codec(step_output_to_json, step_output_decoder())
}

fn step_output_to_json(output: StepOutput) -> json.Json {
  json.object([#("result", json.string(output.result))])
}

fn step_output_decoder() -> decode.Decoder(StepOutput) {
  use result <- decode.field("result", decode.string)
  decode.success(StepOutput(result: result))
}

fn review_signal_codec() -> codec.Codec(ReviewSignal) {
  codec.json_codec(review_signal_to_json, review_signal_decoder())
}

fn review_signal_to_json(signal_value: ReviewSignal) -> json.Json {
  json.object([
    #("decision", decision_to_json(signal_value.decision)),
    #("reviewer", json.string(signal_value.reviewer)),
  ])
}

fn review_signal_decoder() -> decode.Decoder(ReviewSignal) {
  use decision <- decode.field("decision", decision_decoder())
  use reviewer <- decode.field("reviewer", decode.string)
  decode.success(ReviewSignal(decision: decision, reviewer: reviewer))
}

fn decision_to_json(decision: Decision) -> json.Json {
  case decision {
    Approve -> json.string("approve")
    Reject -> json.string("reject")
  }
}

fn decision_decoder() -> decode.Decoder(Decision) {
  decode.then(decode.string, fn(decision) {
    case decision {
      "approve" -> decode.success(Approve)
      "reject" -> decode.success(Reject)
      _ -> decode.failure(Reject, expected: "approve or reject")
    }
  })
}

fn agent_status_codec() -> codec.Codec(AgentStatus) {
  codec.json_codec(agent_status_to_json, agent_status_decoder())
}

fn agent_status_to_json(status: AgentStatus) -> json.Json {
  json.object([
    #("stage", json.string(status.stage)),
    #("task_id", json.string(status.task_id)),
  ])
}

fn agent_status_decoder() -> decode.Decoder(AgentStatus) {
  use stage <- decode.field("stage", decode.string)
  use task_id <- decode.field("task_id", decode.string)
  decode.success(AgentStatus(stage: stage, task_id: task_id))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "agent step result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}

fn receive_error_message(receive_error: error.ReceiveError) -> String {
  case receive_error {
    error.ReceiveDecodeFailed(_) -> "review signal payload could not be decoded"
    error.UnknownSignal(name: name) -> "unknown signal: " <> name
    error.ReceiveCancelled(error.Cancelled(reason: reason)) -> reason
    error.ReceiveNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ReceiveEngineFailure(message: message) -> message
  }
}

fn query_error_message(query_error: error.QueryError) -> String {
  case query_error {
    error.UnknownQuery(name: name) -> "unknown query: " <> name
    error.QueryDecodeFailed(_) -> "query reply could not be decoded"
    error.QueryTimedOut(error.TimedOut(message: message)) -> message
    error.QueryNotRunning(workflow_id: workflow_id) ->
      "query target not running: " <> workflow_id
    error.QueryHandlerFailed(message: message) -> message
    error.QueryEngineFailure(message) -> message
  }
}
