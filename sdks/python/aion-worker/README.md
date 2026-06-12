# aion-worker

Python remote-worker SDK for registering out-of-process Aion activities and serving them from an Aion server task queue. Status: in progress/hardening; install from this checkout until a release is published for your target environment.

## Install

```sh
python -m pip install -e sdks/python/aion-worker
```

## Minimal worker

Register activities before constructing the worker, then start the async run loop:

```python
import asyncio

from aion_worker import ReconnectConfig, Worker, WorkerConfig, activity


@activity(name="examples.greet")
def greet(input: dict[str, str]) -> dict[str, str]:
    return {"message": f"hello, {input['name']}"}


async def main() -> None:
    config = WorkerConfig(
        endpoint="http://127.0.0.1:50051",
        task_queue="default",
        identity="python-worker-1",
        max_concurrency=8,
        reconnect=ReconnectConfig(
            initial_backoff_seconds=0.1,
            max_backoff_seconds=5.0,
            max_attempts=10,
        ),
    )

    await Worker(config).run()


asyncio.run(main())
```

See the main Aion repository at <https://github.com/ablative-io/aion>.

## Regenerating the protobuf stubs

The gRPC stubs under `aion_worker/proto/` are generated from the shared wire
contract at `crates/aion-proto/proto/` (the hatch build hook in
`build_proto.py` runs the same generation at package build time). After a
wire-contract change, regenerate and commit the refreshed stubs:

```bash
cd sdks/python/aion-worker
uv run --extra dev --isolated -- python -c "
from pathlib import Path
from build_proto import generate_proto_stubs
generate_proto_stubs(Path('.').resolve())
"
```
