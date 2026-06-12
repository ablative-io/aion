# Activities and workers guide

Activities are where side effects live: HTTP calls, database writes,
payments, emails. Workflow code dispatches them through the recorded
boundary (`workflow.run`); **workers** — ordinary programs you write against
a worker SDK — execute them and report results back over the gRPC worker
protocol.

This guide covers the Rust worker SDK (`aion-worker`) in depth, the
worker-side failure contract, and retry semantics as they actually are.
Python and TypeScript worker SDKs live under
[`sdks/`](../../sdks/) and follow the same protocol.

## Workers are programs you scaffold, not binaries you install

`aion-worker` is a **library crate** — `cargo install aion-worker` fails
with "there is nothing to install ... contains no binaries". That is by
design: your activities are your code, so a worker is your own crate:

```sh
cargo new my-worker
cd my-worker
cargo add aion-worker@0.4
cargo add tokio --features macros,rt-multi-thread
cargo add serde --features derive
cargo add serde_json
```

## A complete worker

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
    greeting: String,
}

fn greet(input: GreetInput, _context: &ActivityContext) -> HandlerFuture<'_, GreetOutput> {
    Box::pin(async move {
        Ok(GreetOutput {
            greeting: format!("Hello, {}!", input.name),
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WorkerConfig::builder()
        .endpoint("http://127.0.0.1:50051")
        .task_queue("default")
        .identity("my-worker-1")
        .max_concurrency(4)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(10)
        .build()?;

    Worker::builder(config)
        .register_activity("greet", greet)?
        .build()?
        .run()
        .await?;

    Ok(())
}
```

`run()` connects to the server's gRPC address, registers the activity types,
and serves until a non-retryable error or shutdown. Every `WorkerConfig`
field above is **required** — there are no hidden defaults. The endpoint is
the server's `[server] grpc_address`.

## `register_activity` bounds — read these before fighting the compiler

The exact signature (identical on `WorkerBuilder` and `ActivityRegistry`):

```rust
pub fn register_activity<Input, Output, Handler>(
    self,
    activity_type: impl Into<String>,
    handler: Handler,
) -> Result<Self, WorkerError>
where
    Input: Serialize + DeserializeOwned + Send + Sync + 'static,
    Output: Serialize + Send + Sync + 'static,
    Handler: for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
        + Send
        + Sync
        + 'static,
```

In practice:

- **Input types need `#[derive(Serialize, Deserialize)]`** — deriving only
  `Deserialize` does not compile, because the bound is
  `Serialize + DeserializeOwned`.
- **Output types need `#[derive(Serialize)]`.**
- The handler returns `HandlerFuture<'_, Output>` =
  `Pin<Box<dyn Future<Output = Result<Output, ActivityFailure>> + Send + '_>>`
  — wrap an `async move` block in `Box::pin(...)` as shown above. Plain
  `async fn` does not satisfy the bound directly.
- Registering the same activity type twice returns
  `WorkerError::Registration`.

The activity type string must match what the workflow dispatches
(`activity.new("greet", ...)`) and what `workflow.toml` declares.

## The failure contract: classify, don't retry

A handler fails by returning `Err(ActivityFailure)`, and the failure carries
an explicit classification:

```rust
use aion_worker::ActivityFailure;

// Transient — the workflow may try again:
return Err(ActivityFailure::retryable("gateway 503"));

// Permanent — retrying cannot help:
return Err(ActivityFailure::terminal("card declined"));
```

Attach structured detail with `.with_detail(payload)`.

The worker SDK **never retries** a failed activity; it reports the failure
with its classification, and the workflow sees it as
`error.Retryable(message:, details:)` or `error.Terminal(message:, details:)`.

### Retry semantics, honestly

Today, **retries are workflow-driven**:

- The Gleam SDK lets a workflow attach an explicit `RetryPolicy` to an
  activity, and the engine records it — but engine-side automatic
  re-dispatch from that policy **is not consumed yet**. Dispatch always
  stamps attempt 1.
- The working pattern is a bounded retry loop in the workflow: on
  `Error(error.Retryable(..))`, sleep a durable backoff and dispatch a fresh
  recorded attempt. This is also the replay-honest pattern — every attempt
  is its own history event.
  [`examples/order-fulfillment/`](../../examples/order-fulfillment/)
  implements it.
- Tasks delivered to workers carry a one-based delivery `attempt` counter
  (visible as `context.attempt()`); with workflow-driven retries each
  attempt arrives as attempt 1 of a fresh dispatch.

Design accordingly: make activities **idempotent** where you can. A crash
between an activity completing and its result being recorded means the
activity may be delivered again.

## Heartbeats, cancellation, timeouts

`ActivityContext` gives long-running handlers their lifeline:

- `context.heartbeat(Some(progress_payload))` reports progress. Heartbeats
  are explicit — the SDK never emits automatic periodic heartbeats, and the
  worker does not enforce heartbeat timeouts.
- `context.is_cancelled()` supports **cooperative** cancellation: the worker
  never aborts a running handler; check the flag at sensible intervals and
  exit gracefully.
- There is no worker-side activity timeout configuration. The workflow-level
  timeout (`timeout_seconds` in `workflow.toml`) bounds the run as a whole.

## The worker protocol, briefly

One bidirectional gRPC stream per worker (contract:
[`crates/aion-proto/proto/worker.proto`](../../crates/aion-proto/proto/worker.proto)):

- The server acknowledges registration with a `RegisterAck` (assigned worker
  id, authorized namespace, heartbeat window) before dispatching tasks.
- Every consumed result is acknowledged with a `ResultAck`; unacked results
  are re-reported after reconnect, so results are not lost to a dropped
  connection.
- Reconnection uses the configured backoff
  (`reconnect_initial_backoff`/`max_backoff`/`max_attempts`); a server
  `DrainRequest` asks workers to finish in-flight work and reconnect without
  consuming their drop budget.
- A worker silent past the server's `[worker] heartbeat_window`
  (default 30000 ms) is considered lost.

## Matching the pieces up

For an activity to execute, three names must line up:

| Place | What |
|---|---|
| Workflow code | `activity.new("greet", ...)` |
| `workflow.toml` | `activities = ["greet"]` |
| Worker | `.register_activity("greet", greet)` |

And the worker's `task_queue` must be the namespace the workflow runs in
(`default` unless you configure otherwise). If a workflow seems stuck right
after start, check `aion describe <id>` — an `ActivityScheduled` event with
no completion almost always means no connected worker serves that activity
type.

## Python and TypeScript

The same protocol and the same contract (typed handlers,
retryable/terminal classification, explicit heartbeats, cooperative
cancellation) via:

- [`sdks/python/aion-worker`](../../sdks/python/aion-worker/) — used by the
  bundled examples (`examples/hello-world/worker.py`).
- [`sdks/typescript/aion-worker`](../../sdks/typescript/aion-worker/)
