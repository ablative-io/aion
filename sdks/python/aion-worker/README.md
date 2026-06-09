# aion-worker

Python remote-worker SDK for registering out-of-process Aion activities and serving them from an `aion-server` task queue.

## Install

```sh
python -m pip install aion-worker
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
