# aion-worker

Rust remote-worker SDK for registering typed Aion activities and serving them from an `aion-server` task queue.

## Key public types

- `Worker`, `WorkerBuilder`, and `WorkerConfig` configure and run a remote worker.
- `ActivityRegistry`, `TypedActivityDispatcher`, and `ActivityDispatcher` register activity handlers.
- `ActivityContext`, `HeartbeatRequest`, and cancellation handles provide per-task context.
- `WorkerSession`, `GrpcWorkerSession`, and `WorkerSessionEvent` model the worker protocol.
- `ActivityFailure`, `WorkerError`, and `WorkerConfigBuildError` report handler and runtime failures.

## Install

```toml
[dependencies]
aion-worker = "0.1.0"
```

## Minimal worker

Register at least one activity on the builder, build the worker, and start the async run loop:

```rust
use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
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

See the main Aion repository at <https://github.com/ablative-io/aion>.
