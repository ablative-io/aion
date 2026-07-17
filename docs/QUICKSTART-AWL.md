# Quickstart: your first workflow in AWL

Write a workflow in AWL, run it, prove it survives a crash — all in
under five minutes.

## Prerequisites

- Rust toolchain and Cargo ([rustup](https://rustup.rs))

## 1. Install

```sh
cargo install aion-cli --locked
```

The binary is **`aion`**. Verify with `aion --help`.

## 2. Write a workflow

Create `hello.awl`:

```awl
//! Greet someone, then shout their name.
workflow hello
  input name: String
  outcome shouted: type Shouted, route success

type Greeting { greeting: String }
type Shouted  { text: String }

worker hello
  action greet(name: String) -> Greeting
  action shout(text: String) -> Shouted

step greet_and_shout
  name |> greet |> .greeting |> shout |> route shouted
```

This defines a workflow that takes a name, pipes it through two
activities (`greet` → `shout`), and finishes with the shouted result.

Check it compiles:

```sh
aion awl check hello.awl
```

## 3. Write the worker

Activities run in a separate worker process. Create a small Rust project:

```sh
cargo new hello-worker
cd hello-worker
cargo add aion-worker@0.5 tokio --features macros,rt-multi-thread serde --features derive serde_json
```

Replace `src/main.rs`:

```rust
use std::time::Duration;
use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct GreetInput { name: String }

#[derive(Serialize)]
struct GreetOutput { greeting: String }

#[derive(Deserialize)]
struct ShoutInput { text: String }

#[derive(Serialize)]
struct ShoutOutput { text: String }

fn greet(input: GreetInput, _ctx: &ActivityContext) -> HandlerFuture<'_, GreetOutput> {
    Box::pin(async move {
        Ok(GreetOutput { greeting: format!("Hello, {}!", input.name) })
    })
}

fn shout(input: ShoutInput, _ctx: &ActivityContext) -> HandlerFuture<'_, ShoutOutput> {
    Box::pin(async move {
        Ok(ShoutOutput { text: input.text.to_uppercase() })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WorkerConfig::builder()
        .endpoint("http://127.0.0.1:50051")
        .task_queue("hello")
        .identity("hello-worker-1")
        .max_concurrency(4)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(10)
        .build()?;

    Worker::builder(config)
        .register_activity("greet", greet)?
        .register_activity("shout", shout)?
        .build()?
        .run()
        .await?;

    Ok(())
}
```

Go back to the parent directory:

```sh
cd ..
```

## 4. Start the server

Create `aion.toml`:

```toml
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "libsql"
url = "aion.db"

[runtime]
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 1024

[deploy]
enabled = true
max_archive_bytes = 16777216
max_inflated_bytes = 67108864
```

Open three terminals:

**Terminal 1** — start the server:

```sh
aion server --config aion.toml
```

**Terminal 2** — start the worker:

```sh
cargo run --manifest-path hello-worker/Cargo.toml
```

## 5. Deploy and run

**Terminal 3** — deploy the AWL file directly and start a run:

```sh
aion deploy hello.awl
aion start hello --input '{"name":"Ada"}'
```

You should see identifiers for the new run:

```json
{"run_id":"<run-id>","workflow_id":"<workflow-id>"}
```

Check the result:

```sh
aion describe <workflow-id> --pretty
```

The output shows `"status": "Completed"` with the event history and the
final result: `{"text": "HELLO, ADA!"}`.

## 6. Prove it survives

Start another run and kill the server before it finishes:

```sh
aion start hello --input '{"name":"Grace"}'
kill -9 <server-pid>
```

Restart the server:

```sh
aion server --config aion.toml
```

The run picks up exactly where it was. Activities that already completed
are not re-executed — the server replays their recorded results from
history.

## What just happened

You wrote a workflow in AWL — a small, checked language where every
construct works and every word means what it says. The Aion engine gave
it durable execution: event-sourced history, deterministic replay,
crash-proof resumption. The worker performed the actual work; the
workflow orchestrated it.

AWL also gives you:

- `aion awl fmt` — canonical formatting (the printer IS the formatter)
- `aion awl check` — full type checking before anything runs
- `aion awl emit` — generate Gleam source for inspection or extension
- `aion awl schema` — derive JSON Schema from your AWL types
- LSP support for editors (hover, go-to-definition, formatting)

## Next steps

- [AWL language reference](design/aion-authoring/awl/AWL-2-SPEC.md) —
  the full language: types, expressions, flow control, fork/join, loops
- [Getting started (Gleam path)](GETTING-STARTED.md) — write workflows
  directly in Gleam for full language power
- [Workflow authoring guide](guides/workflows.md) — timers, signals,
  queries, child workflows, determinism rules
- [Activities and workers guide](guides/activities-and-workers.md) —
  failure handling, retries, heartbeats
- [Examples](../examples/) — from hello-world to a full dev-brief
  pipeline with parallel review and bounded fix cycles
