# aion_flow

Typed Gleam SDK for authoring durable Aion workflows. Use it to define workflow entry points, declare typed activities, receive signals, expose read-only queries, use deterministic timers/time/randomness, and test workflow code in Gleam.

Workflow code should be deterministic: do not read wall clocks or ambient entropy directly. Use `aion/workflow.now`, `aion/workflow.random`, `aion/workflow.random_int`, and timer primitives so replay sees the same command stream.

## Use in this repository

The package is named `aion_flow` and targets Erlang/BEAM. Repository examples use it as a local path dependency rather than relying on a published package:

```toml
[dependencies]
aion_flow = { path = "../../gleam/aion_flow" }
```

Public modules:

- `aion/workflow` — workflow definitions, activity dispatch, deterministic time/randomness, timers, and child workflows.
- `aion/activity` — typed activity invocation values plus retry, timeout, and heartbeat configuration.
- `aion/signal` — typed signal references and in-engine send/receive helpers.
- `aion/query` — typed read-only query handlers and dispatch helpers.
- `aion/codec` — typed payload codecs for values crossing engine boundaries.
- `aion/duration`, `aion/error`, `aion/child`, and `aion/testing` — supporting durations, errors, child handles, and the pure Gleam harness.

## Define a workflow

A workflow definition names an entry function and carries codecs for input, output, and workflow errors:

```gleam
import aion/workflow

pub fn definition() {
  workflow.define(
    "hello_world",
    request_codec(),
    greeting_codec(),
    workflow_error_codec(),
    run,
  )
}

pub fn run(input: Request) -> Result(Greeting, String) {
  // Durable workflow logic goes here.
  Ok(Greeting(message: "Hello, " <> input.name <> "!"))
}
```

`workflow.define(name, input_codec, output_codec, error_codec, entry_fn)` returns a typed `WorkflowDefinition(input, output, workflow_error)` that package tooling and tests can inspect with `workflow.name`, `workflow.input_codec`, `workflow.output_codec`, `workflow.error_codec`, and `workflow.entry_fn`.

## Declare and call activities

Activities are typed values. `activity.new` stores the activity name, typed input, codecs, and local runner shape; `workflow.run` records the dispatch and returns `Result(output, error.ActivityError)`.

```gleam
import aion/activity
import aion/error
import aion/workflow

fn greet(name: String) -> activity.Activity(String, String) {
  activity.new(
    "greet",
    name,
    string_codec(),
    string_codec(),
    fn(value) { Ok("Hello, " <> value <> "!") },
  )
}

pub fn run(input: Request) -> Result(String, String) {
  case workflow.run(greet(input.name)) {
    Ok(message) -> Ok(message)
    Error(error.Retryable(message:, details: _)) -> Error(message)
    Error(error.Terminal(message:, details: _)) -> Error(message)
    Error(_) -> Error("activity failed")
  }
}
```

An activity created with `activity.new` has no hidden retry, timeout, or heartbeat defaults. Add policies explicitly with `activity.retry`, `activity.timeout`, and `activity.heartbeat` when a workflow needs them.

For homogeneous fanout, use `workflow.all`, `workflow.race`, or `workflow.map` over activity values.

## Codecs

All workflow, activity, signal, and query payloads cross engine boundaries through `aion/codec.Codec(a)`:

```gleam
import aion/codec
import gleam/dynamic/decode
import gleam/json

fn string_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}
```

For custom records, provide a JSON encoder and a `gleam/dynamic/decode.Decoder` for the same shape.

## Signals

Signals are named, typed messages delivered to a running workflow. Construct a `SignalRef(payload)` once and receive it from workflow code:

```gleam
import aion/signal
import aion/workflow

fn approval_signal() -> signal.SignalRef(Bool) {
  signal.new("approval", bool_codec())
}

pub fn wait_for_approval() -> Result(Bool, String) {
  case workflow.receive(approval_signal()) {
    Ok(approved) -> Ok(approved)
    Error(_) -> Error("approval signal failed")
  }
}
```

`signal.send(workflow_id, reference, payload)` is available for callers already inside the engine/Gleam-client boundary. Network-facing callers should use the client SDKs.

## Timers, deterministic time, and timeouts

Use workflow primitives instead of wall-clock functions:

```gleam
import aion/duration
import aion/workflow

pub fn pause_then_read_time() {
  use _ <- result.try(workflow.sleep(duration.minutes(5)))
  workflow.now()
}
```

The timer API also includes `workflow.start_timer`, `workflow.cancel_timer`, `workflow.timer_id`, and `workflow.with_timeout`.

## Queries

Queries are read-only and record no workflow events. A handler returns a typed value through a codec; by convention it must not dispatch activities or mutate workflow state.

```gleam
import aion/query

pub fn register_state_query(current_status: fn() -> String) {
  query.handler("state", string_codec(), current_status)
}
```

`query.dispatch(name, value_codec)` is provided for callers inside the engine boundary and for the Gleam test harness.

## Minimal example

```gleam
import aion/activity
import aion/codec
import aion/duration
import aion/error
import aion/signal
import aion/workflow
import gleam/dynamic/decode
import gleam/json

type Request {
  Request(name: String)
}

fn string_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn request_codec() -> codec.Codec(Request) {
  codec.json_codec(request_to_json, request_decoder())
}

fn request_to_json(request: Request) -> json.Json {
  json.object([#("name", json.string(request.name))])
}

fn request_decoder() -> decode.Decoder(Request) {
  use name <- decode.field("name", decode.string)
  decode.success(Request(name: name))
}

fn greet_activity(name: String) -> activity.Activity(String, String) {
  activity.new("greet", name, string_codec(), string_codec(), fn(name) {
    Ok("Hello, " <> name <> "!")
  })
}

fn approval_signal() -> signal.SignalRef(Bool) {
  codec.json_codec(json.bool, decode.bool)
  |> signal.new("approval")
}

pub fn definition() {
  workflow.define("hello_world", request_codec(), string_codec(), string_codec(), run)
}

pub fn run(input: Request) -> Result(String, String) {
  use greeting <- result.try(
    case workflow.run(greet_activity(input.name)) {
      Ok(value) -> Ok(value)
      Error(_) -> Error("activity failed")
    },
  )
  use _ <- result.try(
    case workflow.sleep(duration.seconds(1)) {
      Ok(value) -> Ok(value)
      Error(_) -> Error("timer failed")
    },
  )
  use approved <- result.try(
    case workflow.receive(approval_signal()) {
      Ok(value) -> Ok(value)
      Error(_) -> Error("signal failed")
    },
  )

  case approved {
    True -> Ok(greeting)
    False -> Error("not approved")
  }
}
```

For a complete end-to-end sample that builds a Gleam workflow, packages it as `.aion`, starts `aion-server`, runs a Python worker, and starts a workflow over HTTP, see [`../../examples/hello-world/README.md`](../../examples/hello-world/README.md).
