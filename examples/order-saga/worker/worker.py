#!/usr/bin/env python3
"""Order-saga Aion worker.

Run this after the Aion server is listening on gRPC localhost:50051. The worker
registers the three forward order-saga activities and their three compensating
activities. Set SIMULATE_CHARGE_FAILURE=true or SIMULATE_SHIPPING_FAILURE=true
to exercise the compensation paths.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
from collections.abc import Awaitable, Callable, Iterable

from aion_worker import (
    ActivityExecutionContext,
    ActivityTask,
    Completed,
    DispatchOutcome,
    Failed,
    GrpcWorkerSession,
    ReconnectConfig,
    WorkerConfig,
    connect_register_replay_and_serve,
)
from aion_worker.proto import common_pb2, worker_pb2

JSON_CONTENT_TYPE = "application/json"
Handler = Callable[[dict[str, object]], Awaitable[DispatchOutcome]]


class OrderSagaDispatcher:
    """Dispatcher for all order fulfillment saga activities."""

    def __init__(self) -> None:
        self._handlers: dict[str, Handler] = {
            "reserve_inventory": self.reserve_inventory,
            "charge_payment": self.charge_payment,
            "ship_order": self.ship_order,
            "release_inventory": self.release_inventory,
            "refund_payment": self.refund_payment,
            "cancel_shipment": self.cancel_shipment,
        }

    def activity_types(self) -> Iterable[str]:
        return self._handlers.keys()

    async def dispatch(self, task: ActivityTask, context: ActivityExecutionContext) -> DispatchOutcome:
        del context
        handler = self._handlers.get(task.activity_type)
        if handler is None:
            return worker_failure(f"unknown activity type: {task.activity_type}")

        try:
            request = decode_json_object(task.input)
            return await handler(request)
        except (KeyError, ValueError, json.JSONDecodeError, UnicodeDecodeError) as exc:
            return worker_failure(str(exc))

    async def reserve_inventory(self, request: dict[str, object]) -> DispatchOutcome:
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

    async def charge_payment(self, request: dict[str, object]) -> DispatchOutcome:
        order_id = required_string(request, "order_id")
        amount = required_int(request, "amount")
        if env_flag("SIMULATE_CHARGE_FAILURE"):
            message = f"simulated charge failure for order {order_id}"
            logging.info("Payment failed intentionally: order_id=%s", order_id)
            return worker_failure(message)

        payment_id = f"pay-{order_id}"
        logging.info("Charging payment: order_id=%s amount=%s payment_id=%s", order_id, amount, payment_id)
        return Completed(json_payload({"order_id": order_id, "payment_id": payment_id, "amount": amount}))

    async def ship_order(self, request: dict[str, object]) -> DispatchOutcome:
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

    async def release_inventory(self, request: dict[str, object]) -> DispatchOutcome:
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

    async def refund_payment(self, request: dict[str, object]) -> DispatchOutcome:
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

    async def cancel_shipment(self, request: dict[str, object]) -> DispatchOutcome:
        order_id = required_string(request, "order_id")
        shipment_id = required_string(request, "shipment_id")
        detail = f"cancelled {shipment_id}"
        logging.info(
            "Compensating shipment: order_id=%s shipment_id=%s",
            order_id,
            shipment_id,
        )
        return Completed(json_payload({"status": "cancelled", "detail": detail}))


def decode_json_object(payload: common_pb2.Payload) -> dict[str, object]:
    if payload.content_type != JSON_CONTENT_TYPE:
        raise ValueError(f"expected {JSON_CONTENT_TYPE} payload, got {payload.content_type!r}")
    value = json.loads(payload.bytes.decode("utf-8"))
    if not isinstance(value, dict):
        raise ValueError("expected JSON object input")
    return value


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


def worker_config() -> WorkerConfig:
    return WorkerConfig(
        endpoint=os.environ.get("AION_WORKER_ENDPOINT", "127.0.0.1:50051"),
        task_queue=os.environ.get("AION_TASK_QUEUE", "default"),
        identity=os.environ.get("AION_WORKER_IDENTITY", "order-saga-python-worker"),
        max_concurrency=int(os.environ.get("AION_WORKER_CONCURRENCY", "4")),
        reconnect=ReconnectConfig(
            initial_backoff_seconds=0.5,
            max_backoff_seconds=5.0,
            max_attempts=10,
        ),
        namespace=os.environ.get("AION_NAMESPACE", "default"),
        subject=os.environ.get("AION_SUBJECT", "worker"),
    )


async def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    if env_flag("SIMULATE_CHARGE_FAILURE"):
        logging.info("SIMULATE_CHARGE_FAILURE is enabled; charge_payment will fail")
    if env_flag("SIMULATE_SHIPPING_FAILURE"):
        logging.info("SIMULATE_SHIPPING_FAILURE is enabled; ship_order will fail")
    config = worker_config()
    dispatcher = OrderSagaDispatcher()
    logging.info("Registering order-saga activities: %s", ", ".join(dispatcher.activity_types()))
    await connect_register_replay_and_serve(
        config=config,
        connect=lambda: GrpcWorkerSession.connect(config),
        dispatcher=dispatcher,
    )


if __name__ == "__main__":
    asyncio.run(main())
