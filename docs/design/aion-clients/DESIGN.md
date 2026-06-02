---
type: design
cluster: aion-clients
title: Aion Clients — Caller-Side SDKs (Rust, Python, TypeScript, Gleam)
---

# Aion Clients — Caller-Side SDKs

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map. This cluster
> builds the **workflow-caller** role from the five-roles table.

## Intention

A workflow caller is anyone who drives a workflow from the outside: a web
backend that starts an onboarding workflow when a user signs up, an
operator who cancels a stuck run, a dashboard that lists everything
running, a service that sends an approval signal, a script that asks a
running workflow what step it is on. None of these write workflow code or
activity code — they reach into a running engine and operate it.

This cluster ships the SDKs that role uses: `aion-client` (Rust),
`aion-client-python` (PyPI), `aion-client-typescript` (npm), and
`aion_client` (Gleam, Hex). Each one connects to an `aion-server`
deployment and exposes the same seven operations — start, signal, query,
cancel, list, describe, subscribe — in the idioms of its language.

The bar is that a developer who has never read the engine internals can
add a dependency, point it at a server URL, and start a workflow with full
type-checking in their language within minutes. A frontend developer using
`aion-client-typescript` should never encounter beamr, Gleam, or Rust. The
four SDKs must behave identically where behaviour is observable: the same
operation against the same server produces the same effect and the same
errors, regardless of which language called it. A shared, language-neutral
behavioural contract makes that identity verifiable rather than aspirational.

When this cluster is done, every supported ecosystem has a first-class,
typed, ergonomic way to operate Aion workflows over the network, and a
conformance suite proves the four SDKs agree.

## Problem

The engine and its server are useless to most of the organisation if the
only way to reach them is hand-rolled HTTP. Application teams live in
Python, TypeScript, Gleam, and Rust; asking each of them to construct
requests, parse responses, manage a WebSocket, reconnect on drop, and map
status codes to errors — correctly, four times over — guarantees four
subtly different and subtly wrong clients.

The danger specific to a *workflow* client is that the operations are not
plain CRUD. Starting a workflow may be idempotent on a caller-supplied key.
A signal must reach a specific run. A query is a synchronous round-trip to
a live workflow process that may time out. Cancellation is a request, not a
guarantee of immediate stop. Event subscription is a long-lived stream that
must survive transient disconnects and resume without losing or duplicating
events. Each of these has a correct behaviour and many plausible-but-wrong
ones. If four SDKs each guess, callers cannot reason about the system.

There is also a hard boundary. The clients do not define the network
protocol — the server (cluster AW, `aion-server`) exposes the HTTP/gRPC +
WebSocket API, and the wire types live in `aion-proto`. The clients consume
that contract; they must not invent fields, endpoints, or semantics the
server does not provide. The risk is a client that drifts from the server's
actual surface and "works" only against a mock.

Finally, these are public packages on four registries with four toolchains.
"Production ready" means idiomatic, documented, typed, and published per
ecosystem — not a Rust crate transliterated into three other languages.

## Solution

A shared behavioural contract plus four idiomatic implementations of it,
all consuming the `aion-proto` wire types and the `aion-server` API.

### The Shared Client Contract

Before any SDK code, this cluster pins down — in language-neutral prose —
exactly what each of the seven operations means, what inputs they take,
what they return, what errors they raise, and the precise semantics of the
hard cases: start idempotency, run targeting, query timeouts, cancellation
as a request, and stream resumption. This contract document is the oracle.
The conformance suite (below) is its executable form, and every SDK is
measured against it. It is owned here, but it describes the *observable*
behaviour of the server's API as the clients must surface it; it does not
re-specify the wire format (that is AW/`aion-proto`).

**Key decision — one behavioural contract, four implementations.** The
correctness of "what a client does" is defined once, language-neutrally,
and shared. Each SDK then expresses it idiomatically. Rejected: letting
each SDK define its own semantics and hoping they converge — that is how
four clients drift. Rejected also: code-generating all four from
`aion-proto` and stopping there — generated stubs give wire types but not
the behavioural layer (reconnection, idempotency handling, typed
input/result ergonomics, error mapping) that makes an SDK worth using.

### The Seven Operations

Every SDK exposes the same surface, named idiomatically per language:

- **connect** — establish a client bound to a server endpoint, with auth
  and TLS configuration. Cheap to hold; reused across operations.
- **start** — begin a workflow run by type name with an input payload;
  returns a handle carrying `WorkflowId` and `RunId`. Supports a
  caller-supplied idempotency key so a retried start does not double-launch.
- **signal** — deliver a named signal with a payload to a workflow (latest
  run, or a specific run if targeted). Fire-and-forget at the API level:
  it returns once the server has accepted the signal for delivery.
- **query** — synchronous round-trip: ask a running workflow a named
  question and receive its reply payload, subject to a deadline.
- **cancel** — request cancellation of a run with a reason. Returns once
  the request is recorded; the workflow stops cooperatively, so this is a
  request, not an immediate kill.
- **list** — return workflow summaries matching a filter (type, status,
  time range, parent), with pagination.
- **describe** — return the full detail of one workflow: its summary plus
  its current status and (optionally) its event history.
- **subscribe** — open a WebSocket stream of live events for a workflow (or
  a filter, or the firehose), yielding events as the server publishes them,
  with automatic resume across transient disconnects.

**Key decision — a handle type, not bare IDs, is returned by start.** Start
returns a `WorkflowHandle` (or the language's equivalent) bundling the
`WorkflowId` and `RunId` and offering the per-workflow operations (signal,
query, cancel, describe, subscribe) as methods. A caller who started a
workflow can immediately operate on it without restating IDs. Bare IDs
remain available for callers who hold only an ID. Rejected: returning only
a `WorkflowId` string — it pushes run-targeting and ID-plumbing onto every
caller.

### Typed Input and Result Where the Language Allows

Workflow inputs, signal payloads, query arguments, and results are
`Payload` on the wire (opaque bytes + content-type, per `aion-core`).
Each SDK offers a typed front door over that:

- **Rust** — generic methods bounded on `serde::Serialize` /
  `serde::DeserializeOwned`; the SDK serialises to JSON `Payload` and
  deserialises results, with an explicit raw-`Payload` escape hatch.
- **Python** — accepts any JSON-serialisable value and (optionally) a
  Pydantic model or a target type for the result; raw `bytes` escape hatch.
- **TypeScript** — generic over the input/result types with JSON
  serialisation; the public API is typed, the wire stays `Payload`.
- **Gleam** — takes an encoder for input and a decoder for the result
  (`gleam/json` / `gleam/dynamic`), staying fully statically typed.

**Key decision — typed sugar over an always-present raw path.** Every SDK
exposes both a typed convenience surface and a raw-`Payload` surface. The
typed path is JSON-by-default; the raw path lets callers use a different
content-type or pre-serialised bytes. Rejected: typed-only — it would
strand callers whose payloads are not JSON or are produced elsewhere.

### Transport and the AW Boundary

The clients speak the `aion-server` API: unary operations over HTTP/gRPC,
event subscription over WebSocket, with `aion-proto` as the single wire
contract. The Rust client wraps the same transport but, per the component
architecture, is *also* usable directly against an embedded `aion` engine
(in-process calls, no network). This cluster designs the **network client**
against `aion-server`; the embedded binding is a thin alternate constructor
that targets the engine's in-process API and reuses the same handle and
typed-payload machinery.

**Key decision — clients consume `aion-proto`; they never define wire
types.** Request/response shapes, the gRPC service, and the WebSocket frame
format are owned by AW. The clients import `aion-proto` (Rust) or are
generated against / hand-mapped to it (Python, TS, Gleam) and must not add
fields or endpoints the server does not serve. Rejected: each client
carrying its own copy of the wire types — they would drift from the server.

### Error Mapping

Each SDK maps the server's error surface to an idiomatic error type with a
stable, documented taxonomy shared in spirit across all four:
`NotFound` (no such workflow/run), `AlreadyExists` (idempotency-key
conflict with a different request), `QueryFailed` / `QueryTimeout`,
`Cancelled`, `Unavailable` (transport/connection), `Unauthenticated`,
`InvalidArgument` (bad payload/filter), and `Server` (an unexpected server
error, carrying the server's detail). No SDK swallows an error or collapses
distinct failures into one opaque type.

**Key decision — a shared, named error taxonomy, idiomatic per language.**
The *set* of distinguishable failures is fixed by the contract; each SDK
renders it in its own idiom (a Rust `enum` + `thiserror`, a Python
exception hierarchy, a TS discriminated union / error subclasses, a Gleam
custom type). Rejected: a single catch-all error per SDK — callers cannot
branch on idempotency conflict vs query timeout vs auth failure.

### Streaming and Resumption

`subscribe` returns a language-native async stream (Rust `Stream`, Python
async iterator, TS `AsyncIterable`, Gleam subject/stream). The stream
carries decoded events. On a transient disconnect the SDK reconnects and
resumes from the last delivered event's sequence so the caller observes a
gap-free, duplicate-free stream; on a terminal failure it surfaces an error
through the stream rather than silently ending. Resumption uses the
per-event sequence number from the `aion-core` envelope.

**Key decision — resumption is the SDK's job, transparently.** The caller
gets a stream that survives blips; it does not hand-roll reconnect logic.
The SDK tracks the last sequence and re-subscribes from it. Rejected:
exposing raw socket lifecycle to the caller — that recreates the per-client
reconnection bugs the SDKs exist to prevent.

### Conformance

A single, language-neutral conformance scenario set (start → signal →
query → list → describe → cancel → subscribe, plus the error and idempotency
cases) runs each SDK against a real `aion-server` instance and asserts
identical observable behaviour. This is the executable form of the shared
contract and the gate that proves the four SDKs agree.

## Structure

```
# Rust — aion-client (crates.io)
crates/aion-client/Cargo.toml
crates/aion-client/src/lib.rs            thin re-export surface
crates/aion-client/src/client.rs         Client + ClientBuilder (connect, auth, TLS)
crates/aion-client/src/handle.rs         WorkflowHandle (signal/query/cancel/describe/subscribe)
crates/aion-client/src/ops.rs            start/list/describe over the transport
crates/aion-client/src/payload.rs        typed <-> Payload (serde) helpers
crates/aion-client/src/stream.rs         event subscription Stream + resumption
crates/aion-client/src/transport.rs      network transport over aion-proto + embedded binding
crates/aion-client/src/error.rs          ClientError taxonomy

# Python — aion-client-python (PyPI)
sdks/python/aion-client/pyproject.toml
sdks/python/aion-client/aion_client/__init__.py   public surface re-exports
sdks/python/aion-client/aion_client/client.py     Client (connect, start, list, describe)
sdks/python/aion-client/aion_client/handle.py     WorkflowHandle (signal/query/cancel/describe/subscribe)
sdks/python/aion-client/aion_client/payload.py    typed <-> Payload (JSON / model) helpers
sdks/python/aion-client/aion_client/stream.py     async event iterator + resumption
sdks/python/aion-client/aion_client/errors.py     exception hierarchy
sdks/python/aion-client/tests/                    unit + conformance harness

# TypeScript — aion-client-typescript (npm)
sdks/typescript/aion-client/package.json
sdks/typescript/aion-client/src/index.ts          public surface re-exports
sdks/typescript/aion-client/src/client.ts         Client (connect, start, list, describe)
sdks/typescript/aion-client/src/handle.ts         WorkflowHandle (signal/query/cancel/describe/subscribe)
sdks/typescript/aion-client/src/payload.ts        typed <-> Payload (JSON) helpers
sdks/typescript/aion-client/src/stream.ts         AsyncIterable event stream + resumption
sdks/typescript/aion-client/src/errors.ts         error union / classes
sdks/typescript/aion-client/test/                 unit + conformance harness

# Gleam — aion_client (Hex)
gleam/aion_client/gleam.toml
gleam/aion_client/src/aion_client.gleam       public surface (connect, start, list, describe)
gleam/aion_client/src/aion_client/handle.gleam  workflow handle ops
gleam/aion_client/src/aion_client/payload.gleam encoder/decoder <-> Payload helpers
gleam/aion_client/src/aion_client/stream.gleam  event stream + resumption
gleam/aion_client/src/aion_client/error.gleam   error custom type
gleam/aion_client/test/                         unit + conformance harness

# Shared (this cluster's connective tissue)
docs/design/aion-clients/CLIENT-CONTRACT.md  language-neutral behavioural contract
conformance/aion-clients/scenarios.json      shared conformance scenario set
conformance/aion-clients/README.md           how each SDK runs the suite
```

## Constraints

- **CO1** — Clients consume the `aion-server` API and `aion-proto` wire
  types; they SHALL NOT define their own wire formats, endpoints, or fields
  the server does not serve. The protocol is owned by cluster AW.
- **CO2** — Clients SHALL NOT depend on the engine internals (`aion`,
  `aion-store`, beamr). The Rust client may depend on `aion-core` (for
  shared domain types like `WorkflowId`/`WorkflowStatus`/`Payload`) and
  `aion-proto`; the embedded constructor may depend on `aion` only behind a
  feature flag, never by default.
- **CO3** — All four SDKs expose the same seven operations with identical
  observable behaviour, verified by the shared conformance suite.
- **CO4** — Every SDK exposes both a typed payload surface (per language)
  and a raw-`Payload` escape hatch; neither is removable.
- **CO5** — Every SDK maps server failures to a documented, branchable
  error taxonomy. No swallowed errors, no single catch-all collapsing
  distinct failures.
- **CO6** — `subscribe` resumes transparently across transient
  disconnects using the per-event sequence number, delivering a gap-free,
  duplicate-free stream; terminal failures surface through the stream, not
  as a silent end.
- **CO7** — `start` honours a caller-supplied idempotency key: a retried
  identical start returns the original handle; a conflicting reuse raises
  `AlreadyExists`.
- **CO8** — Per-ecosystem toolchain gates pass: Rust (`cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo fmt --check`, no
  `#[allow]`/`#[expect]`/`#[ignore]`); Python (`ruff` + `mypy --strict` +
  `pytest`); TypeScript (`tsc --noEmit` strict + `eslint` + the test
  runner); Gleam (`gleam format --check` + `gleam check` + `gleam test`).
  The no-partial-implementation and no-silent-failure standards hold across
  all four.
- **CO9** — Each package is publishable to its registry: crates.io
  (`aion-client`), PyPI (`aion-client-python`), npm
  (`aion-client-typescript`), Hex (`aion_client`), each with README,
  type-checked public surface, and a runnable example for all seven
  operations.

## Non-Goals

- **No worker SDKs.** Executing activities is the AR cluster
  (`aion-worker-*`). Clients *drive* workflows; workers *execute*
  activities. Different role, different protocol.
- **No server or wire protocol.** The HTTP/gRPC + WebSocket API and the
  `aion-proto` wire types are cluster AW. This cluster consumes them.
- **No engine, replay, timers, signals routing, or query dispatch** — those
  are AE/AD/AT. The client sends a signal/query over the API; the engine
  routes and answers it.
- **No authoring SDK.** Defining workflows is `aion_flow` (Gleam, cluster
  AF). A caller starts a workflow by type name; it does not define one.
- **No dashboard.** The monitoring UI is cluster AU; it may use these
  clients but is not designed here.
- **No Elixir client** in this cluster — it follows once the Elixir
  authoring story (`aion_flow_ex`) lands, mirroring the Gleam client.
- **No new auth scheme.** Clients carry whatever credential the server
  requires (bearer token / mTLS as AW defines); they do not invent one.
