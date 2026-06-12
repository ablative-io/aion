# aion-worker

Rust remote-worker SDK for registering typed Aion activities and serving them from an `aion-server` task queue.

This is a **library crate** — it ships no binaries, so `cargo install aion-worker` fails by design. A worker is your own crate:

```sh
cargo new my-worker
cd my-worker
cargo add aion-worker@0.4
cargo add tokio --features macros,rt-multi-thread
cargo add serde --features derive
```

## Key public types

- `Worker`, `WorkerBuilder`, and `WorkerConfig` configure and run a remote worker.
- `ActivityRegistry`, `TypedActivityDispatcher`, and `ActivityDispatcher` register activity handlers.
- `ActivityContext`, `HeartbeatRequest`, and cancellation handles provide per-task context.
- `WorkerSession`, `GrpcWorkerSession`, and `WorkerSessionEvent` model the worker protocol.
- `ActivityFailure`, `WorkerError`, and `WorkerConfigBuildError` report handler and runtime failures.

## Install

```toml
[dependencies]
aion-worker = "0.4.0"
```

## Minimal worker

Register at least one activity on the builder, build the worker, and start the async run loop:

```rust
use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct GreetInput {
    name: String,
}

#[derive(Serialize)]
struct GreetOutput {
    message: String,
}

fn greet(input: GreetInput, _context: &ActivityContext) -> HandlerFuture<'_, GreetOutput> {
    Box::pin(async move {
        Ok(GreetOutput {
            message: format!("hello, {}", input.name),
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WorkerConfig::builder()
        .endpoint("http://127.0.0.1:50051")
        .task_queue("default")
        .identity("rust-worker-1")
        .max_concurrency(8)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(10)
        .build()?;

    Worker::builder(config)
        .register_activity("examples.greet", greet)?
        .build()?
        .run()
        .await?;

    Ok(())
}
```

Trait bounds to know before fighting the compiler:

- **Input** types require `Serialize + DeserializeOwned + Send + Sync + 'static` — derive **both** `Serialize` and `Deserialize`.
- **Output** types require `Serialize + Send + Sync + 'static`.
- Handlers are `for<'context> Fn(Input, &'context ActivityContext)` returning `HandlerFuture<'context, Output>` — a pinned, boxed, `Send` future of `Result<Output, ActivityFailure>`; wrap an `async move` block in `Box::pin(...)` as above.

Every `WorkerConfig` field shown is required — there are no hidden defaults. Handlers fail with an explicit classification — `ActivityFailure::retryable(message)` or `ActivityFailure::terminal(message)` (attach structured detail with `.with_detail(payload)`). The worker never retries on its own; the classification rides back to the workflow, which drives retries. `ActivityContext` exposes `heartbeat(detail)`, `attempt()`, and cooperative cancellation via `is_cancelled()`.

The full worker story (failure contract, retry semantics, protocol) lives in the repository's [activities and workers guide](https://github.com/ablative-io/aion/blob/main/docs/guides/activities-and-workers.md).

See the main Aion repository at <https://github.com/ablative-io/aion>.
