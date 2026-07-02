//// Boundary types for the order fulfillment saga — the authored source of
//// truth (ADR-014, types-first).
////
//// Declare types only here: `aion generate` derives the JSON codecs
//// (`src/aion_order_saga_codecs.gleam`) and the emitted `schemas/*.json`
//// artifacts from these types. Edit a type, run `aion generate`, and commit
//// the type with its regenerated artifacts together.

/// Input of the `cancel_shipment` compensation activity.
pub type CancelShipmentInput {
  CancelShipmentInput(
    order_id: String,
    shipment_id: String,
  )
}

/// Result of one compensating action.
pub type CompensationOutput {
  CompensationOutput(
    status: String,
    detail: String,
  )
}

/// Result of the `reserve_inventory` activity.
pub type InventoryReservation {
  InventoryReservation(
    order_id: String,
    reservation_id: String,
    item: String,
    quantity: Int,
  )
}

/// The saga's start input: the order to fulfil.
pub type OrderInput {
  OrderInput(
    order_id: String,
    item: String,
    quantity: Int,
    amount: Int,
  )
}

/// Result of the `charge_payment` activity.
pub type PaymentReceipt {
  PaymentReceipt(
    order_id: String,
    payment_id: String,
    amount: Int,
  )
}

/// Input of the `refund_payment` compensation activity.
pub type RefundPaymentInput {
  RefundPaymentInput(
    order_id: String,
    payment_id: String,
    amount: Int,
  )
}

/// Input of the `release_inventory` compensation activity.
pub type ReleaseInventoryInput {
  ReleaseInventoryInput(
    order_id: String,
    reservation_id: String,
    item: String,
    quantity: Int,
  )
}

/// Result of the `ship_order` activity; also the saga's recorded output.
pub type Shipment {
  Shipment(
    order_id: String,
    shipment_id: String,
  )
}
