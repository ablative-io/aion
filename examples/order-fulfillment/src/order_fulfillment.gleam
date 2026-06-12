//// Flagship order-fulfillment saga for Aion.
////
//// One coherent business flow exercising the engine end to end:
////
//// 1. `charge_payment` with bounded retries over a durable backoff sleep —
////    transient (`Retryable`) gateway failures are retried, terminal
////    failures are not.
//// 2. A human `approval_decision` signal raced against a durable deadline
////    with `workflow.with_timeout`.
//// 3. An `order_shipping` child workflow (own `[[workflow]]` entry) started
////    on approval and awaited for its recorded terminal.
//// 4. An `order_status` query answerable at every stage; the handler is
////    re-registered as the saga advances so each reply reflects live state.
//// 5. Saga compensation: rejection, approval timeout, and shipping failure
////    all refund the captured payment and complete the order as
////    `cancelled` — a compensated saga is a successful workflow run.
////
//// Every step is durably recorded: kill the engine mid-flight and replay
//// resumes from recorded history without re-running completed activities.
////
//// Note on retries: `charge_payment` carries an explicit `RetryPolicy` and
//// the engine records each failed attempt, but engine-side automatic
//// re-dispatch from that policy is not wired up yet (dispatch always stamps
//// attempt 1). This workflow therefore drives its own bounded retry loop —
//// each attempt is a fresh recorded dispatch carrying the attempt number in
//// the activity input — which is also the honest way to keep retry counts
//// replay-deterministic today.

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
import gleam/option.{type Option, None, Some}
import order_shipping
import order_types.{
  type Approval, type ChargeInput, type OrderError, type OrderInput,
  type OrderResult, type PaymentReceipt, type RefundInput, type RefundReceipt,
  Approval, Approve, ChargeInput, OrderFailed, OrderResult, OrderStatus,
  RefundInput, Reject, ShippingFailed, ShippingInput,
}

/// Bounded attempt budget for `charge_payment`. The same constant feeds the
/// declared `RetryPolicy` and the workflow-driven retry loop.
pub const max_payment_attempts = 3

/// Fixed durable backoff between payment attempts, in milliseconds.
pub const payment_backoff_ms = 50

/// Name of the human-decision signal this saga waits on.
pub const approval_signal_name = "approval_decision"

/// Name of the read-only status query this saga answers at every stage.
pub const status_query_name = "order_status"

pub fn definition() -> workflow.WorkflowDefinition(
  OrderInput,
  OrderResult,
  OrderError,
) {
  workflow.define(
    "order-fulfillment",
    order_types.order_input_codec(),
    order_types.order_result_codec(),
    order_types.order_error_codec(),
    execute,
  )
}

/// Engine entry point.
///
/// The runtime delivers the start input as a raw JSON string: decode it with
/// the input codec, run the typed workflow, and encode the success value back
/// to its JSON string for the recorded result payload.
pub fn run(raw_input: Dynamic) -> Result(String, OrderError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case order_types.order_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) -> Ok(order_types.order_result_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(OrderFailed(
            stage: "decode_input",
            message: "failed to decode workflow input: " <> reason,
          ))
      }
    Error(_) ->
      Error(OrderFailed(
        stage: "decode_input",
        message: "workflow input payload was not a string",
      ))
  }
}

pub fn execute(input: OrderInput) -> Result(OrderResult, OrderError) {
  use _ <- result_try(set_status("received", input.order_id, 0, None))
  use #(receipt, attempts) <- result_try(charge_with_retries(input, 1))
  use _ <- result_try(set_status(
    "awaiting_approval",
    input.order_id,
    attempts,
    Some(receipt.payment_id),
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
  use _ <- result_try(set_status("charging", input.order_id, attempt, None))
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
    Ok(Approval(decision: Approve, approver: approver)) ->
      ship(input, receipt, attempts, approver)
    Ok(Approval(decision: Reject, approver: approver)) ->
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

/// Start the `order_shipping` child workflow and await its terminal. A child
/// failure compensates the captured payment exactly like a rejection.
fn ship(
  input: OrderInput,
  receipt: PaymentReceipt,
  attempts: Int,
  approver: String,
) -> Result(OrderResult, OrderError) {
  use _ <- result_try(set_status(
    "shipping",
    input.order_id,
    attempts,
    Some(receipt.payment_id),
  ))
  case
    workflow.spawn_and_wait(
      order_shipping.workflow_type,
      order_shipping.execute,
      ShippingInput(
        order_id: input.order_id,
        item: input.item,
        quantity: input.quantity,
      ),
      order_types.shipping_input_codec(),
      order_types.shipment_codec(),
      order_types.shipping_error_codec(),
    )
  {
    Ok(shipment) -> {
      use _ <- result_try(set_status(
        "completed",
        input.order_id,
        attempts,
        Some(receipt.payment_id),
      ))
      Ok(OrderResult(
        order_id: input.order_id,
        status: "completed",
        payment_id: receipt.payment_id,
        shipment_id: Some(shipment.shipment_id),
        refund_id: None,
        reason: "approved by " <> approver,
      ))
    }
    Error(error.ChildWorkflowFailed(ShippingFailed(message: message))) ->
      compensate(input, receipt, attempts, "shipping failed: " <> message)
    Error(child_error) ->
      Error(OrderFailed(
        stage: "ship_order",
        message: child_error_message(child_error),
      ))
  }
}

/// Saga compensation: refund the captured payment and complete the order as
/// `cancelled`.
fn compensate(
  input: OrderInput,
  receipt: PaymentReceipt,
  attempts: Int,
  reason: String,
) -> Result(OrderResult, OrderError) {
  use _ <- result_try(set_status(
    "compensating",
    input.order_id,
    attempts,
    Some(receipt.payment_id),
  ))
  case workflow.run(refund_payment_activity(receipt)) {
    Ok(refund) -> {
      use _ <- result_try(set_status(
        "cancelled",
        input.order_id,
        attempts,
        Some(receipt.payment_id),
      ))
      Ok(OrderResult(
        order_id: input.order_id,
        status: "cancelled",
        payment_id: receipt.payment_id,
        shipment_id: None,
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
  payment_id: Option(String),
) -> Result(Nil, OrderError) {
  let status =
    OrderStatus(
      stage: stage,
      order_id: order_id,
      payment_attempts: payment_attempts,
      payment_id: payment_id,
    )
  case
    query.handler(status_query_name, order_types.order_status_codec(), fn() {
      status
    })
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
  signal.new(approval_signal_name, order_types.approval_codec())
}

fn charge_payment_activity(
  input: ChargeInput,
) -> activity.Activity(ChargeInput, PaymentReceipt) {
  activity.new(
    "charge_payment",
    input,
    order_types.charge_input_codec(),
    order_types.payment_receipt_codec(),
    local_charge_payment,
  )
  |> activity.retry(activity.RetryPolicy(
    max_attempts: max_payment_attempts,
    backoff: activity.Fixed(delay: duration.milliseconds(payment_backoff_ms)),
  ))
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
    order_types.refund_input_codec(),
    order_types.refund_receipt_codec(),
    local_refund_payment,
  )
}

/// Local stub used by the pure-Gleam test double. On a real server the
/// connected activity worker executes `charge_payment` (`worker/worker.py`
/// in this example) and fails attempt 1 with a retryable error to
/// demonstrate the retry loop.
fn local_charge_payment(
  input: ChargeInput,
) -> Result(PaymentReceipt, error.ActivityError) {
  Ok(order_types.PaymentReceipt(
    order_id: input.order_id,
    payment_id: order_types.payment_id_for(input.order_id),
    amount_cents: input.amount_cents,
  ))
}

/// Local stub used by the pure-Gleam test double; mirrored by
/// `worker/worker.py` on a real server.
fn local_refund_payment(
  input: RefundInput,
) -> Result(RefundReceipt, error.ActivityError) {
  Ok(order_types.RefundReceipt(
    order_id: input.order_id,
    refund_id: order_types.refund_id_for(input.order_id),
  ))
}

fn result_try(
  result: Result(value, OrderError),
  next: fn(value) -> Result(output, OrderError),
) -> Result(output, OrderError) {
  case result {
    Ok(value) -> next(value)
    Error(workflow_error) -> Error(workflow_error)
  }
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

fn child_error_message(
  child_error: error.ChildError(order_types.ShippingError),
) -> String {
  case child_error {
    error.ChildWorkflowFailed(ShippingFailed(message: message)) -> message
    error.ChildOutputDecodeFailed(_) ->
      "child shipment result could not be decoded"
    error.ChildErrorDecodeFailed(_) ->
      "child shipping error could not be decoded"
    error.ChildEngineFailure(message: message) -> message
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
