"""Runnable aion-client-python example covering all seven workflow operations."""

from __future__ import annotations

import asyncio
import os
import sys
from typing import Any, NoReturn, cast

from aion_client import AlreadyExists, Client, EventStream, QueryTimeout, StreamEvent, TLSConfig, Unavailable


def _endpoint() -> str:
    if len(sys.argv) > 1:
        return sys.argv[1]
    return os.environ.get("AION_SERVER_URL", "http://127.0.0.1:50051")


def _fail(message: str) -> NoReturn:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def _tls_enabled(endpoint: str) -> bool:
    if os.environ.get("AION_INSECURE") == "1":
        return False
    return endpoint.startswith("https://") or endpoint.startswith("grpcs://")


async def run() -> None:
    endpoint = _endpoint()
    async with await Client.connect(
        endpoint,
        auth=os.environ.get("AION_AUTH_TOKEN"),
        tls=TLSConfig(enabled=_tls_enabled(endpoint)),
        namespace="conformance",
        # The WebSocket event stream rides the server's HTTP listener — a
        # separate address from the gRPC endpoint; there is no default.
        stream_endpoint=os.environ.get("AION_STREAM_URL"),
    ) as client:
        handle = await client.start(
            "conformance_echo",
            {"message": "hello", "counter": 1},
            idempotency_key=f"aion-client-python-seven-operations-{os.getpid()}",
        )
        print(f"started workflow={handle.workflow_id} run={handle.run_id}")

        await handle.signal("record", {"value": "signal-observed"})
        print("sent signal record")

        state = cast(dict[str, Any], await handle.query("state", target_type=dict, timeout=5.0))
        print(f"query state={state}")

        summaries = await client.list()
        print(f"listed {len(summaries)} workflow(s)")

        description = await handle.describe(include_history=True)
        print(f"described {len(description.history)} event(s)")

        await handle.cancel(reason="seven-operations example requested cancellation")
        print("cancel requested")

        stream: EventStream[Any] = handle.subscribe(raw=True)
        try:
            event = cast(StreamEvent[Any], await asyncio.wait_for(stream.__anext__(), timeout=5.0))
            print(f"subscribed event seq={event.seq}")
        except asyncio.TimeoutError:
            raise QueryTimeout("timed out waiting for the first subscribed event") from None
        finally:
            await stream.aclose()


async def main() -> None:
    try:
        await run()
    except Unavailable:
        _fail("aion-server is unavailable; check AION_SERVER_URL and the fixture")
    except AlreadyExists:
        _fail("idempotency key was reused for a different start request")
    except QueryTimeout:
        _fail("query or subscribe timed out before the fixture replied")


if __name__ == "__main__":
    asyncio.run(main())
