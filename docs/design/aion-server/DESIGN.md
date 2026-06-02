---
type: design
cluster: aion-server
title: Aion Wire & Server — The Shared Wire Contract and the Standalone Deployable
---

# Aion Wire & Server — The Shared Wire Contract and the Standalone Deployable

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

This cluster is the network skin of Aion. The `aion` engine crate is
transport-agnostic by deliberate design (per COMPONENT-ARCHITECTURE's
boundary rule and aion-engine CO6): it has no HTTP, no gRPC, no WebSocket. A
CLI tool embeds it with nothing but a file-backed store and never touches a
socket. But teams that run many services want a *managed* deployment — a
single binary they point clients and workers at, that streams live workflow
events to a dashboard, isolates tenants, and dispatches activity tasks to
out-of-process workers in any language. That is `aion-server`, and the wire
contract it speaks is `aion-proto`.

Two crates with sharply different shapes. `aion-proto` is a pure, leaf-level
contract: the gRPC service definitions and the serde wire types that
`aion-server`, every client SDK (cluster AL), and every worker SDK (cluster
AR) agree on. It is defined once so the three never drift. `aion-server` is
the deployable: it wraps a live `Engine`, translates wire requests into
`Engine` method calls, tails the engine's event broadcast to feed WebSocket
subscribers, runs the server side of the remote-worker protocol, partitions
everything by namespace, and serves the monitoring dashboard's API and static
assets.

When this cluster is done, `aion-server --store ... --port 7233` is a
running service. A Python client starts a workflow over gRPC. A TypeScript
worker connects, receives an activity task, runs it, and reports the result.
A dashboard opens a WebSocket and watches every activity in a namespace
complete in real time. None of this adds one line of execution logic — every
durable decision still happens in `aion`. This cluster is the boundary
between the engine and the network, nothing more and nothing less.

## Problem

The engine is embeddable and transport-free, which is exactly right for the
library case and exactly insufficient for the server case. Standing up a
managed deployment surfaces problems the engine deliberately does not solve:

**One wire contract, many speakers.** A client SDK in Python, a worker SDK in
TypeScript, and the Rust server must agree byte-for-byte on what a
"start workflow" request is, what an activity-task dispatch looks like, and
how an `Event` serialises onto the wire. If each defines its own shapes, they
rot apart the first time a field is added. The contract must be authored once,
in one crate, that all three depend on — and that crate must be a leaf, so a
Python worker's build does not transitively pull the engine, beamr, or a
store backend.

**Translating the network onto a transport-agnostic API.** The engine offers
plain async Rust methods — `start_workflow`, `signal`, `query`, `cancel`,
`list_workflows`, `result`, `subscribe`. Something must accept an HTTP or
gRPC request, deserialise it into the engine's parameter types (the
`aion-core` domain types), call the method, and serialise the result back —
without ever reaching into engine internals or adding orchestration logic of
its own. Get this seam wrong and execution logic leaks into the server,
violating the boundary the whole architecture rests on.

**Real-time streaming from an append-only log.** The engine appends an event
and publishes it to an in-process broadcast channel (DESIGN-OVERVIEW
"Real-Time Event Streaming"; surfaced as `engine.subscribe(EventFilter)`).
The server must let a remote client open a WebSocket, subscribe to a slice of
that firehose — one workflow, a filtered class, or everything — and receive
events as they happen, then clean up the subscription when the socket closes.
Backpressure, slow consumers, and disconnect must all be handled; a slow
dashboard must not stall the engine.

**Out-of-process workers.** Tier-3 activities run in workers in any language
(DESIGN-OVERVIEW "Execution Tiers"). The server is the rendezvous: workers
connect, advertise which activity types they implement, receive task
dispatches, execute in their own runtime, and report completion, failure, or
a heartbeat for long-running work. The *protocol* (its messages, its
transport, its heartbeat semantics) must be defined in `aion-proto`; the
*server side* of it — accepting connections, matching tasks to workers,
collecting results back into the engine's activity contract — lives here.

**Multi-tenancy.** A shared server serves many tenants. A namespace must
isolate one tenant's workflows, events, and workers from another's: a client
scoped to namespace A cannot start, signal, query, list, or stream a workflow
in namespace B, and a worker registered in A never receives a task from B.

**Serving the dashboard.** The dashboard UI is cluster AU. The server hosts
it — serving the static asset bundle and backing it with the same API and
WebSocket feed every other client uses.

None of these is execution logic. All of them are network, protocol, and
operations — the work the engine deliberately refuses to do.

## Solution

Two crates.

- **`aion-proto`** — the shared wire contract. gRPC service definitions
  (`.proto`) plus the serde request/response/event wire types they map to.
  Depends only on `aion-core`, so the wire `Event` and the domain `Event` are
  one type and never diverge — but pulls in nothing else from the Aion tree,
  keeping it a clean leaf that clients and workers can target without the
  engine. (D1, D2.)
- **`aion-server`** — the standalone binary. Wraps a live `aion::Engine`,
  exposes it over gRPC + HTTP, streams events over WebSocket by tailing the
  engine's `subscribe` broadcast, runs the worker-protocol endpoint,
  enforces namespace isolation, and serves the dashboard assets. Depends on
  `aion`, `aion-proto`, and (at deploy time) a store backend. Contains **no
  workflow execution logic** — it delegates every durable decision to the
  engine. (Per COMPONENT-ARCHITECTURE: "aion-server contains no workflow
  execution logic of its own.")

### Why proto-as-shared-contract (D1)

The wire contract is a single crate depended on by the server, all client
SDKs, and all worker SDKs. This is the load-bearing decision of the cluster:
it guarantees the three categories of consumer cannot disagree on the wire
form because there is exactly one definition. gRPC is the transport for the
request/response API and the worker protocol; the `.proto` files are the
machine-readable contract from which other-language SDKs (AL, AR) generate
their stubs. The Rust side is hand-paired serde types that mirror the proto
messages, so Rust consumers get idiomatic types and the proto stays the
language-neutral source. Rejected: each SDK defining its own wire types —
guaranteed drift, and no single artefact for non-Rust codegen.

### Why aion-proto depends on aion-core (D2)

`aion-proto` depends on `aion-core` and nothing else in the Aion tree. The
alternative — duplicating `Event`, `WorkflowFilter`, `WorkflowSummary`,
`WorkflowStatus`, and the ID newtypes as separate "wire" structs — would
create two definitions of the spine of the system and a hand-maintained
conversion layer that rots. By reusing `aion-core`'s types directly (they
already derive `Serialize`/`Deserialize` per aion-core CO6) the wire `Event`
*is* the domain `Event`. The proto messages that carry them are a thin
envelope (namespace, request id, the serialised core payload). `aion-core` is
itself a leaf with no engine dependency, so a Python worker's transitive
graph stops at `aion-core` + `aion-proto` — never the engine, beamr, or a
store. Rejected: a standalone `aion-proto` with cloned types — it reintroduces
exactly the multiple-definitions problem aion-core was created to prevent.

### The Wire Contract: `aion-proto`

The contract has four message families, all defined once:

1. **Workflow management** — `StartWorkflow`, `Signal`, `Query`, `Cancel`,
   `ListWorkflows`, `DescribeWorkflow`, and their responses. Requests carry a
   namespace and the serialised `aion-core` parameters (`Payload` for inputs,
   `WorkflowFilter` for list); responses carry `WorkflowId`/`RunId`,
   `WorkflowSummary` lists, `Payload` results, or a typed wire error.
2. **Event streaming** — the `SubscriptionRequest` shape the WebSocket layer
   parses (per-workflow id, a filter spec by type/status/namespace, or
   firehose) and the streamed `Event` envelope. The server maps a
   `SubscriptionRequest` onto the engine's `EventFilter` and onto a namespace
   scope.
3. **Worker protocol** — `RegisterWorker` (advertised activity types +
   namespace), `ActivityTask` (the dispatch: activity type, input `Payload`,
   the `WorkflowId`/`ActivityId` correlation), `ActivityResult` (completion
   `Payload` or `ActivityError`), and `Heartbeat` (liveness + optional
   progress for long-running activities). The transport choice for this
   family is D7.
4. **Wire errors** — a `WireError` taxonomy that maps the engine's
   `EngineError`, the store's `StoreError`, query/signal failures, and
   namespace/authorisation failures onto stable, language-neutral codes
   clients and workers can branch on.

**D3 — wire errors are an explicit, stable taxonomy, not stringified
internals.** Every failure that crosses the wire becomes a `WireError` with a
stable code and a human message, mapped from the engine's typed error. A
client must be able to distinguish "workflow not found" from "namespace
denied" from "sequence conflict" by code, not by parsing a string. Rejected:
forwarding `Debug`-formatted engine errors — it leaks internals and gives
clients nothing stable to branch on.

**D4 — config types carrying secrets are `Deserialize`-only.** Per the
project credential policy, `aion-server`'s configuration types (store DSNs,
TLS material, auth tokens) derive `Deserialize` but **not** `Serialize`, so a
loaded credential cannot be accidentally re-serialised into a log, an error,
or a response. Wire data types (the proto messages, events, summaries) are
ordinary data and derive both, per aion-core CO6. The two are kept distinct.

### The Server: translating the network onto the `Engine`

`aion-server` holds one `Arc<Engine>` (or one per namespace — see D8) and a
set of transport adapters. Each adapter does exactly one thing: deserialise a
wire request into the engine's parameter types, call the matching `Engine`
method, serialise the outcome (or a mapped `WireError`) back.

- **gRPC + HTTP API** — `start_workflow`, `signal`, `query`, `cancel`,
  `list_workflows` (→ `WorkflowFilter`), `describe` (→ a `WorkflowSummary`
  plus, optionally, history via the store). gRPC is the primary transport;
  an HTTP/JSON facade over the same handlers serves browsers and curl. Both
  share one handler layer over the `Engine`; the transport is a thin skin.

**D5 — the server is a pure adapter; no orchestration, no execution.** A
handler may deserialise, validate the namespace, call exactly one (or a
small fixed composition of) `Engine` method(s), and serialise the result. It
must not retry, schedule, sequence, or otherwise make a durable decision —
those are the engine's, recorded in the event store. This keeps the boundary
auditable: if a behaviour would change replay, it does not belong in
`aion-server`. Rejected: convenience endpoints that batch or orchestrate
multiple workflow operations with server-side logic — they would put
un-recorded decision-making outside the engine.

### Real-Time Event Streaming over WebSocket

The engine appends an event and publishes it to an in-process broadcast
channel; `engine.subscribe(EventFilter)` returns a stream tail of that
channel (DESIGN-OVERVIEW "Real-Time Event Streaming"). The WebSocket layer is
the network bridge to that stream:

1. A client opens a WebSocket and sends a `SubscriptionRequest`.
2. The server validates the namespace scope, maps the request onto an
   `EventFilter`, and calls `engine.subscribe(...)`.
3. A per-connection task forwards events from the engine stream to the socket
   as serialised `Event` envelopes, until the client closes or the filter's
   workflow terminates.
4. On socket close or error, the subscription stream is dropped and the
   forwarding task ends — no leak.

Three subscription models, all expressed as `SubscriptionRequest` variants
mapped to `EventFilter`: **per-workflow** (one id), **filtered** (by type,
status, or namespace), and **firehose** (all events in the caller's
namespace, for dashboards).

**D6 — a slow WebSocket consumer is dropped, never allowed to stall the
engine.** Each connection has a bounded outbound buffer. If a client cannot
keep up and the buffer fills, that connection is closed with a "lagged" wire
error rather than applying backpressure to the engine's broadcast (which
would slow every other subscriber and the engine itself). The broadcast tail
the engine exposes is already lossy-on-lag by construction; the server
surfaces the lag as a typed disconnect so the client can resubscribe and
re-read history if it needs the gap. Rejected: unbounded per-connection
buffering — it turns one slow dashboard into an engine-wide memory and
latency problem.

### The Remote-Worker Protocol (server side)

Out-of-process workers (cluster AR) connect to this endpoint to run Tier-3
activities. The server is the matchmaker between the engine's activity
dispatch and the pool of connected workers:

1. A worker connects and sends `RegisterWorker` (its namespace + the activity
   types it implements).
2. When a workflow schedules a Tier-3 activity, the engine surfaces an
   activity-task need; the server matches it to a registered worker for that
   activity type in that namespace and sends an `ActivityTask`.
3. The worker executes and returns an `ActivityResult` (a result `Payload` or
   an `ActivityError`), or sends periodic `Heartbeat`s for long-running work.
4. The server feeds the result back into the engine's activity contract so
   the engine records `ActivityCompleted`/`ActivityFailed` and the workflow
   resumes — the *recording and retry decision remain the engine's*, exactly
   as for in-VM activities (aion-engine D7). A worker that disconnects or
   stops heartbeating before reporting causes the engine to treat the
   activity as failed per its retry policy; the server reports the lost
   worker, it does not itself decide the retry.

**D7 — the worker protocol rides gRPC bidirectional streaming, with push
dispatch and worker heartbeats.** A worker opens a long-lived bidirectional
gRPC stream: the server pushes `ActivityTask`s down it; the worker pushes
`ActivityResult`s and `Heartbeat`s up it. Push (not Temporal-style task-queue
polling) because the engine already knows the moment an activity is scheduled
and a connected worker is addressable — polling would add latency and wasted
round-trips for no gain in this architecture. gRPC bidi (not raw WebSocket)
because the request/response API is already gRPC, so workers reuse one
transport, one auth path, and one generated stub family. Heartbeats are
worker→server messages on the same stream; a missed-heartbeat window marks
the worker lost and surfaces the task as failed to the engine. Rejected:
task-queue polling — needless latency given push addressability; rejected:
WebSocket for workers — a second transport and codegen path for no benefit
when the API is already gRPC.

### Multi-Tenancy / Namespace Isolation

**D8 — a namespace is the isolation unit, enforced at the adapter boundary
before any engine call.** Every wire request carries a namespace; the server
authorises the caller for that namespace and scopes the operation to it
*before* calling the engine. List/query/subscribe are filtered to the
namespace; signal/query/cancel verify the target workflow belongs to the
caller's namespace; worker registration and task dispatch are partitioned by
namespace so a worker in A never receives a task from B. Whether namespaces
map to separate `Engine`/store instances or one shared engine with a
namespace dimension on every key is a deploy-time configuration the server
abstracts behind a `NamespaceResolver`; the *enforcement* — no cross-namespace
operation reaches the engine — is invariant either way. Rejected: trusting
clients to stay in their namespace, or filtering only on the way out —
isolation must be enforced before the engine acts, not after.

### Serving the Dashboard

The dashboard frontend is cluster AU; this cluster *hosts* it. The server
serves AU's built static asset bundle from a configured path (or an embedded
bundle) and backs it with the same gRPC/HTTP handlers and the same WebSocket
event feed every other client uses. There is no dashboard-specific API — the
dashboard is just another first-class client of the public contract, which
keeps the server honest: anything the dashboard can do, any client can do.

### Configuration and the Binary

`aion-server` is the only crate in the Aion tree that is a binary with a
top-level `main`. Per CLAUDE.md it is the one place `anyhow` is permitted (for
top-level error reporting in `main`); every library module within it uses
`thiserror`. Configuration (store selection + DSN, listen ports, TLS, auth,
dashboard asset path, namespace mode, worker heartbeat window, WebSocket
buffer bound) is loaded from a file/env into `Deserialize`-only config types
(D4); no value is a hardcoded default baked into the crate (per CLAUDE.md "no
assumed defaults") — the engine's own defaults (e.g. scheduler threads) are
deferred to the engine, and operational values are supplied by the operator.

## Structure

```
crates/aion-proto/
├── Cargo.toml                       — deps: aion-core, prost/tonic, serde (leaf)
├── build.rs                         — [AW-001] compile .proto via tonic-build
├── proto/
│   ├── workflow.proto               — [AW-003] start/signal/query/cancel/list/describe
│   ├── events.proto                 — [AW-004] SubscriptionRequest + Event envelope
│   ├── worker.proto                 — [AW-005] register/task/result/heartbeat (bidi)
│   └── common.proto                 — [AW-002] ids, payload, status, WireError
└── src/
    ├── lib.rs                       — [AW-001] thin re-export surface
    ├── convert.rs                   — [AW-002] proto <-> aion-core conversions
    ├── error.rs                     — [AW-002] WireError taxonomy + mapping
    └── generated.rs                 — [AW-001] include! of tonic-build output

crates/aion-server/
├── Cargo.toml                       — deps: aion, aion-proto, tonic, axum, tokio
└── src/
    ├── main.rs                      — [AW-006] thin binary entry (anyhow only here)
    ├── lib.rs                       — [AW-006] thin re-export surface
    ├── config.rs                    — [AW-006] Deserialize-only config types
    ├── error.rs                     — [AW-006] ServerError (thiserror)
    ├── state.rs                     — [AW-006] shared server state (engine handle, resolver)
    ├── namespace/
    │   ├── mod.rs                   — [AW-007] pub mod + re-exports only
    │   ├── resolver.rs              — [AW-007] NamespaceResolver: scope + authorise
    │   └── guard.rs                 — [AW-007] adapter-boundary enforcement
    ├── api/
    │   ├── mod.rs                   — [AW-008] pub mod + re-exports only
    │   ├── handlers.rs              — [AW-008] shared handler layer over Engine
    │   ├── grpc.rs                  — [AW-009] tonic service impl
    │   └── http.rs                  — [AW-009] axum HTTP/JSON facade
    ├── stream/
    │   ├── mod.rs                   — [AW-010] pub mod + re-exports only
    │   ├── subscribe.rs             — [AW-010] SubscriptionRequest -> EventFilter
    │   └── socket.rs                — [AW-010] WebSocket forward loop + lag handling
    ├── worker/
    │   ├── mod.rs                   — [AW-011] pub mod + re-exports only
    │   ├── registry.rs              — [AW-011] connected-worker registry by ns + type
    │   ├── dispatch.rs              — [AW-011] match task -> worker, feed result to engine
    │   └── heartbeat.rs             — [AW-012] heartbeat window + lost-worker handling
    └── dashboard/
        ├── mod.rs                   — [AW-013] pub mod + re-exports only
        └── assets.rs                — [AW-013] serve AU's static bundle
```

## Constraints

- **CO1** — `unsafe_code = "deny"` in both crates.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses per
  CLAUDE.md. Tests needing a running server gate at runtime (a `*_TEST_URL`
  env var with a logged skip), never `#[ignore]`.
- **CO3** — `lib.rs` / `mod.rs` are declarations and re-exports only.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace).
- **CO5** — `aion-proto` is a leaf crate: it depends on `aion-core` and
  external crates only — no `aion`, no `aion-server`, no store backend.
  Structural; must hold (per D2 and COMPONENT-ARCHITECTURE).
- **CO6** — `aion-server` depends on `aion`, `aion-proto`, and a store backend
  among workspace crates. It contains no workflow execution logic — it
  delegates every durable decision to the `Engine` (per D5 and
  COMPONENT-ARCHITECTURE boundary rule).
- **CO7** — Wire data types (proto messages, streamed events, summaries)
  derive `Serialize` + `Deserialize`; config types that carry secrets are
  `Deserialize`-only (D4, project credential policy).
- **CO8** — Library errors use `thiserror` (`WireError` in proto,
  `ServerError` in server); `anyhow` appears only in `aion-server`'s top-level
  `main`. No `.unwrap()` / `.expect()` in library code; lock/stream poison
  handled explicitly.
- **CO9** — No cross-namespace operation reaches the engine: namespace
  authorisation and scoping happen at the adapter boundary before any `Engine`
  call (per D8).
- **CO10** — A slow or stalled WebSocket consumer is dropped with a typed lag
  error; per-connection buffers are bounded; the engine broadcast is never
  back-pressured by a subscriber (per D6).
- **CO11** — No hardcoded operational defaults (ports, heartbeat window,
  buffer bound, timeouts): all come from config; engine-owned defaults are
  deferred to the engine (per CLAUDE.md "no assumed defaults").
- **CO12** — The wire `Event`/`WorkflowFilter`/`WorkflowSummary`/`WorkflowStatus`
  and ID types are `aion-core`'s types, not re-declared wire clones (per D2).

## Non-Goals

- **No workflow execution logic** — lifecycle, replay, timers, signals,
  queries, concurrency all live in `aion` (clusters AE/AD/AT). This cluster
  translates the network onto the `Engine` and adds nothing executable.
- **No engine event-broadcast implementation** — the engine appends and
  publishes events and exposes `subscribe(EventFilter)` (AE/AD). This cluster
  *tails* that broadcast over WebSocket; it does not implement the broadcast.
- **No worker-side SDKs** — the Rust/Python/TS *workers* are cluster **AR**.
  This cluster defines the worker protocol in `aion-proto` and the *server
  side* endpoint; it does not build the worker clients.
- **No caller client SDKs** — the Python/TS/Gleam/Rust *clients* are cluster
  **AL**. This cluster defines the API in `aion-proto`; it does not build the
  clients.
- **No dashboard frontend** — the dashboard UI is cluster **AU**. This cluster
  serves AU's built assets and backs them with the public API; it builds no UI.
- **No storage backend** — libSQL is AS, PostgreSQL is AX. The server selects
  a backend at deploy time and otherwise treats the store as an opaque
  `EventStore`.
- **No distributed coordination / sharding** — distributed mode (shard
  assignment across instances) is a later concern; this cluster targets the
  single-server standalone deployment and namespace isolation within it.
