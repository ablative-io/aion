---
type: design
cluster: aion-workers
title: Aion Remote Workers — Worker Protocol Client and the Rust, Python, and TypeScript Worker SDKs
---

# Aion Remote Workers — Worker Protocol Client and the Rust, Python, and TypeScript Worker SDKs

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

This cluster is how work that cannot run on beamr gets done anyway. A
workflow running on the engine reaches a step — an LLM call, an ML
inference, a pandas transform, a Node-only API client — and dispatches it as
an activity. Somewhere, in a separate process possibly on a separate
machine, a worker written in Python or TypeScript or Rust is sitting in a
loop, connected to the engine, holding a function registered under that
activity's name. The engine hands it the task. The worker runs the function
in its own runtime, with its own dependencies and its own scaling, and hands
back the result. The engine records that result. On replay, the recorded
result comes back without the worker ever being asked again.

When this cluster is done, a Python data scientist can write a normal Python
function, decorate it, point a worker at an Aion engine, and have it
participate in durable workflows — retries, timeouts, heartbeats, exactly-
once recording — without learning Gleam, Rust, or the BEAM. The same is true
for a TypeScript engineer and a Rust engineer. Three SDKs, three idioms, one
protocol, identical durability semantics. A workflow author does not know or
care which language served an activity; they see a typed result or a
classified error.

It must feel native in each language. The Python SDK uses decorators and
type hints and `asyncio`. The TypeScript SDK uses generics and `async`/`await`
and npm. The Rust SDK uses traits and `async fn` and cargo. None of them
leaks the wire protocol into the author's face. But underneath, all three
speak the exact same conversation with the engine, defined once in
`aion-proto`, so a worker in any language is interchangeable from the
engine's point of view.

## Problem

The engine runs on beamr. beamr runs BEAM bytecode. Vast swathes of the work
real workflows need to do — machine-learning inference, data-science
pipelines, the JavaScript and Python ecosystems, anything heavy enough to
warrant isolation or independent scaling — cannot run on beamr and must not
run in the engine process even when they could. This is Tier 3 in the
execution model: out-of-process work in a worker's own runtime, always
recorded as an activity, retried on failure, returned from cache on replay.

Without worker SDKs, every team wanting to call out to Python or Node would
hand-roll a client against the raw gRPC/WebSocket protocol: connect,
authenticate, frame a registration message, poll or receive a task, decode
an opaque payload, run their function, classify the error correctly, encode
the result, heartbeat a long-running call, and survive a disconnect without
double-executing a side effect. Getting any one of those wrong silently
breaks durability — a mis-classified error retries a non-idempotent charge,
a dropped heartbeat lets the engine time out a job that is still running, a
botched reconnect re-runs an activity whose result was already recorded.

These are exactly the mistakes the SDK must make impossible. The protocol
semantics — registration, task receipt, result/failure reporting with
retryable-vs-terminal classification, heartbeating, reconnection — are
identical across languages and must be implemented once per language to the
same contract, not reinvented per application. And the contract itself is
owned elsewhere: the server endpoint that dispatches tasks and the wire
types both belong to cluster AW (`aion-server` + `aion-proto`). This cluster
builds the **worker side** against that contract.

## Solution

Three SDK packages, one per ecosystem, each implementing the same
worker-side protocol against the wire types defined in `aion-proto` (AW):

- **`aion-worker`** — Rust, on crates.io. Out-of-process Rust activities.
  Its `protocol` module is the **reference implementation** of the worker
  protocol client; the Python and TS SDKs are faithful ports of its
  semantics.
- **`aion-worker-python`** — Python, on PyPI. The home for ML inference and
  data-science work. Decorator-based activity definition, type hints for
  typed I/O, `asyncio` worker loop.
- **`aion-worker-typescript`** — TypeScript/Node, on npm. Generics-based
  typed activities, `async`/`await` worker loop.

### The Boundary With AW (the engine/server side)

This cluster does **not** define the protocol, the wire types, or the server
endpoint. Those are AW:

- `aion-proto` (AW) defines the wire messages — the registration request,
  the activity task, the completion/failure report, the heartbeat, the
  reconnect/resume frames — as gRPC service definitions plus serde types.
- `aion-server` (AW) exposes the **remote worker protocol endpoint** that
  dispatches tasks to connected workers and ingests their results into the
  engine's event log.

This cluster consumes that contract. Where a wire type or RPC is named in
this design, it is **assumed to exist in `aion-proto`** and is referenced,
not defined. If the AW design names a field differently, the SDK adapts to
AW — AW is the source of truth for the wire. Each brief that touches the
wire states this dependency explicitly via `blocked_by`. The Rust SDK
depends on the `aion-proto` crate directly; the Python and TS SDKs target
the same gRPC/protobuf definitions through their language's generated
stubs (`grpcio`/`protobuf` for Python, `@grpc/grpc-js`/`ts-proto` for TS).

### Protocol Semantics (identical across all three SDKs)

The worker-side conversation, in order:

1. **Connect + handshake.** The worker opens a session to the engine's
   worker endpoint, identifying its **task queue** (the named queue the
   engine dispatches matching activities to) and a worker identity. The
   handshake establishes the session the engine uses to route tasks.
2. **Register activity types.** The worker declares the set of activity
   type names it can serve. The engine will only dispatch tasks for
   registered types to this worker. Registering a name with no handler is a
   programming error the SDK rejects at build time, not at dispatch time.
3. **Receive tasks.** Each task carries an activity type, an `ActivityId`,
   the input as an opaque serialised `Payload` (content-type tagged), and
   the attempt number. The SDK decodes the payload to the handler's input
   type, runs the handler, and prepares a result.
4. **Report completion or failure.** On success, the SDK encodes the
   handler's output as a `Payload` and reports completion for that
   `ActivityId`. On failure, it reports a failure carrying the error and a
   **retryable-vs-terminal classification** (mapped to `aion-core`'s
   `ActivityError` taxonomy via the wire). The engine applies the activity's
   retry policy to retryable failures and fails the activity on terminal
   ones.
5. **Heartbeat.** For long-running activities, the handler periodically
   emits a heartbeat (optionally carrying progress detail). The engine uses
   heartbeats to distinguish a slow-but-alive activity from a dead worker,
   and to power activity timeout/cancellation. A handler that stops
   heartbeating past its heartbeat timeout is treated as failed.
6. **Reconnect + resume.** If the session drops, the SDK reconnects with
   backoff, re-registers, and resumes serving. In-flight results that were
   computed but not yet acknowledged are re-reported on reconnect so a
   completed activity is not lost.

### Key Decisions

**Decision D1 — One protocol, three faithful ports; Rust is the
reference.** The `protocol` module of `aion-worker` is the canonical
worker-side implementation. The Python and TS SDKs implement the *same state
machine* — handshake, register, receive, report, heartbeat, reconnect —
against the same `aion-proto` wire contract. A cross-SDK conformance suite
(AR-011) holds all three to identical observable behaviour. Rejected:
generating all three from a single IDL with no hand-written ergonomics layer
— it would produce unidiomatic, awkward SDKs that authors avoid; the
generated stubs are the transport floor, not the SDK.

**Decision D2 — Long-poll receive over the gRPC stream, not client-side
busy polling.** The worker receives tasks via a **server-streamed** gRPC
call (a long-lived `ReceiveTasks` stream the engine pushes onto), not a
tight client poll loop. This matches the push-style dispatch the
process-per-workflow engine favours (microsecond local dispatch) and avoids
the latency and load of Temporal-style task-queue polling. The stream
doubles as the liveness signal. Rejected: HTTP/1 short-poll — higher
latency, more connections, no natural backpressure. Rejected: a bespoke
WebSocket framing — `aion-proto` is gRPC-first (AW); reusing it keeps one
wire contract. (If AW's final protocol chooses WebSocket for workers, the
SDK's transport layer adapts behind the same session API; the SDK's public
surface does not change.)

**Decision D3 — Transport is gRPC, isolated behind a session trait/interface
in each SDK.** Each SDK wraps the generated gRPC stubs behind a small
internal session abstraction (`WorkerSession` in Rust, equivalents in
Python/TS) so the activity-execution machinery never touches raw stubs. This
keeps the bulk of the SDK testable against an in-memory fake session and
isolates the one place that changes if AW revises the transport. Rejected:
calling generated stubs directly throughout — untestable and brittle to wire
changes.

**Decision D4 — Heartbeating is cooperative, surfaced through the activity
context.** A handler receives an **activity context** object (`ActivityContext`
in Rust, `context`/`ctx` in Python/TS) through which it heartbeats and checks
for cancellation. The SDK does not magically heartbeat on the handler's
behalf — a handler that wants to be heartbeated for liveness must call
`ctx.heartbeat()` (a CPU-bound handler that never yields cannot be
heartbeated and must be split or chunked, documented clearly). Heartbeat
detail is an opaque `Payload`. Rejected: an automatic background heartbeat
thread the author can't see — it would report a wedged handler as alive,
defeating the purpose.

**Decision D5 — Cancellation is delivered, never forced.** When the engine
cancels an activity (workflow cancelled, activity timed out, race lost), the
worker is notified through the session and the SDK flips a cancellation flag
on the `ActivityContext`. The handler observes it (`ctx.is_cancelled()` /
awaiting `ctx.cancelled()`) and returns. The SDK does **not** kill the
handler's thread/task out from under it — cooperative cancellation only, so
the handler can release resources cleanly. A handler that ignores
cancellation runs to completion; its result is discarded by the engine.
Rejected: forced thread termination — unsafe across all three runtimes
(orphaned locks, half-written files).

**Decision D6 — Error classification is explicit at the SDK boundary, never
inferred from the language's exception type.** The author declares whether a
failure is retryable or terminal. In Rust, the handler returns
`Result<Output, ActivityFailure>` where `ActivityFailure` carries the
classification. In Python, the SDK provides `RetryableError` and
`TerminalError` exception base classes; an *unclassified* exception that
escapes a handler defaults to **retryable** (transient infrastructure faults
are the common unplanned case) and is logged loudly as unclassified. In TS,
the same: `RetryableError`/`TerminalError` classes, unclassified throw
defaults to retryable-with-warning. This default is the one place a sensible
default is justified and is called out as a deliberate, documented choice
(not silent). Rejected: defaulting unclassified to terminal — it would turn
a transient network blip into a permanently failed workflow.

**Decision D7 — Reconnect re-reports unacknowledged results before serving
new tasks.** The SDK tracks results computed locally but not yet acknowledged
by the engine. On reconnect, after re-registration, it re-reports those
results first. The engine de-duplicates by `ActivityId` (idempotent ingest,
owned by AW). This makes worker-crash-after-compute-before-ack safe: the
activity's recorded result is not lost, and re-reporting an already-recorded
result is a no-op on the engine side. Rejected: dropping un-acked results on
reconnect — it would silently lose completed work and force a needless
retry of a side effect that already happened.

**Decision D8 — Typed activities per language idiom, opaque on the wire.**
The author defines a handler with concrete input/output types in their
language; the SDK handles `Payload` encode/decode at the boundary using the
content-type tag (JSON as the baseline codec across all three, matching
`aion-core`'s `Payload`). The wire stays type-erased (it carries `Payload`),
exactly as `aion-core` mandates — only the SDK layer knows the concrete
types. Rejected: putting language types on the wire — it would break the
type-erased event/store model and prevent a Python result from being read by
a Rust-embedded engine.

**Decision D9 — Concurrency: a bounded pool of concurrent activity
executions per worker, configured, not assumed.** A worker serves up to N
activities concurrently (Rust: tasks on the async runtime; Python: `asyncio`
tasks, with a thread/process pool escape hatch for blocking/CPU-bound
handlers; TS: concurrent promises). N is **configured by the operator**, not
hardcoded — there is no assumed default concurrency cap. The SDK applies
backpressure on the receive stream when the pool is full. Rejected: unbounded
concurrency — a burst of tasks would exhaust the worker's memory; bounded-
but-configurable is the contract.

### Per-Language Ergonomics

**Rust (`aion-worker`).** An activity is an `async fn` (or a type
implementing an `Activity` trait) taking a typed input and an
`&ActivityContext`, returning `Result<Output, ActivityFailure>`. Registered
on a `Worker` builder by type name. `Worker::run().await` connects and serves
until shutdown. Input/output bound by `Serialize`/`DeserializeOwned`.

**Python (`aion-worker-python`).** An activity is an `async def` (or sync
function run on a pool) decorated with `@activity(name=...)`, taking typed
arguments (type hints drive encode/decode) and a `context`. A `Worker`
object registers decorated functions and `await worker.run()` serves.
Packaged as a PyPI wheel; depends on `grpcio`, `protobuf`, and a JSON codec.

**TypeScript (`aion-worker-typescript`).** An activity is an `async`
function registered with `defineActivity<I, O>(name, handler)`, taking a
typed input, a `ctx`, returning a `Promise<O>`. A `Worker` registers
activities and `await worker.run()` serves. Packaged as an npm module with
ESM + CJS builds and bundled type declarations; depends on `@grpc/grpc-js`
and generated proto types.

## Goals

- A Rust worker can register typed activities and serve them against a
  running engine, with completion/failure/heartbeat/reconnect all working.
- A Python worker offers decorator-based typed activities with the same
  durability semantics, installable from PyPI.
- A TypeScript worker offers generics-based typed activities with the same
  durability semantics, installable from npm.
- All three SDKs classify failures explicitly as retryable or terminal and
  map them onto the wire identically.
- All three SDKs heartbeat long-running activities and observe cooperative
  cancellation through an activity context.
- All three SDKs reconnect with backoff, re-register, and re-report
  unacknowledged results so completed work is never lost on a disconnect.
- A cross-SDK conformance suite proves the three SDKs exhibit identical
  observable protocol behaviour against a shared fake/engine harness.

## Non-Goals

- **No protocol or wire-type definition** — `aion-proto` (AW) owns the wire
  messages and gRPC service; this cluster consumes them.
- **No server-side dispatch endpoint** — `aion-server` (AW) dispatches tasks
  and ingests results; this cluster is the worker side only.
- **No client SDKs** — starting/signalling/querying workflows is cluster AL.
  Workers execute activities; clients drive workflows. Different role.
- **No engine, replay, or recording** — the worker returns a result; the
  engine (AE/AD) records it. Idempotent ingest/de-dup by `ActivityId` is
  AW's responsibility.
- **No in-VM NIF activities** — native activities inside the BEAM are a
  different mechanism, cluster AN.
- **No Go worker SDK** — out of scope for this round (the architecture
  allows it later via the same `aion-proto` contract).
- **No authentication/TLS scheme design** — the SDK passes through credential
  and TLS configuration to the transport; the auth model is AW's (the SDK
  accepts whatever AW requires and exposes it as worker config).

## Structure

The cluster spans three packages. Brief annotations in `[brackets]`.

```
# Rust SDK — aion-worker (crates.io)
crates/aion-worker/Cargo.toml
crates/aion-worker/src/lib.rs                 [AR-001] thin re-export surface
crates/aion-worker/src/config.rs              [AR-001] WorkerConfig (endpoint, task queue, identity, concurrency, TLS/creds passthrough)
crates/aion-worker/src/protocol/mod.rs        [AR-001] pub mod + re-exports (reference protocol client)
crates/aion-worker/src/protocol/session.rs    [AR-001] WorkerSession trait + gRPC-backed impl (connect, handshake, register)
crates/aion-worker/src/protocol/task.rs       [AR-002] ActivityTask decode, TaskResult/TaskFailure encode
crates/aion-worker/src/protocol/heartbeat.rs  [AR-004] heartbeat frame send + heartbeat timeout bookkeeping
crates/aion-worker/src/protocol/reconnect.rs  [AR-005] backoff reconnect, re-register, re-report un-acked results
crates/aion-worker/src/runtime/mod.rs         [AR-002] pub mod + re-exports
crates/aion-worker/src/runtime/loop_.rs       [AR-002] receive→dispatch→report worker loop + bounded concurrency
crates/aion-worker/src/runtime/dispatch.rs    [AR-003] handler invocation, payload decode/encode, failure classification
crates/aion-worker/src/context.rs             [AR-003, AR-004] ActivityContext (heartbeat, cancellation, attempt, ids)
crates/aion-worker/src/activity.rs            [AR-006] Activity trait, ActivityFailure, typed registration
crates/aion-worker/src/worker.rs              [AR-006] Worker builder + run(); shutdown wiring
crates/aion-worker/src/error.rs               [AR-001] WorkerError taxonomy

# Python SDK — aion-worker-python (PyPI)
sdks/python/aion-worker/pyproject.toml          [AR-007] PyPI packaging, deps (grpcio, protobuf)
sdks/python/aion-worker/aion_worker/__init__.py [AR-007] public surface re-exports
sdks/python/aion-worker/aion_worker/session.py  [AR-007] protocol session over generated gRPC stubs
sdks/python/aion-worker/aion_worker/loop.py     [AR-007] asyncio receive→dispatch→report loop + bounded concurrency
sdks/python/aion-worker/aion_worker/reconnect.py[AR-007] backoff reconnect, re-register, re-report
sdks/python/aion-worker/aion_worker/activity.py [AR-008] @activity decorator, type-hint codec, registry
sdks/python/aion-worker/aion_worker/context.py  [AR-008] ActivityContext (heartbeat, cancellation)
sdks/python/aion-worker/aion_worker/errors.py   [AR-008] RetryableError, TerminalError, classification
sdks/python/aion-worker/aion_worker/worker.py   [AR-008] Worker object + run()
sdks/python/aion-worker/aion_worker/proto/      [AR-007] generated stubs (build step)

# TypeScript SDK — aion-worker-typescript (npm)
sdks/typescript/aion-worker/package.json        [AR-009] npm packaging (ESM+CJS), deps (@grpc/grpc-js)
sdks/typescript/aion-worker/tsconfig.json       [AR-009] strict TS config
sdks/typescript/aion-worker/src/index.ts         [AR-009] public surface re-exports
sdks/typescript/aion-worker/src/session.ts       [AR-009] protocol session over @grpc/grpc-js
sdks/typescript/aion-worker/src/loop.ts          [AR-009] receive→dispatch→report loop + bounded concurrency
sdks/typescript/aion-worker/src/reconnect.ts     [AR-009] backoff reconnect, re-register, re-report
sdks/typescript/aion-worker/src/activity.ts      [AR-010] defineActivity<I,O>, JSON codec, registry
sdks/typescript/aion-worker/src/context.ts       [AR-010] ActivityContext (heartbeat, cancellation)
sdks/typescript/aion-worker/src/errors.ts        [AR-010] RetryableError, TerminalError, classification
sdks/typescript/aion-worker/src/worker.ts        [AR-010] Worker class + run()
sdks/typescript/aion-worker/src/proto/           [AR-009] generated proto types (build step)

# Cross-SDK conformance
conformance/aion-workers/README.md              [AR-011] the shared protocol-conformance scenario set
conformance/aion-workers/scenarios.json         [AR-011] language-agnostic scenario definitions
conformance/aion-workers/fake_engine/           [AR-011] a fake worker-endpoint harness driving the scenarios
```

## Constraints

- **CO1** — `unsafe_code = "deny"` in `aion-worker` (Rust). No unsafe.
- **CO2** — No lint-bypass directives. Rust: no `#[allow]`/`#[expect]`/
  `#[ignore]`. Python: no blanket `# type: ignore` or `# noqa`. TS: no
  `// @ts-ignore`/`// @ts-nocheck`/`eslint-disable`. Fix the code instead.
- **CO3** — `mod.rs`/`lib.rs` (Rust) and `__init__.py`/`index.ts` (Python/TS)
  are declarations and re-exports only — no logic.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace) in
  every language.
- **CO5** — The wire contract is owned by `aion-proto` (AW). This cluster
  references wire types and RPCs; it does not define or alter them. Where AW
  and this design disagree on a name or shape, AW wins and the SDK adapts.
- **CO6** — The activity-execution machinery (loop, dispatch, context,
  classification, reconnect) MUST be testable without a live engine, against
  an in-memory fake session/endpoint. No test may require a real network
  engine to pass; tests needing one use a runtime env-gate (`AION_WORKER_TEST_URL`)
  and skip with a logged line when unset.
- **CO7** — Payload encode/decode uses the content-type tag from
  `aion-core`'s `Payload`; JSON is the baseline codec present in all three
  SDKs. An SDK MUST NOT silently change content type on round-trip.
- **CO8** — Failure classification (retryable vs terminal) is explicit at the
  author boundary in all three SDKs and maps onto the wire identically (per
  D6). The unclassified-defaults-to-retryable behaviour is logged, never
  silent.
- **CO9** — Cancellation is cooperative only (per D5). No SDK forcibly kills a
  running handler's thread/task.
- **CO10** — Concurrency limits are operator-configured (per D9). No SDK
  hardcodes a concurrency cap or other tunable as an assumed default.
- **CO11** — Reconnect MUST re-report unacknowledged results before serving
  new tasks (per D7). Completed work is never dropped on a disconnect.
- **CO12** — Per-language toolchains gate every brief: Rust uses
  `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt
  --check` + `cargo test`; Python uses `ruff` + `mypy --strict` + `pytest`;
  TS uses `tsc --noEmit` (strict) + `eslint` + `vitest`. All relevant gates
  pass clean before a brief lands.
