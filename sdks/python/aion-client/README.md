# aion-client

Async Python caller SDK for Aion workflows. Status: in progress/hardening. The repository package is named `aion-client` and imported as `aion_client`; install from this checkout until a release is published for your target environment. It exposes connect plus the seven workflow operations: `start`, `signal`, `query`, `cancel`, `list`, `describe`, and `subscribe`.

## Install

```sh
python -m pip install -e sdks/python/aion-client
```

## Server prerequisite

Run an Aion server (`aion server --config <file>`) that implements the AW workflow API. The runnable example uses the AL-007 fixture defaults:

```sh
export AION_SERVER_URL=http://127.0.0.1:50051
export AION_AUTH_TOKEN=dev-token # optional
python examples/seven_operations.py
```

See [`examples/seven_operations.py`](examples/seven_operations.py) for a complete async program covering all seven operations.

## Connect

```python
import os
from aion_client import Client, TLSConfig

endpoint = os.environ["AION_SERVER_URL"]
client = await Client.connect(
    endpoint,
    auth=os.environ.get("AION_AUTH_TOKEN"),
    tls=TLSConfig(enabled=endpoint.startswith(("https://", "grpcs://"))),
    namespace="conformance",
    # The WebSocket event stream rides the server's HTTP listener — a
    # separate address from the gRPC endpoint. There is no default and
    # nothing is derived; subscribe without it raises InvalidArgument.
    stream_endpoint=os.environ.get("AION_STREAM_URL"),
)
```

## start

JSON values are the typed payload path. `idempotency_key` makes start safe to retry: a repeated identical start returns the original handle, while conflicting reuse raises `AlreadyExists`.

```python
handle = await client.start(
    "conformance.echo",
    {"message": "hello", "counter": 1},
    idempotency_key="readme-seven-operations",
)
```

## signal

```python
await handle.signal("record", {"value": "signal-observed"})
```

## query

The SDK decodes JSON results to Python values, or to a supplied `target_type` when appropriate. The current AW query request has no argument payload field, so omit query arguments for now.

```python
state = await handle.query("state", target_type=dict, timeout=5.0)
print(state.get("lastSignal"))
```

## list

```python
summaries = await client.list()
print(f"listed {len(summaries)} workflow(s)")
```

## describe

```python
description = await handle.describe(include_history=True)
print(description.summary, len(description.history))
```

## cancel

Cancellation is a cooperative request: success means the server accepted the request.

```python
await handle.cancel(reason="caller requested cancellation")
```

## subscribe

`handle.subscribe()` returns an async iterator over the client's configured `stream_endpoint` (the server's `/events/stream` WebSocket URL — required, never derived). The initial attach is a live tail; pass `from_seq=1` to replay the full recorded history first. It reconnects after transient disconnects using the last delivered per-workflow sequence number; a graceful server close (WebSocket close-1000) ends iteration normally, and terminal failures are raised from iteration rather than ending silently.

```python
async for event in handle.subscribe(raw=True):
    print(event.seq, event.value)
    break
```

## Typed and raw payloads

Pass ordinary JSON-serializable Python values for typed payloads. For pre-serialized bytes or non-default content, use the raw escape hatch accepted by every payload-bearing operation:

```python
await handle.signal(
    "record",
    raw=b'{"value":"raw"}',
    content_type="application/json",
)
```

## Branching on errors

Every operation raises branchable subclasses of `AionClientError` matching the shared taxonomy.

```python
from aion_client import AlreadyExists, QueryTimeout, Unavailable

try:
    state = await handle.query("state", target_type=dict, timeout=0.01)
except QueryTimeout:
    print("query timed out; use a longer timeout")
except AlreadyExists:
    print("idempotency key was reused for a different start")
except Unavailable:
    print("server or stream transport is unavailable")
```
