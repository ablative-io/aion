---
type: design
cluster: aion-core
title: Aion Core — Domain Model and Persistence Contract
---

# Aion Core — Domain Model and Persistence Contract

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

This cluster defines the vocabulary that every other part of Aion speaks.
The event types, the identifiers, the workflow status enum, the error
taxonomy, and the persistence contract all live here. When the engine
records that an activity completed, when a client asks what workflows are
running, when a store implementation persists history, when a worker
reports a result — they all use the types defined in this cluster.

It must feel inevitable. An engineer reading the `Event` enum should
understand the entire lifecycle of a workflow from the variant names alone.
A store author implementing `EventStore` should find the trait so clear
that there is only one reasonable way to implement it correctly. The types
should make illegal states unrepresentable: a completed workflow cannot
also be running, an activity result cannot exist without an activity
schedule preceding it.

When this cluster is done, the rest of Aion can be built against stable
contracts. Nothing here depends on beamr, on the engine, on networking, or
on any storage backend. This is the still point at the centre — pure types
and one trait.

## Problem

A durable workflow engine is, at its heart, an event-sourcing system. The
authoritative state of every workflow is a sequence of events. If those
events are modelled inconsistently — different shapes in the engine, the
store, the wire protocol, and the replay logic — the system rots from the
inside. Replay desynchronises from recording. A store backend silently
drops a field. The wire protocol can't represent an event the engine
produces.

Aion has many independent consumers of the event model: the engine
(produces events), store backends (persist and retrieve them), the replay
engine (reconstructs state from them), the WebSocket layer (streams them),
clients (query summaries derived from them), and workers (produce activity
results that become events). Every one of these must agree, exactly, on
what an event is. There is no room for a second definition.

The persistence contract has the same problem. The engine must not know or
care whether events are stored in libSQL, PostgreSQL, or an in-memory map.
It must talk to storage through one trait, and that trait must be
implementable by external crates — including Meridian's storage layer and
third-party backends — without pulling in the engine.

This must be settled first, before any engine or SDK code exists, because
everything downstream references it.

## Solution

Two crates, both leaf-level (no Aion dependencies of their own):

- **`aion-core`** — the domain model. Pure types: events, identifiers,
  status, filters, summaries, errors. No behaviour beyond constructors,
  conversions, and trivial accessors. Depended on by everything.
- **`aion-store`** — the persistence contract. The `EventStore` trait, its
  associated error type, and an in-memory reference implementation for
  tests. Depends on `aion-core` and nothing else.

The split matters: clients and workers need the domain types but must never
link the store trait. Keeping types separate from the trait keeps the
dependency graph honest.

### The Event Model

The `Event` enum is the spine of the system. Each variant records one
externally-observable fact in a workflow's life. Events are immutable once
recorded and are only ever appended, never mutated or deleted.

The variants cover the full lifecycle:

- **Workflow lifecycle** — `WorkflowStarted`, `WorkflowCompleted`,
  `WorkflowFailed`, `WorkflowCancelled`.
- **Activity lifecycle** — `ActivityScheduled`, `ActivityStarted`,
  `ActivityCompleted`, `ActivityFailed`.
- **Timers** — `TimerStarted`, `TimerFired`, `TimerCancelled`.
- **Signals** — `SignalReceived`.
- **Child workflows** — `ChildWorkflowStarted`, `ChildWorkflowCompleted`,
  `ChildWorkflowFailed`.

Each event carries an envelope: a monotonic per-workflow sequence number, a
recorded timestamp, and the workflow ID it belongs to. The sequence number
is the basis for optimistic concurrency on append (a writer states the
sequence it expects, the store rejects the append if reality has moved on).

The recorded timestamp is the source of determinism for time: `workflow.now`
returns the timestamp of the event being processed, never the wall clock.
This is why the timestamp lives in the envelope of every event, not just
the lifecycle ones.

**Key decision — events carry serialised payloads, not typed generics.**
Activity inputs and results, signal payloads, and workflow inputs are stored
as opaque serialised values (a `Payload` newtype over bytes plus a
content-type tag), not as generic type parameters. The engine and store are
type-erased; only the SDK layer knows the concrete Gleam types. This keeps
`aion-core` free of generic sprawl and lets a single `Event` type flow
through the store, the wire protocol, and replay without monomorphisation.
Rejected: a generic `Event<T>` — it would force the type parameter through
the entire engine and store, and a store cannot be generic over every
workflow's payload types simultaneously.

### Identifiers

- `WorkflowId` — globally unique, assigned at workflow start. Wraps a UUID.
- `ActivityId` — unique within a workflow. The scheduling sequence position
  doubles as the activity's identity, so replay can match a recorded result
  to the activity call deterministically.
- `TimerId` — unique within a workflow, author-assignable (for named,
  cancellable timers) or engine-assigned (for anonymous sleeps).
- `RunId` — distinguishes successive runs of the same logical workflow when
  a workflow is reset or continued-as-new. A `WorkflowId` plus a `RunId`
  identifies one concrete execution.

**Key decision — IDs are newtypes, not bare strings or UUIDs.** Every
identifier is a distinct newtype so the compiler rejects passing a
`TimerId` where a `WorkflowId` is expected. The cost is a little boilerplate;
the benefit is that a whole class of mix-up bugs cannot compile.

### Workflow Status

`WorkflowStatus` is the derived, queryable state of a workflow: `Running`,
`Completed`, `Failed`, `Cancelled`, `TimedOut`. Status is not stored as a
mutable field — it is a projection over the event history (the last
lifecycle event determines it). The store may cache it for query
performance, but the events remain authoritative.

### Filters and Summaries

- `WorkflowFilter` — the query surface for listing workflows: by type, by
  status, by time range, by parent. Used by `EventStore::query` and the
  client/server list APIs.
- `WorkflowSummary` — the lightweight projection returned by queries:
  ID, type, status, start time, end time. Enough to render a dashboard row
  without loading the full history.

### The EventStore Trait

The single persistence contract. Defined in `aion-store`, implemented by
every backend. Async, `Send + Sync + 'static`.

Responsibilities:

- **`append`** — append events to a workflow's history, atomically, with an
  expected-sequence guard for optimistic concurrency. All events in a call
  land together or none do.
- **`read_history`** — return the complete ordered event history for a
  workflow. The input to replay.
- **`list_active`** — return the IDs of all non-terminal workflows. Used on
  engine startup to know what to replay.
- **`query`** — return summaries matching a `WorkflowFilter`.
- **`schedule_timer`** / **`expired_timers`** — persist durable timers and
  retrieve those due to fire. The durable timer substrate.

**Key decision — one trait, not several.** Timers live on the same trait as
events rather than a separate `TimerStore`. A workflow's timers and events
are written in the same logical transaction (scheduling a timer is itself an
event), so splitting them across traits would fracture atomicity. A backend
that wants to store timers in a separate table still can — that's its
internal choice — but the contract keeps them together. Rejected: separate
`EventStore` + `TimerStore` + `VisibilityStore` traits — premature
decomposition that complicates the atomic-append guarantee and the common
case (one backend serving all three).

**Key decision — the store is told the expected sequence; it does not
invent it.** `append` takes `expected_seq` and fails with a conflict error
if the stored head doesn't match. This makes the store a dumb, correct
ledger and puts sequencing authority in the engine, where the single writer
per workflow lives. Rejected: store-assigned sequence numbers — they would
hide write conflicts that the engine must see to preserve replay integrity.

### The In-Memory Reference Store

`aion-store` ships `InMemoryStore` — a correct, non-durable `EventStore`
backed by maps and a mutex. It is the reference implementation: the
behaviour every other backend must match. The cross-backend equivalence
tests (in later store clusters) run against it as the oracle. It is also
what the engine's own tests use, so engine tests need no database.

### Error Taxonomy

- `aion-core` defines the domain errors that appear inside events
  (`ActivityError` with a retryable/terminal classification, `WorkflowError`).
- `aion-store` defines `StoreError` — the errors an `EventStore` can return:
  `SequenceConflict`, `NotFound`, `Backend` (wrapping an implementation
  error), `Serialization`.

The retryable/terminal split on `ActivityError` is load-bearing: the engine
consults it to decide whether a failed activity is retried per its policy or
fails the workflow. Modelling it in the type, not in a string or a bool,
makes the contract explicit at every boundary it crosses.

## Structure

```
crates/aion-core/src/lib.rs          thin re-export surface
crates/aion-core/src/event.rs        Event enum + envelope + variants
crates/aion-core/src/payload.rs      Payload newtype + content-type tag
crates/aion-core/src/ids.rs          WorkflowId, ActivityId, TimerId, RunId
crates/aion-core/src/status.rs       WorkflowStatus + projection from events
crates/aion-core/src/filter.rs       WorkflowFilter + WorkflowSummary
crates/aion-core/src/error.rs        ActivityError, WorkflowError

crates/aion-store/src/lib.rs         thin re-export surface
crates/aion-store/src/store.rs       EventStore trait
crates/aion-store/src/error.rs       StoreError
crates/aion-store/src/memory.rs      InMemoryStore reference impl
crates/aion-store/src/timer.rs       TimerEntry + timer-facing types
```

## Constraints

- **CO1** — `unsafe_code = "deny"`. No unsafe in either crate.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses.
- **CO3** — `mod.rs` / `lib.rs` are declarations and re-exports only.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace).
- **CO5** — `aion-core` and `aion-store` are leaf crates: they depend on no
  other Aion crate. This is structural and must hold.
- **CO6** — Every type that crosses the wire or the store derives both
  `Serialize` and `Deserialize`. (Distinct from Meridian's config policy —
  these are data types, not credentials.)
- **CO7** — `WorkflowStatus` is always derivable from event history. No code
  path may set status independently of the events that justify it.
- **CO8** — The `InMemoryStore` must pass the same behavioural test suite
  that future durable backends will be held to. It is the oracle, so it must
  be correct, not merely convenient.
- **CO9** — Identifiers are newtypes; no bare `Uuid` or `String` in public
  signatures where an ID is meant.

## Non-Goals

- No durable storage backend here — libSQL is cluster AS, PostgreSQL is AX.
- No replay logic — that is cluster AD. This cluster defines the events
  replay consumes, not the replay itself.
- No wire/serialisation protocol for the network — that is `aion-proto` in
  cluster AW. These types derive serde; the gRPC mapping is separate.
- No engine behaviour — no lifecycle management, no scheduling. Cluster AE.
- No event compaction or snapshotting — a later concern once long-running
  workflow histories become a measured problem.
