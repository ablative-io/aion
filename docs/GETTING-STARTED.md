# Getting started with Aion

This guide takes you from nothing to a completed durable workflow using only
published artifacts — no repository checkout. You will:

1. install the `aion` CLI,
2. write and package a Gleam workflow with one activity, one signal, and one
   query,
3. write a Rust activity worker,
4. configure and run the server,
5. deploy, start, query, signal, and complete a workflow run,
6. kill the server mid-run and watch the run survive.

Working from a repository checkout instead? Use
[`examples/hello-world/README.md`](../examples/hello-world/README.md).

## Prerequisites

- Rust toolchain and Cargo ([rustup](https://rustup.rs) recommended)
- [Gleam](https://gleam.run/getting-started/installing/) with Erlang/OTP on
  your `PATH`

## 1. Install the CLI

```sh
cargo install aion-cli --locked
```

The crate is named `aion-cli`; the installed binary is **`aion`**. It is the
one user-facing binary: it packages workflows, runs the server (`aion server`),
and operates workflows over gRPC. Verify:

```sh
aion --help
```

You should see the subcommands: `server`, `new`, `package`, `deploy`,
`versions`, `route`, `unload`, `start`, `signal`, `query`, `cancel`, `list`,
`describe`.

> Prefer a generated starting point? `aion new <name>` scaffolds a complete,
> buildable project — workflow, schemas, `workflow.toml`, a dev `aion.toml`,
> and a README with these same steps (`--template hello-world`,
> `approval-flow`, or `saga`; `--worker rust` adds a worker crate). This
> guide builds the same thing by hand so you see every part.

> There is no separate server binary and no installable worker binary.
> `cargo install aion-server` and `cargo install aion-worker` are not how
> Aion is installed: the server is `aion server`, and workers are ordinary
> Rust/Python/TypeScript programs you write against a worker SDK (step 4).

## 2. Write the workflow

Workflows are written in [Gleam](https://gleam.run) with the
[`aion_flow`](https://hex.pm/packages/aion_flow) SDK and compiled to BEAM
bytecode that the server executes durably.

```sh
gleam new my_flow
cd my_flow
gleam add aion_flow gleam_json
```

Replace `src/my_flow.gleam` with this workflow. It greets the caller via a
remote `greet` activity, publishes a `status` query, waits durably for an
`approval` signal, and returns a JSON result:

```gleam
import aion/activity
import aion/codec
import aion/error
import aion/query
import aion/signal
import aion/workflow
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleam/result

pub type HelloInput {
  HelloInput(name: String)
}

pub type GreetingOutput {
  GreetingOutput(greeting: String)
}

pub type Approval {
  Approval(approver: String)
}

pub type FlowError {
  FlowFailed(message: String)
}

/// Engine entry point — the function named by `workflow.toml`.
///
/// The contract: the runtime delivers the start input as a **raw JSON string**
/// inside a `Dynamic`. Decode the string, parse it with your input codec, run
/// the typed workflow, and **encode the success value back to a JSON string**
/// for the recorded result payload.
pub fn run(raw_input: Dynamic) -> Result(String, FlowError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case input_codec().decode(raw_json) {
        Ok(input) -> execute(input)
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(FlowFailed("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(FlowFailed("workflow input payload was not a string"))
  }
}

fn execute(input: HelloInput) -> Result(String, FlowError) {
  // Activities are the only side-effect boundary. The engine records the
  // dispatch, a connected worker executes it, and replay returns the
  // recorded result instead of running it again.
  use greeting <- result.try(greet(input))
  use _ <- result.try(set_status("awaiting_approval"))
  use approval <- result.try(await_approval())
  use _ <- result.try(set_status("approved"))
  Ok(
    json.to_string(
      json.object([
        #("greeting", json.string(greeting)),
        #("approved_by", json.string(approval.approver)),
      ]),
    ),
  )
}

fn greet(input: HelloInput) -> Result(String, FlowError) {
  case workflow.run(greet_activity(input)) {
    Ok(GreetingOutput(greeting: greeting)) -> Ok(greeting)
    Error(activity_error) ->
      Error(FlowFailed(activity_error_message(activity_error)))
  }
}

fn greet_activity(
  input: HelloInput,
) -> activity.Activity(HelloInput, GreetingOutput) {
  // The final argument is a local implementation used only by the
  // `aion/testing` harness; a deployed workflow always dispatches to a worker.
  activity.new("greet", input, input_codec(), greeting_codec(), local_greet)
}

fn local_greet(input: HelloInput) -> Result(GreetingOutput, error.ActivityError) {
  Ok(GreetingOutput(greeting: "Hello, " <> input.name <> "! (test harness)"))
}

fn await_approval() -> Result(Approval, FlowError) {
  // A durable wait: the run suspends here — for seconds or months — and
  // survives server restarts while it waits.
  case workflow.receive(approval_signal()) {
    Ok(approval) -> Ok(approval)
    Error(_) -> Error(FlowFailed("failed to receive the approval signal"))
  }
}

fn approval_signal() -> signal.SignalRef(Approval) {
  signal.new("approval", approval_codec())
}

fn set_status(stage: String) -> Result(Nil, FlowError) {
  // Re-register the `status` query handler with a fresh closure at each
  // state change; queries are answered at yield points and never touch
  // workflow history.
  case query.handler("status", status_codec(), fn() { stage }) {
    Ok(Nil) -> Ok(Nil)
    Error(_) -> Error(FlowFailed("failed to register the status query"))
  }
}

fn input_codec() -> codec.Codec(HelloInput) {
  codec.json_codec(hello_input_to_json, hello_input_decoder())
}

fn hello_input_to_json(input: HelloInput) -> json.Json {
  json.object([#("name", json.string(input.name))])
}

fn hello_input_decoder() -> decode.Decoder(HelloInput) {
  use name <- decode.field("name", decode.string)
  decode.success(HelloInput(name: name))
}

fn greeting_codec() -> codec.Codec(GreetingOutput) {
  codec.json_codec(greeting_to_json, greeting_decoder())
}

fn greeting_to_json(output: GreetingOutput) -> json.Json {
  json.object([#("greeting", json.string(output.greeting))])
}

fn greeting_decoder() -> decode.Decoder(GreetingOutput) {
  use greeting <- decode.field("greeting", decode.string)
  decode.success(GreetingOutput(greeting: greeting))
}

fn approval_codec() -> codec.Codec(Approval) {
  codec.json_codec(approval_to_json, approval_decoder())
}

fn approval_to_json(approval: Approval) -> json.Json {
  json.object([#("approver", json.string(approval.approver))])
}

fn approval_decoder() -> decode.Decoder(Approval) {
  use approver <- decode.field("approver", decode.string)
  decode.success(Approval(approver: approver))
}

fn status_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn activity_error_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    _ -> "greet activity failed"
  }
}
```

The full authoring surface — timers, timeout races, child workflows,
determinism rules — is in the
[workflow authoring guide](guides/workflows.md).

## 3. Describe and build the package

Add `workflow.toml` next to `gleam.toml`:

```toml
# Packaging descriptor read by `aion package`. Reference: docs/packaging.md.
[[workflow]]
entry_module = "my_flow"          # also the workflow type clients start
entry_function = "run"
timeout_seconds = 3600
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = ["greet"]            # every activity the workflow dispatches
output = "my-flow.aion"
```

Unknown keys in `workflow.toml` are hard errors — typos fail loudly. Create
the two JSON Schema files it references:

`schemas/input.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["name"],
  "additionalProperties": false,
  "properties": {
    "name": { "type": "string", "minLength": 1 }
  }
}
```

`schemas/output.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["greeting", "approved_by"],
  "properties": {
    "greeting": { "type": "string" },
    "approved_by": { "type": "string" }
  }
}
```

Build and package:

```sh
gleam build
aion package .
```

You should see a JSON report naming the archive and its content-hash version:

```json
{
  "packages": [
    {
      "workflow_type": "my_flow",
      "output": "/.../my_flow/my-flow.aion",
      "version": "<sha-256 content hash>",
      "deployed_name": "my_flow$<hash>",
      "modules": 42
    }
  ],
  "excluded": [
    { "module": "gleeunit", "reason": "dev_dependency" },
    { "module": "aion@testing", "reason": "sdk_test_only" }
  ]
}
```

(The real `excluded` list is longer — dev dependencies and the SDK's
test-only modules are excluded from the archive by design; each entry
names its reason.)

(`aion package . --build` compiles and packages in one step. The full
packaging reference is [`docs/packaging.md`](packaging.md).)

## 4. Write the activity worker

Activities run outside the workflow VM, in workers you write against a worker
SDK (Rust here; Python and TypeScript SDKs also exist under `sdks/`). The
Rust SDK is a **library** — `cargo install aion-worker` fails with
"no binaries". Scaffold your own crate:

```sh
cd ..
cargo new my-worker
cd my-worker
cargo add aion-worker@0.5
cargo add tokio --features macros,rt-multi-thread
cargo add serde --features derive
cargo add serde_json
```

Replace `src/main.rs`:

```rust
use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

// Input types need Serialize + DeserializeOwned; output types need Serialize.
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
            greeting: format!("Hello, {}! Welcome to Aion.", input.name),
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Every field is explicit; there are no hidden defaults.
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

The handler's `Ok` value is serialized as the activity result — here a JSON
object the workflow's `greeting_codec` decodes. Failure classification,
retries, heartbeats, and cancellation are covered in the
[activities and workers guide](guides/activities-and-workers.md).

## 5. Configure and run the server

The server reads a TOML config file. Two keys have **no default** and must be
set (`runtime.query_timeout_ms`, `websocket.event_broadcast_capacity`), and
the `[deploy]` section must be enabled — with both size ceilings — for
`aion deploy` to work. Missing keys fail startup with an error naming the key.

Create `aion.toml` in a fresh working directory:

```toml
[server]
listen_address = "127.0.0.1:8080"   # HTTP/JSON API + dashboard
grpc_address = "127.0.0.1:50051"    # gRPC API + worker protocol

[store]
backend = "libsql"                  # durable; "memory" loses state on stop
url = "aion.db"                     # embedded libSQL file, created on start

[runtime]
# REQUIRED, no default: reply deadline for workflow queries, in milliseconds.
query_timeout_ms = 10000

[websocket]
# REQUIRED, no default: capacity of the global live-event broadcast channel.
event_broadcast_capacity = 1024

[deploy]
# Runtime package deploy/route/unload. Dark by default; both ceilings are
# REQUIRED when enabled, and max_inflated_bytes must be >= max_archive_bytes.
enabled = true
max_archive_bytes = 16777216        # 16 MiB upload ceiling
max_inflated_bytes = 67108864       # 64 MiB decompressed-contents ceiling
```

Every key can also be set by environment variable as
`AION_<SECTION>_<KEY>` (for example `AION_RUNTIME_QUERY_TIMEOUT_MS=10000`).
The complete config reference is in the
[operations guide](guides/operations.md).

Start the server (terminal 1):

```sh
aion server --config aion.toml
```

You should see JSON log lines on stdout, including the HTTP and gRPC listen
addresses. Leave it running. In terminal 2, start the worker:

```sh
cargo run --manifest-path my-worker/Cargo.toml
```

## 6. Deploy and run the workflow

In terminal 3, deploy the archive to the running server:

```sh
aion deploy my_flow/my-flow.aion
```

You should see a JSON response with `workflow_type`, `content_hash`,
`freshly_loaded: true`, and `route_changed: true`. Runtime-deployed packages
**persist in the store**: after a restart, the server reloads them before
recovering workflows, so deployed code survives restarts too.

Start a run (the workflow type is the entry module name, `my_flow`):

```sh
aion start my_flow --input '{"name":"Ada"}'
```

You should see the assigned identifiers:

```json
{"run_id":"<run-id>","workflow_id":"<workflow-id>"}
```

The run executes `greet` on your worker, then suspends waiting for the
`approval` signal. Ask it where it is:

```sh
aion query <workflow-id> status
```

You should see `{"result":"awaiting_approval"}`. Queries are answered live by the
handler the workflow registered — they read state, never change it.

## 7. Prove the durability

While the run is waiting, kill the server hard:

```sh
kill -9 <server pid>
```

Restart it (same directory, same config):

```sh
aion server --config aion.toml
```

On startup the server reloads the deployed package from the store, replays
the run's event history, and the run is waiting for `approval` again —
exactly where it was. The `greet` activity is **not** re-executed; replay
returns its recorded result.

## 8. Approve and complete

```sh
aion signal <workflow-id> approval --payload '{"approver":"ada"}'
aion describe <workflow-id> --pretty
```

`describe` shows the summary with `"status": "Completed"` and the event
history: workflow start, `greet` scheduled and completed, the signal, and
workflow completion carrying the JSON result
`{"greeting":"Hello, Ada! Welcome to Aion.","approved_by":"ada"}`.

A completed run no longer answers live queries — `aion query` against it now
returns `error[not_running]`. That is expected: terminal runs are inspected
with `describe`, not queried. See the
[errors reference](errors.md) for every error code.

## 9. Versions, routing, rollback

Each deployed package version is identified by its content hash, and new
starts route to one version per workflow type:

```sh
aion versions                       # every loaded version, route flags
aion route my_flow <content-hash>   # roll back / forward to a loaded version
aion unload my_flow <content-hash>  # remove a non-routed, unpinned version
```

Running workflows keep executing the version they started on while new
starts use the routed version. The full deploy, versioning, and recovery
model is in the [operations guide](guides/operations.md).

## Where to go next

- [Workflow authoring guide](guides/workflows.md) — the entry contract,
  determinism rules, timers, signals, queries, child workflows.
- [Activities and workers guide](guides/activities-and-workers.md) — worker
  scaffolding, failure classification, retry semantics as they actually are.
- [Operations guide](guides/operations.md) — full config reference, deploy
  surface, persistence and recovery, metrics.
- [Errors reference](errors.md) — every error code, with hints.
- [`docs/API.md`](API.md) — HTTP/gRPC/WebSocket transports.
- [`docs/examples/order-saga.md`](examples/order-saga.md) — the flagship
  order-fulfillment saga: retries, timeout races, child workflows, and saga
  compensation in one walkthrough.
