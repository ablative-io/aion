//// {{name}} — order saga (saga template).
////
//// One business flow exercising the engine end to end:
////
//// 1. A `charge_payment` activity, served by a remote worker, with a
////    workflow-driven bounded retry loop over a durable backoff sleep —
////    transient (`Retryable`) failures retry, terminal failures do not.
//// 2. A human `approval_decision` signal raced against a durable deadline
////    with `workflow.with_timeout`.
//// 3. An `order_status` query answerable at every stage; the handler is
////    re-registered as the saga advances so each reply reflects live state.
//// 4. Saga compensation: rejection and approval timeout both refund the
////    captured payment via `refund_payment` and complete the order as
////    `cancelled` — a compensated saga is a successful workflow run.
////
//// Every step is durably recorded: kill the server mid-flight and replay
//// resumes from recorded history without re-running completed activities.
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
import gleam/option.{type Option, None, Some}
import gleam/result

pub type OrderInput {
  OrderInput(order_id: String, amount_cents: Int, approval_timeout_ms: Int)
}

pub type ChargeInput {
  ChargeInput(order_id: String, amount_cents: Int, attempt: Int)
}

pub type PaymentReceipt {
  PaymentReceipt(order_id: String, payment_id: String, amount_cents: Int)
}

pub type RefundInput {
  RefundInput(order_id: String, payment_id: String, amount_cents: Int)
}

pub type RefundReceipt {
  RefundReceipt(order_id: String, refund_id: String)
}

pub type Decision {
  Approved
  Rejected
}

pub type Approval {
  Approval(decision: Decision, approver: String)
}

pub type OrderStatus {
  OrderStatus(stage: String, order_id: String, payment_attempts: Int)
}

pub type OrderResult {
  OrderResult(
    order_id: String,
    status: String,
    payment_id: String,
    refund_id: Option(String),
    reason: String,
  )
}

pub type OrderError {
  InvalidInput(message: String)
  OrderFailed(stage: String, message: String)
}

/// Bounded attempt budget for `charge_payment`.
pub const max_payment_attempts = 3

/// Fixed durable backoff between payment attempts, in milliseconds.
pub const payment_backoff_ms = 1000

/// Name of the human-decision signal this saga waits on.
pub const approval_signal_name = "approval_decision"

/// Name of the read-only status query this saga answers at every stage.
pub const status_query_name = "order_status"

/// Your typed workflow: charge with retries, race the approval decision
/// against its deadline, and compensate on rejection or timeout.
pub fn handle(input: OrderInput) -> Result(OrderResult, OrderError) {
  use _ <- result.try(set_status("received", input.order_id, 0))
  use #(receipt, attempts) <- result.try(charge_with_retries(input, 1))
  use _ <- result.try(set_status(
    "awaiting_approval",
    input.order_id,
    attempts,
  ))
  await_approval(input, receipt, attempts)
}

/// Dispatch `charge_payment`, retrying transient failures over a durable
/// backoff sleep until the attempt budget is spent. Each retry is a fresh
/// recorded activity dispatch; replay resolves every attempt from history.
fn charge_with_retries(
  input: OrderInput,
  attempt: Int,
) -> Result(#(PaymentReceipt, Int), OrderError) {
  use _ <- result.try(set_status("charging", input.order_id, attempt))
  let charge =
    ChargeInput(
      order_id: input.order_id,
      amount_cents: input.amount_cents,
      attempt: attempt,
    )
  case workflow.run(charge_payment_activity(charge)) {
    Ok(receipt) -> Ok(#(receipt, attempt))
    Error(error.Retryable(message: message, details: _))
      if attempt < max_payment_attempts
    ->
      case workflow.sleep(duration.milliseconds(payment_backoff_ms)) {
        Ok(Nil) -> charge_with_retries(input, attempt + 1)
        Error(error.EngineFailure(message: sleep_message)) ->
          Error(OrderFailed(
            stage: "payment_backoff",
            message: "backoff sleep failed after transient charge failure ("
              <> message
              <> "): "
              <> sleep_message,
          ))
      }
    Error(activity_error) ->
      Error(OrderFailed(
        stage: "charge_payment",
        message: "attempt "
          <> int.to_string(attempt)
          <> " of "
          <> int.to_string(max_payment_attempts)
          <> ": "
          <> activity_error_message(activity_error),
      ))
  }
}

/// Race the human decision against the durable approval deadline.
fn await_approval(
  input: OrderInput,
  receipt: PaymentReceipt,
  attempts: Int,
) -> Result(OrderResult, OrderError) {
  case
    workflow.with_timeout(
      fn() { workflow.receive(approval_signal()) },
      duration.milliseconds(input.approval_timeout_ms),
    )
  {
    Ok(Approval(decision: Approved, approver: approver)) -> {
      use _ <- result.try(set_status("completed", input.order_id, attempts))
      Ok(OrderResult(
        order_id: input.order_id,
        status: "completed",
        payment_id: receipt.payment_id,
        refund_id: None,
        reason: "approved by " <> approver,
      ))
    }
    Ok(Approval(decision: Rejected, approver: approver)) ->
      compensate(input, receipt, attempts, "rejected by " <> approver)
    Error(error.TimedOutError(error.TimedOut(message: _))) ->
      compensate(
        input,
        receipt,
        attempts,
        "approval timed out after "
          <> int.to_string(input.approval_timeout_ms)
          <> "ms",
      )
    Error(error.InnerError(receive_error)) ->
      Error(OrderFailed(
        stage: "await_approval",
        message: receive_error_message(receive_error),
      ))
    Error(error.TimeoutEngineFailure(message: message)) ->
      Error(OrderFailed(stage: "await_approval", message: message))
  }
}

/// Saga compensation: refund the captured payment and complete the order as
/// `cancelled`. A compensated saga is a successful workflow run.
fn compensate(
  input: OrderInput,
  receipt: PaymentReceipt,
  attempts: Int,
  reason: String,
) -> Result(OrderResult, OrderError) {
  use _ <- result.try(set_status("compensating", input.order_id, attempts))
  case workflow.run(refund_payment_activity(receipt)) {
    Ok(refund) -> {
      use _ <- result.try(set_status("cancelled", input.order_id, attempts))
      Ok(OrderResult(
        order_id: input.order_id,
        status: "cancelled",
        payment_id: receipt.payment_id,
        refund_id: Some(refund.refund_id),
        reason: reason,
      ))
    }
    Error(activity_error) ->
      Error(OrderFailed(
        stage: "refund_payment",
        message: "compensation failed ("
          <> reason
          <> "): "
          <> activity_error_message(activity_error),
      ))
  }
}

/// Re-register the `order_status` handler with the current saga state.
/// Registration is workflow code, so replay re-registers automatically and a
/// recovered workflow answers queries without extra author code.
fn set_status(
  stage: String,
  order_id: String,
  payment_attempts: Int,
) -> Result(Nil, OrderError) {
  let status =
    OrderStatus(
      stage: stage,
      order_id: order_id,
      payment_attempts: payment_attempts,
    )
  case
    query.handler(status_query_name, order_status_codec(), fn() { status })
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(OrderFailed(
        stage: "register_status",
        message: query_error_message(query_error),
      ))
  }
}

fn approval_signal() -> workflow.SignalRef(Approval) {
  signal.new(approval_signal_name, approval_codec())
}

fn charge_payment_activity(
  input: ChargeInput,
) -> activity.Activity(ChargeInput, PaymentReceipt) {
  activity.new(
    "charge_payment",
    input,
    charge_input_codec(),
    payment_receipt_codec(),
    local_charge_payment,
  )
}

fn refund_payment_activity(
  receipt: PaymentReceipt,
) -> activity.Activity(RefundInput, RefundReceipt) {
  activity.new(
    "refund_payment",
    RefundInput(
      order_id: receipt.order_id,
      payment_id: receipt.payment_id,
      amount_cents: receipt.amount_cents,
    ),
    refund_input_codec(),
    refund_receipt_codec(),
    local_refund_payment,
  )
}

/// Local stub used only by the `aion/testing` harness; a deployed workflow
/// always dispatches `charge_payment` to a connected worker (`worker/`).
fn local_charge_payment(
  input: ChargeInput,
) -> Result(PaymentReceipt, error.ActivityError) {
  Ok(PaymentReceipt(
    order_id: input.order_id,
    payment_id: "pay-" <> input.order_id,
    amount_cents: input.amount_cents,
  ))
}

/// Local stub used only by the `aion/testing` harness; mirrored by the
/// worker's `refund_payment` handler on a real server.
fn local_refund_payment(
  input: RefundInput,
) -> Result(RefundReceipt, error.ActivityError) {
  Ok(RefundReceipt(order_id: input.order_id, refund_id: "ref-" <> input.order_id))
}

// ---------------------------------------------------------------------------
// Generated plumbing — written by `aion new`. You normally never edit this.
//
// `run` is the engine entry point named by `workflow.toml`. The runtime
// delivers the start input as a raw JSON string inside a `Dynamic`: decode
// it, parse it with the input codec, run the typed `handle`, and encode the
// success value back to a JSON string for the recorded result payload. The
// codecs mirror the JSON Schemas in `schemas/` and the worker's activity
// input/output types.
// ---------------------------------------------------------------------------

pub fn run(raw_input: Dynamic) -> Result(String, OrderError) {
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

fn input_codec() -> codec.Codec(OrderInput) {
  codec.json_codec(order_input_to_json, order_input_decoder())
}

fn order_input_to_json(input: OrderInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("amount_cents", json.int(input.amount_cents)),
    #("approval_timeout_ms", json.int(input.approval_timeout_ms)),
  ])
}

fn order_input_decoder() -> decode.Decoder(OrderInput) {
  use order_id <- decode.field("order_id", decode.string)
  use amount_cents <- decode.field("amount_cents", decode.int)
  use approval_timeout_ms <- decode.field("approval_timeout_ms", decode.int)
  decode.success(OrderInput(
    order_id: order_id,
    amount_cents: amount_cents,
    approval_timeout_ms: approval_timeout_ms,
  ))
}

fn output_codec() -> codec.Codec(OrderResult) {
  codec.json_codec(order_result_to_json, order_result_decoder())
}

fn order_result_to_json(order_result: OrderResult) -> json.Json {
  json.object([
    #("order_id", json.string(order_result.order_id)),
    #("status", json.string(order_result.status)),
    #("payment_id", json.string(order_result.payment_id)),
    #("refund_id", json.nullable(order_result.refund_id, json.string)),
    #("reason", json.string(order_result.reason)),
  ])
}

fn order_result_decoder() -> decode.Decoder(OrderResult) {
  use order_id <- decode.field("order_id", decode.string)
  use status <- decode.field("status", decode.string)
  use payment_id <- decode.field("payment_id", decode.string)
  use refund_id <- decode.field("refund_id", decode.optional(decode.string))
  use reason <- decode.field("reason", decode.string)
  decode.success(OrderResult(
    order_id: order_id,
    status: status,
    payment_id: payment_id,
    refund_id: refund_id,
    reason: reason,
  ))
}

fn charge_input_codec() -> codec.Codec(ChargeInput) {
  codec.json_codec(charge_input_to_json, charge_input_decoder())
}

fn charge_input_to_json(input: ChargeInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("amount_cents", json.int(input.amount_cents)),
    #("attempt", json.int(input.attempt)),
  ])
}

fn charge_input_decoder() -> decode.Decoder(ChargeInput) {
  use order_id <- decode.field("order_id", decode.string)
  use amount_cents <- decode.field("amount_cents", decode.int)
  use attempt <- decode.field("attempt", decode.int)
  decode.success(ChargeInput(
    order_id: order_id,
    amount_cents: amount_cents,
    attempt: attempt,
  ))
}

fn payment_receipt_codec() -> codec.Codec(PaymentReceipt) {
  codec.json_codec(payment_receipt_to_json, payment_receipt_decoder())
}

fn payment_receipt_to_json(receipt: PaymentReceipt) -> json.Json {
  json.object([
    #("order_id", json.string(receipt.order_id)),
    #("payment_id", json.string(receipt.payment_id)),
    #("amount_cents", json.int(receipt.amount_cents)),
  ])
}

fn payment_receipt_decoder() -> decode.Decoder(PaymentReceipt) {
  use order_id <- decode.field("order_id", decode.string)
  use payment_id <- decode.field("payment_id", decode.string)
  use amount_cents <- decode.field("amount_cents", decode.int)
  decode.success(PaymentReceipt(
    order_id: order_id,
    payment_id: payment_id,
    amount_cents: amount_cents,
  ))
}

fn refund_input_codec() -> codec.Codec(RefundInput) {
  codec.json_codec(refund_input_to_json, refund_input_decoder())
}

fn refund_input_to_json(input: RefundInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("payment_id", json.string(input.payment_id)),
    #("amount_cents", json.int(input.amount_cents)),
  ])
}

fn refund_input_decoder() -> decode.Decoder(RefundInput) {
  use order_id <- decode.field("order_id", decode.string)
  use payment_id <- decode.field("payment_id", decode.string)
  use amount_cents <- decode.field("amount_cents", decode.int)
  decode.success(RefundInput(
    order_id: order_id,
    payment_id: payment_id,
    amount_cents: amount_cents,
  ))
}

fn refund_receipt_codec() -> codec.Codec(RefundReceipt) {
  codec.json_codec(refund_receipt_to_json, refund_receipt_decoder())
}

fn refund_receipt_to_json(receipt: RefundReceipt) -> json.Json {
  json.object([
    #("order_id", json.string(receipt.order_id)),
    #("refund_id", json.string(receipt.refund_id)),
  ])
}

fn refund_receipt_decoder() -> decode.Decoder(RefundReceipt) {
  use order_id <- decode.field("order_id", decode.string)
  use refund_id <- decode.field("refund_id", decode.string)
  decode.success(RefundReceipt(order_id: order_id, refund_id: refund_id))
}

fn approval_codec() -> codec.Codec(Approval) {
  codec.json_codec(approval_to_json, approval_decoder())
}

fn approval_to_json(approval: Approval) -> json.Json {
  json.object([
    #("decision", decision_to_json(approval.decision)),
    #("approver", json.string(approval.approver)),
  ])
}

fn approval_decoder() -> decode.Decoder(Approval) {
  use decision <- decode.field("decision", decision_decoder())
  use approver <- decode.field("approver", decode.string)
  decode.success(Approval(decision: decision, approver: approver))
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

fn order_status_codec() -> codec.Codec(OrderStatus) {
  codec.json_codec(order_status_to_json, order_status_decoder())
}

fn order_status_to_json(status: OrderStatus) -> json.Json {
  json.object([
    #("stage", json.string(status.stage)),
    #("order_id", json.string(status.order_id)),
    #("payment_attempts", json.int(status.payment_attempts)),
  ])
}

fn order_status_decoder() -> decode.Decoder(OrderStatus) {
  use stage <- decode.field("stage", decode.string)
  use order_id <- decode.field("order_id", decode.string)
  use payment_attempts <- decode.field("payment_attempts", decode.int)
  decode.success(OrderStatus(
    stage: stage,
    order_id: order_id,
    payment_attempts: payment_attempts,
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
