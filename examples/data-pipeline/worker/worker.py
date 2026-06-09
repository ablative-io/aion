#!/usr/bin/env python3
"""Data-pipeline Aion worker.

Run this after the Aion server is listening on gRPC localhost:50051. The worker
registers `fetch_url`, `process_item`, and `aggregate_results` activity stubs and
serves tasks until interrupted.
"""

from __future__ import annotations

import asyncio
import logging
import os

from aion_worker import ActivityRegistry, ReconnectConfig, Worker, WorkerConfig

registry = ActivityRegistry()


@registry.activity(name="fetch_url")
def fetch_url(url: str) -> dict[str, str]:
    """Return simulated page content for a URL.

    The example intentionally avoids real HTTP requests so activity behavior is
    predictable and safe to run anywhere.
    """

    content = f"Simulated content fetched from {url} for the Aion data pipeline example"
    return {"url": url, "content": content}


@registry.activity(name="process_item")
def process_item(fetched: dict[str, str]) -> dict[str, object]:
    """Process one fetched document into a small summary."""

    url = fetched["url"]
    content = fetched["content"]
    word_count = len(content.split())
    return {
        "url": url,
        "word_count": word_count,
        "summary": f"{url}: {word_count} words processed",
    }


@registry.activity(name="aggregate_results")
def aggregate_results(items: list[dict[str, object]]) -> dict[str, object]:
    """Combine processed items into the workflow's final output."""

    total_words = sum(int(item["word_count"]) for item in items)
    summaries = [str(item["summary"]) for item in items]
    return {
        "total_urls": len(items),
        "total_words": total_words,
        "summaries": summaries,
    }


def worker_config() -> WorkerConfig:
    """Build worker configuration from environment variables."""

    return WorkerConfig(
        endpoint=os.environ.get("AION_WORKER_ENDPOINT", "127.0.0.1:50051"),
        task_queue=os.environ.get("AION_TASK_QUEUE", "default"),
        identity=os.environ.get("AION_WORKER_IDENTITY", "data-pipeline-python-worker"),
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
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    await Worker(worker_config(), registry=registry).run()


if __name__ == "__main__":
    asyncio.run(main())
