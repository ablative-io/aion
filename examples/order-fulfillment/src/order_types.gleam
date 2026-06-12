//// Shared domain types and codecs for the order-fulfillment saga.
////
//// Both workflow entries (`order_fulfillment` parent, `order_shipping` child)
//// exchange these values across the type-erased engine boundary, so the
//// codecs live in one module and both sides decode exactly what the other
//// encoded.

import aion/codec
import gleam/dynamic/decode
import gleam/json
import gleam/option.{type Option, None, Some}

/// Workflow start input for one order.
pub type OrderInput {
  OrderInput(
    order_id: String,
    item: String,
    quantity: Int,
    amount_cents: Int,
    approval_timeout_ms: Int,
  )
}

/// Input for one `charge_payment` attempt. The attempt number is part of the
/// activity input so external activity implementations can behave
/// deterministically per attempt (the demo worker fails attempt 1).
pub type ChargeInput {
  ChargeInput(order_id: String, amount_cents: Int, attempt: Int)
}

/// Successful `charge_payment` result.
pub type PaymentReceipt {
  PaymentReceipt(order_id: String, payment_id: String, amount_cents: Int)
}

/// Input for the `refund_payment` compensation activity.
pub type RefundInput {
  RefundInput(order_id: String, payment_id: String, amount_cents: Int)
}

/// Successful `refund_payment` result.
pub type RefundReceipt {
  RefundReceipt(order_id: String, refund_id: String)
}

/// Human decision delivered by the `approval_decision` signal.
pub type Decision {
  Approve
  Reject
}

/// Payload of the `approval_decision` signal.
pub type Approval {
  Approval(decision: Decision, approver: String)
}

/// Start input for the `order_shipping` child workflow.
pub type ShippingInput {
  ShippingInput(order_id: String, item: String, quantity: Int)
}

/// Successful child workflow result.
pub type Shipment {
  Shipment(order_id: String, shipment_id: String, carrier: String)
}

/// Child workflow failure surfaced to the awaiting parent.
pub type ShippingError {
  ShippingFailed(message: String)
}

/// Reply value of the parent's `order_status` query.
///
/// `stage` walks: `received` -> `charging` -> `awaiting_approval` ->
/// `shipping` -> `completed`, or `compensating` -> `cancelled` on the
/// rejection/timeout path.
pub type OrderStatus {
  OrderStatus(
    stage: String,
    order_id: String,
    payment_attempts: Int,
    payment_id: Option(String),
  )
}

/// Terminal business outcome of the saga. A compensated order completes the
/// workflow with `status: "cancelled"` — compensation is a successful saga
/// outcome, not a workflow failure.
pub type OrderResult {
  OrderResult(
    order_id: String,
    status: String,
    payment_id: String,
    shipment_id: Option(String),
    refund_id: Option(String),
    reason: String,
  )
}

/// Workflow failure: an infrastructure or terminal activity fault the saga
/// could not absorb.
pub type OrderError {
  OrderFailed(stage: String, message: String)
}

pub fn order_input_codec() -> codec.Codec(OrderInput) {
  codec.json_codec(order_input_to_json, order_input_decoder())
}

fn order_input_to_json(input: OrderInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("item", json.string(input.item)),
    #("quantity", json.int(input.quantity)),
    #("amount_cents", json.int(input.amount_cents)),
    #("approval_timeout_ms", json.int(input.approval_timeout_ms)),
  ])
}

fn order_input_decoder() -> decode.Decoder(OrderInput) {
  use order_id <- decode.field("order_id", decode.string)
  use item <- decode.field("item", decode.string)
  use quantity <- decode.field("quantity", decode.int)
  use amount_cents <- decode.field("amount_cents", decode.int)
  use approval_timeout_ms <- decode.field("approval_timeout_ms", decode.int)
  decode.success(OrderInput(
    order_id: order_id,
    item: item,
    quantity: quantity,
    amount_cents: amount_cents,
    approval_timeout_ms: approval_timeout_ms,
  ))
}

pub fn charge_input_codec() -> codec.Codec(ChargeInput) {
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

pub fn payment_receipt_codec() -> codec.Codec(PaymentReceipt) {
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

pub fn refund_input_codec() -> codec.Codec(RefundInput) {
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

pub fn refund_receipt_codec() -> codec.Codec(RefundReceipt) {
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

pub fn approval_codec() -> codec.Codec(Approval) {
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

pub fn shipping_input_codec() -> codec.Codec(ShippingInput) {
  codec.json_codec(shipping_input_to_json, shipping_input_decoder())
}

fn shipping_input_to_json(input: ShippingInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("item", json.string(input.item)),
    #("quantity", json.int(input.quantity)),
  ])
}

fn shipping_input_decoder() -> decode.Decoder(ShippingInput) {
  use order_id <- decode.field("order_id", decode.string)
  use item <- decode.field("item", decode.string)
  use quantity <- decode.field("quantity", decode.int)
  decode.success(ShippingInput(
    order_id: order_id,
    item: item,
    quantity: quantity,
  ))
}

pub fn shipment_codec() -> codec.Codec(Shipment) {
  codec.json_codec(shipment_to_json, shipment_decoder())
}

fn shipment_to_json(shipment: Shipment) -> json.Json {
  json.object([
    #("order_id", json.string(shipment.order_id)),
    #("shipment_id", json.string(shipment.shipment_id)),
    #("carrier", json.string(shipment.carrier)),
  ])
}

fn shipment_decoder() -> decode.Decoder(Shipment) {
  use order_id <- decode.field("order_id", decode.string)
  use shipment_id <- decode.field("shipment_id", decode.string)
  use carrier <- decode.field("carrier", decode.string)
  decode.success(Shipment(
    order_id: order_id,
    shipment_id: shipment_id,
    carrier: carrier,
  ))
}

pub fn shipping_error_codec() -> codec.Codec(ShippingError) {
  codec.json_codec(shipping_error_to_json, shipping_error_decoder())
}

fn shipping_error_to_json(error: ShippingError) -> json.Json {
  let ShippingFailed(message) = error
  json.object([
    #("type", json.string("shipping_failed")),
    #("message", json.string(message)),
  ])
}

fn shipping_error_decoder() -> decode.Decoder(ShippingError) {
  use message <- decode.field("message", decode.string)
  decode.success(ShippingFailed(message: message))
}

pub fn order_status_codec() -> codec.Codec(OrderStatus) {
  codec.json_codec(order_status_to_json, order_status_decoder())
}

fn order_status_to_json(status: OrderStatus) -> json.Json {
  json.object([
    #("stage", json.string(status.stage)),
    #("order_id", json.string(status.order_id)),
    #("payment_attempts", json.int(status.payment_attempts)),
    #("payment_id", json.nullable(status.payment_id, json.string)),
  ])
}

fn order_status_decoder() -> decode.Decoder(OrderStatus) {
  use stage <- decode.field("stage", decode.string)
  use order_id <- decode.field("order_id", decode.string)
  use payment_attempts <- decode.field("payment_attempts", decode.int)
  use payment_id <- decode.field("payment_id", decode.optional(decode.string))
  decode.success(OrderStatus(
    stage: stage,
    order_id: order_id,
    payment_attempts: payment_attempts,
    payment_id: payment_id,
  ))
}

pub fn order_result_codec() -> codec.Codec(OrderResult) {
  codec.json_codec(order_result_to_json, order_result_decoder())
}

fn order_result_to_json(result: OrderResult) -> json.Json {
  json.object([
    #("order_id", json.string(result.order_id)),
    #("status", json.string(result.status)),
    #("payment_id", json.string(result.payment_id)),
    #("shipment_id", json.nullable(result.shipment_id, json.string)),
    #("refund_id", json.nullable(result.refund_id, json.string)),
    #("reason", json.string(result.reason)),
  ])
}

fn order_result_decoder() -> decode.Decoder(OrderResult) {
  use order_id <- decode.field("order_id", decode.string)
  use status <- decode.field("status", decode.string)
  use payment_id <- decode.field("payment_id", decode.string)
  use shipment_id <- decode.field("shipment_id", decode.optional(decode.string))
  use refund_id <- decode.field("refund_id", decode.optional(decode.string))
  use reason <- decode.field("reason", decode.string)
  decode.success(OrderResult(
    order_id: order_id,
    status: status,
    payment_id: payment_id,
    shipment_id: shipment_id,
    refund_id: refund_id,
    reason: reason,
  ))
}

pub fn order_error_codec() -> codec.Codec(OrderError) {
  codec.json_codec(order_error_to_json, order_error_decoder())
}

fn order_error_to_json(error: OrderError) -> json.Json {
  let OrderFailed(stage: stage, message: message) = error
  json.object([
    #("type", json.string("order_failed")),
    #("stage", json.string(stage)),
    #("message", json.string(message)),
  ])
}

fn order_error_decoder() -> decode.Decoder(OrderError) {
  use stage <- decode.field("stage", decode.string)
  use message <- decode.field("message", decode.string)
  decode.success(OrderFailed(stage: stage, message: message))
}

/// Demo value used by local stubs and the demo worker for `shipment_id`.
pub fn shipment_id_for(order_id: String) -> String {
  "ship-" <> order_id
}

/// Demo value used by local stubs and the demo worker for `payment_id`.
pub fn payment_id_for(order_id: String) -> String {
  "pay-" <> order_id
}

/// Demo value used by local stubs and the demo worker for `refund_id`.
pub fn refund_id_for(order_id: String) -> String {
  "re-" <> order_id
}

/// Helper kept here so both modules render `Option(String)` consistently in
/// human-readable detail strings.
pub fn option_text(value: Option(String)) -> String {
  case value {
    Some(text) -> text
    None -> "none"
  }
}
