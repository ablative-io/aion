//// Human-in-the-loop approval gate with an approval signal and durable timeout.
////
//// The workflow submits a document for approval, waits for an
//// `approval_decision` signal, and uses a durable timeout to archive stale
//// requests. Completed workflow steps are recorded by Aion, so after an engine
//// restart replay resumes from the pending signal/timer wait without
//// re-running already completed activities.

import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option, None, Some}

pub type ApprovalInput {
  ApprovalInput(document_id: String, timeout_minutes: Int)
}

pub type ApprovalDecision {
  Approved
  Rejected
}

pub type ApprovalSignal {
  ApprovalSignal(decision: ApprovalDecision)
}

pub type ApprovalResult {
  ApprovalResult(decision: String, action_taken: String, reason: String)
}

type DocumentActivityInput {
  DocumentActivityInput(document_id: String, reason: String)
}

type DocumentActivityOutput {
  DocumentActivityOutput(action_taken: String)
}

pub type WorkflowError {
  ActivityFailed(message: String)
  SignalFailed(message: String)
  TimerFailed(message: String)
}

pub fn definition() ->
  workflow.WorkflowDefinition(ApprovalInput, ApprovalResult, WorkflowError) {
  workflow.define(
    "approval-gate",
    approval_input_codec(),
    approval_result_codec(),
    workflow_error_codec(),
    run,
  )
}

pub fn run(input: ApprovalInput) -> Result(ApprovalResult, WorkflowError) {
  let timeout = duration.minutes(input.timeout_minutes)
  let deadline_name = "approval-deadline-" <> input.document_id

  // Start a named durable timer so the deadline is visible in event history.
  // The current SDK races an awaited operation with a timer through
  // `workflow.with_timeout`; `workflow.race` is activity-only and cannot race a
  // signal receive against a timer reference directly.
  case workflow.start_timer(deadline_name, timeout) {
    Ok(deadline) -> wait_for_decision(input, timeout, Some(deadline))
    Error(engine_error) -> Error(TimerFailed(engine_error_message(engine_error)))
  }
}

fn wait_for_decision(
  input: ApprovalInput,
  timeout: duration.Duration,
  deadline: Option(workflow.TimerRef),
) -> Result(ApprovalResult, WorkflowError) {
  case
    workflow.with_timeout(fn() { workflow.receive(approval_decision_signal()) }, timeout)
  {
    Ok(ApprovalSignal(decision: Approved)) -> {
      cancel_deadline(deadline)
      publish_document(input.document_id)
    }
    Ok(ApprovalSignal(decision: Rejected)) -> {
      cancel_deadline(deadline)
      archive_document(
        input.document_id,
        "approval signal rejected the document",
        "rejected",
      )
    }
    Error(error.TimedOutError(error.TimedOut(message: _))) ->
      archive_document(
        input.document_id,
        "approval timed out before a signal arrived",
        "timed_out",
      )
    Error(error.InnerError(receive_error)) ->
      Error(SignalFailed(receive_error_message(receive_error)))
  }
}

fn cancel_deadline(deadline: Option(workflow.TimerRef)) -> Nil {
  case deadline {
    Some(timer_ref) -> {
      let _ = workflow.cancel_timer(timer_ref)
      Nil
    }
    None -> Nil
  }
}

fn publish_document(document_id: String) -> Result(ApprovalResult, WorkflowError) {
  case
    workflow.run(publish_activity(DocumentActivityInput(
      document_id: document_id,
      reason: "approval signal approved the document",
    )))
  {
    Ok(output) ->
      Ok(ApprovalResult(
        decision: "approved",
        action_taken: output.action_taken,
        reason: "approval signal approved the document",
      ))
    Error(activity_error) ->
      Error(ActivityFailed(activity_error_message(activity_error)))
  }
}

fn archive_document(
  document_id: String,
  reason: String,
  decision: String,
) -> Result(ApprovalResult, WorkflowError) {
  case
    workflow.run(archive_activity(DocumentActivityInput(
      document_id: document_id,
      reason: reason,
    )))
  {
    Ok(output) ->
      Ok(ApprovalResult(
        decision: decision,
        action_taken: output.action_taken,
        reason: reason,
      ))
    Error(activity_error) ->
      Error(ActivityFailed(activity_error_message(activity_error)))
  }
}

fn approval_decision_signal() -> workflow.SignalRef(ApprovalSignal) {
  signal.new("approval_decision", approval_signal_codec())
}

fn publish_activity(
  input: DocumentActivityInput,
) -> activity.Activity(DocumentActivityInput, DocumentActivityOutput) {
  activity.new(
    "publish_document",
    input,
    document_activity_input_codec(),
    document_activity_output_codec(),
    local_publish_document,
  )
}

fn archive_activity(
  input: DocumentActivityInput,
) -> activity.Activity(DocumentActivityInput, DocumentActivityOutput) {
  activity.new(
    "archive_document",
    input,
    document_activity_input_codec(),
    document_activity_output_codec(),
    local_archive_document,
  )
}

fn local_publish_document(
  input: DocumentActivityInput,
) -> Result(DocumentActivityOutput, error.ActivityError) {
  Ok(DocumentActivityOutput(action_taken: "published " <> input.document_id))
}

fn local_archive_document(
  input: DocumentActivityInput,
) -> Result(DocumentActivityOutput, error.ActivityError) {
  Ok(DocumentActivityOutput(
    action_taken: "archived " <> input.document_id <> " because " <> input.reason,
  ))
}

fn approval_input_codec() -> codec.Codec(ApprovalInput) {
  codec.json_codec(approval_input_to_json, approval_input_decoder())
}

fn approval_input_to_json(input: ApprovalInput) -> json.Json {
  json.object([
    #("document_id", json.string(input.document_id)),
    #("timeout_minutes", json.int(input.timeout_minutes)),
  ])
}

fn approval_input_decoder() -> decode.Decoder(ApprovalInput) {
  use document_id <- decode.field("document_id", decode.string)
  use timeout_minutes <- decode.field("timeout_minutes", decode.int)
  decode.success(ApprovalInput(
    document_id: document_id,
    timeout_minutes: timeout_minutes,
  ))
}

fn approval_signal_codec() -> codec.Codec(ApprovalSignal) {
  codec.json_codec(approval_signal_to_json, approval_signal_decoder())
}

fn approval_signal_to_json(signal: ApprovalSignal) -> json.Json {
  json.object([#("decision", approval_decision_to_json(signal.decision))])
}

fn approval_signal_decoder() -> decode.Decoder(ApprovalSignal) {
  use decision <- decode.field("decision", approval_decision_decoder())
  decode.success(ApprovalSignal(decision: decision))
}

fn approval_decision_to_json(decision: ApprovalDecision) -> json.Json {
  case decision {
    Approved -> json.string("approved")
    Rejected -> json.string("rejected")
  }
}

fn approval_decision_decoder() -> decode.Decoder(ApprovalDecision) {
  decode.then(decode.string, fn(decision) {
    case decision {
      "approved" -> decode.success(Approved)
      "rejected" -> decode.success(Rejected)
      _ -> decode.failure(Rejected, expected: "approved or rejected")
    }
  })
}

fn approval_result_codec() -> codec.Codec(ApprovalResult) {
  codec.json_codec(approval_result_to_json, approval_result_decoder())
}

fn approval_result_to_json(result: ApprovalResult) -> json.Json {
  json.object([
    #("decision", json.string(result.decision)),
    #("action_taken", json.string(result.action_taken)),
    #("reason", json.string(result.reason)),
  ])
}

fn approval_result_decoder() -> decode.Decoder(ApprovalResult) {
  use decision <- decode.field("decision", decode.string)
  use action_taken <- decode.field("action_taken", decode.string)
  use reason <- decode.field("reason", decode.string)
  decode.success(ApprovalResult(
    decision: decision,
    action_taken: action_taken,
    reason: reason,
  ))
}

fn document_activity_input_codec() -> codec.Codec(DocumentActivityInput) {
  codec.json_codec(document_activity_input_to_json, document_activity_input_decoder())
}

fn document_activity_input_to_json(input: DocumentActivityInput) -> json.Json {
  json.object([
    #("document_id", json.string(input.document_id)),
    #("reason", json.string(input.reason)),
  ])
}

fn document_activity_input_decoder() -> decode.Decoder(DocumentActivityInput) {
  use document_id <- decode.field("document_id", decode.string)
  use reason <- decode.field("reason", decode.string)
  decode.success(DocumentActivityInput(document_id: document_id, reason: reason))
}

fn document_activity_output_codec() -> codec.Codec(DocumentActivityOutput) {
  codec.json_codec(document_activity_output_to_json, document_activity_output_decoder())
}

fn document_activity_output_to_json(output: DocumentActivityOutput) -> json.Json {
  json.object([#("action_taken", json.string(output.action_taken))])
}

fn document_activity_output_decoder() -> decode.Decoder(DocumentActivityOutput) {
  use action_taken <- decode.field("action_taken", decode.string)
  decode.success(DocumentActivityOutput(action_taken: action_taken))
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
    SignalFailed(message) ->
      json.object([
        #("type", json.string("signal_failed")),
        #("message", json.string(message)),
      ])
    TimerFailed(message) ->
      json.object([
        #("type", json.string("timer_failed")),
        #("message", json.string(message)),
      ])
  }
}

fn workflow_error_decoder() -> decode.Decoder(WorkflowError) {
  use error_type <- decode.field("type", decode.string)
  use message <- decode.field("message", decode.string)
  case error_type {
    "signal_failed" -> decode.success(SignalFailed(message: message))
    "timer_failed" -> decode.success(TimerFailed(message: message))
    _ -> decode.success(ActivityFailed(message: message))
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

fn receive_error_message(receive_error: error.ReceiveError) -> String {
  case receive_error {
    error.ReceiveDecodeFailed(_) -> "approval signal payload could not be decoded"
    error.UnknownSignal(name: name) -> "unknown signal: " <> name
    error.ReceiveCancelled(error.Cancelled(reason: reason)) -> reason
    error.ReceiveNonDeterministic(error.NonDeterminismViolation(message: message)) ->
      message
    error.ReceiveEngineFailure(message: message) -> message
  }
}

fn engine_error_message(engine_error: error.EngineError) -> String {
  case engine_error {
    error.EngineFailure(message: message) -> message
  }
}
