# Aion — Durable Workflow Engine for Gleam, Rust, and BEAM

> **Status note:** This is a design/vision document. Crash/restart recovery,
> durable timers, and WebSocket event streaming are implemented in the current
> build; broader ecosystem items such as dashboard UX, zero-downtime upgrades,
> and some cross-language SDK transports remain in progress unless a current
> user-facing guide marks them implemented.

> **Aion** is the Greek conception of eternal, unbounded time — distinct
> from Chronos, who is sequential, ticking, chronological time. A durable
> workflow engine lives in Aion's time: workflows persist across crashes,
> restarts, and arbitrary delays. A workflow that sleeps for three months
> and resumes is living in eternal time, not clock time. Where Temporal
> named itself after time, Aion names itself after the deeper, more durable
> conception of it.
>
> The engine runs on **beamr**, the Rust BEAM VM. Aion runs on beamr.

## Vision

Aion is a durable workflow execution engine built on Rust, Gleam, and the
BEAM virtual machine. It provides the same guarantees as Temporal —
workflows that survive crashes, automatic replay from event history,
durable timers, signals, child workflows — but leverages the BEAM's native
process model for concurrency, supervision, and fault tolerance instead of
building those capabilities from scratch.

The engine ships as:
- A **Gleam library** (`aion_flow`) for writing type-safe, concurrent
  workflows
- A **Rust library** (`aion`) for embedding the engine in any Rust
  application
- An optional **standalone server** (`aion-server`) for teams that want a
  managed service
- A **pluggable persistence layer** with an embedded default (zero
  external infrastructure required)
- **Worker and client SDKs** in multiple languages for implementing
  activities and driving workflows

Aion is general purpose. Meridian is the first consumer, not the only one.

---

## Why Gleam + Rust + BEAM

The three technologies complement each other precisely:

**Gleam** provides compile-time type safety for workflow definitions.
Activity inputs, outputs, signal payloads, and query returns are all typed.
Invalid workflow compositions are caught at compile time, not at runtime
three days into a long-running workflow. Gleam compiles to BEAM bytecode,
giving it direct access to the process model.

**BEAM** (via beamr) provides the execution runtime. The BEAM was designed
for telecom switches — systems that must run forever, handle millions of
concurrent connections, recover from failures automatically, and upgrade
without downtime. These are exactly the requirements of a workflow engine.
Each workflow execution is a lightweight process. Activities are child
processes. Supervision handles crash recovery. Message passing delivers
signals. Selective receive enables complex coordination patterns.

**Rust** provides the infrastructure layer. Persistence, the event store,
the external API, timer management, and heavy computation live in Rust.
Rust's performance, safety, and ecosystem (async I/O, database drivers,
gRPC, WebSocket) make it the right choice for the parts that don't benefit
from the BEAM's process model.

The combination gives us what Temporal built with Go alone — but with the
BEAM handling the hard parts (concurrency, supervision, fault tolerance)
natively instead of through application-level constructs.

---

## Architecture

```
+------------------------------------------------------------------+
|                        Applications                               |
|  (Meridian, CI/CD pipelines, payment processing, data pipelines)  |
+------------------------------------------------------------------+
         |                    |                    |
         v                    v                    v
+------------------+  +----------------+  +------------------+
|   Gleam SDK      |  |   Rust SDK     |  |  HTTP/gRPC/WS    |
|  (aion_flow)     |  |  (aion crate)  |  |  (aion-server)   |
+------------------+  +----------------+  +------------------+
         |                    |                    |
         +--------------------+--------------------+
                              |
                    +---------+---------+
                    |    Aion Engine     |
                    |                   |
                    |  +-------------+  |
                    |  | Scheduler   |  |  Workflow lifecycle,
                    |  +-------------+  |  assignment, replay
                    |  | Replay      |  |
                    |  +-------------+  |  Event-driven state
                    |  | Timer Svc   |  |  reconstruction
                    |  +-------------+  |
                    |  | Signal Rtr  |  |  Durable timers,
                    |  +-------------+  |  signal delivery
                    |  | Query Svc   |  |
                    |  +-------------+  |  Real-time event
                    |  | Event Pub   |  |  fan-out (WebSocket)
                    |  +-------------+  |
                    +--------+----------+
                             |
              +--------------+--------------+
              |                             |
    +---------+----------+     +------------+-----------+
    |   beamr Runtime    |     |    Event Store         |
    |                    |     |                        |
    |  BEAM processes    |     |  +------------------+  |
    |  Mailboxes         |     |  | Embedded SQLite  |  |
    |  Supervision       |     |  +------------------+  |
    |  Selective receive  |     |  | Pluggable trait  |  |
    |  Timer wheel       |     |  +------------------+  |
    |  GC                |     |  | PostgreSQL       |  |
    |  Hot code loading  |     |  +------------------+  |
    |                    |     |  | Custom (Meridian)|  |
    +--------------------+     +------------------------+
              |
    +---------+-----------------------------+
    |  Remote Activity Workers (any lang)    |
    |  Python | TypeScript | Go | Rust ...   |
    +----------------------------------------+
```

### Layer Responsibilities

**beamr Runtime** — Executes workflow and activity code as BEAM processes.
Handles scheduling, message passing, supervision, garbage collection, and
(after hot-loading work) live code upgrades. This layer exists today and
has full process model support.

**Aion Engine** — Manages workflow lifecycles. Assigns workflow executions
to beamr processes. Drives replay from event history on restart. Manages
durable timers. Routes signals and queries to the right workflow process.
Publishes events for real-time streaming. This is the core new work.

**Event Store** — Append-only log of workflow events. The source of truth
for workflow state. Pluggable: ships with an embedded SQLite implementation
for zero-infrastructure deployments, but any backend can be plugged in via
a Rust trait.

**Gleam SDK** — The library workflow authors use. Provides types for
workflows, activities, signals, queries, timers, and child workflows.
Translates these abstractions into BEAM process operations. Published to
Hex as a standalone package.

**Rust SDK** — Embedding API for Rust applications. Configure the engine,
load workflow modules, start/signal/query/cancel workflows, plug in custom
storage.

**HTTP/gRPC/WebSocket API** — Optional server mode. Exposes the engine over
the network for teams that want a managed deployment. WebSocket streams
real-time workflow events. Includes a monitoring dashboard.

**Remote Activity Workers** — Out-of-process activity implementations in
any language. Connect to the engine, receive activity tasks, execute them
in their own runtime, return results. The escape hatch for languages that
can't run on beamr and the home for heavy, isolated, or independently
scaled work.

---

## The Core Concept: Determinism

Everything in Aion's design flows from one principle:

> **Workflow code must be deterministic. Side effects must be activities.**

A workflow is re-executed from the beginning during replay (after a crash
or restart). For replay to reconstruct the exact same state, the workflow
function must take the same path every time it runs with the same recorded
history. This requires determinism:

- **No reading the clock** — `workflow.now()` returns a recorded timestamp,
  not the wall clock
- **No randomness** — `workflow.random()` returns a deterministic value
  seeded from the workflow ID
- **No direct I/O** — all external interaction goes through activities
- **No non-deterministic data structures** — iteration order must be stable

The dividing line for any piece of work is **not "simple vs heavy" — it is
"deterministic vs side-effectful":**

| Work | Where it runs | Recorded? | On replay |
|------|---------------|-----------|-----------|
| Deterministic (JSON transform, formatting, math, parsing) | Inline in workflow, or a NIF | No | Re-executed (safe — same result) |
| Side-effectful (HTTP, DB, LLM, file I/O, run command, clock) | An **activity** | Yes | Recorded result returned, NOT re-executed |

This is the most important concept in the engine. It is what makes durable
execution work, and it shapes the entire API. A fast native NIF that runs a
shell command is still a side effect — it must be invoked through the
activity contract so the recorded result is returned on replay rather than
running the command twice.

---

## Core Concepts

### Workflows

A workflow is a Gleam function that describes a process — a sequence of
steps, decisions, and coordination points. Workflow code must be
deterministic (see above). The only way a workflow interacts with the
outside world is through activities, signals, and timers.

Each workflow execution runs as a BEAM process. The process IS the state.
Unlike Temporal, where workflow state is reconstructed from a database on
every worker task, Aion's workflow state lives in the process until it
yields or the VM restarts. Replay is only needed after a restart, not on
every scheduling decision.

```gleam
import aion/workflow
import aion/activity

pub fn order_workflow(order: Order) -> Result(Receipt, OrderError) {
  // Pure computation — deterministic, no activity needed
  let validated = validate_order(order)?

  // Side effect — must be an activity, with retry and timeout
  let charge = workflow.run(
    activity.new("charge-payment", fn() { charge_payment(validated) })
    |> activity.retry(max: 3, backoff: activity.Exponential(base: 1000))
    |> activity.timeout(seconds: 30)
  )?

  // Wait for external signal
  let shipped = workflow.receive("shipment-confirmed")
    |> workflow.with_timeout(days: 7)?

  Ok(Receipt(order:, charge:, shipped:))
}
```

### Activities

Activities are where side effects happen — API calls, database writes, LLM
invocations, file operations, git commands, running processes. Activities
can fail and are retried according to a policy. When an activity completes,
its result is recorded in the event store. On replay, the recorded result
is returned without re-executing the activity.

Activities are implemented in one of three tiers (see "Execution Tiers"
below). Regardless of tier, an activity invocation runs as a child BEAM
process linked to the workflow process. This gives us:

- **Automatic failure propagation** via process links
- **Concurrent execution** — multiple activities run as independent
  processes
- **Natural cancellation** — killing a workflow process propagates to all
  linked activity processes
- **Supervision** — the workflow process acts as a supervisor for its
  activity children

### Signals

Signals are messages sent to a running workflow from the outside world. A
workflow can wait for a signal by name, with an optional timeout. Signals
are recorded in the event store for durability — on replay, a recorded
signal is delivered immediately without waiting.

Delivery mechanism: the engine routes the signal to the workflow process's
mailbox. The workflow uses selective receive to wait for it. This is native
BEAM — no polling, no database round-trips during normal execution,
microsecond latency.

```gleam
let approval = workflow.receive("manager-approval")
  |> workflow.with_timeout(hours: 48)

case approval {
  Ok(Approved(by:)) -> continue_processing()
  Ok(Rejected(reason:)) -> cancel_order(reason)
  Error(TimedOut) -> escalate_to_director()
}
```

```rust
engine.signal(workflow_id, "manager-approval", Approved { by: "alice" })?;
```

### Queries

Queries are read-only inspections of a running workflow's state. They do
not affect execution and are not recorded in the event store. The workflow
defines query handlers that return data about its current state.

Implementation: the engine sends a query message to the workflow process.
The process handles it via a registered query handler and replies. Since
this is just BEAM message passing, it's fast and non-disruptive.

```gleam
pub fn handle_query(query: String) -> Result(Dynamic, QueryError) {
  case query {
    "current-step" -> Ok(dynamic.from(current_step))
    "order-status" -> Ok(dynamic.from(order_status))
    _ -> Error(UnknownQuery(query))
  }
}
```

### Timers

Timers allow a workflow to sleep for a specified duration — seconds,
hours, days, even months. The sleep is durable: it survives restarts.

Two-tier implementation:
- **In-process timers** use beamr's native timer wheel. Millisecond
  granularity, zero overhead. Used during normal execution.
- **Durable timers** are recorded in the event store and managed by the
  engine's timer service. On restart, the timer service checks for expired
  timers and delivers them. On replay, already-fired timers are skipped
  instantly.

```gleam
workflow.sleep(hours: 24)

let timer = workflow.start_timer("sla-deadline", hours: 24)
// ... later ...
workflow.cancel_timer(timer)
```

### Child Workflows

A workflow can spawn other workflows as children. Child workflows have
their own event history, their own process, and their own lifecycle.
The parent can wait for the child to complete, or fire and forget.

Implementation: the parent workflow process spawns a child workflow
process with a link. The engine creates a new event history for the child.
The parent receives the child's result via its mailbox.

```gleam
let results = workflow.all([
  workflow.spawn("build-service-a", build_workflow, ServiceA),
  workflow.spawn("build-service-b", build_workflow, ServiceB),
  workflow.spawn("build-service-c", build_workflow, ServiceC),
])

let build = workflow.spawn_and_wait("build", build_workflow, config)?
let test = workflow.spawn_and_wait("test", test_workflow, build)?
let deploy = workflow.spawn_and_wait("deploy", deploy_workflow, test)?
```

---

## Execution Tiers

Work in Aion runs in one of three tiers. The tier is chosen based on
determinism (does it need to be a recorded activity?) and locality (does it
run in the BEAM or out-of-process?).

### Tier 1 — Workflow Logic (Gleam on BEAM)

Deterministic orchestration: decisions, control flow, coordination, data
transformation. Runs in the workflow process. Pure. Re-executed on replay.

### Tier 2 — In-VM Work (NIFs + Gleam/Erlang activities)

Runs inside beamr.

- **Deterministic NIF helpers** — fast native Rust functions called inline
  from workflow code: JSON transformation, template rendering, parsing,
  crypto, formatting. No event recorded; re-run on replay. Safe because
  deterministic.
- **Light in-VM activities** — small side-effectful operations that run on
  beamr's dirty scheduler (read a small file, run a quick command). Backed
  by a NIF or Gleam/Erlang code, but invoked through the **activity
  contract** so the result is recorded and returned on replay. Microsecond
  dispatch.

### Tier 3 — Remote Work (worker SDKs, any language)

Runs out-of-process in a worker's own runtime. The heavy lifting:
long-running work, LLM calls, big computations, ML inference, anything in
Python/TypeScript/Go, anything that benefits from isolation or independent
scaling. **Always activities** — every remote worker call is recorded,
retried on failure, and returns its cached result on replay. This is
Temporal's model exactly: workers in any language doing the real work, the
engine orchestrating and recording.

> **A note on language reach.** Gleam has two mutually-exclusive
> compilation targets: Erlang (`.beam`, runs on beamr) and JavaScript
> (`.js`, runs on Node/Deno). Aion workflows compile to the **Erlang
> target**, so in-workflow externals (Tier 1/2) are BEAM languages only —
> Erlang, Elixir, LFE. JavaScript/TypeScript cannot be called inline from
> a beamr workflow. JS/TS heavy lifting lives in **Tier 3 remote workers**,
> which are independent of Gleam's compilation target. (A future second
> Aion backend running JS-target Gleam on a JS runtime is conceivable —
> the event-sourcing model is target-agnostic — but that is a separate
> engine implementation and out of scope here.)

---

## Execution Model

### Process-per-Workflow

Every workflow execution is a BEAM process. This is the fundamental
architectural decision that differentiates Aion from Temporal.

Temporal manages workflow state externally — the server stores a mutable
state record in a database, and workers are stateless machines that load
the state, execute one step, save the state back, and move on. Every
scheduling decision requires a database read, an execution step, and a
database write.

Aion's workflow state lives in the process. The process runs continuously
(subject to the scheduler's preemptive reduction counting) and only
interacts with the event store when an activity completes, a signal
arrives, or a timer fires. During pure workflow logic (conditionals, loops,
data transformation), there is zero persistence overhead.

This means:
- **Lower latency** — no database round-trip per step
- **Higher throughput** — millions of concurrent workflows via lightweight
  processes
- **Simpler programming model** — workflow code is a normal function, not
  a state machine that yields between steps
- **Natural local state** — variables, accumulators, intermediate results
  all live in process memory

### Supervision

Aion uses a three-level supervision tree:

```
Engine Supervisor
  |
  +-- Workflow Supervisor (per workflow type)
  |     |
  |     +-- Workflow Process (per execution)
  |           |
  |           +-- Activity Process (per activity)
  |           +-- Activity Process
  |           +-- Child Workflow Process
  |
  +-- Workflow Supervisor (another type)
        |
        +-- ...
```

If an activity process crashes:
1. The link propagates the exit signal to the workflow process
2. The workflow process (trapping exits) receives the signal as a message
3. The activity's retry policy determines what happens — retry, fail the
   workflow, or escalate

If a workflow process crashes (bug in workflow code, OOM, etc.):
1. The workflow supervisor is notified
2. The engine replays the workflow from its event history, restoring it to
   the last persisted state
3. Execution continues from the last completed activity

If the entire VM restarts:
1. The engine reads all active workflow IDs from the event store
2. Each workflow is replayed from its complete event history
3. All workflows resume where they left off

### Concurrency Within a Workflow

Unlike Temporal where concurrency within a workflow is limited to language
primitives (goroutines, Promise.all), Aion workflows have the full BEAM
concurrency toolkit:

```gleam
// Fan out — all activities run as concurrent BEAM processes
let results = workflow.all([
  activity.new("fetch-user", fn() { fetch_user(user_id) }),
  activity.new("fetch-orders", fn() { fetch_orders(user_id) }),
  activity.new("fetch-prefs", fn() { fetch_preferences(user_id) }),
])

// Race — first to finish wins, others cancelled
let result = workflow.race([
  activity.new("primary-api", fn() { call_primary(request) }),
  activity.new("fallback-api", fn() { call_fallback(request) }),
])

// Dynamic fan-out — spawn activities from data
let results = workflow.map(order.items, fn(item) {
  activity.new("process-" <> item.id, fn() { process_item(item) })
})
```

This is native to the execution model — `workflow.all` spawns N processes,
links them to the workflow process, and uses selective receive to collect
results. No special runtime support needed beyond what the BEAM provides.

---

## Durable Execution

### Event Store

Every externally-observable action is recorded as an event in an
append-only log:

```
WorkflowStarted { workflow_id, workflow_type, input, timestamp }
ActivityScheduled { activity_id, activity_type, input }
ActivityStarted { activity_id, worker }
ActivityCompleted { activity_id, result }
ActivityFailed { activity_id, error, attempt }
TimerStarted { timer_id, duration }
TimerFired { timer_id }
SignalReceived { signal_name, payload }
ChildWorkflowStarted { child_id, workflow_type, input }
ChildWorkflowCompleted { child_id, result }
WorkflowCompleted { result }
WorkflowFailed { error }
WorkflowCancelled { reason }
```

The event log is the source of truth. Workflow state can always be
reconstructed by replaying these events through the workflow function.

### Replay

When a workflow needs to be restored (after VM restart, process crash, or
load balancing), the engine:

1. Loads the complete event history for the workflow
2. Creates a new BEAM process for the workflow
3. Runs the workflow function from the beginning
4. For each `workflow.run(activity)` call, checks the event history:
   - If `ActivityCompleted` exists → return the recorded result, don't
     execute
   - If `ActivityFailed` with all retries exhausted → return the recorded
     error
   - If no event → this is where execution resumes, run the activity for
     real
5. Same for signals (return recorded signal or wait), timers (return
   immediately if fired or set a real timer), and child workflows

The replay is invisible to the workflow code. From the workflow's
perspective, it's just running normally — it doesn't know whether
`workflow.run()` actually executed the activity or returned a cached
result.

---

## Real-Time Event Streaming (WebSocket)

The event store is an append-only log; streaming is tailing that log. When
the engine appends an event, it also publishes it to an in-process
broadcast channel. WebSocket handlers subscribe to this channel, filter for
their client's subscription, and push events to connected clients as they
happen. This is a first-class feature, not an add-on.

Subscription models:
- **Per-workflow** — connect to a workflow ID, receive its events live
  (ActivityScheduled, ActivityCompleted, SignalReceived, ...)
- **Filtered** — subscribe to events matching a query (all workflows of
  type X, all failures, everything in namespace Y)
- **Firehose** — all events, for dashboards and observability tooling

For the Meridian integration, this replaces or augments the current
`ServiceEvent` bus: instead of Meridian-specific event types, the engine
emits standard workflow events that Meridian's WebSocket layer forwards to
the dashboard. The frontend gets real-time workflow execution visibility
for free.

```rust
// Server-side subscription
let mut events = engine.subscribe(
    EventFilter::workflow(workflow_id)
);
while let Some(event) = events.recv().await {
    websocket.send(serde_json::to_string(&event)?).await?;
}
```

---

## Persistence

### The Event Store Trait

```rust
/// Core persistence contract for workflow event history.
#[async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Append events to a workflow's history. Atomic — all or none.
    async fn append(
        &self,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError>;

    /// Read the complete event history for a workflow.
    async fn read_history(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Event>, StoreError>;

    /// List active workflow IDs (for replay on startup).
    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError>;

    /// Query workflows by type, status, or time range.
    async fn query(
        &self,
        filter: &WorkflowFilter,
    ) -> Result<Vec<WorkflowSummary>, StoreError>;

    /// Record a durable timer.
    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Get all timers that should have fired by now.
    async fn expired_timers(
        &self,
        as_of: DateTime<Utc>,
    ) -> Result<Vec<TimerEntry>, StoreError>;
}
```

### Embedded Mode

The default implementation uses **haematite** through
`aion-store-haematite`. With no `[store.cluster]` section it opens or creates
a local data directory and owns every shard. A configured `[store.cluster]`
boot uses the same backend for quorum-replicated, multi-node operation.

libSQL, through `aion-store-libsql`, is the alternative durable backend. It
provides a zero-infrastructure local database file for development,
single-node deployments, and embedded tools. The `EventStore` trait keeps
either backend behind the same engine boundary.

```rust
use aion::EngineBuilder;
use aion_store_haematite::HaematiteStore;

let store = HaematiteStore::create("workflows")?;
let engine = EngineBuilder::new().store(store).build().await?;
```

### Pluggable Backends

```rust
// libSQL alternative
let store = aion_store_libsql::LibSqlStore::open("workflows.db")?;

// Custom
impl EventStore for MyStore { /* ... */ }
```

### In-Process Cache

During normal execution, the BEAM process IS the state — no reads from the
event store are needed. The store is written to (appends) but not read from
until a replay is required. For the engine's own bookkeeping (active
workflow index, timer management, query serving), an in-process Rust cache
maintains a hot view of workflow metadata, populated on startup and kept
current as events are appended.

---

## Workflow Packaging — the `.aion` Format

A workflow is deployed as a single file: a `.aion` package. It is an
archive (zip container) holding everything needed to run the workflow:

- **manifest** — entry module, entry function, input/output schema,
  timeout, declared activities, and a version hash
- **compiled `.beam` files** — the workflow module, its dependencies, and
  the stdlib beams it needs
- **source** (optional) — the `.gleam` source, for inspection and
  recompilation
- **content hash** — the hash of the compiled beams, which doubles as the
  workflow's version identifier and an integrity check

The engine ingests a `.aion` by reading the manifest, unpacking the beams
into beamr's module loader, and registering. One file in, deployable
workflow out — no scattered build artifacts to manage.

This format is the connective tissue between three concerns:

- **Deployment** — deploying a workflow is copying one file.
- **Versioning** — the content hash *is* the version. A new `.aion` with
  different beams is a new version automatically (this answers the
  workflow-versioning open question).
- **Hot code loading** — the `.aion` is the unit of zero-downtime upgrade.
  Deploy a new package, the engine hot-loads the new module versions,
  running workflows keep their pinned version, new workflows use the new
  one.

### Optional Server-Side Authoring

When `aion-server` is configured with a path to the `gleam` binary, it can
compile and package workflows server-side. The full loop becomes: **write
Gleam source → server compiles + type-checks → reports errors → packages as
`.aion` → hot-loads → runs**. Authors get Gleam's type errors inline
without running the toolchain locally. This is optional — without the
toolchain, you deploy pre-compiled `.aion` files. (The engine shells out to
the `gleam` binary rather than embedding the compiler, which is structured
as a binary rather than a stable library crate.)

## Hot Code Loading

beamr's architecture supports hot code loading with focused engineering
work (assessed by Bono at 3-4 briefs):

1. **Dual-version module registry** — each module can have a current and
   old version simultaneously
2. **Process version pinning** — a running process keeps its pinned
   module version until it makes a fully-qualified external call
3. **Dynamic import resolution** — cross-module calls resolve at call
   time, not load time
4. **Automatic purge** — old version is garbage collected when no
   processes reference it (via Arc reference counting)

For workflow execution, this means:
- Running workflows continue executing on the version they started with
- New workflow starts use the latest version
- When a running workflow makes a new external call (e.g., starting a new
  activity, entering a new phase), it picks up the new version
- Zero downtime, zero restarts

This matches and exceeds Temporal's model, where updating workflow
definitions requires deploying new worker binaries and a rolling restart.

---

## Deployment Modes

### Embedded

The engine runs as a library inside your Rust application. No external
services, no network overhead.

```rust
use aion::{Engine, EngineBuilder};
use aion_store_haematite::HaematiteStore;

#[tokio::main]
async fn main() -> Result<()> {
    let engine = EngineBuilder::new()
        .store(HaematiteStore::create("app-data")?)
        .load_workflows("workflows/ebin/")?
        .build()
        .await?;

    let handle = engine.start("payment-flow", order_input).await?;
    handle.result().await?;
    engine.shutdown().await
}
```

Best for: CLI tools, single-service deployments, applications with moderate
workflow volume.

### Standalone Server

The engine runs as its own service, exposing HTTP/gRPC + WebSocket.
Multiple applications connect as clients.

```
aion-server --store postgres://... --port 7233
```

Provides multi-tenant workflow management, monitoring dashboard, distributed
task queues for remote workers, real-time event streaming, and high
availability (with a replicated event store).

Best for: teams with many services that need workflow orchestration,
production environments requiring operational visibility.

### Distributed

Multiple engine instances coordinate through a shared event store.
Workflows are sharded across instances. Instances can be added or removed
for scaling. Requires a distributed-capable event store and a coordination
layer for shard assignment.

Best for: high-volume, high-availability deployments.

---

## Where Aion Exceeds Temporal

| Dimension | Temporal | Aion |
|-----------|----------|------|
| **Min deployment** | 4 services + database | Single binary, zero deps |
| **Concurrency** | Language-level (goroutines, promises) | BEAM processes (millions, preemptive) |
| **Supervision** | Retry policies on activities | OTP supervision trees (hierarchical) |
| **Local dispatch latency** | Task queue poll (~ms) | Process spawn (~us) |
| **Signal delivery** | DB round-trip (~100ms) | Mailbox message (~us) |
| **Type safety** | Varies by SDK (Go weak, TS better) | Full compile-time (Gleam) |
| **Code updates** | Rolling worker restart | Hot code loading, zero downtime |
| **State location** | Database (read/write per step) | Process memory (persist on events only) |
| **Embeddable** | No (requires running server) | Yes (library mode) |
| **GC model** | Language-level (Go STW) | Per-process, no stop-the-world |
| **Failure model** | Retry + timeout | Supervision trees + retry + timeout |

### The key insight

Temporal had to build a distributed process management system in Go — task
queues for dispatching work, a history service for maintaining state, a
matching service for connecting workers to tasks. These are all things the
BEAM provides natively. By building on beamr, we start with the runtime
Temporal wished it had and add the durability layer that BEAM traditionally
lacks.

---

## Integration With Meridian

Meridian is the first consumer of Aion, but the integration is clean —
Meridian implements the `EventStore` trait against its storage layer, and
its domain-specific operations (Norn step execution, git operations, LLM
calls, notification) are activities.

```gleam
import aion/workflow
import aion/activity
import meridian/activities.{scout, develop, review, commit, notify}

pub fn dev_workflow(brief: Brief, config: Config) -> Result(DevResult, DevError) {
  let scout_result = workflow.run(
    activity.new("scout", fn() { scout(brief, config) })
    |> activity.retry(max: 2)
    |> activity.timeout(minutes: 10)
  )?

  // Concurrent development of independent requirements
  let dev_results = workflow.all(
    scout_result.requirements
    |> list.map(fn(req) {
      activity.new("dev-" <> req.id, fn() { develop(brief, req, scout_result) })
      |> activity.timeout(minutes: 30)
    })
  )?

  let reviewed = review_loop(dev_results, max_iterations: 2)?

  let commit_result = workflow.run(
    activity.new("commit", fn() { commit(reviewed) })
  )?

  workflow.run(
    activity.new("notify", fn() { notify(config.notify_member, commit_result) })
  )?

  Ok(DevResult(requirements: reviewed, commit: commit_result))
}

fn review_loop(
  results: List(DevResult),
  max_iterations max: Int,
) -> Result(List(ReviewedResult), DevError) {
  let review = workflow.run(activity.new("review", fn() { review(results) }))?

  case review.approved, max > 0 {
    True, _ -> Ok(review.results)
    False, True -> {
      let fixed = workflow.all(
        review.fixes
        |> list.map(fn(fix) {
          activity.new("fix-" <> fix.req_id, fn() { develop_fix(fix) })
        })
      )?
      review_loop(fixed, max: max - 1)
    }
    False, False -> Error(ReviewFailed(review.feedback))
  }
}
```

The workflow above is type-safe, concurrent (requirements developed in
parallel), durable (survives crashes), and cancellable (kill the process).
It's a normal Gleam function orchestrated by Aion.

---

## Open Questions

To resolve during detailed design:

1. **Workflow versioning strategy** — *Resolved (2026-06-02).* Workflow
   module names are namespaced by the `.aion` content hash, so each
   deployed version is a **distinct, immutable module** in beamr's registry
   rather than a same-name version-swap. Consequences:
   - Version N and version N+5 coexist as separate modules — no conflict.
   - Sidesteps beamr's two-deep module-version limit entirely for workflow
     modules. Critical for Aion, where a workflow may sleep for months and
     would otherwise pin an old version and block further deploys.
   - Replay-safe by construction: an in-flight execution always runs the
     exact module set it started on; that module set never mutates.
   - Complementary to beamr's dual-version hot code loading, which still
     governs shared/stdlib modules, the engine's own modules, and in-place
     system upgrades. Application-level versioning (content-hash naming) on
     top; VM-level hot-loading (two-deep) underneath.

   Remaining sub-question: how the SDK surfaces a deliberate migration when
   an operator *wants* in-flight executions to adopt new logic (rare —
   usually you let them finish on their pinned version).

2. **Event store compaction** — Long-running workflows accumulate large
   histories. Snapshotting (persist current state, truncate old events)?
   Temporal's "continue as new."

3. **Multi-tenancy** — In server mode, separate beamr instances per tenant
   or shared instance with namespace isolation?

4. **Distributed coordination** — For distributed mode, how to assign
   workflow shards? Consistent hashing, leader election, delegated to the
   event store?

5. **Activity heartbeating** — Long-running activities reporting progress
   so the engine knows they're alive. Heartbeat message to the workflow, or
   direct report to the engine?

6. **Search and visibility** — Temporal has a visibility store (often
   Elasticsearch) for searching by custom attributes. Do we want this, and
   where does it live?

7. **Testing utilities** — Temporal provides a test framework that
   simulates time and mocks activities. The Gleam SDK needs an equivalent.

8. **Error classification** — Retryable vs terminal errors. Model
   explicitly in Gleam's type system (RetryableError vs TerminalError)?

9. **Remote worker protocol** — Task-queue polling (Temporal-style) vs
   push dispatch. Transport (gRPC vs WebSocket). Heartbeat and reconnection
   semantics.

---

## Summary

Aion combines three technologies at their points of maximum leverage:

- **Gleam** for type-safe workflow authoring
- **BEAM** (via beamr) for concurrent, supervised, fault-tolerant execution
- **Rust** for durable persistence, performance-critical infrastructure,
  and embedding

The result is a workflow engine that matches Temporal's durability
guarantees while exceeding it in concurrency (BEAM processes vs language
threads), deployment flexibility (embedded to distributed), type safety
(Gleam vs dynamic SDKs), code update story (hot loading vs rolling
restart), and operational simplicity (zero-infrastructure default).

It is general purpose by design. Meridian is the first application built on
it, not a constraint that shapes it.

See **COMPONENT-ARCHITECTURE.md** for the full crate and package breakdown.
