#!/usr/bin/env python3
"""Batch-orchestrator Aion worker.

Run this after the Aion server is listening on gRPC localhost:50051. The
worker registers the `process-batch-item` activity stub the child workflow
schedules and serves tasks until interrupted.

The stub mirrors the deterministic contract documented in
`src/batch_orchestrator_item.gleam`: work items whose id or payload contains
`fail` are terminal failures; everything else is processed successfully.
"""

from __future__ import annotations

import asyncio
import logging
import os

from aion_worker import (
    ActivityRegistry,
    ReconnectConfig,
    TerminalError,
    Worker,
    WorkerConfig,
)

registry = ActivityRegistry()


@registry.activity(name="process-batch-item")
def process_batch_item(item: dict[str, str]) -> dict[str, str]:
    """Process one work item, failing deterministically on `fail` markers."""

    item_id = item["id"]
    payload = item["payload"]
    if "fail" in item_id or "fail" in payload:
        raise TerminalError(f"deterministic failure for item {item_id}")
    return {
        "item_id": item_id,
        "processed_payload": f"processed:{payload}",
        "detail": f"processed item {item_id}",
    }


def worker_config() -> WorkerConfig:
    """Build worker configuration from environment variables."""

    return WorkerConfig(
        endpoint=os.environ.get("AION_WORKER_ENDPOINT", "127.0.0.1:50051"),
        task_queue=os.environ.get("AION_TASK_QUEUE", "default"),
        identity=os.environ.get(
            "AION_WORKER_IDENTITY", "batch-orchestrator-python-worker"
        ),
        max_concurrency=int(os.environ.get("AION_WORKER_CONCURRENCY", "8")),
        namespace=os.environ.get("AION_WORKER_NAMESPACE", "default"),
        subject=os.environ.get("AION_WORKER_SUBJECT", "worker"),
        reconnect=ReconnectConfig(
            initial_backoff_seconds=0.5,
            max_backoff_seconds=5.0,
            max_attempts=10,
        ),
    )


async def main() -> None:
    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )
    await Worker(worker_config(), registry=registry).run()


if __name__ == "__main__":
    asyncio.run(main())
