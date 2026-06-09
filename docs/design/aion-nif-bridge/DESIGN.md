---
type: design
cluster: aion-nif-bridge
title: NIF Bridge — Production NIF Implementations Wired Through the Durability Layer
---

# NIF Bridge

## Intention

Connect the last mile. The BEAM process calls `aion_flow_ffi` NIFs, and those NIFs go through the durability handoff so every side effect is recorded, replayed on recovery, and deterministic. When this cluster lands, a workflow that starts, runs activities on remote workers, sleeps, receives signals, spawns children, and completes produces a full, replayable event history — the Temporal developer expectation, not just for basic cases, but for all of them.

## Problem

The `aion_flow` SDK declares 18 FFI functions that workflow code calls through `@external(erlang, "aion_flow_ffi", ...)`. Only `run_activity/3` has a production NIF implementation, and even that one bypasses the durability layer — it dispatches to the worker but records no events.

The durability cluster (AD) built the full handoff pipeline: Resolver checks history (replay path), Recorder appends events (live path), and `resolve_or_execute_live()` orchestrates the two. But this pipeline is never invoked from the NIF boundary.

The time/signals cluster (AT) built timer scheduling, signal routing, query dispatch, child workflow spawning, and concurrency primitives. These are all tested through the Erlang test double, never through production NIFs connected to the real services.

The result: workflows start and run code, but no durable events are recorded beyond WorkflowStarted. Activities dispatch to workers but produce no ActivityScheduled/ActivityCompleted events. Workflow completion is never detected. The engine has all the pieces — they are just not connected through the NIF boundary.

## Solution

### NifContext

A `NifContext` constructed per NIF call. It resolves the calling BEAM PID to a `WorkflowHandle` via the Registry, provides access to the workflow's `Recorder` through interior mutability (`Arc<Mutex<Recorder>>`), and carries the tokio `Handle` for running async Recorder methods from the synchronous dirty NIF thread.

NifContext is the single integration point between the NIF boundary and the durability layer. Every production NIF constructs one, checks history, and either returns the recorded result (replay) or executes live and records events.

### Replay Path

When a NIF call matches an already-recorded event in history, the Resolver returns the recorded result directly. The NIF returns it to the BEAM process without executing any side effect. This is the determinism guarantee: re-executing a workflow produces the same sequence of results.

### Live Path

When no recorded event matches (first execution), the NIF records the scheduling event, executes the side effect (dispatch to worker, schedule timer, deliver signal, etc.), records the completion event, and returns the result. All recording goes through the single-writer Recorder.

### Completion Detection

When a BEAM workflow process exits (normal return or crash), the engine detects the exit through beamr's process monitoring, records WorkflowCompleted or WorkflowFailed, and updates the Registry. This closes the lifecycle gap where workflows stay Running forever after their process dies.

## Decisions

**D1 — NifContext resolved per call, not cached.** Each NIF call resolves the calling PID to a workflow handle. This is O(n) over active workflows but correct: a cached reference could go stale across process lifecycle events. Rejected caching: stale references would violate the single-writer invariant.

**D2 — Async Recorder methods run via tokio Handle::block_on from dirty NIF thread.** Dirty NIF threads have no async runtime. NifContext uses `Handle::block_on` to run `Recorder::record_*` methods synchronously from the dirty thread. Rejected spawning async tasks: it would break the blocking NIF contract.

**D3 — Deterministic now/random are pure functions, not NIF-to-service calls.** `workflow.now` returns the last recorded event's timestamp. `workflow.random` is seeded from WorkflowId + RunId. These read from the Resolver's history cursor. Rejected routing through a service: it would add latency for pure computations.

**D4 — Timer/signal/query/child NIFs delegate to AT's concrete services.** The AT cluster built ConcreteSignalRouter, timer scheduling, query dispatch, and child workflow spawning. The NIFs call these through the engine's DelegatedSeams. Rejected reimplementing: it would violate single-owner and duplicate tested code.

**D5 — Process exit detection uses beamr monitor, not polling.** The engine monitors each workflow process via beamr's process_monitor. On exit, the monitor callback records the terminal event. Rejected polling: it wastes CPU and introduces detection latency.

**D6 — Concurrency NIFs compose activity dispatch, not independent services.** `collect_all`, `collect_race`, and `collect_map` are coordination patterns over multiple activity dispatches. They compose the activity NIF path. Rejected a dedicated service: the coordination is workflow-local logic.

**D7 — WorkflowHandle gains interior-mutable Recorder access.** The Recorder requires `&mut self`. WorkflowHandle is shared (Clone). The handle wraps the Recorder in `Arc<Mutex<Recorder>>` so the NIF bridge can access it. The single-writer invariant means only one thread ever calls into the Recorder, so the Mutex is uncontended.

## Goals

1. All 18 `aion_flow_ffi` functions have production NIF implementations that record durable events.
2. A workflow produces a complete, ordered event history in the EventStore.
3. Crash + restart replays history and returns recorded results without re-executing side effects.
4. Workflow process exit is detected and recorded as WorkflowCompleted or WorkflowFailed.
5. The hello-world tutorial works end-to-end with full event history.
6. Deterministic execution: `workflow.now` and `workflow.random` produce the same values on replay.
7. All existing AD/AT/AE tests continue to pass.

## Non-Goals

- No new AD/AT/AE design — this cluster consumes those implementations.
- No changes to the `aion_flow` Gleam SDK — the FFI interface is fixed.
- No changes to the gRPC wire protocol.
- No retry policy implementation — that is AT's machinery, consumed via the existing seam.
- No new event types — all events are already defined in aion-core.

## Structure

```
crates/aion/src/runtime/
├── engine_nifs.rs         — [NB-001] NifContext + expanded NIF entry registration
├── nif_context.rs         — [NB-001] PID→handle resolution, Recorder access, tokio handle
├── nif_activity.rs        — [NB-002] run_activity NIF through durability handoff
├── nif_determinism.rs     — [NB-003] now, random, random_int NIFs
├── nif_timer.rs           — [NB-004] sleep, start_timer, cancel_timer, with_timeout NIFs
├── nif_signal.rs          — [NB-005] receive_signal, send_signal NIFs
├── nif_query.rs           — [NB-006] register_query, reply_query, dispatch_query NIFs
├── nif_child.rs           — [NB-007] spawn_child, await_child NIFs
└── nif_concurrency.rs     — [NB-008] collect_all, collect_race, collect_map NIFs

crates/aion/src/lifecycle/
└── completion.rs          — [NB-009] Process exit detection → terminal event recording

crates/aion/src/registry/
└── handle.rs              — [NB-001] Interior-mutable Recorder (Arc<Mutex<Recorder>>)
```

## Constraints

- CO1: Every NIF goes through replay check. No side effect without checking history first.
- CO2: All recording goes through the single-writer Recorder. No direct EventStore::append.
- CO3: Deterministic execution. No wall clock or entropy source in NIF-visible paths.
- CO4: aion crate remains transport-agnostic. Remote dispatch stays in aion-server.
- CO5-CO9: Standard codebase constraints (unsafe deny, no lint bypasses, 500-line limit, beamr boundary in runtime only, InMemoryStore tests).
