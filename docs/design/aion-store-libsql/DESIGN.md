---
type: design
cluster: aion-store-libsql
title: Aion Store libSQL — The Default Durable Event Store
---

# Aion Store libSQL — The Default Durable Event Store

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map. This cluster
> implements the `EventStore` trait defined in cluster AC (`aion-store`)
> over libSQL. It consumes the domain types from `aion-core` and the
> contract and conformance suite from `aion-store`; it defines neither.

## Intention

When an engine operator embeds Aion and writes no storage configuration at
all, this is the store they get. It is the floor of the system: a single
file on disk, zero external infrastructure, full durability. The same crate
also serves the distributed path — the local file becomes an embedded
replica that syncs to a remote primary — without the operator learning a
second backend.

This cluster exists to make durability the easy default. The in-memory
reference store (`InMemoryStore`, cluster AC) proves the contract but
forgets everything on restart. A workflow that sleeps for three months and
resumes — Aion living in eternal time — needs its history to survive every
crash, every restart, every machine reboot in between. `LibSqlStore`
provides exactly that and nothing more: it is a faithful, durable
implementation of `EventStore`, held to the identical behavioural standard
as the oracle.

When this cluster is done, an operator writes `LibSqlStore::open("app.db")`
and has a production-grade durable event store. The engine cannot tell it
apart from the in-memory oracle except that it remembers. The shared
conformance suite from AC-007 passes against it unchanged, and a second,
durability-specific suite proves what the in-memory store structurally
cannot: that state survives a close and reopen.

## Problem

The `EventStore` contract (AC-005) is precise but unimplemented for
durability. `InMemoryStore` (AC-006) satisfies the contract behaviourally
but holds everything in process memory behind a mutex — it is the oracle,
not a deployable backend. Every real Aion deployment that does not bring its
own backend needs durable persistence, and the design names libSQL as that
default.

Three things make this non-trivial:

**Atomicity under a sequence guard.** The contract requires that `append`
apply a batch of events all-or-none, gated on an expected sequence: the
writer states the sequence it expects to follow, and the store must reject
the append — writing nothing — if the stored head has moved on. Over a SQL
backend this is a single transaction that reads the current head, compares
it to the expected sequence, and either inserts every event or rolls back
entirely. Getting the boundary wrong (a partial write, a head read outside
the transaction, a race between two writers) corrupts replay integrity
silently. This is the load-bearing operation of the whole crate.

**Faithful translation of a typed contract to SQL.** `read_history`,
`list_active`, `query`, `schedule_timer`, and `expired_timers` each map a
typed method to SQL over an append-only events table and a durable timers
table. `query` in particular must translate a `WorkflowFilter` (by type,
status, time range, parent) into a WHERE clause without leaking SQL concerns
back across the trait boundary, and `WorkflowStatus` is a projection over
events (CO7 from AC) — the store must derive it the same way the oracle
does, never inventing a stored status field that drifts from the event log.

**Two deployment shapes from one backend.** libSQL's Rust SDK offers a local
file database and an embedded replica that syncs to a remote primary. The
crate must expose both — pure-local embedded mode for zero-infra, and
embedded-replica sync for the distributed path — through one configuration
type, without the engine above caring which is in use.

If this crate is built ad hoc — its own idea of what an event is, its own
loose notion of atomicity, its own status logic — it desynchronises from the
oracle and from every other backend. It must instead be a transcription of
the AC contract into durable storage, verified against the AC conformance
suite as its correctness oracle.

## Solution

One crate, `aion-store-libsql`, depending only on `aion-store` (and through
it `aion-core`) plus the external `libsql` SDK and supporting crates. It
provides `LibSqlStore`: an `impl EventStore` over libSQL.

### Schema

Two tables, created idempotently on open via a small embedded migration:

- **`events`** — the append-only log. Columns: `workflow_id` (text),
  `seq` (integer, monotonic per workflow), `event` (blob — the serialised
  `Event`), `recorded_at` (text, RFC3339). Primary key `(workflow_id, seq)`
  so the per-workflow sequence is unique and ordered by construction, and a
  read of a workflow's history is an indexed range scan. A secondary index
  supports `query` and `list_active` projections.
- **`timers`** — the durable timer table. Columns: `workflow_id`,
  `timer_id`, `fire_at` (text, RFC3339). Primary key
  `(workflow_id, timer_id)` so re-scheduling the same timer replaces the
  prior row (idempotent under replay). An index on `fire_at` makes
  `expired_timers` a range scan.

Events are stored as serialised blobs, not exploded into columns. The store
is type-erased by design (D2 in AC): it persists the opaque `Event` and
reads it back, never interpreting payloads. Only the columns the store must
filter or order on — `workflow_id`, `seq`, `recorded_at`, and the small set
of summary fields needed for `query`/`list_active` — are first-class.

**Key decision — summary fields are denormalised onto lifecycle event
rows, not stored as a mutable workflow record.** `query` and `list_active`
need workflow type, status, start time, and end time without deserialising
every event. Rather than maintain a separate mutable `workflows` table
(which would risk drift from the event log, violating CO7), the store
derives summaries by reading the relevant lifecycle event rows (the
`WorkflowStarted` row carries type and start; the terminal event row carries
status and end). Status is computed from the last lifecycle event exactly as
`aion-core`'s projection defines it. Rejected: a mutable status column
updated on each append — it duplicates authority that belongs to the event
log and is the classic source of status/history skew.

### Atomic Append

`append` opens a libSQL transaction, reads the current maximum `seq` for the
workflow (the head), and compares the expected next sequence against it.
On mismatch it rolls back and returns `StoreError::SequenceConflict`,
writing nothing. On match it inserts every event in the batch with
contiguous sequence numbers and commits. The head read and the inserts live
in the same transaction, so two writers racing on the same expected sequence
resolve to exactly one commit and one conflict — the SQL backend's
transactional isolation, not application-level locking, provides the
guarantee.

**Key decision — the guard is enforced inside the transaction, not by a
pre-check.** Reading the head, comparing, and inserting are one atomic unit.
A pre-flight head read followed by a separate insert transaction would admit
a TOCTOU race that the contract forbids. Rejected: optimistic insert relying
solely on the `(workflow_id, seq)` primary-key uniqueness to surface
conflicts — it would conflate a genuine sequence conflict with other
constraint violations and could leave a partial batch if the collision
lands mid-insert.

### Configuration

A `LibSqlConfig` type, **`Deserialize`-only** (a config type, never
serialised back out), selects and tunes the mode:

- **Embedded** — a local file path. Zero infrastructure.
- **Embedded replica** — a local file path plus a remote primary URL and an
  auth token; the local database syncs to the primary. The distributed
  deployment path.

The config also exposes libSQL durability and WAL settings (sync interval
for replica mode, WAL/journal options for the local file) so an operator
tunes durability-versus-throughput per deployment, with no hardcoded
"sensible defaults" baked into the crate — values are surfaced for the
operator to set.

**Key decision — one config type, both modes, distinguished by an enum
variant, not two store types.** `LibSqlStore` is a single type; its
constructor takes a `LibSqlConfig` whose variant chooses embedded versus
embedded-replica. The engine above sees one store regardless. Rejected: a
separate `LibSqlReplicaStore` — it would duplicate the entire `EventStore`
impl for a difference that is purely how the underlying connection is
opened.

### Conformance and Durability Testing

The crate's correctness oracle is the **shared behavioural conformance
suite from AC-007** (`run_event_store_suite`). The crate runs that suite
against `LibSqlStore` over a temporary local file, as a real runtime test —
no external service is needed for embedded mode, so it is never gated. If
`LibSqlStore` deviates from `InMemoryStore` on any contract behaviour, the
shared suite fails.

Beyond the shared suite, the crate adds **persistence-across-reopen tests**
that the in-memory store structurally cannot cover: append history, drop the
store, reopen the same file, and assert the history, active list, and timers
are intact. This is the one behavioural axis the oracle cannot exercise, and
it is the entire point of a durable backend.

The **embedded-replica sync tests** need a remote libSQL primary. Those —
and only those — are runtime env-gated: they read an `AION_LIBSQL_TEST_URL`
(and token) env var, and when it is unset they emit a `tracing` skip line
and return `Ok(())` trivially. They are never `#[ignore]`d. The embedded
local-file tests, including the full conformance run, require no service and
always execute.

### The Turso-Native Path

The from-scratch Rust rewrite (Turso Database, formerly "Limbo") is still
**beta** as of mid-2026 — not production-ready. libSQL (the C fork with a
Rust SDK) is production-ready today, so this crate targets libSQL. Because
the `aion-store` trait abstracts the backend, swapping libSQL's internals
for Turso-native later — or adding a sibling `aion-store-turso` crate — is a
drop-in change with zero engine impact. This is captured as a decision so
the choice is deliberate and revisitable, not accidental.

## Structure

```
crates/aion-store-libsql/src/lib.rs          thin re-export surface
crates/aion-store-libsql/src/config.rs       LibSqlConfig (Deserialize-only) + mode enum
crates/aion-store-libsql/src/connection.rs   open embedded / embedded-replica connections
crates/aion-store-libsql/src/schema.rs       table DDL + idempotent migration
crates/aion-store-libsql/src/store.rs        LibSqlStore + EventStore impl wiring
crates/aion-store-libsql/src/append.rs       atomic append with the sequence guard
crates/aion-store-libsql/src/read.rs         read_history, list_active, query (filter -> SQL)
crates/aion-store-libsql/src/timer.rs        schedule_timer, expired_timers
crates/aion-store-libsql/src/error.rs        mapping libSQL/serde errors into StoreError

crates/aion-store-libsql/tests/conformance.rs   runs the AC-007 suite against LibSqlStore
crates/aion-store-libsql/tests/persistence.rs    reopen-after-close durability tests
crates/aion-store-libsql/tests/replica_sync.rs   embedded-replica sync (env-gated)
```

## Constraints

- **CO1** — `unsafe_code = "deny"`. No unsafe in the crate.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint or test bypasses
  per CLAUDE.md. Tests needing a remote primary use a runtime env-gate, not
  `#[ignore]`.
- **CO3** — `lib.rs` is declarations and re-exports only; logic lives in
  named modules.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace). The
  `EventStore` impl is split across `append`, `read`, and `timer` modules to
  honour this.
- **CO5** — The crate depends only on `aion-store` (and transitively
  `aion-core`) among Aion crates — no dependency on the engine, beamr, or
  any other backend. Structural; must hold.
- **CO6** — All database access is raw SQL inside this crate, which is
  permitted because this crate **is** a storage backend (it is the storage
  layer). No other crate may issue SQL against this store; they go through
  the `EventStore` trait.
- **CO7** — `WorkflowStatus` is derived from event history using
  `aion-core`'s projection. No mutable stored status field; the store must
  never let a cached projection diverge from the events.
- **CO8** — `LibSqlStore` must pass the shared AC-007 conformance suite
  unmodified. It is held to the same behavioural standard as the oracle; the
  suite is the correctness contract.
- **CO9** — `append`'s sequence guard and event inserts execute in a single
  libSQL transaction. A sequence mismatch writes nothing and returns
  `StoreError::SequenceConflict`; no partial batch is ever persisted.
- **CO10** — `LibSqlConfig` is `Deserialize`-only and carries no hardcoded
  durability/WAL defaults; tunable values are surfaced for the operator to
  set, per CLAUDE.md "no assumed defaults".
- **CO11** — All `libsql` and `serde` errors are mapped into the
  `StoreError` taxonomy (`Backend`, `Serialization`) at the crate boundary;
  no foreign error type leaks across the `EventStore` surface.

## Non-Goals

- No new `EventStore` trait, `StoreError`, `InMemoryStore`, or conformance
  suite — those are cluster AC, consumed here unchanged.
- No PostgreSQL backend — that is cluster AX.
- No engine, replay, timer-firing, or scheduling behaviour — the store
  persists and retrieves; the engine (cluster AE) and durability/replay
  (cluster AD) drive it. `expired_timers` returns due timers; it does not
  fire them.
- No Turso-native (Limbo) backend — libSQL today; Turso is a future drop-in
  swap once it leaves beta (recorded as decision D6).
- No event compaction or snapshotting — a later concern, shared across all
  backends, once long-running history size is a measured problem.
- No wire/gRPC concerns — the store reads and writes serialised `Event`
  blobs; the network protocol is `aion-proto` (cluster AW).
