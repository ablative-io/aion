---
type: design
cluster: aion-engine
title: Aion Engine — Lifecycle, Process Management, Supervision, and the Embedding API
---

# Aion Engine — Lifecycle, Process Management, Supervision, and the Embedding API

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

This is the heart of the engine — the crate you embed. When this cluster is
done, a Rust application can take an `EventStore`, a set of loaded `.aion`
workflow packages, and a set of registered NIFs, call
`EngineBuilder::new().store(...).build().await`, and get back a live `Engine`
that starts workflows, runs each as a BEAM process on an embedded beamr
runtime, supervises them through a three-level tree, dispatches their
activities as linked child processes, and cancels or reports on them by ID.

It must feel like the BEAM was always meant to host workflows. Starting a
workflow is spawning a process. Cancelling it is killing that process —
which, through the links, kills its activity children too. A crash in an
activity propagates an exit signal that the trapping workflow process
receives as a message. The supervision tree is not bolted on; it is the
shape of the system. An engineer reading `Engine` should see one clear
embedding contract — build, start, signal, query, cancel, result, list,
subscribe, shutdown — and never need to reach into beamr directly.

The crate is **transport-agnostic**. It has no HTTP, no gRPC, no WebSocket.
Networking is `aion-server`'s job (cluster AW). This keeps the embedded path
free of network dependencies, so a CLI tool or a single service can run
durable workflows with nothing but a file-backed store. This cluster owns
*lifecycle, process management, supervision, and module loading*. It defers
the mechanics of durability/replay (cluster AD) and the mechanics of
time/signals/queries/children/concurrency (cluster AT) to their owners,
surfacing their entry points on the `Engine` API and stating the seams
explicitly so the clusters compose without overlap or gaps.

## Problem

`aion-core` gives us the vocabulary (events, IDs, status, `EventStore`),
`aion-store` gives us a place to put events, and `aion-package` gives us a
validated, version-stamped bundle of beams ready to register. beamr gives us
a process runtime with spawning, links, monitors, trap-exit, mailboxes, a
configurable scheduler, and a NIF registry. None of these, alone, runs a
workflow. Something has to:

- **Own the beamr runtime** — configure its scheduler thread count, register
  the NIFs that workflow code calls through, hold the handle, and shut it
  down cleanly.
- **Map a `WorkflowId` to a running process** — a registry that knows which
  beamr process is executing which workflow, so a signal, query, or cancel
  request can find its target, and so `list_workflows` can report what is
  live.
- **Drive the lifecycle** — start (spawn the process, append `WorkflowStarted`,
  register it), complete (the process returns, append `WorkflowCompleted`,
  deregister), fail, cancel (kill the process, append `WorkflowCancelled`),
  and the suspend/resume transitions a long-lived workflow undergoes.
- **Stand up the supervision tree** — an engine supervisor over per-workflow-
  type supervisors over workflow processes over activity child processes, with
  crash propagation through links and trap-exit handling at the workflow level.
- **Dispatch in-VM activities** — spawn an activity as a linked child process,
  run it, capture its result or exit, and hand control back to the workflow.
- **Load workflow modules** — take a `Package` from `aion-package`, apply the
  content-hash namespacing to its module names, register the namespaced beams
  with beamr's module loader, and remember which version a workflow runs.

Without this crate, every consumer would have to reimplement process-to-
workflow mapping, supervision wiring, and module loading against beamr's raw
primitives — exactly the boilerplate Aion exists to remove.

## Solution

One crate, `aion`, depending on `aion-core`, `aion-store`, `aion-package`,
and `beamr`. It is the engine library. Everything in the engine layer above
it (`aion-server`) builds on it; everything below it (the stores, the
package format, the core types) it consumes.

### Design Principles

1. **The process is the state.** A running workflow's state lives in its
   beamr process, not in a database row reloaded per step (per
   DESIGN-OVERVIEW "Process-per-Workflow"). The store is written on
   observable actions; it is read only to replay.
2. **Transport-agnostic.** No HTTP/gRPC/WebSocket in this crate. The
   embedding API is plain Rust async methods (per COMPONENT-ARCHITECTURE
   boundary rule).
3. **Supervision is the shape, not an add-on.** Lifecycle transitions map
   onto beamr spawn/link/monitor/trap-exit. Cancellation is process death
   propagated through links.
4. **Lifecycle here, mechanics elsewhere.** This crate owns *when* a workflow
   starts/suspends/resumes/cancels/completes and *which process* runs it. It
   does not own *how* replay reconstructs state (AD) or *how* a signal/timer/
   query/child/concurrency-primitive is implemented (AT). It surfaces their
   API entry points and calls into their machinery.
5. **No silent failures.** Every store error, beamr error, package error, and
   lock-poison case is a typed `EngineError` variant — propagated, never
   swallowed (per CLAUDE.md).

### Crate Layout and How It Fits

`aion` is organised into folder modules, each a clear responsibility:

- **`runtime`** — embeds beamr. Owns scheduler configuration (thread count
  from the builder), the runtime handle, NIF registration, and clean
  shutdown. The single place that touches beamr's `SchedulerConfig`, native
  registry, and module loader.
- **`registry`** — the active-execution registry: a concurrent map from
  `WorkflowId` (plus `RunId`) to a `WorkflowHandle` (beamr pid, workflow
  type, status). The lookup path for signal/query/cancel and the source for
  `list_workflows`.
- **`lifecycle`** — the start/suspend/resume/cancel/complete state machine,
  expressed over the registry and the runtime. Appends the lifecycle events
  through the store and keeps the registry consistent with them.
- **`supervision`** — wires the three-level tree onto beamr's link/monitor/
  trap-exit primitives. Defines the engine supervisor and the per-type
  workflow supervisors, and the crash-propagation policy.
- **`activity`** — in-VM activity dispatch: spawn a linked child process, run
  the activity, collect the result or the exit signal, surface it to the
  workflow process. (The *recording* of activity events and the *retry
  decision* are AD/AT seams referenced here.)
- **`loader`** — bridges `aion-package` to beamr: applies the content-hash
  namespacing (from `aion-package`'s scheme) to a `Package`'s modules and
  registers them with the runtime; records the loaded version.
- **`engine`** — the `Engine` and `EngineBuilder`: the public embedding API.
  Composes runtime + store + loaded workflows + registry into the methods
  consumers call. The handle returned by `start_workflow`.
- **`error`** — `EngineError`, the crate's `thiserror` taxonomy.

### Embedding the beamr Runtime

beamr exposes a `SchedulerConfig { thread_count: Option<usize> }`, a native
registry (`NativeFn = fn(&[Term], &mut ProcessContext) -> Result<Term, Term>`
registered by MFA, with a dirty-scheduler flag), process spawn/link/monitor/
trap-exit facilities, lock-free mailboxes with selective receive, a timer
wheel, and `wake_with_result` for suspending a process and delivering a
host-side async result. The `runtime` module is the only part of `aion` that
imports beamr. It accepts a thread count from the builder, builds the
scheduler, registers the engine's own NIFs plus any host-supplied NIFs, and
exposes a runtime handle the rest of the crate uses to spawn workflow and
activity processes.

**D1 — `runtime` is the sole beamr boundary.** Only the `runtime` module
imports `beamr`. Every other module talks to beamr through `runtime`'s typed
handle (`spawn_workflow`, `spawn_activity`, `register_module`, `cancel_pid`,
`shutdown`). Rejected: scattering beamr calls across modules — it would make
the embedding contract impossible to audit and couple lifecycle logic to VM
internals. The single boundary keeps beamr swappable and the seam reviewable.

**D2 — Scheduler thread count is builder-supplied, not defaulted in this
crate.** The `EngineBuilder` takes the scheduler thread count from the
caller; if the caller does not set it, `aion` passes `thread_count: None` to
beamr, which means beamr applies its own `available_parallelism()` default.
`aion` itself hardcodes no number (per CLAUDE.md "no assumed defaults"). The
default lives in beamr where the runtime knowledge is, not invented here.

### Workflow Lifecycle

A workflow's life is a small state machine the `lifecycle` module owns:

- **Start** — assign a `WorkflowId` and `RunId`, append `WorkflowStarted`
  (via the store, through the AD append path), spawn a beamr process running
  the workflow's namespaced entry module/function over the input `Payload`,
  register the handle, and return a `WorkflowHandle` to the caller.
- **Suspend** — when a workflow blocks on a durable wait (a timer that
  outlives the process, a signal that has not arrived) the process yields;
  the engine marks the handle `Suspended` in the registry. The *mechanism* of
  durable waiting lives in AT; the lifecycle *transition* is here.
- **Resume** — when the awaited timer fires or signal arrives, the workflow
  process is woken (or, after a VM restart, replayed by AD) and the handle
  returns to `Running`.
- **Cancel** — kill the workflow process (which, through links, kills its
  activity children), append `WorkflowCancelled`, deregister.
- **Complete / Fail** — the workflow function returns `Ok`/`Err`; the engine
  appends `WorkflowCompleted`/`WorkflowFailed`, stores the result `Payload`,
  deregisters, and unblocks any `result()` awaiter.

**D3 — Status is read from the store projection, never tracked
independently.** The registry caches a status for fast lookup, but the
authoritative status is `aion-core`'s projection over event history (per
aion-core CO7). On any disagreement the projection wins, and the registry is
corrected. Rejected: a registry-owned mutable status as source of truth — it
would let the live view drift from the durable record, defeating replay
integrity.

### Process-per-Workflow and the Active-Execution Registry

The `registry` module holds a concurrent map keyed by `(WorkflowId, RunId)`
to a `WorkflowHandle` carrying the beamr pid, the workflow type, the loaded
version, and the cached status. Every lifecycle transition updates it. Lookup
is the first step of signal/query/cancel routing (the routing *delivery* into
the mailbox is AT; the *lookup* is here). `list_workflows` reads it for live
workflows and falls through to the store's `query` for terminal ones.

**D4 — Registry keyed by `(WorkflowId, RunId)`, lock-poison handled
explicitly.** A logical workflow may have successive runs (reset / continue-
as-new, per aion-core's `RunId`); the live registry keys on both so a stale
run cannot shadow a new one. The map is behind a lock whose poison is mapped
to `EngineError::RegistryPoisoned`, never `.unwrap()`-ed (per CLAUDE.md).
Rejected: keying on `WorkflowId` alone — it cannot represent two runs of one
workflow and would mis-route during a continue-as-new.

### Three-Level Supervision Tree

Per DESIGN-OVERVIEW "Supervision":

```
Engine Supervisor
  └─ Workflow Supervisor (per workflow type)
       └─ Workflow Process (per execution)
            ├─ Activity Process (per activity)
            └─ Child Workflow Process
```

The `supervision` module builds this onto beamr's four primitives (links,
monitors, exit signals, trap-exit; beamr D7 — supervision strategy is library
code over the primitives). The engine supervisor is the root; one workflow
supervisor exists per registered workflow type; each workflow execution is a
process under its type's supervisor; activity invocations are linked children
of the workflow process.

- **Activity crash** → the link propagates the exit to the workflow process,
  which traps exits and receives it as a message; the *retry-or-fail decision*
  is the activity policy (AT/AD seam) consulted by the workflow.
- **Workflow crash** → the workflow supervisor is notified; the engine asks
  AD to replay the workflow from history, restoring it to the last persisted
  state, then re-registers it.
- **VM restart** → on startup AD reads active workflow IDs and replays each;
  this cluster provides the registry re-population and supervisor re-creation
  that replay slots into.

**D5 — Workflow processes trap exits; activity processes do not.** A workflow
process sets trap-exit so an activity child's crash arrives as a message it
can act on (apply retry policy / fail the workflow), rather than killing the
workflow outright. Activity processes do not trap exits — when a workflow is
cancelled, the link kills its activities cleanly with no special handling.
Rejected: trapping exits on activities — it would swallow cancellation and
require manual teardown, defeating the natural propagation the BEAM gives us.

**D6 — One workflow supervisor per workflow *type*, not per execution.**
Supervisors are created per registered workflow type (the entry module), and
all executions of that type live under it. This bounds the supervisor count
to the number of deployed workflow types and gives a natural place to apply
type-level restart policy. Rejected: a supervisor per execution — it doubles
the process count and adds a supervision layer with nothing to coordinate.

### In-VM Activity Dispatch

The `activity` module dispatches a Tier-2 in-VM activity (per DESIGN-OVERVIEW
"Execution Tiers") as a child BEAM process linked to the workflow process: it
spawns the activity body, lets it run (on the dirty scheduler if the NIF is
flagged dirty), and surfaces the outcome — a result `Payload` or an exit
signal carrying an `ActivityError` — back to the workflow process through the
link/mailbox. This is the in-VM execution path only.

**D7 — `aion` dispatches in-VM activities; recording and retry are seams, not
owned here.** The act of spawning the linked child, running it, and
propagating its outcome is this cluster. *Recording* `ActivityScheduled`/
`ActivityStarted`/`ActivityCompleted`/`ActivityFailed` events is the AD append
path; *deciding* whether a failed activity is retried per its `RetryPolicy` is
AT's activity machinery (consulting the retryable/terminal split AD/aion-core
model). `aion` calls those; it does not reimplement them. Remote (Tier-3)
activity dispatch is `aion-server`'s worker protocol (AW) — out of scope here.
Rejected: folding recording and retry into the dispatch path — it would
duplicate AD/AT and put event-sourcing logic in two places.

### Loading Workflow Modules from `.aion` Packages

The `loader` module takes a validated `Package` (from `aion-package`, which
has already verified integrity and computed the content hash) and registers
its beams with beamr. It applies `aion-package`'s namespacing scheme — the
pure `(logical module name, content hash) → deployed module name` transform —
to every module, registers the namespaced beams through the `runtime` handle,
and records the loaded version so that `start_workflow` for that type spawns
the entry module under its namespaced name.

**D8 — The namespacing transform is consumed from `aion-package`, never
re-derived.** `aion-package` owns the bijection (its CO12); `aion` calls it
to map names at registration time and at spawn time. This guarantees the
engine and any tooling agree on module names. Because each version is a
distinct namespaced module, version N and N+5 coexist, in-flight executions
keep the exact module set they started on (replay-safe by construction), and
beamr's two-deep same-name limit never binds for workflow modules (per
DESIGN-OVERVIEW open-question resolution). Rejected: the engine deriving its
own naming — it would risk divergence from the package's recorded hash and
break replay.

**D9 — `load_workflows` registers; it does not run.** Loading a package
registers modules and records the version; no workflow executes until
`start_workflow` is called. A package whose entry module is absent, or whose
modules collide with an already-registered namespaced name from a *different*
hash, is a typed `EngineError::Load`. Rejected: auto-starting a workflow on
load — deployment and execution are distinct operations and must stay so.

### The Engine Embedding API

The `engine` module exposes the contract from COMPONENT-ARCHITECTURE:

- **`EngineBuilder`** — `new()`, `.store(impl EventStore)`,
  `.scheduler_threads(n)`, `.load_workflows(path | Package)`,
  `.register_nifs(...)`, `.build().await -> Result<Engine, EngineError>`.
  Build wires the runtime, registers NIFs and loaded modules, repopulates the
  registry and re-creates supervisors from the store's `list_active`
  (delegating the actual replay to AD), and returns a live `Engine`.
- **`Engine`** — `start_workflow(type, input) -> WorkflowHandle`,
  `cancel(&id, reason)`, `result(&id) -> Result<Payload, WorkflowError>`,
  `list_workflows(filter) -> Vec<WorkflowSummary>`, and `shutdown()`.
- **Surfaced-but-delegated** — `signal(&id, name, payload)`,
  `query(&id, name) -> Payload`, and `subscribe(EventFilter) -> stream` appear
  on `Engine` for API completeness, but their *mechanics* live in AT (signal
  routing, query dispatch) and AD/AT (event publishing). This cluster defines
  the method surface and the registry lookup they start from; it does not
  implement their delivery.

**D10 — `build` is the only place that assembles the engine; everything else
goes through `Engine`.** There is one construction path (the builder) and one
runtime object (`Engine`). The builder validates that a store is present
(`EngineError::MissingStore` otherwise) and that loaded workflows' NIF
dependencies are satisfiable. Rejected: free functions that spawn workflows
without an `Engine` — they would bypass the registry and supervision, leaving
orphaned processes the engine cannot find, cancel, or report on.

**D11 — `shutdown` is graceful and total.** `shutdown()` stops accepting new
starts, lets the store finish in-flight appends, instructs the runtime to
drain and stop the scheduler, and returns once beamr has stopped. A workflow
mid-execution at shutdown is left `Suspended` in its durable history so a
later engine can replay it (per AD). Rejected: dropping the runtime to stop —
it would abandon in-flight appends and leak scheduler threads.

### Testing Strategy

Engine tests use `InMemoryStore` from `aion-store` (per aion-core D8) — no
database. A minimal test workflow package (a tiny compiled-beam fixture, or a
test-only in-VM module registered directly through the runtime) exercises
start → activity dispatch → complete, cancel-propagation, and registry
consistency. Supervision tests assert that an activity-process exit reaches a
trapping workflow process and that cancelling a workflow kills its linked
activity children.

## Goals

1. `EngineBuilder` builds an `Engine` from an `EventStore` plus loaded
   workflows plus NIFs, with caller-supplied scheduler thread count and no
   hardcoded default in `aion`.
2. `start_workflow` spawns a beamr process for a loaded workflow type, appends
   `WorkflowStarted`, registers the handle, and returns a `WorkflowHandle`.
3. The active-execution registry maps `(WorkflowId, RunId)` to a live handle
   and is the lookup for cancel/signal/query and the source for
   `list_workflows`; lock poison is a typed error.
4. The three-level supervision tree (engine → per-type → workflow → activity)
   is built on beamr links/monitors/trap-exit; an activity crash reaches the
   trapping workflow process, and cancelling a workflow kills its activity
   children.
5. In-VM activities dispatch as linked child processes whose result or exit is
   surfaced to the workflow process (recording/retry delegated to AD/AT).
6. `.aion` packages load by applying `aion-package`'s content-hash namespacing
   and registering the namespaced beams; versions coexist and `start_workflow`
   spawns the correct namespaced entry module.
7. `cancel`, `result`, `list_workflows`, and a graceful `shutdown` work
   end-to-end against `InMemoryStore`, with `signal`/`query`/`subscribe`
   surfaced on `Engine` as delegation points to AT/AD.

## Non-Goals

- **No replay/determinism machinery** — event-append-on-observable-action, the
  replay engine, `workflow.now`/`workflow.random`, and recovery-on-startup are
  cluster **AD**. This cluster *calls* the append path and *triggers* replay;
  it does not implement them.
- **No durable timers, signal routing, query service, child-workflow spawning,
  or concurrency primitives (all/race/map)** — cluster **AT**. This cluster
  surfaces `signal`/`query` on `Engine` and handles the lifecycle suspend/
  resume around them; the delivery and the primitives are AT's.
- **No HTTP/gRPC/WebSocket or server** — cluster **AW**. `aion` is
  transport-agnostic.
- **No Rust NIF *authoring* helper** — that is `aion-nif` (cluster **AN**).
  This crate *registers* NIFs handed to the builder; it does not provide the
  macros/builders to write them.
- **No Gleam SDK** — `aion_flow` is cluster **AF**.
- **No durable storage backend** — libSQL is AS, PostgreSQL is AX. Engine tests
  use `InMemoryStore`.
- **No remote (Tier-3) activity worker dispatch** — that rides the worker
  protocol in `aion-server` (AW).

## Cluster Boundary Statements (AE / AD / AT)

To compose without overlap or gaps:

- **AE owns** the workflow lifecycle state machine (start/suspend/resume/
  cancel/complete), the active-execution registry (`WorkflowId`/`RunId` →
  process), the three-level supervision tree and trap-exit policy, in-VM
  activity *dispatch* (spawn linked child, propagate outcome), `.aion` module
  loading and content-hash namespacing application, and the `Engine`/
  `EngineBuilder` embedding API including `shutdown`.
- **AD owns** appending events on observable actions, the replay engine,
  determinism (`workflow.now`/`workflow.random`), and recovery-on-startup. AE
  *calls* AD's append path on every lifecycle/activity event and *triggers*
  AD's replay on workflow crash and on VM restart; AE provides the registry
  re-population and supervisor re-creation that replay slots into.
- **AT owns** durable timers, signal routing into mailboxes, the query service,
  child-workflow spawning, and the concurrency primitives (`all`/`race`/`map`).
  AE *surfaces* `signal`/`query`/`subscribe` on `Engine` and owns the lifecycle
  suspend/resume transitions that wrap an AT durable wait; AT implements the
  delivery and the primitives. The activity *retry decision* is AT's policy
  machinery; AE supplies the dispatch and the trapped-exit signal it acts on.

The seam test: if a question is *"which process runs this / when does it
start, suspend, die"* it is AE; if it is *"how is state reconstructed / what
is recorded"* it is AD; if it is *"how does a signal/timer/query/child/
concurrency primitive actually work"* it is AT.

## Structure

```
crates/aion/
├── Cargo.toml                      — deps: aion-core, aion-store, aion-package, beamr
└── src/
    ├── lib.rs                      — [AE-001] thin re-export surface
    ├── error.rs                    — [AE-002] EngineError thiserror taxonomy
    ├── runtime/
    │   ├── mod.rs                  — [AE-003] pub mod + re-exports only
    │   ├── handle.rs               — [AE-003] RuntimeHandle: spawn/register/cancel/shutdown
    │   ├── config.rs               — [AE-003] scheduler config (builder-supplied threads)
    │   └── nif.rs                  — [AE-004] NIF registration surface
    ├── registry/
    │   ├── mod.rs                  — [AE-005] pub mod + re-exports only
    │   ├── handle.rs               — [AE-005] WorkflowHandle (pid, type, version, status)
    │   └── table.rs                — [AE-005] active-execution registry, lock-poison handling
    ├── loader/
    │   ├── mod.rs                  — [AE-006] pub mod + re-exports only
    │   └── load.rs                 — [AE-006] Package → namespaced beams → runtime register
    ├── supervision/
    │   ├── mod.rs                  — [AE-007] pub mod + re-exports only
    │   ├── tree.rs                 — [AE-007] engine + per-type supervisor construction
    │   └── policy.rs               — [AE-007] trap-exit + crash-propagation policy
    ├── activity/
    │   ├── mod.rs                  — [AE-008] pub mod + re-exports only
    │   └── dispatch.rs             — [AE-008] in-VM activity child spawn + outcome propagation
    ├── lifecycle/
    │   ├── mod.rs                  — [AE-009] pub mod + re-exports only
    │   ├── start.rs                — [AE-009] start: spawn + WorkflowStarted + register
    │   ├── terminate.rs            — [AE-010] cancel/complete/fail transitions
    │   └── transition.rs           — [AE-011] suspend/resume transitions
    └── engine/
        ├── mod.rs                  — [AE-012] pub mod + re-exports only
        ├── builder.rs              — [AE-012] EngineBuilder + build()
        ├── engine.rs               — [AE-013] Engine: start/cancel/result/list/shutdown
        └── delegated.rs            — [AE-014] signal/query/subscribe surface (AT/AD delegation)
```

## Constraints

- **CO1** — `unsafe_code = "deny"`. No unsafe in the crate.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses per
  CLAUDE.md. Tests that need a runtime gate it at runtime, never `#[ignore]`.
- **CO3** — `lib.rs` / `mod.rs` are declarations and re-exports only.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace).
- **CO5** — `aion` depends only on `aion-core`, `aion-store`, `aion-package`,
  and `beamr` among workspace crates. No `aion-server`, no store backend
  crate, no networking crate. Structural; must hold.
- **CO6** — Transport-agnostic: no HTTP, gRPC, or WebSocket dependency or code
  anywhere in the crate (per COMPONENT-ARCHITECTURE boundary rule).
- **CO7** — Only the `runtime` module imports `beamr`; every other module uses
  the `RuntimeHandle` (per D1).
- **CO8** — All library errors are `thiserror` `EngineError` variants; no
  `anyhow` in this library crate. No `.unwrap()` / `.expect()` outside tests.
- **CO9** — Mutex/RwLock poison is handled explicitly and mapped to a typed
  `EngineError`; no `.unwrap()` on a lock guard (per CLAUDE.md).
- **CO10** — `WorkflowStatus` is read from `aion-core`'s event projection; the
  registry's cached status is never the source of truth (per aion-core CO7).
- **CO11** — `aion` hardcodes no scheduler thread count, no timeout, and no
  retry default; configurable values come from the builder or are deferred to
  beamr's own default (per CLAUDE.md "no assumed defaults").
- **CO12** — Module namespacing uses `aion-package`'s transform unchanged; the
  engine never re-derives namespaced names (per D8).
- **CO13** — Engine tests run against `InMemoryStore`; no test requires a
  database (per aion-core D8).
