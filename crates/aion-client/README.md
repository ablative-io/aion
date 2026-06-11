# aion-client

Rust caller SDK for connecting to an `aion-server` deployment and operating Aion workflows. The crate exposes connect plus the seven workflow operations: `start`, `signal`, `query`, `cancel`, `list`, `describe`, and `subscribe`.

## Key public types

- `Client`, `ClientBuilder`, `ClientAuth`, and `TlsOptions` configure gRPC connections.
- `WorkflowHandle` scopes operations to a started workflow run.
- `StartOptions`, `WorkflowDescription`, and `ListPage` model workflow operations.
- `EventStream`, `ResumingEventStream`, and `SubscribeTarget` stream events.
- `to_payload` and `from_payload` bridge typed values with `aion-core` payloads.

## Install

```toml
[dependencies]
aion-client = "0.1.0"
```

## Server prerequisite

Run an `aion-server` that implements the AW workflow API. The runnable example uses the AL-007 server fixture defaults:

```sh
export AION_SERVER_URL=http://127.0.0.1:50051
export AION_AUTH_TOKEN=dev-token # optional
cargo run -p aion-client --example seven_operations
```

See [`examples/seven_operations.rs`](examples/seven_operations.rs) for a complete program covering all seven operations.

## Connect

```rust
use aion_client::{ClientAuth, ClientBuilder};

let mut builder = ClientBuilder::new(std::env::var("AION_SERVER_URL")?)
    .with_namespace("conformance");
if let Ok(token) = std::env::var("AION_AUTH_TOKEN") {
    builder = builder.with_auth(ClientAuth::bearer(token));
}
let client = builder.build().await?;
```

## start

`start_typed` serializes a typed value to JSON and returns a `WorkflowHandle` that carries the workflow and run IDs. `StartOptions::idempotency_key` makes caller retries safe: the same request returns the original handle and conflicting reuse returns `ClientError::AlreadyExists`.

```rust
use aion_client::StartOptions;
use serde::Serialize;

#[derive(Serialize)]
struct StartInput {
    message: &'static str,
    counter: u32,
}

let handle = client
    .start_typed(
        "conformance.echo",
        &StartInput { message: "hello", counter: 1 },
        StartOptions {
            idempotency_key: Some("readme-seven-operations".to_owned()),
            ..StartOptions::default()
        },
    )
    .await?;
```

## signal

```rust
#[derive(Serialize)]
struct SignalInput {
    value: &'static str,
}

handle
    .signal_typed("record", &SignalInput { value: "signal-observed" })
    .await?;
```

## query

Query results can be decoded into any `serde::Deserialize` type. The current AW query request has no argument payload field, so pass `&()` for typed query arguments.

```rust
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct EchoState {
    last_signal: Option<String>,
}

let state: EchoState = handle.query_typed("state", &(), Duration::from_secs(5)).await?;
```

## list

```rust
use aion_client::ListPage;
use aion_core::WorkflowFilter;

let summaries = client
    .list(
        &WorkflowFilter {
            workflow_type: Some("conformance.echo".to_owned()),
            ..WorkflowFilter::default()
        },
        ListPage::default(),
    )
    .await?;
```

## describe

```rust
let description = handle.describe().await?;
println!("history events: {}", description.history.len());
```

## cancel

Cancellation is a cooperative request: success means the server accepted it, not that the workflow has already reached a terminal state.

```rust
handle.cancel("caller requested cancellation").await?;
```

## subscribe

`subscribe` returns a `Stream<Item = Result<Event, ClientError>>`. Transient disconnects are retried with the next per-workflow sequence number so delivered events are gap-free and duplicate-free; terminal failures are yielded as stream errors.

```rust
use futures::StreamExt;

let mut events = handle.subscribe();
while let Some(event) = events.next().await {
    let event = event?;
    println!("event seq={}", event.seq());
    break;
}
```

## Typed and raw payloads

Typed helpers (`start_typed`, `signal_typed`, `query_typed`, `to_payload`, `from_payload`) use JSON by default. For pre-serialized or non-JSON data, use the raw `aion_core::Payload` escape hatch with the raw operation variants:

```rust
use aion_core::{ContentType, Payload};

let raw = Payload::new(ContentType::Json, br#"{"value":"raw"}"#.to_vec());
handle.signal("record", raw).await?;
```

## Branching on errors

Every operation returns the shared branchable taxonomy via `ClientError`.
Each variant carries an `ErrorDetail` with the server's human detail message
and, when the wire supplied one, the structured `error_type` discriminator.

```rust
use aion_client::ClientError;

match handle.query_typed::<_, serde_json::Value>("state", &(), Duration::from_millis(10)).await {
    Ok(value) => println!("state: {value}"),
    Err(ClientError::QueryTimeout { detail }) => eprintln!("query timed out: {detail}"),
    Err(ClientError::UnknownQuery { detail }) => eprintln!("unknown query: {detail}"),
    Err(ClientError::Unavailable { detail }) => eprintln!("server is unavailable: {detail}"),
    Err(error) => return Err(error),
}
```
