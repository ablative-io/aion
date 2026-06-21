//// Hand-written activity bodies and the package's activity manifest.
////
//// This is the single per-activity artifact an author writes (ADR-014): the
//// typed `manifest()` below declares each activity once — its name, tier, and
//// input/output value types — and `aion generate` derives the rest (the io
//// types and codecs, the typed codec wrappers, the `activity.new` wrappers, the
//// Python worker plumbing, and the wire-compat golden). The bodies are the
//// deterministic in-VM doubles the SDK's test harness runs; in production each
//// activity name is served by the Python worker in `worker/`.

import aion/activity
import aion/error
import aion_order_saga_codecs as codecs
import aion_order_saga_io as io

pub fn reserve_inventory(
  input: io.OrderInput,
) -> Result(io.InventoryReservation, error.ActivityError) {
  Ok(io.InventoryReservation(
    order_id: input.order_id,
    reservation_id: "res-" <> input.order_id,
    item: input.item,
    quantity: input.quantity,
  ))
}

pub fn charge_payment(
  input: io.OrderInput,
) -> Result(io.PaymentReceipt, error.ActivityError) {
  Ok(io.PaymentReceipt(
    order_id: input.order_id,
    payment_id: "pay-" <> input.order_id,
    amount: input.amount,
  ))
}

pub fn ship_order(
  input: io.OrderInput,
) -> Result(io.Shipment, error.ActivityError) {
  Ok(io.Shipment(
    order_id: input.order_id,
    shipment_id: "ship-" <> input.order_id,
  ))
}

pub fn release_inventory(
  input: io.ReleaseInventoryInput,
) -> Result(io.CompensationOutput, error.ActivityError) {
  Ok(io.CompensationOutput(
    status: "released",
    detail: "released " <> input.reservation_id,
  ))
}

pub fn refund_payment(
  input: io.RefundPaymentInput,
) -> Result(io.CompensationOutput, error.ActivityError) {
  Ok(io.CompensationOutput(
    status: "refunded",
    detail: "refunded " <> input.payment_id,
  ))
}

pub fn cancel_shipment(
  input: io.CancelShipmentInput,
) -> Result(io.CompensationOutput, error.ActivityError) {
  Ok(io.CompensationOutput(
    status: "cancelled",
    detail: "cancelled " <> input.shipment_id,
  ))
}

/// The package's activity declarations — the single source of truth the
/// generator reads. Declaration order is load-bearing: it fixes the order of
/// the generated wrappers, the worker registry, and the `workflow.toml`
/// activities list.
pub fn manifest() -> List(activity.Declaration) {
  [
    activity.declare(
      "reserve_inventory",
      activity.RemotePython,
      activity.type_ref("OrderInput", codecs.order_input_codec()),
      activity.type_ref(
        "InventoryReservation",
        codecs.inventory_reservation_codec(),
      ),
    ),
    activity.declare(
      "charge_payment",
      activity.RemotePython,
      activity.type_ref("OrderInput", codecs.order_input_codec()),
      activity.type_ref("PaymentReceipt", codecs.payment_receipt_codec()),
    ),
    activity.declare(
      "ship_order",
      activity.RemotePython,
      activity.type_ref("OrderInput", codecs.order_input_codec()),
      activity.type_ref("Shipment", codecs.shipment_codec()),
    ),
    activity.declare(
      "release_inventory",
      activity.RemotePython,
      activity.type_ref(
        "ReleaseInventoryInput",
        codecs.release_inventory_input_codec(),
      ),
      activity.type_ref(
        "CompensationOutput",
        codecs.compensation_output_codec(),
      ),
    ),
    activity.declare(
      "refund_payment",
      activity.RemotePython,
      activity.type_ref(
        "RefundPaymentInput",
        codecs.refund_payment_input_codec(),
      ),
      activity.type_ref(
        "CompensationOutput",
        codecs.compensation_output_codec(),
      ),
    ),
    activity.declare(
      "cancel_shipment",
      activity.RemotePython,
      activity.type_ref(
        "CancelShipmentInput",
        codecs.cancel_shipment_input_codec(),
      ),
      activity.type_ref(
        "CompensationOutput",
        codecs.compensation_output_codec(),
      ),
    ),
  ]
}
