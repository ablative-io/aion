//// The assistant workflow: an operator-paced aion-authoring chat session.
////
//// Pure orchestration over two recorded activities:
////
////   assistant_provision -> [ assistant round; await operator ]*   [bounded]
////
//// Round one dispatches the `assistant` agent activity with the full
//// working contract plus the operator's objective. Every later round is the
//// operator's continuation message, dispatched into the SAME norn session
//// (the worker pins `{workflow_id}-assistant` + `--resume-if-exists`, so
//// repeated dispatches of the one activity type resume one conversation).
////
//// Between rounds the workflow parks on the ONE control signal,
//// `assistant_continue` — the engine's selective receive wakes on exactly
//// one signal name, so continue and end share the name and discriminate in
//// the payload: `{"message": "..."}` starts the next round, `{"end": true}`
//// finishes cleanly as `OperatorEnded`. A payload that decodes but carries
//// neither (or a blank message) is a no-op nudge: the session keeps
//// waiting. A payload that does not decode at all is ALSO tolerated as a
//// no-op — an operator typo must never kill a live session.
////
//// The session is bounded at `max_rounds` agent rounds; spending the budget
//// is the `RoundCapExhausted` disposition, never an error. Agent rounds
//// carry NO timeout by design: a wedged round is cancelled/intervened,
//// never timed out.
////
//// An `assistant_status` query answers `{phase, round}` live state; the
//// handler is re-registered at every stage transition, so replay re-arms it
//// automatically.
////
//// This module is the determinism boundary: it issues only recorded
//// activity dispatches and signal receives, and branches on their recorded
//// payloads. No wall clock, no entropy, no direct IO.

import aion/codec
import aion/error
import aion/query
import aion/signal
import aion/workflow
import assistant/activities
import assistant/prompts
import assistant_codecs as codecs
import assistant_io as io
import gleam/dynamic.{type Dynamic}
import gleam/option
import gleam/string

/// Name of the live `{phase, round}` status query this workflow answers.
pub const status_query_name = "assistant_status"

/// Name of the ONE control signal the session listens on. Payload:
/// `{"message": "..."}` continues, `{"end": true}` ends cleanly.
pub const continue_signal_name = "assistant_continue"

/// The session's agent-round budget. Operator-paced rounds make runaway
/// loops impossible, but the budget still bounds the recorded history of a
/// single session; a spent budget is an honest `RoundCapExhausted`
/// disposition and the operator starts a fresh session.
pub const max_rounds = 50

/// Typed definition binding the generated codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  io.Input,
  io.Output,
  io.AssistantError,
) {
  workflow.define(
    "assistant",
    validated_input_codec(),
    codecs.output_codec(),
    codecs.assistant_error_codec(),
    execute,
  )
}

/// The generated input codec with decoded-input validation layered on
/// decode: a blank `objective` is rejected at the boundary (there is nothing
/// to open the session with), surfacing through the same
/// `{"aion_error":"input_decode",...}` envelope as any other bad input.
/// `repo_path` may be empty — that is the documented "no repository, scratch
/// workspace" mode. Encoding is the generated encoder, untouched.
fn validated_input_codec() -> codec.Codec(io.Input) {
  let generated = codecs.input_codec()
  codec.Codec(encode: generated.encode, decode: fn(raw_json) {
    case generated.decode(raw_json) {
      Ok(input) ->
        case string.trim(input.objective) {
          "" ->
            Error(
              codec.DecodeError(
                reason: "objective must not be blank — state the session's opening ask",
                path: ["objective"],
              ),
            )
          _ -> Ok(input)
        }
      Error(decode_error) -> Error(decode_error)
    }
  })
}

/// Engine entry point.
///
/// The runtime delivers the start input as a raw JSON string;
/// `workflow.entrypoint` decodes it with the definition's input codec,
/// drives `execute`, and encodes the outcome back to JSON text. An
/// undecodable input records the SDK's documented
/// `{"aion_error":"input_decode",...}` envelope as the failure payload.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  workflow.entrypoint(definition(), raw_input)
}

/// Typed workflow body: provision the session workspace, then the bounded
/// operator-paced round loop.
pub fn execute(input: io.Input) -> Result(io.Output, io.AssistantError) {
  use _ <- result_try(set_status("provisioning", 0))
  use workspace <- result_try(run_provision(input))
  session_loop(workspace, 1, prompts.first_round(input, workspace))
}

/// One agent round, then — budget permitting — the operator wait. The first
/// entry is round 1 and `max_rounds >= 1`, so at least one round always runs
/// and the budget is never overrun.
fn session_loop(
  workspace: io.Workspace,
  round: Int,
  prompt: String,
) -> Result(io.Output, io.AssistantError) {
  use _ <- result_try(set_status("working", round))
  use reply <- result_try(run_assistant(prompt))
  case round >= max_rounds {
    True -> Ok(build_output(io.RoundCapExhausted, round, reply, workspace))
    False -> await_operator(workspace, round, reply)
  }
}

/// Park on the control signal until the operator continues or ends. A
/// decodable payload with a blank (or absent) message and no end flag, and
/// an undecodable payload alike, are no-op nudges: consume the occurrence
/// and keep waiting — an operator typo must never kill a live session.
fn await_operator(
  workspace: io.Workspace,
  round: Int,
  reply: String,
) -> Result(io.Output, io.AssistantError) {
  use _ <- result_try(set_status("awaiting_operator", round))
  case signal.receive(continue_signal()) {
    Ok(continuation) ->
      case option.unwrap(continuation.end, False) {
        True -> Ok(build_output(io.OperatorEnded, round, reply, workspace))
        False ->
          case string.trim(option.unwrap(continuation.message, "")) {
            "" -> await_operator(workspace, round, reply)
            message ->
              session_loop(workspace, round + 1, prompts.continuation(message))
          }
      }
    Error(error.ReceiveDecodeFailed(_)) ->
      await_operator(workspace, round, reply)
    Error(receive_error) ->
      Error(io.AssistantError(
        stage: "await_operator",
        message: receive_error_message(receive_error),
      ))
  }
}

/// The typed control-signal reference — the generated `Continuation` codec
/// carries both optional fields (`message`, `end`).
fn continue_signal() -> signal.SignalRef(io.Continuation) {
  signal.new(continue_signal_name, codecs.continuation_codec())
}

// --- activity dispatches -----------------------------------------------------

fn run_provision(input: io.Input) -> Result(io.Workspace, io.AssistantError) {
  // The workflow execution's unique id (recorded in WorkflowStarted, stable
  // across replay) keys the worker-side workspace at `<root>/<run_id>/repo`.
  // It is the SAME id the agent-harness workspace-root template
  // `{workflow_id}` expands, so the provisioned workspace and the agent
  // session land in the same per-run directory (the #175 pattern).
  use run_id <- result_try(case workflow.id() {
    Ok(run_id) -> Ok(run_id)
    Error(error.EngineFailure(message: message)) ->
      Error(io.AssistantError(
        stage: "provision",
        message: "workflow id unavailable — cannot key the worker-side workspace directory: "
          <> message,
      ))
  })
  case
    workflow.run(
      activities.provision(io.ProvisionInput(
        repo_path: input.repo_path,
        run_id: run_id,
      )),
    )
  {
    Ok(workspace) -> Ok(workspace)
    Error(activity_error) -> stage_error("provision", activity_error)
  }
}

/// Dispatch one assistant round: prompt in, terminal text out.
fn run_assistant(prompt: String) -> Result(String, io.AssistantError) {
  case workflow.run(activities.assistant(prompt)) {
    Ok(reply) -> Ok(reply)
    Error(activity_error) -> stage_error("assistant", activity_error)
  }
}

// --- status query ------------------------------------------------------------

/// Re-register the status handler with the current phase and round, so
/// `assistant_status` queries answer live state at every yield point
/// (re-registration per stage, per docs/guides/workflows.md).
fn set_status(phase: String, round: Int) -> Result(Nil, io.AssistantError) {
  let status = io.AssistantStatus(phase: phase, round: round)
  case
    query.handler(status_query_name, codecs.assistant_status_codec(), fn() {
      status
    })
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(io.AssistantError(
        stage: "register_status",
        message: query_error_message(query_error),
      ))
  }
}

// --- helpers -----------------------------------------------------------------

fn build_output(
  disposition: io.Disposition,
  rounds: Int,
  last_reply: String,
  workspace: io.Workspace,
) -> io.Output {
  io.Output(
    disposition: disposition,
    rounds: rounds,
    last_reply: last_reply,
    workspace_path: workspace.path,
  )
}

fn stage_error(
  stage: String,
  activity_error: error.ActivityError,
) -> Result(value, io.AssistantError) {
  Error(io.AssistantError(
    stage: stage,
    message: activity_error_message(activity_error),
  ))
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

fn receive_error_message(receive_error: error.ReceiveError) -> String {
  case receive_error {
    error.UnknownSignal(name: name) -> "unknown signal: " <> name
    error.ReceiveDecodeFailed(_) -> "signal payload could not be decoded"
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

fn result_try(
  result: Result(value, io.AssistantError),
  next: fn(value) -> Result(output, io.AssistantError),
) -> Result(output, io.AssistantError) {
  case result {
    Ok(value) -> next(value)
    Error(workflow_error) -> Error(workflow_error)
  }
}
