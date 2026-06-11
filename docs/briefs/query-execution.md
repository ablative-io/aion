# Implementation Brief #45 — Real Workflow Query Execution for the Aion Engine

**Repo:** `/Users/tom/Developer/ablative/aion`, main @ `b5cf40e9` ("docs: query endpoint is wire-surface only, not implemented end-to-end").
**beamr:** crates.io `0.4.9` (local mirror at `~/Developer/ablative/beamr`, same version — READ-ONLY; any beamr change ships through Tom's separate process and is explicitly flagged below).
**Coordination warning:** another agent is actively editing `crates/aion/src/engine/delegated.rs`, `engine/builder.rs`, `engine/mod.rs`, `error.rs` (aion), and `aion-server/src/error.rs` (event-publisher feature, new `crates/aion/src/publish/` + `engine/startup.rs`). All file:line references below are HEAD state (`git show HEAD:<path>`). **Rebase on the event-publisher feature before starting; do not assume working-tree state.** No `git stash` — commit verified waves immediately (hard project rule).

This brief is self-contained: an implementing agent needs no prior conversation context.

---

## 1. Verified current-state map

### Wire surface (complete, works end-to-end up to the engine)

- `QueryRequest{namespace, workflow_id, run_id, query_name}` / `QueryResponse{oneof outcome: Payload result | WireError error}` — `crates/aion-proto/proto/workflow.proto:46-58`. **No query-arguments payload at the wire** (CLIENT-CONTRACT documents args as future AW work).
- HTTP `query_workflow` → `handlers::query` — `crates/aion-server/src/api/http.rs:159`; gRPC `WorkflowService::query` — `api/grpc.rs:80`.
- `handlers::query` (`api/handlers.rs:145-181`): namespace guard → `resolve_run_id` (latest run default) → `engine.query(&workflow_id, &run_id, name)` → wraps success in `outcome::Result`. All errors currently leave via `map_workflow_operation_error` (`handlers.rs:370`) as transport-level `WireError`; the `QueryResponse.error` oneof is **never populated by the server today**.

### Engine side (stub)

- `Engine::query` (`crates/aion/src/engine/delegated.rs:243-258` at HEAD): resolves `(id, run)` in the live registry → `workflow_not_found` on miss (no terminal-history check, unlike `Engine::signal` at :217-230) → delegates to the `QueryService` seam.
- `DeferredQueryService` (`engine/delegated.rs:196-208` at HEAD): always `Err(EngineError::Runtime { "query service seam is not configured" })`. **Nothing ever installs a real one**: `EngineBuilder::query_service` exists (`engine/builder.rs:371-380` at HEAD) but no production caller; `aion-server/src/state.rs:106-119` installs only `signal_router_factory`.
- `crates/aion/src/query/service.rs` — AT-007's `QueryService<H: EngineHandle>` is **implemented and unit-tested against fakes** (resolve residency → deliver `WorkflowMailboxMessage::Query{name, payload, reply_to}` → `tokio::time::timeout` on the oneshot). Typed `QueryError { UnknownQuery, Timeout, NotRunning, Unknown, ReplyDropped, Engine }`.
- `QueryMailboxEngine::deliver_workflow_message` (`runtime/nif_query_mailbox.rs:50-73`): checks a handler is registered, then **replies hardcoded `Ok("{}")` at :62**. Never delivers anything into the workflow process. Its `resolve_workflow` (:32-48) ignores run id and ignores `HandleResidency::Suspended`.
- `QueryHandlerRef { pid, handler: Term }` (`runtime/nif_query.rs:37-41`): `register_query` stores the Gleam handler **fun Term** in a Rust-side map (`QueryHandlers.handlers`). The handler is **never executed in production**. ⚠ **Latent soundness bug:** beamr's GC is moving (roots rewritten in place — `beamr/src/process/gc.rs`, `process/mod.rs:430-459`); a Term held in a Rust map is not a GC root and **dangles after the first workflow-process GC**. The fix below removes the stored Term entirely.
- `QueryHandlers.pending` (reply oneshots keyed by query_id) is only populated by `#[cfg(test)] insert_pending_reply` (`nif_query.rs:330-341`). `reply_query` NIF works but only over that test path.
- `dispatch_query` NIF (`nif_query.rs:157-177`): in-engine Gleam→Gleam query; **hardcoded `Duration::from_secs(30)`** at :169 (violates the no-hardcoded-defaults rule).
- `RuntimeHandle` delivery surface (`runtime/handle/delivery.rs`): signal / activity / timer wake markers exist; **no query marker, no query dispatch**.
- `EngineError` (`crates/aion/src/error.rs` at HEAD): no query variants. Signals got `SignalRouter(#[from] SignalRouterError)`; queries got nothing.
- `consume_wake_marker` (`runtime/nif_wake.rs:27-60`): the three suspending awaits consume exactly one marker atom per invocation from {`activity_complete`, `activity_failed`, `aion_activity_result`, `aion_signal_received`, `aion_timer_fired`}.
- Suspending awaits (two-phase suspend; park via `ctx.request_suspend(None)`, re-invoked from the top on any mailbox wake — `beamr .../interpreter/opcodes/trampoline.rs:206-232 handle_suspend`):
  - `receive_signal` — `runtime/nif_signal.rs:314-365` (pin in `EngineNifState::pending_awaits`).
  - `sleep` — `runtime/nif_timer.rs` (`timer_call` Suspend arm :186-196, `park_sleep` :92-108).
  - `await_activity_result` — `runtime/nif_activity_dispatch.rs:213-241`.
  - **Not yield points:** `await_child` / `collect_all|race|map` are **dirty NIFs that block** (`runtime/engine_nifs.rs:160-167`, `run_until_exit`); a workflow inside them cannot service queries until they return.
- `EngineNifState` (`runtime/nif_state.rs`): per-engine NIF state; has `query_bridge`, `query_handlers`, `pending_awaits`, `cleanup_process(pid)`.
- Gleam SDK (`gleam/aion_flow/src/aion/query.gleam:23-37`): `query.handler(name, codec, reply)` builds `encoded_reply = fn(query_id) { ffi.reply_query(query_id, codec.encode(reply())) }` and registers it via `ffi.register_query(name, encoded_reply, config)` — awaiting a `query_id` that can never arrive today. Await wrappers: `signal.gleam:42`, `workflow/run.gleam:48`, `workflow/timer.gleam:31,79`.
- Test harness `gleam/aion_flow/test/aion_flow_ffi.erl` re-implements the FFI for pure-Gleam tests (register_query/3, reply_query/2 etc.).

### beamr capabilities (verified at 0.4.9, the version aion pins)

- **Native→BEAM closure trampoline exists**: `ProcessContext::set_trampoline(fun, args)` / `set_continuation_trampoline(fun, args, NativeContinuation)` (`beamr/src/native/context/mod.rs:1316-1345`); the interpreter applies the closure on the *same process* with free variables loaded (`interpreter/opcodes/trampoline.rs:60-150`). aion's `with_timeout` NIF already uses it (`runtime/nif_timeout.rs:78-116`, `NativeContinuation::AionTimeout` with opaque `state_id` + resume fn).
- **Continuation resume cannot suspend**: `handle_native_continuation` (`trampoline.rs:153-204`) only handles `ContinuationStep::Done | Call`; a suspend requested inside a resume is ignored. So a trampolined query handler cannot be followed by "park again" without a beamr change.
- **Closures with captured env cannot be spawned as processes**: every spawn-fun BIF requires `closure.num_free() == 0` (`native/process_bifs/mod.rs:384-390, 443-447`); `SpawnFacility::spawn_lambda` carries only module + lambda index. Heaps are per-process; no cross-process term sharing.
- **Process dictionary is GC-rooted and rewritten in place** (`process/mod.rs:75, 327-348, 446-449`; `gc/tests.rs:518-545`), with `erlang:put/2`, `get/1` BIFs registered (`native/dictionary_bifs.rs`). This is the safe place to keep handler funs.
- `Scheduler::enqueue_atom_message(pid, atom)` (`scheduler/mod.rs:979`) — used by all aion wake markers, with aion-side retry for just-spawned/executing windows (`runtime/handle/delivery.rs:267-300`).

### Contract (authoritative)

- `docs/design/aion-clients/CLIENT-CONTRACT.md`: query is a synchronous, deadline-bounded round trip (never fire-and-forget); error rows: `QueryFailed` ← "QueryResponse.error … handler ran and reported an application-level failure"; `QueryTimeout` ← `WireErrorCode::QueryTimeout`; `InvalidArgument` ← `WireErrorCode::UnknownQuery` and `WireErrorCode::NotRunning`.
- `WireErrorCode` (`crates/aion-proto/src/error.rs:37-57`, proto enum `common.proto:60-71`): closed set `{not_found, namespace_denied, sequence_conflict, unknown_query, query_timeout, not_running, lagged, invalid_input, backend}`. **There is no `query_failed` wire code** (see Q1).
- Design brief AT-007 (`docs/design/aion-time-signals/briefs/AT-007.md`): C19 distinct message kind + one-shot reply; C20 never records, answered **at a yield point**; C21 unknown query typed error, workflow does not panic; C22 engine-configured timeout (CO10: not hardcoded), terminal/unknown → NotRunning/Unknown, **never replay/resume a workflow solely to answer**.
- **Design gap (explicit):** AT-007 R1 scopes only the *dispatch service*; it delegates handler registration to AF and "the engine-side dispatch it binds to" to AE — but **no brief specifies the mechanism that executes the registered handler fun inside the workflow process**. AF-007 covers the typed Gleam surface only. This brief proposes that mechanism (Section 2) — that is the intended new design work, not scope creep.

### SDK error mappings (verified; what already works)

- Rust `aion-client`: `unknown_query|not_running|invalid_input → InvalidArgument`, `query_timeout → QueryTimeout` (`src/error.rs:130-138`); `QueryResponse.outcome.Error` handled, with `code==Backend → QueryFailed` (`src/ops.rs:481-486`); local deadline → `QueryTimeout` (`ops.rs:197`).
- TypeScript: `query_timeout → QueryTimeoutError`, `unknown_query|not_running → InvalidArgumentError`, `"query_failed" → QueryFailedError` (case exists but no wire code emits it), `backend → ServerError` (`sdks/typescript/aion-client/src/errors.ts:167-198`); `queryRaw` maps `response.error` through `mapWireError` (`client.ts:264-281`).
- Python: `map_query_error` maps specific codes, **falls back to `QueryFailed`** for unrecognized errors, but `backend → ServerError` (`aion_client/errors.py:218-231`).
- Gleam client: `WireUnknownQuery → InvalidArgument`, `WireQueryTimeout → QueryTimeout`, `WireNotRunning → InvalidArgument` (`gleam/aion_client/src/aion_client/error.gleam:46-56`).

Conclusion: **unknown_query, query_timeout, not_running need zero SDK work.** Only `QueryFailed` is encoded inconsistently across SDKs (Q1).

---

## 2. Candidate execution mechanisms

Queries must run the registered handler fun against the workflow's in-memory state. The fun's captured environment lives on the workflow process heap, so only that process can apply it (verified: no env-carrying spawn, no cross-heap sharing — beamr findings above).

### Candidate A — Yield-point query pump (sentinel return to the SDK) — **RECOMMENDED**

Delivery enqueues a pending-query record (Rust side, per pid) plus an `aion_query` wake marker. Each of the three suspending await natives, on every invocation (fresh entry *and* wake re-entry), checks the pending-query queue first; if non-empty it returns a sentinel `{error, <<"aion_query:{json}">>}` to Gleam instead of suspending. A small pump loop in `aion_flow`'s await wrappers recognises the sentinel, looks the handler up **in the process dictionary** (GC-safe), applies it inside an Erlang `try/catch` helper, replies through the `reply_query` NIF (one-shot to the caller), and re-enters the same await — which re-resolves identically (await identity is pinned in `pending_awaits`, replay resolution is from history).

| Criterion | Assessment |
|---|---|
| No history writes | ✓ structurally — reply path is the oneshot in `QueryHandlers.pending`; nothing touches a Recorder. Optional hardening: refuse recording NIFs while servicing (Q5). |
| Works while parked | ✓ — marker wakes the parked native; round-trip is ms-scale. |
| Works during active execution | ✓ at the next yield point (sleep/receive_signal/await_activity entry). ✗ inside dirty blocking `await_child`/`collect_*` — query waits, may time out (Q7). |
| Replay safety | ✓ — pump iterations are invisible to history; awaits re-resolve from recorded events; handlers re-register deterministically during replay so recovered processes can answer. |
| Suspended-residency workflows | Typed `NotRunning` (honest; AT-007 forbids resume-solely-to-answer). |
| beamr changes | **None.** |
| Latency | Parked: one wake + one scheduler slice (~ms). Active: bounded by time-to-next-yield-point. |

### Candidate B — Native trampoline execution (`set_continuation_trampoline` from the woken await)

The woken await native trampolines the handler fun directly (the with_timeout pattern), continuation resume re-runs the await resolution. Elegant — no SDK loop, works for raw-Erlang workflows automatically — but **blocked on three hard facts**: (1) a continuation resume cannot suspend (`handle_native_continuation` ignores suspend requests), and "handler ran, await still pending" is the *common* case → requires a beamr change (honor `take_suspend()` after resume, re-park at the saved native-call position; small but Tom-gated); (2) the handler fun must be fetched from a Rust map as a raw Term → GC-unsound without a beamr long-lived-root API (bigger Tom-gated change); (3) an exception in the trampolined closure crashes the workflow process (`native_call.rs raise_exception`) → still needs an Erlang catch wrapper anyway. Score: no-history ✓, parked ✓, active ✓ (same yield points), replay ✓, suspended-residency same, **beamr changes: 2 (one small, one substantial)**, latency same as A. Rejected for now; viable follow-up if Tom wants raw-FFI consumers serviced without pump cooperation.

### Candidate C — Snapshot/state-projection queries

SDK eagerly evaluates+encodes `reply()` at registration and at every state transition, publishing the encoded payload into an engine-side per-(pid,name) snapshot map; queries are answered engine-side without touching the process. No-history ✓, parked ✓ (instantly), active ✓, replay ✓, suspended-residency **could answer from the last snapshot** (uniquely), beamr changes none, latency best (no round trip). Rejected: changes query *semantics* (answers can be stale relative to the live heap), imposes encode cost on every yield for workflows that are never queried, requires a much larger SDK/registration redesign, and contradicts AT-007's "answered from a registered query handler" reply-channel model. Worth keeping in mind as a future "cached query" opt-in.

### Candidate D — Dedicated query process sharing state

Impossible today: spawn of env-carrying closures unsupported; per-process heaps; no immutable sharing. Listed for completeness only.

**Recommendation: Candidate A.** It is the only design implementable entirely inside aion, it matches AT-007's EARS text ("answered at engine yield points") literally, and it fixes the latent handler-Term GC hazard as a side effect.

---

## 3. Implementation plan (Candidate A)

### 3.1 Sentinel protocol (cross-cutting contract)

- New wake-marker atom: `aion_query`.
- Sentinel returned by suspending awaits to Gleam: `{error, <<"aion_query:", Json/binary>>}` where `Json = {"query_id":"<uuid>","name":"<query name>"}` (JSON, not `:`-splitting — names are author-chosen). Existing await error prefixes (`timeout:`, `unknown:` …) already use this string-protocol style.
- Handler funs live in the **process dictionary** under key `{aion_query_handler, NameBinary}` (2-tuple of atom + binary), written by the SDK at registration. Rust never stores or reads the fun Term again.

### 3.2 `crates/aion` — runtime

**`runtime/nif_state.rs`**
- Add `pub(super) pending_queries: DashMap<u64, VecDeque<PendingQuery>>` (pid → FIFO), `PendingQuery { query_id: String, name: String }`.
- Extend `cleanup_process(pid)` to drain that queue **and** remove+drop the matching `QueryHandlers.pending` senders (dropping the oneshot sender makes the caller's `QueryService` observe `ReplyDropped` — the query-racing-completion path). To enable keyed cleanup, change `PendingMap` to `HashMap<String, (u64 /*pid*/, QueryReplySender)>` or keep a per-pid index. Also remove the pid's `(pid, name)` handler-name registrations.

**`runtime/nif_query.rs`**
- `QueryHandlerRef`: **delete the `handler: Term` field** (GC hazard). Registration becomes a name-set: `HandlerMap = HashSet<(u64, String)>` (or keep map to a unit struct). `registered_handler` → `is_query_registered(state, pid, name) -> bool`.
- `register_query_impl`: drop the Term arg handling; NIF arity changes **register_query/3 → register_query/2** (`name`, `config`) — no backwards compatibility per CLAUDE.md. Update `engine_nifs.rs` registration table.
- `reply_query_impl`: unchanged shape (Ok path). On send-failure keep the typed `reply_dropped:` error string; the pump must treat reply errors as non-fatal (log-and-continue) — a late reply after caller timeout must not crash the workflow.
- **New** `reply_query_error/2` NIF (`query_id`, `message`): resolves the pending sender and sends `Err(QueryError::HandlerFailed { message })` (new variant, below). Register in `engine_nifs.rs` (dirty, like `reply_query`).
- `dispatch_query_impl`: replace the hardcoded `Duration::from_secs(30)` with the engine-configured query timeout carried in `QueryBridgeState` (added at install time).
- `install_query_bridge`: accept and store `Arc<RuntimeHandle>` + `query_timeout: Duration` in `QueryBridgeState`, pass both into `QueryMailboxEngine`.

**`runtime/nif_query_mailbox.rs`** — make `deliver_workflow_message` real:
1. Match `WorkflowMailboxMessage::Query { name, reply_to, payload }` (payload currently always `{}` — wire carries no args).
2. `is_query_registered(state, pid, name)`? No → `reply_to.send(Err(QueryError::UnknownQuery(name)))`, done (never disturbs the workflow).
3. Yes → `query_id = Uuid::new_v4()` (host-side identifier; never workflow-visible state, no determinism impact), insert sender into `QueryHandlers.pending` keyed by query_id (with pid), push `PendingQuery` onto `pending_queries[pid]`, then `runtime.deliver_query_request(pid)` (marker enqueue with the standard retry). If marker delivery fails: remove the pending entries and reply `Err(QueryError::Engine(Delivery{..}))`.
- `resolve_workflow`: respect `HandleResidency::Suspended → WorkflowResidency::NonResident` and terminal cached status if available (today it returns `Resident` for suspended handles — wrong).

**`runtime/handle/delivery.rs`**
- `pub(crate) fn deliver_query_request(&self, workflow_pid: Pid) -> Result<(), EngineError>` — `ensure_live_pid` + `enqueue_signal_marker_with_retry(pid, atom "aion_query")`, mirroring `deliver_signal_received`.
- `pub(crate) fn query_marker_atom(&self) -> Atom`.

**`runtime/nif_wake.rs`** — add `runtime.query_marker_atom()` to the consumable marker array (doc comment update: a query marker consumed by an await that then resolves without checking is *not* safe to ignore, hence the entry-check below runs on every invocation, not only wakes).

**The three suspending awaits** — at the top of each invocation, after engine-state recovery and (where applicable) after `consume_wake_marker`, but **before** the await's own resolution (queries-first; see Q6):
- `nif_signal.rs::receive_signal` (insert around :354, before `receive_signal_impl`),
- `nif_timer.rs::timer_call` for the `sleep` path (or inside `sleep_impl` before resolution),
- `nif_activity_dispatch.rs::await_activity_result_with_context` (insert after the recorded-resolution fast path? **No** — before it; a recorded resolution returning instantly in a tight replay loop must still service queued queries).
Shared helper in a new `runtime/nif_query_pump.rs`: `pub(super) fn take_pending_query_sentinel(state: &EngineNifState, pid: u64) -> Option<Term>` popping one `PendingQuery` and building the sentinel error term. Each await returns it directly when `Some`. The await's `pending_awaits` pin is untouched (the pump re-enters and resumes the same logical await).
- Edge: marker/queue counts need no strict pairing — the entry-check runs every invocation, so a marker consumed "for" a query that was already serviced is harmless (same property the existing markers rely on, `nif_wake.rs:22-26`).

### 3.3 `crates/aion` — query service + engine wiring

**`query/service.rs`**
- Add `QueryError::HandlerFailed { message: String }` ("the workflow's query handler ran and reported an application-level failure").
- Add `pub async fn query_process(&self, process: WorkflowProcessHandle, name, args) -> QueryServiceResult` — identical to `query()` minus the resolve step (run-exact dispatch when the caller already resolved a handle). `query()` keeps using resolve (used by `dispatch_query`).
- Update `nif_query.rs::query_error_reason` for the new variant (`handler_failed:<msg>`); update `gleam/aion_flow/src/aion/query.gleam::query_error` if a distinct SDK-side variant is wanted (`error.QueryEngineFailure` fallback is acceptable).

**New `query/concrete.rs`** — `ConcreteQueryService` implementing the `engine::delegated::QueryService` seam (mirror of `signal/router.rs::ConcreteSignalRouter`):
- Fields: the mailbox `EngineHandle` (the `QueryMailboxEngine` installed in the NIF bridge — expose it from `install_query_bridge` or construct over `Arc<EngineNifState>` + registry + runtime), `query_timeout: Duration`.
- `async fn query(&self, target: &WorkflowHandle, name) -> Result<Payload, EngineError>`:
  1. `target.residency() == HandleResidency::Suspended` → `Err(QueryError::NotRunning(workflow_id).into())` (AT-007: never resume solely to answer).
  2. Terminal guard: like `ConcreteSignalRouter::route` (`signal/router.rs:42-57`), read history under the recorder lock and return `NotRunning` if the run is terminal (covers the exit-monitor race window).
  3. `QueryService::new(mailbox_engine, self.query_timeout).query_process(WorkflowProcessHandle::new(target.pid()), name, json "{}")`.
  4. Map `QueryError → EngineError::Query(e)`.
- Export from `query/mod.rs`.

**`error.rs` (aion)** — add to `EngineError`:
```rust
/// Live workflow query dispatch failed after the target was resolved.
#[error("query error: {0}")]
Query(#[from] crate::query::QueryError),
```
(Coordinate with the in-flight publisher edits to this file.)

**`engine/delegated.rs`** — `Engine::query`: mirror `Engine::signal`'s registry-miss handling — on miss, `read_history`; if `run_has_terminal_history` → `Err(EngineError::Query(QueryError::NotRunning(id)))`; else `workflow_not_found`. (Today a completed workflow yields `WorkflowNotFound`, violating the contract's NotRunning row.)

**`engine/builder.rs`**
- New `pub const fn query_timeout(mut self, timeout: Duration) -> Self` (Option<Duration> field; **no default** — CLAUDE.md "no assumed defaults").
- In `build()` (after `install_engine_nif_seams`, where `runtime`/`nif_state`/`registry` exist): if a `query_timeout` was supplied **and** the seam is still the deferred one, install `ConcreteQueryService` into `DelegatedSeams` (same pattern as the `signal_router_factory` block, `builder.rs:462-471` at HEAD). Explicit `.query_service(...)` override still wins. No timeout + no override → seam stays `DeferredQueryService` (typed "not configured" error, current behavior).
- `install_query_bridge` call gains the runtime + timeout arguments (pass timeout `Option` → bridge only constructed for dispatch_query when configured; or require it — implementer's choice, but no silent 30s).

### 3.4 `gleam/aion_flow` (SDK)

- **`src/aion_flow_query_pump.erl` (new, plain Erlang module compiled into the package)** — the only place that can `try/catch`:
  ```erlang
  -module(aion_flow_query_pump).
  -export([service/1]).
  %% Arg: the sentinel JSON binary. Decodes query_id+name, fetches
  %% erlang:get({aion_query_handler, Name}), applies Handler(QueryId)
  %% inside try/catch; on raise or missing handler calls
  %% aion_flow_ffi:reply_query_error(QueryId, Reason). Always returns ok.
  ```
  Handler raise must reply `HandlerFailed` and **must not** crash the workflow process (C21 analogue).
- **`src/aion/internal/ffi.gleam`** — `register_query/2` (arity change), new `reply_query_error/2` external, new `service_query(sentinel_payload: String) -> Nil` external to `aion_flow_query_pump:service/1`, new pdict-put external (`erlang:put/2`) or do the put inside a tiny `aion_flow_query_pump:register/2` helper so Gleam never touches raw pdict types.
- **`src/aion/query.gleam`** — `handler()`: store `encoded_reply` in the pdict via the helper, then `ffi.register_query(name, register_config())`. Docs: registration must happen before the first yield point that should answer it; re-registration on replay is automatic because workflow code re-executes.
- **New `src/aion/internal/pump.gleam`** — `pub fn run(do: fn() -> Result(String, String)) -> Result(String, String)`: call `do()`; on `Error("aion_query:" <> payload)` → `ffi.service_query(payload)` then recurse; else pass through. Tail-recursive.
- Wrap the three suspending await call sites: `signal.gleam:42`, `workflow/run.gleam:48` (`await_activity_result`), `workflow/timer.gleam:31` (`sleep`). `with_timeout` (`timer.gleam:79`) needs no wrapping — its inner awaits are the yield points.
- **`test/aion_flow_ffi.erl` harness** — update `register_query` to /2 (+pdict store to mirror production), add `reply_query_error/2`, keep `dispatch_query` harness semantics; pure-Gleam tests must stay green.
- Rebuild any committed fixture/package artifacts that embed aion_flow (check `examples/` packaging if CI builds them).

### 3.5 Timeout machinery (single source of truth)

- **Where the deadline lives:** `ConcreteQueryService.query_timeout`, set from `EngineBuilder::query_timeout`, set from aion-server config. Enforced by `tokio::time::timeout` around the oneshot in `query/service.rs` (already implemented).
- On timeout: `QueryError::Timeout` returns immediately; additionally **clean up** — remove the pending sender by query_id and best-effort remove the queued `PendingQuery` (so a never-woken workflow doesn't accumulate stale queue entries; a late `reply_query` then gets `unknown_query_id:*`, which the pump logs and ignores).
- `dispatch_query` NIF uses the same configured value (3.2).
- Caller-side deadlines (SDKs) already exist and are independent (`ops.rs:197` etc.).

### 3.6 `crates/aion-server`

- **`config/mod.rs` (+ `env.rs`, `file.rs`)** — `RuntimeConfig.query_timeout: Duration`; TOML key `query_timeout_ms` (or `[query] timeout_ms` table, match existing style of `drain_timeout`/`worker.heartbeat_window`), env `AION_QUERY_TIMEOUT_MS`, CLI override optional. Default value: see Q2 (server config does have defaults for analogous knobs, e.g. heartbeat 30s).
- **`state.rs`** — the promised ~3-line install: add `.query_timeout(runtime.query_timeout)` to the `EngineBuilder` chain (`state.rs:106-119`).
- **`error.rs`** — extend `wire_from_engine` + `engine_trace_fields` with the `EngineError::Query(e)` arm:
  - `UnknownQuery → WireError::unknown_query(...)` (client-side → InvalidArgument per contract)
  - `Timeout → WireError::query_timeout(...)`
  - `NotRunning → WireError::not_running(...)`
  - `Unknown → WireError::not_found(...)`
  - `ReplyDropped → not_running` ("workflow ended before answering") — see Q3
  - `HandlerFailed → ` per Q1 decision (Backend+`error_type:"QueryFailed"` today, or new `query_failed` code)
  - `Engine(_) → backend`
- **`api/handlers.rs::query`** — populate the `QueryResponse.error` oneof for **query-semantic** failures (`EngineError::Query(_)` mappings above) instead of failing the transport call; keep namespace/not-found/backend as transport-level errors as today. This matches the contract row ("QueryResponse.error") and what all four SDK query ops already parse. Unit tests beside the existing handler tests (`handlers.rs:531`, `:839`).
- **Integration test `tests/query_workflow.rs` + fixture** per `tests/fixtures/README.md` conventions (commit `.erl` **and** `.beam`; regenerate with `erlc -Werror -o crates/aion-server/tests/fixtures <file>.erl`; no toolchain needed at test time):
  - `aion_fixture_query.erl` exporting e.g. `queryable/1`: registers a handler via `aion_flow_ffi:register_query/2` + `erlang:put({aion_query_handler, <<"state">>}, fun(QId) -> aion_flow_ffi:reply_query(QId, <<"{\"n\":1}">>) end)`, then loops on `aion_flow_ffi:receive_signal(<<"release">>, <<"{}">>)` with a hand-rolled pump (`{error, <<"aion_query:", Rest/binary>>} -> aion_flow_query_pump:service(Rest)` — or fixture-local equivalent since aion_flow modules may not be in the fixture package; fixture-local is simpler and proves the raw protocol).
  - Also a `raising` handler (calls `erlang:error`) and use the no-pump `wait/0`-style export for the timeout test.
  - Drive through the full HTTP handler path (`handlers::query`) with the namespace guard, as `namespace_restart.rs` does.

### 3.7 `crates/aion` engine e2e tests

New `crates/aion/tests/engine_query.rs` (+ fixture additions to `crates/aion/tests/fixtures/`, README updated) — see Section 4.

### 3.8 Docs

- Update `CLIENT-CONTRACT.md` only if Q1 chooses a new wire code (taxonomy table + `common.proto` enum + proto error.rs + 4 SDK maps).
- `docs/design` brief JSONs: none of AT-007's requirements change; if Tom wants the pump mechanism recorded, add an AE/AT addendum note — flag, don't self-author cluster JSON without the design pipeline.

---

## 4. Test plan

**Unit (crates/aion):**
1. `nif_query_tests.rs` rewrite: registration stores names only; delivery inserts pending + queue + marker; unknown name replies UnknownQuery without touching the queue; `reply_query_error` → `HandlerFailed`.
2. `query/service.rs`: existing AT-007 tests stay green; add `query_process` coverage + `HandlerFailed` propagation.
3. `ConcreteQueryService`: suspended-residency → NotRunning; terminal-history → NotRunning; happy path against a fake mailbox engine.
4. Pump sentinel: await native invoked with a queued PendingQuery returns the sentinel and leaves `pending_awaits` pinned.

**Engine e2e (`crates/aion/tests/engine_query.rs`, fixtures with hand-rolled pump loop):**
5. **Happy path + determinism (no history writes):** start fixture parked in `receive_signal`; `engine.query` returns the handler payload; assert `store.read_history` byte-identical before/after (count *and* content).
6. **Query during replay:** `recovery_restart.rs` pattern — record progress, drop engine, rebuild (replay re-registers the handler), query the recovered workflow while/after it replays; assert correct answer and history unchanged; then complete the workflow and compare the full history with a never-queried control run (determinism proof).
7. **Query against suspended-residency workflow:** force `HandleResidency::Suspended` (same hook the signal handoff tests use) → typed NotRunning, no resume occurred (residency unchanged, no new events).
8. **Unknown query:** registered workflow, wrong name → `EngineError::Query(UnknownQuery)`; workflow still answers a follow-up valid query (not disturbed).
9. **Handler raising:** `raising` handler → `HandlerFailed`; workflow process still live (subsequent signal completes it normally); no events appended.
10. **Timeout:** fixture parked in plain Erlang `receive` (no pump, like `wait/0`) + short builder timeout → `QueryError::Timeout`; late-reply tolerance: then send the wake signal and assert the workflow completes cleanly despite the dropped reply sender.
11. **Concurrent queries:** N (e.g. 8) simultaneous `engine.query` futures against one parked workflow → all answered, all distinct query_ids drained, queue empty afterwards.
12. **Query racing completion:** fire completion signal and query concurrently in a loop; every outcome is either a valid payload or typed NotRunning/ReplyDropped-mapped error; never a hang, never a panic, never an appended event from the query path; `cleanup_process` leaves no pending entries (assert maps empty).
13. **Query during active execution:** fixture in a sleep/compute loop → query answered within the yield-point bound.
14. (If Q5 accepted) handler that calls `dispatch_activity` → `HandlerFailed`, zero events.

**Server integration (`crates/aion-server/tests/query_workflow.rs`):** happy path over `handlers::query` (outcome=result), unknown query (outcome.error `unknown_query`), timeout (outcome.error `query_timeout`), terminal workflow (`not_running`), namespace-denied unchanged, plus handler-failure encoding per Q1.

**Gleam (`gleam/aion_flow`):** pump unit tests against the test harness (sentinel loop, handler raise → reply_query_error, unknown name), existing query tests updated for register_query/2.

**Gates:** `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `gleam test` in aion_flow; no `#[allow]`/`#[ignore]`/`_var` bypasses.

---

## 5. Open decisions for Tom

- **Q1 — QueryFailed wire encoding.** Today only the Rust client yields `QueryFailed` (from `outcome.error` code=`backend`, `ops.rs:482`); TS maps `backend→ServerError` (its `query_failed` case is dead), Python `backend→ServerError`, Gleam has no path. Options: (a) keep `backend` + `error_type:"QueryFailed"` and patch TS/Python/Gleam query ops to branch on `error_type` — no wire-contract change; (b) add `WireErrorCode::QueryFailed` (`query_failed`) — honest, TS already cases it, but extends the "stable, closed" code set: `common.proto` enum, `aion-proto/src/error.rs` (+`downgrade` table), CLIENT-CONTRACT row, Rust/Python/Gleam client maps. **Recommend (b)** — it's pre-1.0 and the contract row already names the concept.
- **Q2 — Query-timeout configuration default.** Engine builder takes an explicit `Duration` (no default, per CLAUDE.md). Does aion-server config ship a default (e.g. `10s`, consistent with other server-config defaults like heartbeat 30s) or require the operator to set it? Recommend a server-config default of 10s; engine stays explicit.
- **Q3 — ReplyDropped / completion-race mapping.** Workflow exits between delivery and reply → caller sees `ReplyDropped`. Map to `not_running` (recommended; "workflow ended before answering") or `backend`?
- **Q4 — Suspended-residency queries.** This brief returns typed NotRunning (AT-007: never resume solely to answer). Future opt-in resume-on-query (or Candidate-C snapshots for suspended workflows) — want a follow-up brief?
- **Q5 — Determinism guard while servicing.** Set a per-pid "servicing query" flag between sentinel return and reply; recording NIFs (`resolve_command`/recorder paths) refuse with a typed error → handler misuse becomes `HandlerFailed` instead of a silent history write. Strong CLAUDE.md-invariant-2 enforcement, ~50 lines. In scope now (recommended) or follow-up?
- **Q6 — Yield-point priority.** Pump services queries **before** resolving a ready await (operator queries can't be starved by a busy workflow; costs the workflow one pump round-trip per queued query). Confirm, or prefer resolution-first?
- **Q7 — Dirty blocking awaits are not yield points.** `await_child`/`collect_*` block on dirty threads (`engine_nifs.rs:160-167`); queries during them wait (possibly to timeout). Accept + document now; converting them to two-phase suspends is a separate (sizeable) brief. Confirm acceptance.
- **Q8 — beamr follow-ups (all Tom-gated, none required by this brief):** (i) honor suspend requests in `handle_native_continuation` (`beamr/src/interpreter/opcodes/trampoline.rs:153-204`, ~15 lines + tests) — unlocks Candidate B for raw-FFI consumers; (ii) rename/generalize `NativeContinuation::AionTimeout` to a neutral `AionNative` (cosmetic); (iii) a long-lived native GC-root registration API if any embedder ever needs to hold Terms across calls. Ship any of these through the beamr process, never from aion work.
- **Q9 — `dispatch_query` from workflow code.** It is a live, nondeterministic read reachable from workflow code (documented as engine-boundary/test-harness only, `query.gleam:39-45`). Leave as-is (documented), or gate it behind the Q5 flag so workflow code calling it under replay fails typed?

---

## 6. Sequencing & wave structure

Prereq 0: rebase on the in-flight event-publisher feature (touches `engine/delegated.rs`, `engine/builder.rs`, `engine/mod.rs`, `error.rs`, `aion-server/src/error.rs`). Resolve Q1–Q3 (taxonomy/config) before Wave 3; Q5/Q6 before Wave 1 freeze.

- **Wave 1 — engine core (1 agent, `crates/aion` only):** EngineError::Query + QueryError::HandlerFailed; nif_state pending queries + cleanup; real `QueryMailboxEngine` delivery; `deliver_query_request` + `aion_query` marker; register_query/2 + reply_query_error; await-native sentinel checks + `nif_query_pump.rs`; `ConcreteQueryService` + `query_process`; builder `query_timeout` + install; `Engine::query` terminal handling; dispatch_query timeout plumb-through; unit tests. *Exit: clippy/fmt/`cargo test -p aion` green (engine e2e for queries lands in Wave 2 with fixtures).*
- **Wave 2 — SDK + engine e2e (2 agents in parallel):**
  - 2a: `aion_flow` pump (`aion_flow_query_pump.erl`, `internal/pump.gleam`, ffi/query.gleam updates, harness updates, Gleam tests).
  - 2b: engine fixtures (`aion_fixture_query.erl/.beam`, README) + `tests/engine_query.rs` (tests 5–14).
- **Wave 3 — server + taxonomy (1 agent):** server config + `state.rs` install; `error.rs` Query arm; handlers outcome.error encoding + unit tests; `tests/query_workflow.rs` + server fixture; Q1 wire-code work across proto + 4 SDK maps if chosen; CLIENT-CONTRACT touch-up.
- **Wave 4 — review:** Fable-level rigorous review per CLAUDE.md (brief + intent + files), determinism proof re-run, full workspace gates.

Estimated size: Wave 1 ≈ 1.5–2k LoC incl. tests; Wave 2 ≈ 800; Wave 3 ≈ 600 (+ ~300 if Q1(b)).

---

### Appendix: key file:line index

| Concern | Location |
|---|---|
| Engine query entry | `crates/aion/src/engine/delegated.rs:243-258` (HEAD) |
| Deferred seam | `crates/aion/src/engine/delegated.rs:196-208` (HEAD) |
| Builder hook / build wiring | `crates/aion/src/engine/builder.rs:371-380, 440-540` (HEAD) |
| AT-007 service + QueryError | `crates/aion/src/query/service.rs` |
| Mailbox stub (`Ok("{}")`) | `crates/aion/src/runtime/nif_query_mailbox.rs:50-73` (:62) |
| Handler Term storage (GC hazard) | `crates/aion/src/runtime/nif_query.rs:37-41, 117-135` |
| Pending replies (test-only fill) | `crates/aion/src/runtime/nif_query.rs:330-341` |
| Hardcoded 30s | `crates/aion/src/runtime/nif_query.rs:169` |
| Wake markers / delivery | `crates/aion/src/runtime/nif_wake.rs`, `runtime/handle/delivery.rs:64-69, 151-158, 267-300` |
| Suspending awaits | `nif_signal.rs:314-365`, `nif_timer.rs:92-196`, `nif_activity_dispatch.rs:213-241` |
| Dirty blocking awaits | `crates/aion/src/runtime/engine_nifs.rs:160-167` |
| Engine NIF state | `crates/aion/src/runtime/nif_state.rs` |
| Signal-router precedent | `crates/aion/src/signal/router.rs`, `aion-server/src/state.rs:106-119` |
| EngineError | `crates/aion/src/error.rs` (HEAD) |
| Wire codes | `crates/aion-proto/src/error.rs:37-71`, `proto/common.proto:60-71` |
| Server handler / mapping | `aion-server/src/api/handlers.rs:145-181, 370-382`, `src/error.rs::wire_from_engine` |
| Gleam SDK query surface | `gleam/aion_flow/src/aion/query.gleam`, `src/aion/internal/ffi.gleam`, awaits at `signal.gleam:42`, `workflow/run.gleam:48`, `workflow/timer.gleam:31,79` |
| beamr trampoline (read-only ref) | `beamr/src/native/context/mod.rs:1316-1345`, `interpreter/opcodes/trampoline.rs` |
| beamr spawn-fun limit (read-only ref) | `beamr/src/native/process_bifs/mod.rs:384-390, 443-447` |
| Fixture conventions | `crates/aion-server/tests/fixtures/README.md`, `crates/aion/tests/fixtures/README.md` |
