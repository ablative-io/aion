//// Durable order fulfillment saga workflow.
////
//// The workflow accepts an order, performs payment, inventory, shipping, and
//// confirmation activities in sequence, and compensates completed work in
//// reverse order if a later step fails.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic/decode
import gleam/json

pub type OrderInput {
  OrderInput(order_id: String, item: String, quantity: Int, amount: Int)
}

pub type OrderConfirmation {
  OrderConfirmation(order_id: String, shipment_id: String, confirmation_id: String)
}

pub type SagaFailed {
  SagaFailed(
    failed_step: String,
    reason: String,
    completed_steps: List(String),
    compensations: List(CompensationResult),
  )
}

pub type CompensationResult {
  CompensationResult(step: String, status: String, detail: String)
}

pub type PaymentReceipt {
  PaymentReceipt(order_id: String, payment_id: String, amount: Int)
}

pub type InventoryReservation {
  InventoryReservation(order_id: String, reservation_id: String, item: String, quantity: Int)
}

pub type Shipment {
  Shipment(order_id: String, shipment_id: String)
}

pub type ConfirmationRequest {
  ConfirmationRequest(order_id: String, shipment_id: String)
}

pub type ReleaseInventoryInput {
  ReleaseInventoryInput(order_id: String, reservation_id: String, item: String, quantity: Int)
}

pub type RefundPaymentInput {
  RefundPaymentInput(order_id: String, payment_id: String, amount: Int)
}

pub type CompensationOutput {
  CompensationOutput(status: String, detail: String)
}

pub fn definition() -> workflow.WorkflowDefinition(OrderInput, OrderConfirmation, SagaFailed) {
  workflow.define(
    "order-saga",
    order_input_codec(),
    order_confirmation_codec(),
    saga_failed_codec(),
    run,
  )
}

pub fn run(input: OrderInput) -> Result(OrderConfirmation, SagaFailed) {
  case workflow.run(charge_payment_activity(input)) {
    Ok(payment) -> reserve_inventory(input, payment)
    Error(activity_error) ->
      Error(SagaFailed(
        failed_step: "charge_payment",
        reason: activity_error_message(activity_error),
        completed_steps: [],
        compensations: [],
      ))
  }
}

fn reserve_inventory(
  input: OrderInput,
  payment: PaymentReceipt,
) -> Result(OrderConfirmation, SagaFailed) {
  case workflow.run(reserve_inventory_activity(input)) {
    Ok(reservation) -> ship_order(input, payment, reservation)
    Error(activity_error) -> {
      let refund = refund_payment(payment)
      Error(SagaFailed(
        failed_step: "reserve_inventory",
        reason: activity_error_message(activity_error),
        completed_steps: ["charge_payment"],
        compensations: [refund],
      ))
    }
  }
}

fn ship_order(
  input: OrderInput,
  payment: PaymentReceipt,
  reservation: InventoryReservation,
) -> Result(OrderConfirmation, SagaFailed) {
  case workflow.run(ship_order_activity(input)) {
    Ok(shipment) -> confirm_order(input, payment, reservation, shipment)
    Error(activity_error) -> {
      let release = release_inventory(reservation)
      let refund = refund_payment(payment)
      Error(SagaFailed(
        failed_step: "ship_order",
        reason: activity_error_message(activity_error),
        completed_steps: ["charge_payment", "reserve_inventory"],
        compensations: [release, refund],
      ))
    }
  }
}

fn confirm_order(
  input: OrderInput,
  payment: PaymentReceipt,
  reservation: InventoryReservation,
  shipment: Shipment,
) -> Result(OrderConfirmation, SagaFailed) {
  let _payment = payment
  let _reservation = reservation

  case workflow.run(confirm_order_activity(ConfirmationRequest(
    order_id: input.order_id,
    shipment_id: shipment.shipment_id,
  ))) {
    Ok(confirmation) -> Ok(confirmation)
    Error(activity_error) -> {
      let release = release_inventory(reservation)
      let refund = refund_payment(payment)
      Error(SagaFailed(
        failed_step: "confirm_order",
        reason: activity_error_message(activity_error),
        completed_steps: ["charge_payment", "reserve_inventory", "ship_order"],
        compensations: [release, refund],
      ))
    }
  }
}

fn release_inventory(reservation: InventoryReservation) -> CompensationResult {
  let input = ReleaseInventoryInput(
    order_id: reservation.order_id,
    reservation_id: reservation.reservation_id,
    item: reservation.item,
    quantity: reservation.quantity,
  )

  case workflow.run(release_inventory_activity(input)) {
    Ok(output) -> CompensationResult(
      step: "release_inventory",
      status: output.status,
      detail: output.detail,
    )
    Error(activity_error) -> CompensationResult(
      step: "release_inventory",
      status: "failed",
      detail: activity_error_message(activity_error),
    )
  }
}

fn refund_payment(payment: PaymentReceipt) -> CompensationResult {
  let input = RefundPaymentInput(
    order_id: payment.order_id,
    payment_id: payment.payment_id,
    amount: payment.amount,
  )

  case workflow.run(refund_payment_activity(input)) {
    Ok(output) -> CompensationResult(
      step: "refund_payment",
      status: output.status,
      detail: output.detail,
    )
    Error(activity_error) -> CompensationResult(
      step: "refund_payment",
      status: "failed",
      detail: activity_error_message(activity_error),
    )
  }
}

fn charge_payment_activity(input: OrderInput) -> activity.Activity(OrderInput, PaymentReceipt) {
  activity.new(
    "charge_payment",
    input,
    order_input_codec(),
    payment_receipt_codec(),
    local_charge_payment,
  )
}

fn reserve_inventory_activity(
  input: OrderInput,
) -> activity.Activity(OrderInput, InventoryReservation) {
  activity.new(
    "reserve_inventory",
    input,
    order_input_codec(),
    inventory_reservation_codec(),
    local_reserve_inventory,
  )
}

fn ship_order_activity(input: OrderInput) -> activity.Activity(OrderInput, Shipment) {
  activity.new(
    "ship_order",
    input,
    order_input_codec(),
    shipment_codec(),
    local_ship_order,
  )
}

fn confirm_order_activity(
  input: ConfirmationRequest,
) -> activity.Activity(ConfirmationRequest, OrderConfirmation) {
  activity.new(
    "confirm_order",
    input,
    confirmation_request_codec(),
    order_confirmation_codec(),
    local_confirm_order,
  )
}

fn release_inventory_activity(
  input: ReleaseInventoryInput,
) -> activity.Activity(ReleaseInventoryInput, CompensationOutput) {
  activity.new(
    "release_inventory",
    input,
    release_inventory_input_codec(),
    compensation_output_codec(),
    local_release_inventory,
  )
}

fn refund_payment_activity(
  input: RefundPaymentInput,
) -> activity.Activity(RefundPaymentInput, CompensationOutput) {
  activity.new(
    "refund_payment",
    input,
    refund_payment_input_codec(),
    compensation_output_codec(),
    local_refund_payment,
  )
}

fn local_charge_payment(input: OrderInput) -> Result(PaymentReceipt, error.ActivityError) {
  Ok(PaymentReceipt(
    order_id: input.order_id,
    payment_id: "pay-" <> input.order_id,
    amount: input.amount,
  ))
}

fn local_reserve_inventory(
  input: OrderInput,
) -> Result(InventoryReservation, error.ActivityError) {
  Ok(InventoryReservation(
    order_id: input.order_id,
    reservation_id: "res-" <> input.order_id,
    item: input.item,
    quantity: input.quantity,
  ))
}

fn local_ship_order(input: OrderInput) -> Result(Shipment, error.ActivityError) {
  Ok(Shipment(order_id: input.order_id, shipment_id: "ship-" <> input.order_id))
}

fn local_confirm_order(
  input: ConfirmationRequest,
) -> Result(OrderConfirmation, error.ActivityError) {
  Ok(OrderConfirmation(
    order_id: input.order_id,
    shipment_id: input.shipment_id,
    confirmation_id: "conf-" <> input.order_id,
  ))
}

fn local_release_inventory(
  input: ReleaseInventoryInput,
) -> Result(CompensationOutput, error.ActivityError) {
  Ok(CompensationOutput(
    status: "released",
    detail: "released " <> input.reservation_id,
  ))
}

fn local_refund_payment(
  input: RefundPaymentInput,
) -> Result(CompensationOutput, error.ActivityError) {
  Ok(CompensationOutput(status: "refunded", detail: "refunded " <> input.payment_id))
}

fn order_input_codec() -> codec.Codec(OrderInput) {
  codec.json_codec(order_input_to_json, order_input_decoder())
}

fn order_input_to_json(input: OrderInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("item", json.string(input.item)),
    #("quantity", json.int(input.quantity)),
    #("amount", json.int(input.amount)),
  ])
}

fn order_input_decoder() -> decode.Decoder(OrderInput) {
  use order_id <- decode.field("order_id", decode.string)
  use item <- decode.field("item", decode.string)
  use quantity <- decode.field("quantity", decode.int)
  use amount <- decode.field("amount", decode.int)
  decode.success(OrderInput(
    order_id: order_id,
    item: item,
    quantity: quantity,
    amount: amount,
  ))
}

fn payment_receipt_codec() -> codec.Codec(PaymentReceipt) {
  codec.json_codec(payment_receipt_to_json, payment_receipt_decoder())
}

fn payment_receipt_to_json(receipt: PaymentReceipt) -> json.Json {
  json.object([
    #("order_id", json.string(receipt.order_id)),
    #("payment_id", json.string(receipt.payment_id)),
    #("amount", json.int(receipt.amount)),
  ])
}

fn payment_receipt_decoder() -> decode.Decoder(PaymentReceipt) {
  use order_id <- decode.field("order_id", decode.string)
  use payment_id <- decode.field("payment_id", decode.string)
  use amount <- decode.field("amount", decode.int)
  decode.success(PaymentReceipt(
    order_id: order_id,
    payment_id: payment_id,
    amount: amount,
  ))
}

fn inventory_reservation_codec() -> codec.Codec(InventoryReservation) {
  codec.json_codec(inventory_reservation_to_json, inventory_reservation_decoder())
}

fn inventory_reservation_to_json(reservation: InventoryReservation) -> json.Json {
  json.object([
    #("order_id", json.string(reservation.order_id)),
    #("reservation_id", json.string(reservation.reservation_id)),
    #("item", json.string(reservation.item)),
    #("quantity", json.int(reservation.quantity)),
  ])
}

fn inventory_reservation_decoder() -> decode.Decoder(InventoryReservation) {
  use order_id <- decode.field("order_id", decode.string)
  use reservation_id <- decode.field("reservation_id", decode.string)
  use item <- decode.field("item", decode.string)
  use quantity <- decode.field("quantity", decode.int)
  decode.success(InventoryReservation(
    order_id: order_id,
    reservation_id: reservation_id,
    item: item,
    quantity: quantity,
  ))
}

fn shipment_codec() -> codec.Codec(Shipment) {
  codec.json_codec(shipment_to_json, shipment_decoder())
}

fn shipment_to_json(shipment: Shipment) -> json.Json {
  json.object([
    #("order_id", json.string(shipment.order_id)),
    #("shipment_id", json.string(shipment.shipment_id)),
  ])
}

fn shipment_decoder() -> decode.Decoder(Shipment) {
  use order_id <- decode.field("order_id", decode.string)
  use shipment_id <- decode.field("shipment_id", decode.string)
  decode.success(Shipment(order_id: order_id, shipment_id: shipment_id))
}

fn confirmation_request_codec() -> codec.Codec(ConfirmationRequest) {
  codec.json_codec(confirmation_request_to_json, confirmation_request_decoder())
}

fn confirmation_request_to_json(request: ConfirmationRequest) -> json.Json {
  json.object([
    #("order_id", json.string(request.order_id)),
    #("shipment_id", json.string(request.shipment_id)),
  ])
}

fn confirmation_request_decoder() -> decode.Decoder(ConfirmationRequest) {
  use order_id <- decode.field("order_id", decode.string)
  use shipment_id <- decode.field("shipment_id", decode.string)
  decode.success(ConfirmationRequest(order_id: order_id, shipment_id: shipment_id))
}

fn order_confirmation_codec() -> codec.Codec(OrderConfirmation) {
  codec.json_codec(order_confirmation_to_json, order_confirmation_decoder())
}

fn order_confirmation_to_json(confirmation: OrderConfirmation) -> json.Json {
  json.object([
    #("order_id", json.string(confirmation.order_id)),
    #("shipment_id", json.string(confirmation.shipment_id)),
    #("confirmation_id", json.string(confirmation.confirmation_id)),
  ])
}

fn order_confirmation_decoder() -> decode.Decoder(OrderConfirmation) {
  use order_id <- decode.field("order_id", decode.string)
  use shipment_id <- decode.field("shipment_id", decode.string)
  use confirmation_id <- decode.field("confirmation_id", decode.string)
  decode.success(OrderConfirmation(
    order_id: order_id,
    shipment_id: shipment_id,
    confirmation_id: confirmation_id,
  ))
}

fn release_inventory_input_codec() -> codec.Codec(ReleaseInventoryInput) {
  codec.json_codec(release_inventory_input_to_json, release_inventory_input_decoder())
}

fn release_inventory_input_to_json(input: ReleaseInventoryInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("reservation_id", json.string(input.reservation_id)),
    #("item", json.string(input.item)),
    #("quantity", json.int(input.quantity)),
  ])
}

fn release_inventory_input_decoder() -> decode.Decoder(ReleaseInventoryInput) {
  use order_id <- decode.field("order_id", decode.string)
  use reservation_id <- decode.field("reservation_id", decode.string)
  use item <- decode.field("item", decode.string)
  use quantity <- decode.field("quantity", decode.int)
  decode.success(ReleaseInventoryInput(
    order_id: order_id,
    reservation_id: reservation_id,
    item: item,
    quantity: quantity,
  ))
}

fn refund_payment_input_codec() -> codec.Codec(RefundPaymentInput) {
  codec.json_codec(refund_payment_input_to_json, refund_payment_input_decoder())
}

fn refund_payment_input_to_json(input: RefundPaymentInput) -> json.Json {
  json.object([
    #("order_id", json.string(input.order_id)),
    #("payment_id", json.string(input.payment_id)),
    #("amount", json.int(input.amount)),
  ])
}

fn refund_payment_input_decoder() -> decode.Decoder(RefundPaymentInput) {
  use order_id <- decode.field("order_id", decode.string)
  use payment_id <- decode.field("payment_id", decode.string)
  use amount <- decode.field("amount", decode.int)
  decode.success(RefundPaymentInput(
    order_id: order_id,
    payment_id: payment_id,
    amount: amount,
  ))
}

fn compensation_output_codec() -> codec.Codec(CompensationOutput) {
  codec.json_codec(compensation_output_to_json, compensation_output_decoder())
}

fn compensation_output_to_json(output: CompensationOutput) -> json.Json {
  json.object([
    #("status", json.string(output.status)),
    #("detail", json.string(output.detail)),
  ])
}

fn compensation_output_decoder() -> decode.Decoder(CompensationOutput) {
  use status <- decode.field("status", decode.string)
  use detail <- decode.field("detail", decode.string)
  decode.success(CompensationOutput(status: status, detail: detail))
}

fn compensation_result_to_json(result: CompensationResult) -> json.Json {
  json.object([
    #("step", json.string(result.step)),
    #("status", json.string(result.status)),
    #("detail", json.string(result.detail)),
  ])
}

fn compensation_result_decoder() -> decode.Decoder(CompensationResult) {
  use step <- decode.field("step", decode.string)
  use status <- decode.field("status", decode.string)
  use detail <- decode.field("detail", decode.string)
  decode.success(CompensationResult(step: step, status: status, detail: detail))
}

fn saga_failed_codec() -> codec.Codec(SagaFailed) {
  codec.json_codec(saga_failed_to_json, saga_failed_decoder())
}

fn saga_failed_to_json(error: SagaFailed) -> json.Json {
  json.object([
    #("type", json.string("saga_failed")),
    #("failed_step", json.string(error.failed_step)),
    #("reason", json.string(error.reason)),
    #("completed_steps", json.array(error.completed_steps, json.string)),
    #(
      "compensations",
      json.array(error.compensations, compensation_result_to_json),
    ),
  ])
}

fn saga_failed_decoder() -> decode.Decoder(SagaFailed) {
  use failed_step <- decode.field("failed_step", decode.string)
  use reason <- decode.field("reason", decode.string)
  use completed_steps <- decode.field("completed_steps", decode.list(decode.string))
  use compensations <- decode.field(
    "compensations",
    decode.list(compensation_result_decoder()),
  )
  decode.success(SagaFailed(
    failed_step: failed_step,
    reason: reason,
    completed_steps: completed_steps,
    compensations: compensations,
  ))
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(_) -> "activity timed out"
    error.ActivityCancelled(_) -> "activity was cancelled"
    error.ActivityNonDeterministic(_) -> "activity was non-deterministic"
    error.ActivityEngineFailure(message:) -> message
  }
}
