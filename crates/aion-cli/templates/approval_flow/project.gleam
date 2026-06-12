//// {{name}} — human-in-the-loop approval workflow (approval-flow template).
////
//// Races an `approval_decision` signal against a durable deadline with
//// `workflow.with_timeout`, answers a `status` query at every stage, and
//// completes with the decision. The wait is durable: the run suspends —
//// for seconds or months — and survives server restarts while it waits.
////
//// Edit `handle` and the helpers above the generated-code marker; the raw
//// engine plumbing and JSON codecs live below it.

import aion/codec
import aion/duration
import aion/error
import aion/query
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/result

pub type ApprovalInput {
  ApprovalInput(request_id: String, timeout_seconds: Int)
}

pub type Decision {
  Approved
  Rejected
}

pub type ApprovalSignal {
  ApprovalSignal(decision: Decision, approver: String)
}

pub type ApprovalOutput {
  ApprovalOutput(request_id: String, decision: String, decided_by: String)
}

pub type WorkflowError {
  InvalidInput(message: String)
  SignalFailed(message: String)
  QueryFailed(message: String)
  TimerFailed(message: String)
}

/// Name of the human-decision signal this workflow waits on.
pub const approval_signal_name = "approval_decision"

/// Name of the read-only query this workflow answers at every stage.
pub const status_query_name = "status"

/// Your typed workflow: wait for the approval decision or the deadline,
/// whichever comes first, and report both outcome paths.
pub fn handle(input: ApprovalInput) -> Result(ApprovalOutput, WorkflowError) {
  use _ <- result.try(set_status("awaiting_approval"))
  case
    workflow.with_timeout(
      fn() { workflow.receive(approval_signal()) },
      duration.seconds(input.timeout_seconds),
    )
  {
    Ok(ApprovalSignal(decision: Approved, approver: approver)) -> {
      use _ <- result.try(set_status("approved"))
      Ok(ApprovalOutput(
        request_id: input.request_id,
        decision: "approved",
        decided_by: approver,
      ))
    }
    Ok(ApprovalSignal(decision: Rejected, approver: approver)) -> {
      use _ <- result.try(set_status("rejected"))
      Ok(ApprovalOutput(
        request_id: input.request_id,
        decision: "rejected",
        decided_by: approver,
      ))
    }
    Error(error.TimedOutError(error.TimedOut(message: _))) -> {
      use _ <- result.try(set_status("timed_out"))
      Ok(ApprovalOutput(
        request_id: input.request_id,
        decision: "timed_out",
        decided_by: "",
      ))
    }
    Error(error.InnerError(receive_error)) ->
      Error(SignalFailed(receive_error_message(receive_error)))
    Error(error.TimeoutEngineFailure(message: message)) ->
      Error(TimerFailed(message))
  }
}

/// Re-register the `status` query handler with a fresh closure at each state
/// change. Queries are answered at yield points and never touch workflow
/// history, so a recovered run answers them after replay with no extra code.
fn set_status(stage: String) -> Result(Nil, WorkflowError) {
  case query.handler(status_query_name, status_codec(), fn() { stage }) {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) -> Error(QueryFailed(query_error_message(query_error)))
  }
}

fn approval_signal() -> workflow.SignalRef(ApprovalSignal) {
  signal.new(approval_signal_name, approval_signal_codec())
}

// ---------------------------------------------------------------------------
// Generated plumbing — written by `aion new`. You normally never edit this.
//
// `run` is the engine entry point named by `workflow.toml`. The runtime
// delivers the start input as a raw JSON string inside a `Dynamic`: decode
// it, parse it with the input codec, run the typed `handle`, and encode the
// success value back to a JSON string for the recorded result payload. The
// codecs mirror the JSON Schemas in `schemas/`.
// ---------------------------------------------------------------------------

pub fn run(raw_input: Dynamic) -> Result(String, WorkflowError) {
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

fn input_codec() -> codec.Codec(ApprovalInput) {
  codec.json_codec(approval_input_to_json, approval_input_decoder())
}

fn approval_input_to_json(input: ApprovalInput) -> json.Json {
  json.object([
    #("request_id", json.string(input.request_id)),
    #("timeout_seconds", json.int(input.timeout_seconds)),
  ])
}

fn approval_input_decoder() -> decode.Decoder(ApprovalInput) {
  use request_id <- decode.field("request_id", decode.string)
  use timeout_seconds <- decode.field("timeout_seconds", decode.int)
  decode.success(ApprovalInput(
    request_id: request_id,
    timeout_seconds: timeout_seconds,
  ))
}

fn output_codec() -> codec.Codec(ApprovalOutput) {
  codec.json_codec(approval_output_to_json, approval_output_decoder())
}

fn approval_output_to_json(output: ApprovalOutput) -> json.Json {
  json.object([
    #("request_id", json.string(output.request_id)),
    #("decision", json.string(output.decision)),
    #("decided_by", json.string(output.decided_by)),
  ])
}

fn approval_output_decoder() -> decode.Decoder(ApprovalOutput) {
  use request_id <- decode.field("request_id", decode.string)
  use decision <- decode.field("decision", decode.string)
  use decided_by <- decode.field("decided_by", decode.string)
  decode.success(ApprovalOutput(
    request_id: request_id,
    decision: decision,
    decided_by: decided_by,
  ))
}

fn approval_signal_codec() -> codec.Codec(ApprovalSignal) {
  codec.json_codec(approval_signal_to_json, approval_signal_decoder())
}

fn approval_signal_to_json(signal_value: ApprovalSignal) -> json.Json {
  json.object([
    #("decision", decision_to_json(signal_value.decision)),
    #("approver", json.string(signal_value.approver)),
  ])
}

fn approval_signal_decoder() -> decode.Decoder(ApprovalSignal) {
  use decision <- decode.field("decision", decision_decoder())
  use approver <- decode.field("approver", decode.string)
  decode.success(ApprovalSignal(decision: decision, approver: approver))
}

fn decision_to_json(decision: Decision) -> json.Json {
  case decision {
    Approved -> json.string("approved")
    Rejected -> json.string("rejected")
  }
}

fn decision_decoder() -> decode.Decoder(Decision) {
  decode.then(decode.string, fn(decision) {
    case decision {
      "approved" -> decode.success(Approved)
      "rejected" -> decode.success(Rejected)
      _ -> decode.failure(Rejected, expected: "approved or rejected")
    }
  })
}

fn status_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn receive_error_message(receive_error: error.ReceiveError) -> String {
  case receive_error {
    error.ReceiveDecodeFailed(_) ->
      "approval signal payload could not be decoded"
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
