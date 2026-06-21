"""Hand-written order-saga activity bodies served by the generated worker.

`worker.py` is generated plumbing (do not edit); it decodes each task's JSON
input and routes `task.activity_type` to the matching async handler here. Each
handler returns a `DispatchOutcome` (`Completed` with the result payload, or a
classified `Failed`). Set `SIMULATE_CHARGE_FAILURE=true` or
`SIMULATE_SHIPPING_FAILURE=true` to exercise the compensation paths.
"""

from __future__ import annotations

import json
import logging
import os

from aion_worker import Completed, DispatchOutcome, Failed
from aion_worker.proto import common_pb2, worker_pb2

JSON_CONTENT_TYPE = "application/json"


async def reserve_inventory(request: dict[str, object]) -> DispatchOutcome:
    order_id = required_string(request, "order_id")
    item = required_string(request, "item")
    quantity = required_int(request, "quantity")
    reservation_id = f"res-{order_id}"
    logging.info(
        "Reserving inventory: order_id=%s item=%s quantity=%s reservation_id=%s",
        order_id,
        item,
        quantity,
        reservation_id,
    )
    return Completed(
        json_payload(
            {
                "order_id": order_id,
                "reservation_id": reservation_id,
                "item": item,
                "quantity": quantity,
            }
        )
    )


async def charge_payment(request: dict[str, object]) -> DispatchOutcome:
    order_id = required_string(request, "order_id")
    amount = required_int(request, "amount")
    if env_flag("SIMULATE_CHARGE_FAILURE"):
        message = f"simulated charge failure for order {order_id}"
        logging.info("Payment failed intentionally: order_id=%s", order_id)
        return worker_failure(message)

    payment_id = f"pay-{order_id}"
    logging.info("Charging payment: order_id=%s amount=%s payment_id=%s", order_id, amount, payment_id)
    return Completed(json_payload({"order_id": order_id, "payment_id": payment_id, "amount": amount}))


async def ship_order(request: dict[str, object]) -> DispatchOutcome:
    order_id = required_string(request, "order_id")
    item = required_string(request, "item")
    quantity = required_int(request, "quantity")
    if env_flag("SIMULATE_SHIPPING_FAILURE"):
        message = f"simulated shipping failure for order {order_id}"
        logging.info("Shipping failed intentionally: order_id=%s", order_id)
        return worker_failure(message)

    shipment_id = f"ship-{order_id}"
    logging.info(
        "Shipping order: order_id=%s item=%s quantity=%s shipment_id=%s",
        order_id,
        item,
        quantity,
        shipment_id,
    )
    return Completed(json_payload({"order_id": order_id, "shipment_id": shipment_id}))


async def release_inventory(request: dict[str, object]) -> DispatchOutcome:
    order_id = required_string(request, "order_id")
    reservation_id = required_string(request, "reservation_id")
    item = required_string(request, "item")
    quantity = required_int(request, "quantity")
    detail = f"released {quantity} x {item} from {reservation_id}"
    logging.info(
        "Compensating inventory reservation: order_id=%s reservation_id=%s item=%s quantity=%s",
        order_id,
        reservation_id,
        item,
        quantity,
    )
    return Completed(json_payload({"status": "released", "detail": detail}))


async def refund_payment(request: dict[str, object]) -> DispatchOutcome:
    order_id = required_string(request, "order_id")
    payment_id = required_string(request, "payment_id")
    amount = required_int(request, "amount")
    detail = f"refunded {amount} from {payment_id}"
    logging.info(
        "Compensating payment charge: order_id=%s payment_id=%s amount=%s",
        order_id,
        payment_id,
        amount,
    )
    return Completed(json_payload({"status": "refunded", "detail": detail}))


async def cancel_shipment(request: dict[str, object]) -> DispatchOutcome:
    order_id = required_string(request, "order_id")
    shipment_id = required_string(request, "shipment_id")
    detail = f"cancelled {shipment_id}"
    logging.info(
        "Compensating shipment: order_id=%s shipment_id=%s",
        order_id,
        shipment_id,
    )
    return Completed(json_payload({"status": "cancelled", "detail": detail}))


def required_string(value: dict[str, object], field: str) -> str:
    field_value = value[field]
    if not isinstance(field_value, str) or not field_value:
        raise ValueError(f"expected non-empty string field {field!r}")
    return field_value


def required_int(value: dict[str, object], field: str) -> int:
    field_value = value[field]
    if not isinstance(field_value, int) or isinstance(field_value, bool) or field_value < 1:
        raise ValueError(f"expected positive integer field {field!r}")
    return field_value


def env_flag(name: str) -> bool:
    return os.environ.get(name, "").strip().lower() in {"1", "true", "yes", "on"}


def json_payload(value: object) -> common_pb2.Payload:
    return common_pb2.Payload(
        content_type=JSON_CONTENT_TYPE,
        bytes=json.dumps(value, separators=(",", ":")).encode("utf-8"),
    )


def worker_failure(message: str) -> DispatchOutcome:
    return Failed(
        worker_pb2.ActivityError(
            kind=worker_pb2.ACTIVITY_ERROR_KIND_TERMINAL,
            message=message,
        )
    )
