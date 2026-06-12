# aion_client

Gleam caller SDK for Aion workflow servers. It exposes connect plus the seven workflow operations: `start`, `signal`, `query`, `cancel`, `list`, `describe`, and `subscribe`.

## Install

```sh
gleam add aion_client
```

## Server prerequisite

Run an Aion server (`aion server --config <file>`) that implements the AW workflow API. The example test mirrors the AL-007 fixture values and can be run with:

```sh
export AION_SERVER_URL=http://127.0.0.1:8080
export AION_AUTH_TOKEN=dev-token # optional
gleam test
```

See [`test/example_test.gleam`](test/example_test.gleam) for a complete seven-operation flow using the SDK surface and fixture transport. The default transport reports `Unavailable` until the server HTTP/WebSocket adapter is supplied by the embedding application.

## Connect

```gleam
import aion_client
import gleam/option.{Some}

let assert Ok(client) =
  aion_client.connect(aion_client.Config(
    endpoint: "http://127.0.0.1:8080",
    bearer_token: Some("dev-token"),
    namespace: "conformance",
    tls: False,
  ))
```

## start

Typed payloads use a Gleam JSON encoder. The idempotency key makes retrying the same start safe; conflicting reuse is surfaced as `error.AlreadyExists`.

```gleam
import gleam/json
import gleam/option.{Some}

let assert Ok(handle) =
  aion_client.start(
    client,
    aion_client.StartOptions(
      workflow_id: "echo-readme",
      workflow_type: "conformance.echo",
      task_queue: "conformance",
      idempotency_key: Some("readme-seven-operations"),
    ),
    #("hello", 1),
    fn(input) {
      let #(message, counter) = input
      json.object([
        #("message", json.string(message)),
        #("counter", json.int(counter)),
      ])
    },
  )
```

## signal

```gleam
import aion_client/handle as workflow_handle

let assert Ok(Nil) =
  workflow_handle.signal(handle, "record", "signal-observed", fn(value) {
    json.object([#("value", json.string(value))])
  })
```

## query

```gleam
import gleam/dynamic/decode

let assert Ok(last_signal) =
  workflow_handle.query(handle, "state", Nil, fn(_) { json.null() }, decode.string)
```

## list

```gleam
let assert Ok(summaries) =
  aion_client.list(client, aion_client.ListOptions(namespace: Some("conformance")))
```

## describe

```gleam
let assert Ok(description) = workflow_handle.describe(handle)
```

## cancel

Cancellation is a cooperative request: success means the server accepted the request.

```gleam
let assert Ok(Nil) = workflow_handle.cancel(handle, "caller requested cancellation")
```

## subscribe

`handle.subscribe` returns an `EventStream`. Stream collection yields `EventItem`, `StreamError`, or `StreamEnd`; transient reconnect/resume semantics are exercised in the example test through `stream.subscribe_with_stub`.

```gleam
import aion_client/stream

let events = workflow_handle.subscribe(handle, decode.string) |> stream.collect
```

## Typed and raw payloads

Typed operations accept encoders/decoders from `gleam/json` and `gleam/dynamic/decode`. The raw escape hatch is the public `payload.Payload(content_type:, bytes:)` type and `*_raw` functions:

```gleam
import aion_client/payload

let raw = payload.Payload(content_type: payload.json_content_type, bytes: "{\"value\":\"raw\"}")
let assert Ok(Nil) = workflow_handle.signal_raw(handle, "record", raw)
```

## Branching on errors

All operations return `Result(_, error.Error)`, so callers can branch on the shared taxonomy.

```gleam
import aion_client/error
import gleam/io

case workflow_handle.query(handle, "state", Nil, fn(_) { json.null() }, decode.string) {
  Ok(state) -> io.println(state)
  Error(error.QueryTimeout) -> io.println("query timed out; use a longer timeout")
  Error(error.AlreadyExists) -> io.println("idempotency key conflict")
  Error(error.Unavailable) -> io.println("server or stream transport is unavailable")
  Error(_) -> io.println("workflow operation failed")
}
```
