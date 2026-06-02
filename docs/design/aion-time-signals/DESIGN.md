---
type: design
cluster: aion-time-signals
title: Aion Time, Signals & Queries — Live Interaction and Concurrency Mechanics
---

# Aion Time, Signals & Queries — Live Interaction and Concurrency Mechanics

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

This cluster builds the surfaces through which a running workflow touches
the outside world and coordinates concurrent work. A workflow sleeps for a
month and wakes. An external system signals an awaiting workflow and the
workflow continues in microseconds. An operator inspects a live workflow
without perturbing it. A parent spawns children and fans work out across the
BEAM. Timers, signals, queries, child workflows, and the `all`/`race`/`map`
primitives are the live half of durable execution.

The discipline that makes durable execution work runs straight through here:
**every interaction that changes what a workflow observed is recorded as an
`aion-core` `Event`; every interaction that merely observes records nothing.**
A timer firing, a signal arriving, a child completing — these change what the
workflow saw, so they become `TimerFired`, `SignalReceived`,
`ChildWorkflowCompleted`. A query reads state and changes nothing, so it is
never an event. Get this wrong in either direction and replay desynchronises:
a missed recording means replay waits forever for something live execution
already saw; a spurious recording means replay returns an observation that
never happened.

These mechanisms must be fast on the live path and exact on the recovery
path. Fast: delivery rides beamr's native mailbox and timer wheel, not a
store round-trip. Exact: the durable record carries precisely what `AD`'s
replay needs to return the same observation on restore. When this cluster is
done, a workflow can sleep, wait, be queried, spawn children, and fan out —
durably, concurrently, and at BEAM speed.

## Problem

A durable workflow engine must reconcile two properties that pull in
opposite directions: **microsecond live latency** and **exact recovery after
a crash**. Each interaction surface hits this tension differently.

A workflow may sleep for milliseconds or for months, and the sleep must
survive a VM restart. An in-process timer-wheel entry is lost on restart, so
a durable record is required — but consulting the store on every short sleep
would throw away the BEAM's microsecond latency. One mechanism cannot serve
both the live path and the recovery path well.

A signal must reach the workflow process's mailbox in microseconds during
normal execution, yet must also be durable: if the VM restarts after the
signal is recorded but before the workflow consumed it, replay must
redeliver it without the external sender sending again. The ordering of
"record" and "deliver" is load-bearing — get it backwards and a consumed but
unrecorded signal is lost on crash.

A query must read a live workflow's state without recording an event,
without blocking the workflow's progress, and without any risk to
determinism. A read that accidentally became an event would desynchronise
replay forever.

Child workflows and the fan-out primitives all need to spawn linked BEAM
processes, collect results via selective receive, and propagate cancellation
when the workflow dies or a race loses — and each spawn and result must be
recorded so replay reconstructs the same set of children with the same
outcomes.

All of this sits at the seam between the **engine (AE)**, which owns the
workflow process lifecycle and supervision, and **durability (AD)**, which
owns event append and replay. The live-delivery and durable-recording
mechanics must be defined precisely enough that AE can drive them and AD can
replay them, without this cluster owning either lifecycle or replay.

## Solution

A set of engine-side services within the `aion` crate, each pairing a live,
BEAM-native delivery path with a durable `aion-core` `Event`. They consume an
**engine seam** (a handle/trait owned by AE) to resolve a `WorkflowId` to a
live process, deliver to a mailbox, request a child spawn, and arm/disarm a
timer-wheel entry. They never manage process lifecycle themselves.

### The Engine Seam

This cluster does not own workflow processes. It consumes a narrow,
explicit engine-facing handle that exposes exactly four capabilities:

- resolve a `WorkflowId` to a live process handle (or report it
  non-resident / terminal / unknown),
- deliver a message to a workflow process's mailbox,
- request AE to start a child workflow execution as a linked process,
- arm and disarm a beamr timer-wheel entry for a given fire time.

Everything in this cluster is expressed against that seam. AE provides the
implementation; this cluster's tests provide a fake. This keeps the
lifecycle/supervision boundary clean (CO6) and lets the whole cluster be
tested against `InMemoryStore` plus a fake engine handle.

### Two-Tier Timers

Every durable timer is **both** recorded in the event store and armed on
beamr's timer wheel.

- The **wheel** gives millisecond-granularity firing during normal
  execution with zero store reads. When the wheel fires, the timer service
  records `TimerFired` and delivers it to the owning workflow process's
  mailbox.
- The **record** (`EventStore::schedule_timer` with the `TimerId` and
  `fire_at`, plus a `TimerStarted` event) is the recovery source. On engine
  startup and on a periodic recovery tick, the service polls
  `EventStore::expired_timers(now)` and, for each entry whose `TimerFired`
  has not yet been recorded, records `TimerFired` and delivers it.

The two paths are reconciled by an **exactly-once** rule (D1, CO7): a timer
already recorded as fired by the wheel is skipped by recovery, and a timer
fired by recovery (because its process was not resident on the wheel — e.g.
it slept across a restart) is not double-fired by a stale wheel entry.
Recovery is idempotent.

**Named vs anonymous.** `start_timer` takes an author-assigned `TimerId`; it
is durable and cancellable. `cancel_timer` disarms the wheel entry and
records `TimerCancelled`; cancelling an already-fired timer is a no-op, not
an error. `sleep` is an anonymous timer with an engine-assigned `TimerId`
derived from sequence position — not separately cancellable (cancelling a
sleep means cancelling the workflow).

### Signal Router

The router **records `SignalReceived`** (signal name + `Payload`)
before/atomically with delivery, then **delivers the signal to the target
workflow process's mailbox** so the workflow's selective receive picks it up
in microseconds. Record-before-deliver (D4) is deliberate: a crash after
delivery but before consumption is recoverable because the event is already
durable; the reverse ordering would lose a consumed-but-unrecorded signal.

If the target process is not resident (suspended awaiting replay, being
restored), the router still records `SignalReceived`; the signal is delivered
to the mailbox once AE makes the process resident, or returned by AD's replay
when the workflow re-reaches its `receive`. Signalling a terminal or unknown
workflow returns a typed error and records nothing.

The router resolves a `WorkflowId` to a process via the engine seam; it does
not manage residency itself.

### Query Service

A query is dispatched to the workflow process as a **distinct message kind**
and answered from a **registered query handler**, replying on a one-shot
reply channel. **No event is ever appended for a query** (D6, CO7). The query
is answered at a yield point so it never preempts in-progress deterministic
logic mid-step, and it never mutates workflow state.

Failure modes are explicit (D7): an unknown query name returns a typed
`QueryError` (no panic); a query that gets no reply within the
engine-configured timeout returns `QueryError::Timeout` rather than hanging;
querying a terminal or unknown workflow returns a typed `NotRunning`/`Unknown`
error and never replays the workflow solely to answer.

### Child Workflow Spawning

Spawning a child asks AE (via the seam) to start a new workflow execution —
a distinct `WorkflowId`/`RunId` with its own event history — as a process
**linked** to the parent, and records `ChildWorkflowStarted` in the parent's
history. The parent then either:

- **awaits** the result via its mailbox (`spawn_and_wait`), recording
  `ChildWorkflowCompleted` on success or `ChildWorkflowFailed` on child
  failure; or
- **fires and forgets** — records `ChildWorkflowStarted` and detaches
  (monitor rather than blocking await).

Cancelling the parent propagates over the link to the child. This cluster
owns the spawn-request, result-collection, and recording mechanics; AE owns
the child's lifecycle and supervision.

### Concurrency Primitives: all / race / map

The fan-out primitives are **selective-receive collectors over linked
children** — no bespoke join runtime, because the BEAM mailbox and links
already provide fan-out, collection, and cancellation (D9).

- **`all`** spawns N linked children, blocks in selective receive until all
  N results arrive, and returns them **in input order**. Any child failure
  fails the whole `all` and cancels the remaining children.
- **`race`** spawns N, returns the **first** result to arrive, then cancels
  the remaining children and records their cancellation.
- **`map`** applies a function to each element of a runtime list to produce
  the child specs (dynamic fan-out), then collects like `all` (ordered,
  fail-fast).

Each spawned child carries a **per-spawn correlation token** derived
deterministically from the spawning sequence position (D11), carried in its
result message, so selective receive matches an arriving result to the
correct spawn position even when many children of the same type run
concurrently. The deterministic derivation means replay matches recorded
results to spawn positions identically.

**Cancellation** (D10) terminates losing/remaining linked children via their
links (exit signal) and records the cancellation so replay reconstructs which
children were started and which were cancelled rather than completed. The
workflow process traps exits, so a child exit arrives as a message it can
record and act on, not as a fatal signal.

### The AT/AD Seam

This cluster defines the **live** production and durable recording of
`TimerStarted`/`TimerFired`/`TimerCancelled`, `SignalReceived`, and the
`ChildWorkflow*` events, and the live delivery over mailbox and timer wheel.
**AD defines replay** of those same events: on restore, `sleep` returns
immediately for an already-fired timer, `receive` returns a recorded signal
without waiting, and an awaited child returns its recorded result without
re-spawning. This cluster does not implement replay; it guarantees the
recorded events carry exactly what replay needs — timer id + `fire_at`,
signal name + payload, child id + result/error, and the spawn correlation
token (D12, S13).

### The AT/AE Seam

AE owns workflow lifecycle, process management, supervision, and module
loading; this cluster consumes the engine seam for residency resolution,
mailbox delivery, child-spawn requests, and wheel arm/disarm (CO6). The
boundary is structural: no code in this cluster spawns, supervises, or tears
down a workflow process directly — it always goes through the seam, which AE
implements.

## Structure

```
crates/aion/src/engine_seam.rs            engine-facing handle/trait: residency,
                                          child spawn, mailbox deliver, wheel arm/disarm
crates/aion/src/time/mod.rs               timer module declarations + re-exports
crates/aion/src/time/timer_service.rs     durable timer: schedule + wheel arm + TimerFired
crates/aion/src/time/recovery.rs          expired_timers polling on startup + periodic tick
crates/aion/src/time/named.rs             start_timer / cancel_timer / sleep
crates/aion/src/signal/mod.rs             signal module declarations + re-exports
crates/aion/src/signal/router.rs          record SignalReceived + mailbox delivery
crates/aion/src/signal/resume.rs          non-resident delivery + resume handoff
crates/aion/src/query/mod.rs              query module declarations + re-exports
crates/aion/src/query/service.rs          dispatch + handler reply + timeout/unknown/not-running
crates/aion/src/child/mod.rs              child-workflow module declarations + re-exports
crates/aion/src/child/spawn.rs            linked spawn, ChildWorkflow* events, await + fire-and-forget
crates/aion/src/concurrency/mod.rs        concurrency module declarations + re-exports
crates/aion/src/concurrency/correlation.rs per-spawn correlation tokens + matching
crates/aion/src/concurrency/all.rs        all: fan-out, ordered collect, fail-fast
crates/aion/src/concurrency/race.rs       race: first wins, cancel + record the rest
crates/aion/src/concurrency/map.rs        map: dynamic fan-out from a runtime list
```

## Constraints

- **CO1** — `unsafe_code = "deny"` across the cluster's modules.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses.
- **CO3** — `lib.rs` / `mod.rs` are declarations and re-exports only.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace).
- **CO5** — Library errors use `thiserror`; no `anyhow` in library code; no
  `unwrap`/`expect` in library code; lock poisoning handled explicitly.
- **CO6** — These modules live in the `aion` crate. The seam with AE
  (workflow process lifecycle, supervision, module loading, residency
  resolution) is via an explicit engine-facing handle/trait this cluster
  consumes; this cluster does not manage process lifecycle.
- **CO7** — Every interaction that changes what a workflow observed is
  recorded as an `aion-core` `Event` before/at the point of delivery; queries
  record nothing. No code path delivers a recordable observation without
  recording it.
- **CO8** — Durable timers go through `EventStore::schedule_timer` /
  `expired_timers`; signals and child results go through `EventStore` append.
  No bespoke side-channel persistence outside the `aion-store` contract.
- **CO9** — Tests run against `aion-store`'s `InMemoryStore`; timer expiry,
  signal delivery + durability, query reply, child spawn, and `all`/`race`/
  `map` each have explicit test coverage. Time-dependent tests inject the
  clock — no wall-clock sleeps in tests.
- **CO10** — Timer poll intervals, query timeouts, and recovery-tick cadence
  are engine-configured, never hardcoded defaults.

## Non-Goals

- No workflow process lifecycle, supervision, module loading, or scheduling
  — that is AE. This cluster consumes an engine-facing handle.
- No replay / determinism logic — that is AD. This cluster produces and
  records the live events; AD reads them back on restore.
- No Gleam SDK surface (`workflow.receive` / `sleep` / `all` / `race` /
  `map` / query handler registration) — that is AF. This cluster defines the
  engine-side mechanisms those bind to via NIFs.
- No network transport for how an external signal or query arrives
  (HTTP/gRPC/WS) — that is AW. This cluster defines the in-engine delivery
  the network layer calls into.
- No activity execution/retry mechanics beyond spawning activity child
  processes for `all`/`race`/`map` — activity invocation semantics and retry
  policy live in AE.
- No event-store backend — durable timers and event append go through the
  `aion-store` contract (libSQL is AS); this cluster uses `InMemoryStore` in
  tests.
