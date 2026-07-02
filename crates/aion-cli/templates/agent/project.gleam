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
//// The boundary types are authored in `src/{{name}}_io.gleam`; their JSON
//// codecs (`src/{{name}}_codecs.gleam`) and the `schemas/*.json` artifacts
//// are generated from those types by `aion generate` (ADR-014, types-first).
//// Edit `handle` and the helpers above the generated-code marker; the raw
//// engine plumbing lives below it.

import {{name}}_codecs as codecs
import {{name}}_io as io
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
import gleam/result

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
pub fn handle(input: io.Input) -> Result(io.Output, AgentError) {
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
) -> Result(io.StepOutput, AgentError) {
  let step = io.StepInput(task_id: task_id, prompt: prompt, context: context)
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
  input: io.Input,
  scouted: io.StepOutput,
  acted: io.StepOutput,
  verified: io.StepOutput,
) -> Result(io.Output, AgentError) {
  case
    workflow.with_timeout(
      fn() { workflow.receive(review_signal()) },
      duration.milliseconds(input.review_timeout_ms),
    )
  {
    Ok(io.ReviewSignal(decision: io.Approve, reviewer: reviewer)) -> {
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
    Ok(io.ReviewSignal(decision: io.Reject, reviewer: reviewer)) -> {
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
  input: io.Input,
  scouted: io.StepOutput,
  acted: io.StepOutput,
  verified: io.StepOutput,
  disposition: String,
  reviewer: String,
  reason: String,
) -> io.Output {
  io.Output(
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
  let status = io.AgentStatus(stage: stage, task_id: task_id)
  case
    query.handler(status_query_name, codecs.agent_status_codec(), fn() {
      status
    })
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) -> Error(QueryFailed(query_error_message(query_error)))
  }
}

fn review_signal() -> workflow.SignalRef(io.ReviewSignal) {
  signal.new(review_signal_name, codecs.review_signal_codec())
}

fn step_activity(
  stage: String,
  step: io.StepInput,
) -> activity.Activity(io.StepInput, io.StepOutput) {
  activity.new(
    stage,
    step,
    codecs.step_input_codec(),
    codecs.step_output_codec(),
    local_step,
  )
}

/// Local stub used only by the `aion/testing` harness; a deployed workflow
/// always dispatches each step to a connected worker (`worker/`), which is
/// where the real agent runtime lives.
fn local_step(
  input: io.StepInput,
) -> Result(io.StepOutput, error.ActivityError) {
  Ok(io.StepOutput(result: input.prompt <> " :: " <> input.context))
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

// ---------------------------------------------------------------------------
// Generated plumbing — written by `aion new`. You normally never edit this.
//
// `run` is the engine entry point named by `workflow.toml`. The runtime
// delivers the start input as a raw JSON string inside a `Dynamic`: decode
// it, parse it with the generated input codec, run the typed `handle`, and
// encode the success value back to a JSON string for the recorded result
// payload. The codecs are generated from the types in `{{name}}_io`.
// ---------------------------------------------------------------------------

pub fn run(raw_input: Dynamic) -> Result(String, AgentError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.input_codec().decode(raw_json) {
        Ok(input) ->
          case handle(input) {
            Ok(output) -> Ok(codecs.output_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(InvalidInput("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(InvalidInput("workflow input payload was not a string"))
  }
}
