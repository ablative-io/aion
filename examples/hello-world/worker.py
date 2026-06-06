#!/usr/bin/env python3
"""Hello-world Aion worker.

Run this after the Aion server is listening on gRPC localhost:50051. The worker
registers the `greet` activity and serves tasks until interrupted.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
from collections.abc import Iterable

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


class GreetingDispatcher:
    """Dispatcher for the hello-world `greet` activity."""

    def activity_types(self) -> Iterable[str]:
        return ["greet"]

    async def dispatch(self, task: ActivityTask, context: ActivityExecutionContext) -> DispatchOutcome:
        del context
        if task.activity_type != "greet":
            return worker_failure(f"unknown activity type: {task.activity_type}")

        try:
            request = decode_json_object(task.input)
            name = request["name"]
            if not isinstance(name, str) or not name:
                raise ValueError("expected non-empty string field 'name'")
        except (KeyError, ValueError, json.JSONDecodeError, UnicodeDecodeError) as exc:
            return worker_failure(str(exc))

        return Completed(json_payload({"greeting": f"Hello, {name}! Welcome to Aion."}))


def decode_json_object(payload: common_pb2.Payload) -> dict[str, object]:
    if payload.content_type != JSON_CONTENT_TYPE:
        raise ValueError(f"expected {JSON_CONTENT_TYPE} payload, got {payload.content_type!r}")
    value = json.loads(payload.bytes.decode("utf-8"))
    if not isinstance(value, dict):
        raise ValueError("expected JSON object input")
    return value


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
        identity=os.environ.get("AION_WORKER_IDENTITY", "hello-world-python-worker"),
        max_concurrency=int(os.environ.get("AION_WORKER_CONCURRENCY", "4")),
        reconnect=ReconnectConfig(
            initial_backoff_seconds=0.5,
            max_backoff_seconds=5.0,
            max_attempts=10,
        ),
    )


async def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    config = worker_config()
    dispatcher = GreetingDispatcher()
    await connect_register_replay_and_serve(
        config=config,
        connect=lambda: GrpcWorkerSession.connect(config),
        dispatcher=dispatcher,
    )


if __name__ == "__main__":
    asyncio.run(main())
