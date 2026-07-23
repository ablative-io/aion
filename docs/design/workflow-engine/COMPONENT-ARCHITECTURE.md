# Aion — Component Architecture

> **Status note:** This architecture document includes both implemented surfaces
> and planned ecosystem shape. The engine recovery/timer seams and server
> WebSocket route are implemented in the current build; dashboard UX and some
> cross-language SDK transports are still hardening.

Companion to **DESIGN-OVERVIEW.md**. This document enumerates the crates,
packages, and their boundaries. It defines what we build and ship, who
consumes each piece, and how the pieces depend on one another.

---

## The Five Roles

Every component exists to serve one of five distinct roles. A given
person/system usually touches only one or two.

| Role | What they do | What they use |
|------|--------------|---------------|
| **Engine operator** | Deploys and runs the engine | `aion` (embed) or `aion-server` (standalone) |
| **Workflow author** | Defines workflows | `aion_flow` (Gleam) or AWL through `aion awl` |
| **In-VM activity author** | Writes native/BEAM activities | `aion-nif` (Rust), or plain Gleam |
| **Remote activity author** | Writes out-of-process activities | `aion-worker-*` (their language) |
| **Workflow caller** | Starts/signals/queries workflows | `aion-client-*` (their language) |

This separation is the organising principle. A frontend developer who only
triggers workflows needs `aion-client-typescript` and never sees beamr,
Gleam, or Rust. A workflow author needs only `aion_flow` and never touches
the engine internals.

---

## Component Map

```
# Aion

## Engine (Rust — crates.io)
- aion                  Core engine library. Embeds beamr, owns event
                        store wiring, replay, timers, signals, queries,
                        event publishing. The thing you embed.
- aion-server           Standalone binary. Wraps `aion` with HTTP/gRPC +
                        WebSocket API and the monitoring dashboard.
- aion-store            Event store trait + in-memory reference impl.
- aion-store-haematite  Default durable event store. Local in single-node
                        mode; quorum-replicated under `[store.cluster]`.
- aion-store-libsql     Alternative embedded libSQL event store.
- aion-proto            Shared wire types (events, requests) for client/
                        worker/server. gRPC + serde.
- aion-package          The `.aion` package format: read/write a single-file
                        archive of manifest + compiled .beam + (optional)
                        source + content hash.
- aion-toolchain        Optional. Shells out to the `gleam` binary to
                        compile, type-check, and package Gleam source into a
                        `.aion`. Unlocks server-side authoring endpoints.

## Workflow Authoring SDKs (compile to .beam, run on beamr)
- aion-awl              AWL parser, checker, canonical printer, schema
                        derivation, and compiler.
- aion-awl-lsp          Language Server Protocol adapter for AWL.
- aion-awl-package      Compiles and assembles AWL workflows into `.aion`
                        archives.
- aion_flow             Gleam package (Hex). Define workflows + activities.
                        The primary authoring surface.
- aion_flow_ex          Elixir package (Hex). Same concepts, idiomatic
                        Elixir. Later phase — depends on beamr Elixir
                        coverage.

## In-VM Activity Helpers (native, inside the BEAM)
- aion-nif              Rust helper crate for writing + registering NIFs
                        that Gleam/Elixir activities call. Deterministic
                        helpers and light in-VM activities.

## Remote Activity Worker SDKs (own runtime, network protocol)
- aion-worker           Rust remote-worker SDK.
- aion-worker-python    Python remote-worker SDK (PyPI).
- aion-worker-typescript TypeScript/Node remote-worker SDK (npm).

## Client SDKs (start / signal / query / cancel)
- aion-client           Rust client (also built into `aion` for embedded).
- aion-client-python    Python client (PyPI).
- aion-client-typescript TypeScript client (npm).
- aion_client           Gleam client (Hex).

## Foundation (existing)
- beamr                 The BEAM VM. Already exists. Aion depends on it.
- beamr-nif (or in-tree) NIF registration surface beamr already exposes.
```

---

## Engine Crates (Rust)

### `aion` — core engine library

The heart of the system. Everything else in the engine layer builds on it.

Responsibilities:
- Embed and configure the beamr runtime (scheduler threads, module loading)
- Workflow lifecycle: start, suspend, resume, cancel, complete
- Replay engine: reconstruct workflow state from event history
- Timer service: durable timers backed by the event store + beamr's wheel
- Signal router: deliver signals to workflow process mailboxes
- Query service: dispatch queries to workflow processes, collect replies
- Event publisher: broadcast appended events for real-time streaming
- The Rust embedding API (`Engine`, `EngineBuilder`, handles)
- The built-in Rust client surface (start/signal/query/cancel)

Depends on: `beamr`, `aion-store`, `aion-proto`.

Public API sketch:
```rust
let engine = EngineBuilder::new()
    .store(store)                    // impl EventStore
    .scheduler_threads(num_cpus::get())
    .load_workflows("ebin/")?        // compiled .beam modules
    .register_nifs(my_nifs)?         // in-VM activity NIFs
    .build()
    .await?;

let id = engine.start_workflow("dev_workflow", "run", input).await?;
engine.signal(&id, "approval", payload).await?;
let state = engine.query(&id, "current-step").await?;
engine.cancel(&id, "user requested").await?;
let result = engine.result(&id).await?;
let mut stream = engine.subscribe(EventFilter::workflow(id));
```

Boundary rule: `aion` is transport-agnostic. It has no HTTP, gRPC, or
WebSocket code. That lives in `aion-server`. This keeps the embedded path
free of network dependencies.

### `aion-server` — standalone binary

A thin wrapper that exposes `aion` over the network. Owns the operational
surface that embedded users don't need.

Responsibilities:
- HTTP/gRPC API for start/signal/query/cancel/list
- WebSocket endpoint for real-time event streaming
- Remote worker protocol endpoint (task dispatch to out-of-process workers)
- Monitoring dashboard (workflow list, history viewer, live event feed)
- Multi-tenancy / namespace isolation (server mode only)
- Haematite cluster boot with configured membership, quorum replication,
  static shard ownership, request forwarding, and automatic adoption of a
  declared dead peer's shards
- Config: store selection, ports, auth, TLS

Depends on: `aion`, `aion-proto`, `aion-store-*` (selected at deploy).

Boundary rule: `aion-server` contains no workflow execution logic of its
own — it delegates everything to `aion`. It is purely the network and
operations skin.

### `aion-store` — persistence contract

Defines the `EventStore` trait (see DESIGN-OVERVIEW.md) and a small set of
shared types (`Event`, `WorkflowId`, `TimerId`, `WorkflowFilter`,
`WorkflowSummary`, `StoreError`). Ships an in-memory reference
implementation for tests.

Depends on: nothing in the Aion tree (leaf crate). This is deliberate — the
trait must be implementable by external crates (Meridian, third parties)
without pulling in the engine.

### `aion-store-haematite` — default durable backend

`impl EventStore` over haematite. With no `[store.cluster]` section it opens
or creates a local data directory and owns all shards. In distributed mode it
routes appends through quorum replication, scopes enumeration to configured
shard ownership, and supports survivor adoption after a declared peer dies.

Cluster membership, peer addresses, and shard ownership are configuration,
not discovery. The virtual `shard_count` is fixed when a database is created;
there is no reshard path.

Depends on: `aion-store`, `haematite`.

### `aion-store-libsql` — alternative backend

`impl EventStore` over libSQL (via the `libsql` Rust SDK). It provides a local
database file with zero external infrastructure and remains selectable as the
lightweight alternative to the default haematite backend.

Depends on: `aion-store`.

### `aion-package` — the `.aion` package format

Read/write the single-file workflow package. A `.aion` file is an archive
(zip container) holding:
- **manifest** — entry module, entry function, input/output schema,
  timeout, declared activities, version hash
- **compiled `.beam` files** — the workflow module + its dependencies +
  the stdlib beams it needs
- **source** (optional) — the `.gleam` source, for inspection and
  recompilation
- **content hash** — hash of the compiled beams; serves as the workflow's
  version identifier and an integrity check

The engine ingests a `.aion` by reading the manifest, unpacking the beams
into beamr's module loader, and registering. One file in, deployable
workflow out. This format is the connective tissue between **deployment**
(copy one file), **versioning** (the content hash is the version), and
**hot code loading** (the `.aion` is the unit of zero-downtime upgrade).

Depends on: `aion-store` (for version records) — otherwise standalone.

### `aion-toolchain` — optional server-side authoring (proposed)

Shells out to the `gleam` binary to compile, type-check, and package Gleam
source server-side. When `aion-server` is configured with a path to the
`gleam` binary, this unlocks authoring endpoints: submit source → compile +
type-check → report errors → package as `.aion` → (optionally) hot-load.

Rationale for shelling out rather than embedding: the Gleam compiler is
written in Rust but is structured as a binary, not a stable embeddable
library crate. The binary is a single self-contained executable, trivial to
bundle. Keeping this optional keeps the core engine free of the toolchain
dependency — without it, you deploy pre-compiled `.aion` files.

The full authoring loop it enables: **write Gleam source → server compiles
+ type-checks → packages as `.aion` → hot-loads → runs** — a better
authoring experience than Temporal offers.

Depends on: `aion-package`; invokes the external `gleam` binary.

### `aion-proto` — wire types

Shared serialisation types for the client/worker/server boundary. gRPC
service definitions (`.proto`) and the corresponding serde types. Used by
`aion-server`, all client SDKs, and all worker SDKs so the wire contract is
defined once.

Depends on: nothing in the Aion tree (leaf crate).

---

## Workflow Authoring SDKs (Gleam / Elixir → .beam)

### `aion_flow` — Gleam SDK (primary)

Published to Hex. The library workflow authors import. Pure Gleam — it does
not embed beamr or the engine. Its functions are backed by `@external`
declarations (Erlang target) that resolve at runtime when beamr loads the
compiled workflow and the engine has registered the corresponding NIFs.

Modules:
```
aion/workflow   run, all, race, map, spawn, spawn_and_wait,
                receive, sleep, start_timer, cancel_timer,
                now, random, with_timeout
aion/activity   new, retry, timeout, heartbeat, RetryPolicy,
                Backoff (Exponential | Linear | Fixed)
aion/signal     send, receive, SignalRef
aion/query      handler registration, reply
aion/child      spawn, await, ChildHandle
aion/error      RetryableError, TerminalError classification
aion/testing    time simulation, activity mocking, replay assertions
```

Type-safety guarantee: activity inputs/outputs, signal payloads, and query
returns are all statically typed. Invalid compositions fail at compile
time.

Depends on: `gleam_stdlib`, and (for `@external`) the runtime NIFs the
engine registers.

### `aion_flow_ex` — Elixir SDK (later phase)

The same workflow/activity/signal/query/timer/child concepts, expressed
idiomatically in Elixir, compiled to `.beam`, run on the same engine.

Blocked on: beamr expanding its opcode + BIF coverage to what Elixir's
compiler emits for workflow-style code (scope assessment owned by Bono).
Distinct Hex package name from `aion_flow` since Gleam and Elixir share the
Hex registry.

---

## In-VM Activity Helpers (native, inside the BEAM)

### `aion-nif` — Rust NIF helper

A helper crate that makes it easy to write native Rust functions and
register them as NIFs callable from Gleam/Elixir workflows. Two use cases
(see DESIGN-OVERVIEW.md "Execution Tiers"):

1. **Deterministic helpers** — JSON transform, template rendering, parsing,
   crypto, formatting. Called inline from workflow code. Not recorded.
2. **Light in-VM activities** — small side-effectful operations (read a
   file, run a quick command) invoked through the activity contract so the
   result is recorded.

Provides macros/builders for declaring a NIF, mapping BEAM terms to/from
Rust types, and registering with the engine. Wraps the beamr NIF surface
that `beamr-meridian` already demonstrates.

Depends on: `beamr` (NIF API), `aion` (activity contract for the recorded
case).

Open question: whether to fold this into the `aion` crate behind a feature
flag. Recommendation: keep separate — an activity author who only writes
NIFs should not pull the whole engine.

---

## Remote Activity Worker SDKs (out-of-process, any language)

These connect to a running engine (via `aion-server`'s worker protocol),
receive activity tasks, execute them in their own runtime, and return
results. Always Tier 3. Independent of Gleam's compilation target — this is
how JS/TS/Python/Go do heavy lifting.

### `aion-worker` — Rust

Out-of-process Rust activities. For Rust work that should be isolated from
the engine process or scaled independently.

### `aion-worker-python` — Python (PyPI)

Python activity workers. The home for ML inference, data-science libraries,
and existing Python business logic.

### `aion-worker-typescript` — TypeScript/Node (npm)

TS/Node activity workers. For JS-ecosystem work and teams that live in
Node.

All worker SDKs share the same protocol defined in `aion-proto`: register
activity types, poll/receive tasks, report completion/failure, send
heartbeats. The protocol (polling vs push, gRPC vs WebSocket, heartbeat
semantics) is an open question in DESIGN-OVERVIEW.md.

---

## Client SDKs (start / signal / query / cancel)

For application code that drives workflows from the outside. Distinct from
worker SDKs (which implement activities) and authoring SDKs (which define
workflows).

### `aion-client` — Rust

Built into the `aion` crate for the embedded case (direct API calls). Also
available as a standalone client for talking to `aion-server` over the
network.

### `aion-client-python` — Python (PyPI)
### `aion-client-typescript` — TypeScript (npm)
### `aion_client` — Gleam (Hex)

Thin HTTP/gRPC clients over the `aion-server` API: start a workflow, send a
signal, run a query, cancel, list. Any language with an HTTP client can do
this without a dedicated SDK; these exist for ergonomics and type safety in
their ecosystems.

---

## Dependency Graph

```
                         beamr  (existing VM)
                           ^
                           |
        +------------------+------------------+
        |                  |                  |
    aion-nif             aion            aion_flow (Gleam, @external)
        |                  ^                  |
        |        +---------+---------+        | (runtime binding via beamr)
        |        |         |         |        |
        |   aion-store  aion-proto   |        |
        |        ^         ^         |        |
        |        |         |         |        |
        |  aion-store-haematite  aion-store-libsql
        |        |
        +--------+
                 |
            aion-server  ----<gRPC/WS>----  aion-worker-*  (Python/TS/Rust)
                 |                            aion-client-*  (Python/TS/Gleam)
                 |
            (dashboard, HTTP/gRPC/WS API)
```

Key boundaries:
- `aion-store` and `aion-proto` are leaf crates (no Aion deps) so external
  implementors can target them cleanly.
- `aion` is transport-agnostic; networking lives only in `aion-server`.
- Authoring SDKs (`aion_flow`) bind to the engine at runtime through beamr's
  NIF resolution, not through a compile-time Rust dependency.
- Worker and client SDKs depend only on `aion-proto`'s wire contract, never
  on the engine internals.

---

## Naming and Registry Notes

- `aion` (crates.io) and `aion_flow` (Hex) can share the conceptual name
  across ecosystems because they live in different registries — the
  Temporal precedent (`temporalio` across npm, PyPI, Maven, crates).
- Gleam and Elixir both publish to **Hex**, so `aion_flow` (Gleam) and the
  Elixir SDK need distinct package names — hence `aion_flow_ex` for Elixir.
- Before locking the name in, verify availability on crates.io, Hex, PyPI,
  and npm. Known collisions to check: the NCsoft "Aion" MMORPG and a few
  crypto/blockchain projects — unlikely to cause confusion in this domain
  but worth confirming the package names are free.

---

## Build & Ship Order (indicative, not a commitment)

This is dependency ordering, not a phased rollout plan (the design is for
the ideal end state; sequencing is a separate exercise).

1. `aion-store` + `aion-store-haematite` — persistence contract + default
2. `aion-package` — the `.aion` format (load path needed early by the engine)
3. `aion` core — engine on beamr, embedded Rust API, replay, timers,
   signals, queries
4. `aion_flow` (Gleam) — authoring SDK + `aion-nif` helper
5. `aion-proto` + `aion-server` — network API, WebSocket streaming,
   dashboard
6. `aion-client-*` — caller SDKs
7. `aion-worker` + `aion-worker-*` — remote activity workers
8. `aion-toolchain` — optional server-side compile/check/package
9. `aion-store-libsql` — alternative embedded backend
10. Hot code loading in beamr (Bono's 3-4 briefs) — zero-downtime upgrades
11. `aion_flow_ex` (Elixir) — second authoring language

---

## Open Architecture Questions

1. **`aion-nif` folding** — separate crate or feature flag on `aion`?
   (Recommendation: separate.)
2. **Worker transport** — gRPC vs WebSocket vs both for the remote worker
   protocol.
3. **Client codegen** — hand-write each client SDK, or generate from
   `aion-proto`?
4. **Dashboard packaging** — bundled into `aion-server` binary, or a
   separate static frontend served alongside?
5. **Store trait granularity** — single `EventStore` trait, or split into
   `EventStore` + `TimerStore` + `VisibilityStore` so backends can
   implement subsets?
6. **Meridian's store** — does Meridian implement `aion-store` directly, or
   do we ship a `meridian-aion` adapter crate in the Meridian tree?
