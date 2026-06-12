//// Shipping child workflow for the order-fulfillment saga.
////
//// The engine resolves a spawned child's workflow type against its loaded
//// packages by entry module name, exactly the way `start` resolves a
//// top-level workflow type. This module is therefore its own `[[workflow]]`
//// entry in `workflow.toml`: the parent spawns children of the type named by
//// `workflow_type` below, and the `order-shipping.aion` archive must be
//// loaded into the engine alongside `order-fulfillment.aion`.

import aion/activity
import aion/codec
import aion/error
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import order_types.{
  type Shipment, type ShippingError, type ShippingInput, ShippingFailed,
}

/// The child workflow type the parent passes to `workflow.spawn_and_wait`.
/// A deployed workflow type is its entry module name, so this is exactly
/// this module's name.
pub const workflow_type = "order_shipping"

pub fn definition() -> workflow.WorkflowDefinition(
  ShippingInput,
  Shipment,
  ShippingError,
) {
  workflow.define(
    "order-shipping",
    order_types.shipping_input_codec(),
    order_types.shipment_codec(),
    order_types.shipping_error_codec(),
    execute,
  )
}

/// Engine entry point for one child execution.
///
/// The runtime delivers the start input as a raw JSON string. Success and
/// failure are both encoded back to JSON text here: the engine records these
/// exact payloads as the child terminal, and the awaiting parent decodes them
/// with the same codecs `order_types` exports.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case order_types.shipping_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(shipment) -> Ok(order_types.shipment_codec().encode(shipment))
            Error(shipping_error) ->
              Error(order_types.shipping_error_codec().encode(shipping_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            order_types.shipping_error_codec().encode(ShippingFailed(
              "failed to decode shipping input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        order_types.shipping_error_codec().encode(ShippingFailed(
          "shipping input payload was not a string",
        )),
      )
  }
}

/// Dispatch the `ship_order` activity and return the recorded shipment.
pub fn execute(input: ShippingInput) -> Result(Shipment, ShippingError) {
  case workflow.run(ship_order_activity(input)) {
    Ok(shipment) -> Ok(shipment)
    Error(activity_error) ->
      Error(ShippingFailed(activity_error_message(activity_error)))
  }
}

fn ship_order_activity(
  input: ShippingInput,
) -> activity.Activity(ShippingInput, Shipment) {
  activity.new(
    "ship_order",
    input,
    order_types.shipping_input_codec(),
    order_types.shipment_codec(),
    local_ship_order,
  )
}

/// Local stub used by the pure-Gleam test double. On a real server the
/// connected activity worker executes `ship_order` (`worker/worker.py` in
/// this example) and must mirror this deterministic contract.
fn local_ship_order(
  input: ShippingInput,
) -> Result(Shipment, error.ActivityError) {
  Ok(order_types.Shipment(
    order_id: input.order_id,
    shipment_id: order_types.shipment_id_for(input.order_id),
    carrier: "aion-express",
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
