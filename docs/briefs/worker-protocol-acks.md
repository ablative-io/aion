# Implementation Brief — Worker Protocol Acks Wave (tasks #39 + #47 + attempt field + deferred #46 drain signal)

**Repo:** `/Users/tom/Developer/ablative/aion`, main @ `682f6356` ("fix: id-keyed skip-tolerant activity replay walk + cursor cleanups").
**Coordination warning:** the working tree has in-flight edits from other agents in `crates/aion-server/src/stream/*` + `namespace/*` (#37 server splice wave) and `sdks/python/aion-worker/aion_worker/{loop,reconnect}.py` (post-#46 follow-ups). **Every file:line below is committed-HEAD state (`git show 682f6356:<path>`). Rebase on whatever has landed before starting; never assume working-tree state. No `git stash` — commit verified waves immediately (hard project rule).**
**Prerequisite reading:** `docs/briefs/worker-reconnect-policy.md` (the #46 decision record — the drain signal specified here is the "proper fix" it defers), and commit `efc24bbb` (the register-deadlock fix that established today's no-ack registration contract this wave replaces).

This brief is self-contained: an implementing agent needs no prior conversation context.

This is **one wire-contract break**, not four. Per CLAUDE.md NO BACKWARDS COMPATIBILITY, the worker protocol is REPLACED in a single commit wave — no versioned-alongside frames, no compat decode paths, no deprecated fields. A worker built from this tree speaks only the new contract; a server built from this tree speaks only the new contract. Mixed-version operation is not supported and no code path may pretend it is.

---

## DECISIONS — ADOPTED (orchestrator under Tom's standing directive, 2026-06-11; Tom may override)

- **O1 → ship the full RegisterAck payload** (`worker_id`, authorized `namespace`, `heartbeat_window_ms`). The wire breaks once; a bare ack would mean breaking it again the first time server→worker config propagation is needed. The fields are read-only facts the server already owns.
- **O2 → derive ack/send deadlines from `reconnect.max_backoff`** (recommended). It is already the policy's "longest tolerable pause" and the #46 health threshold; a second knob for the same concept would drift.
- **O3 → align Python/TS to the Rust rule**: shutdown during an error-class pending drop surfaces the drop error; drain/clean-close exits clean. SIGTERM exit codes in Py/TS change during fault recovery — that is the point: a supervisor should see "this worker was mid-fault" distinctly from "this worker drained cleanly".

## DECISIONS — made in this brief (defended in the sections cited)

- **D1 — Register-ack is a positive-only frame; there is NO nack frame.** Header receipt stops being the registration-success signal because it is not proxy-safe and carries no server data (§2.1). Denials keep flowing exclusively as the RPC's gRPC error status (`PermissionDenied`/`Unauthenticated`/`InvalidArgument`), preserving the entire existing SDK denial-classification machinery untouched. A nack frame would be a second, drift-prone denial channel duplicating the taxonomy that already works.
- **D2 — `RegisterAck` carries `worker_id`, the authorized `namespace`, and `heartbeat_window_ms`** (§2.1). These are server-assigned/server-owned values workers cannot otherwise know; the frame is the only honest vehicle for them. (Contents flagged to Tom as O1 — additive product surface.)
- **D3 — The worker does not enter the serve loop (and does not replay unacked results) until the ack arrives.** Between Register-sent and ack the worker queues nothing and rejects nothing — the server cannot legally send tasks before the ack (server sends ack as the guaranteed first frame, §3.1), so there is nothing to queue. A task/drain/result-ack frame arriving before `RegisterAck` is a protocol violation → typed decode error → retryable, budgeted drop (a server-side ordering bug must surface loudly, not wedge a worker).
- **D4 — Ack-wait and report-send deadlines derive from `reconnect.max_backoff`** (Rust) / `max_backoff_seconds` (Py) / `maxDelayMs` (TS) (§2.2, §4). No new config, no invented constant: the max-backoff cap is already the policy's own operator-stated definition of "the longest tolerable pause" (it is the session-health threshold from #46, `crates/aion-worker/src/protocol/reconnect.rs:184-192`). A send or ack-wait that outlives it is by the operator's own definition a dead session. (Derivation flagged to Tom as O2.)
- **D5 — The server does NOT persist ack state.** `ResultAck` is session-local bookkeeping; the durable truth remains the workflow event history. An ack lost in transit costs one redundant re-report on the next session, which the server acks-and-drops idempotently (§2.2, failure table F3). At-least-once stays honest end to end.
- **D6 — Every well-formed `ActivityResult` frame is acked, including duplicates with no pending waiter.** A result the server can no longer apply (dispatch already timed out / completed / failed-over) is a result whose obligation is discharged — re-reporting it forever can never change engine state. Not acking it would make the tracker grow without bound. Malformed results (missing ids) cannot be acked (no key) and are logged at error level — replacing today's silent `if let Ok` drop (`crates/aion-server/src/api/worker_grpc.rs:140`).
- **D7 — `attempt` is stamped by the producer at the engine dispatch seam and validated by consumers.** The `aion::ActivityDispatcher` trait gains an `attempt: u32` parameter (replace, don't add alongside); the engine NIF entry passes `1` today in exactly one documented place (it is the first delivery — there is no retry executor yet, §2.3); the server stamps the wire from that parameter; all three SDKs **read** the wire value and **reject `attempt == 0` as malformed** (proto3 zero = "producer didn't stamp it"). `WIRE_DEFAULT_ATTEMPT` is deleted from all three SDKs — that is the entire point.
- **D8 — Heartbeat keying does NOT gain attempt.** Server liveness tracks "this worker owns this (workflow, activity) in-flight task" (`crates/aion-server/src/worker/heartbeat.rs:59-60`); two attempts of one activity are never concurrently in flight on one worker, and adding attempt to the key would break liveness continuity across redelivery. Attempt is surfaced via `ActivityContext` (all three SDKs already expose the property) and in task-receipt logs.
- **D9 — Drain semantics (per the approved #46 record):** drain frame → finish in-flight work, stop pulling, close, redial after `initial_backoff` **without consuming or resetting the drop budget** (health-reset still applies if the session independently proved healthy); denial-class status → terminal; unannounced close → budgeted retryable drop. Drain classification **latches for the session**: once a drain frame is seen, the subsequent stream end — clean OR abrupt — is drain-class (resolves server-crash-mid-drain honestly, F6/F7). The message keeps the name `DrainRequest` (the wire tag and shape are unchanged; only the contract text changes — renaming is churn with zero wire effect).
- **D10 — The three-way drain-frame divergence is eliminated by making the drain event a first-class session event in all three SDKs:** Rust stops mapping it to a clean serve end (`crates/aion-worker/src/runtime/loop_.rs:207-209`), TS stops silently skipping it (`sdks/typescript/aion-worker/src/session.ts:169-176` — its hand-written stub doesn't even decode the field, `src/proto/worker.ts:49-51`), Python stops raising `TransportError` on it (`sdks/python/aion-worker/aion_worker/session.py:338`).
- **D11 — Shutdown-during-backoff outcome aligns on the Rust rule, refined by drain class:** when shutdown interrupts recovery, a pending **drain-class or clean-close** drop ends the run `Ok`; a pending **error-class** drop surfaces its error in all three SDKs. The drain signal gives the principled basis: the expected operator path (server deploy) is now drain-class and exits clean everywhere, so what remains pending at shutdown is a genuine fault, and CLAUDE.md's no-silent-failures standard says surface it. This changes Python/TS exit behavior under SIGTERM-during-error-recovery (flagged to Tom as O3).
- **D12 — Collateral correctness fix, in scope of the server wave:** `PendingActivities` is keyed by bare `ActivityId` (`crates/aion-server/src/worker/bridge.rs:52-54`) while the dispatcher fabricates ids from a process-local counter (`bridge.rs:441-443`). After a server restart the counter resets, so a stale re-report from a worker's previous session can complete a **different** workflow's new dispatch that reuses the same sequence position. Result-ack institutionalizes re-reporting, so this latent collision must be closed in the same wave: key by `(WorkflowId, ActivityId)` like every other tracker in the stack (`heartbeat.rs:59-60`, Rust `reconnect.rs:59-63`, Python `reconnect.py` key, TS nested map).

## Open decisions for Tom (everything else above is decided)

- **O1 — RegisterAck payload.** D2 proposes `worker_id` + `namespace` + `heartbeat_window_ms`. A bare `RegisterAck {}` also satisfies #39; the three fields are additive product surface (worker-side observability + the first server→worker config propagation). Confirm or strip.
- **O2 — Deadline derivation.** D4 derives ack-wait and report-send deadlines from the existing `reconnect.max_backoff`. The alternative under the no-arbitrary-defaults rule is a new REQUIRED explicit config field (`report_deadline`) in all three SDK configs. Derivation keeps config surface flat; explicit field is more honest if Tom thinks "longest backoff pause" and "longest send wait" are genuinely independent knobs.
- **O3 — Shutdown-during-error-backoff alignment (D11).** Aligning Py/TS to surface the pending error changes their SIGTERM exit codes during fault recovery (clean → nonzero). Operationally visible; confirm.

---

## 1. Verified current-state map (all references @ 682f6356)

### 1.1 Wire contract

- `crates/aion-proto/proto/worker.proto` — single bidi stream `WorkerProtocol::StreamWorker` (:10-12). `WorkerToServer` oneof `{register=1, result=2, heartbeat=3}` (:20-26). `ServerToWorker` oneof `{task=1, drain=2}` (:29-34). `DrainRequest {}` documented as "finish already-assigned work and stop expecting new tasks" (:14-17) — note this is a *shutdown* notice today, not a *reconnect* signal. `ActivityTask{workflow_id=1, activity_id=2, activity_type=3, input=4}` (:45-50) — **no attempt field**. `ActivityResult{workflow_id=1, activity_id=2, oneof outcome{result=3, error=4}}` (:53-60). `Heartbeat{workflow_id=1, activity_id=2, progress=3}` (:78-82).
- Hand-mirrored prost/serde structs: `crates/aion-proto/src/worker.rs` (`ProtoRegisterWorker` :43, `ProtoActivityTask` :54-67, `ProtoDrainRequest` :71, `ProtoActivityResult` :75-101, `ProtoHeartbeat` :105-115) with JSON+prost round-trip tests (:292-316). Tonic stubs are build-generated (`src/generated.rs:14` `tonic::include_proto!("aion")`, `build.rs`).
- Python stubs are protoc-generated by the hatch hook `sdks/python/aion-worker/build_proto.py` into `aion_worker/proto/worker_pb2*.py` (committed). TS stubs are **hand-written** interfaces: `sdks/typescript/aion-worker/src/proto/worker.ts` — whose `ServerToWorker` (:49-51) declares only `task` (the `drain` field that exists on the wire is not even decodable in TS today).

### 1.2 Server (`crates/aion-server`)

- `src/api/worker_grpc.rs::stream_worker` — reads the first inbound frame and demands `RegisterWorker` **before returning the response stream** (:53-66); authorizes + registers (:71-76, `ConnectedWorkerRegistry::accept_registration`, registry.rs:107-122); spawns a write forwarder copying the registry channel (`worker_rx`) onto the gRPC response channel `task_tx` (:85-96); then `process_inbound` (:128-179). **There is no ack of any kind: header receipt = success, gRPC error status = denial** (`status_from_server_error` :243-250 maps `NamespaceDenied → PermissionDenied`, else `Internal`).
- `process_inbound` Result arm (:138-149): decodes, `heartbeat.complete_task`, `drain.notify_activity_drained`, `pending.complete_activity` — **all results discarded with `let _ =`/`if let Ok`**; a malformed result vanishes silently; nothing is ever sent back to the worker.
- `WorkerMessage{ActivityTask, DrainRequest}` is the server-internal dispatch→stream channel type (`src/worker/registry.rs:17-23`); `broadcast_drain` try_sends `DrainRequest` to every worker on graceful shutdown (registry.rs:206-221, called from `src/shutdown.rs:123` inside `drain_after_first_signal` :113-143, which then waits `drain_timeout` for the heartbeat tracker to empty and fails leftovers as retryable lost-worker errors :145-164).
- `WorkerActivityDispatcher` (the production engine seam, installed at `src/state.rs:96-105,122`): `dispatch_blocking` fabricates `workflow_id = WorkflowId::new_v4()` and `activity_id` from a process-local `AtomicU64` per call (`src/worker/bridge.rs:441-443`), inserts a sync-channel waiter into `PendingActivities` **keyed by bare `ActivityId`** (:52-54, :460), pushes `ProtoActivityTask` (:484-499 — no attempt), and blocks up to `timeout` (hardcoded 30s at :158; `with_timeout` :201 exists but `state.rs` never calls it). Completion path: `PendingActivities::complete` removes the waiter; a second result for the same id finds nothing and returns `false` (:63-69) — this is the "idempotent ingest" the SDKs rely on.
- `HeartbeatTracker` keys in-flight tasks by `(WorkerId, WorkflowId, ActivityId)` (`src/worker/heartbeat.rs:59-60`); lost workers fail tasks back through the sink as retryable `lost_worker_error` (:276-292, dispatch.rs:240-246).
- Server config: `worker.heartbeat_window` (config/mod.rs:210-216, default 30s :499), `drain_timeout` (:259). No dispatch-timeout or ack knobs exist.

### 1.3 Rust worker (`crates/aion-worker`)

- `src/protocol/session.rs` — `WorkerSession` trait (:55-104) with the no-ack contract documented at :67-71; `GrpcWorkerSession::open_registered_stream` queues `RegisterWorker` before issuing the RPC and treats header receipt as success (:183-216 — the efc24bbb deadlock fix; pinned by `tests/grpc_registration.rs`, whose `ScriptedRealServer` :37-82 explicitly "never acks"). `decode_server_message` (:372-386): `Task → WorkerSessionEvent::Task`, `Drain → WorkerSessionEvent::Drain`, empty → decode error. Sends go through an mpsc(16) channel polled by tonic (:192, :218-235) — **if the server stops reading, `send().await` blocks forever; there is no deadline anywhere on the send path.**
- `src/protocol/reconnect.rs` — `UnackedResultTracker` keyed `(workflow uuid, sequence position)` (:59-69) with an `acknowledge()` method (:88-95) that **nothing ever calls in production** (no ack frame exists); `re_report_unacked` re-sends the whole snapshot on every reconnect, no deadline, fail-on-first-error (:338-378); `ReconnectBackoff::max_delay` doc'd as the session-health threshold (:184-192).
- `src/worker.rs::run_with_connector_until` (:184-298) — the #46 loop: `re_report_unacked` at :218 is **outside any shutdown race** (a hung re-report send wedges shutdown); drop classification at :233-251 maps `ServeEnd::StreamClosed → DropCause::CleanClose` (budgeted); budget reset :253-271; shutdown-during-backoff :288-296 surfaces the pending error via `DropCause::into_shutdown_result` (:368-373).
- `src/runtime/loop_.rs` — `ServeEnd::{Shutdown, StreamClosed}` (:56-66); a **wire Drain frame breaks the serve loop into `StreamClosed`** (:207-209), i.e. Rust today treats drain as a clean serve end → budgeted clean-close drop. `SessionHealth` (:70-78). `src/runtime/report.rs::report_finished` records into the tracker **before** sending (:149-164).
- `src/protocol/task.rs` — `WIRE_DEFAULT_ATTEMPT: u32 = 1` (:8), stamped at decode (:64), documented as the three-SDK parity hack (:10-14).
- `src/error.rs` — `WorkerError::{Connect, Handshake, Registration, Decode, Encode, Transport, CleanCloseExhausted}` (:5-58).

### 1.4 Python worker (`sdks/python/aion-worker`, HEAD — working tree is mid-edit)

- `aion_worker/session.py` — `register()` sends the register frame then `_wait_for_connection()` (:215-220, :290-296): **success = gRPC headers/connection readiness**, the Python analogue of the no-ack contract. Outbound is `asyncio.Queue(maxsize=16)` with `put_nowait` (full → `TransportError`, :269, :304) — Python sends fail fast rather than hang; its hang risk is the connection/ack wait. `decode_server_message` (:326-338): `task` → `TaskReceived`; **any other set oneof (including the drain frame the server actually sends) raises `TransportError`** — a graceful server drain kills a Python session as an error-class, budgeted drop today.
- `aion_worker/reconnect.py` — `UnackedResultTracker` (:115) with an `acknowledge()` (:129) nothing calls in production; `re_report_unacked` (:280); `ServerClosedStreamError` (:81).
- `aion_worker/loop.py` — `serve()` returns `ShutdownRequested | StreamFinished`; `connect_register_replay_and_serve` (:189+) implements the #46 budget (`dropped_attempt` :189, :244-245); shutdown during drop backoff **returns cleanly** regardless of drop class (loop condition + `_sleep_or_shutdown`); `_run_and_report` builds `ActivityContext` with `attempt=WIRE_DEFAULT_ATTEMPT` (:318; constant at `context.py:12`).
- `aion_worker/worker.py` — `Worker.run` → `connect_register_replay_and_serve`.

### 1.5 TypeScript worker (`sdks/typescript/aion-worker`)

- `src/session.ts` — `register()` writes the frame; success = the write callback completing (:154-167) — no ack, no header wait. `receiveTasks` iterates the duplex stream and **yields only `task` frames; every other frame (drain included) is silently skipped** (:169-176). `WIRE_DEFAULT_ATTEMPT = 1` (:302) stamped in `decodeTask` (:322). `write()` resolves on the grpc-js flush callback (:235-245) — can hang under backpressure; no deadline.
- `src/reconnect.ts` — `UnackedResultTracker` nested-map keyed (workflowId → activityId) with unused-in-production `acknowledge` (:144-153); `reReportUnacked` (:301-326); `ServerClosedStreamError` (:219).
- `src/loop.ts` — `runWorkerLoop` #46 budget (:129-232); clean close with factory → budgeted `ServerClosedStreamError` (:183-198); shutdown during drop backoff **returns cleanly** (:239-244); recovery replay loop (:204-305).
- `src/worker.ts` — `LiveSessionRouter` repoints heartbeats on reconnect (:53-88); abort closes the current session (:177-196).

### 1.6 Engine attempt provenance

- The domain model already carries attempt: `aion_core::Event::ActivityFailed { attempt: u32 }` (`crates/aion-core/src/event.rs:129-143`), recorder API takes it (`crates/aion/src/durability/recorder.rs:495-506`). Every producer currently passes `1` (`runtime/nif_activity.rs:153`, `runtime/nif_concurrency.rs:152`, `durability/executor.rs:187`).
- The dispatch seam the server implements is `aion::activity::bridge::ActivityDispatcher` — `dispatch(name, input, config)` / `dispatch_from_process(.., caller_pid)` / `dispatch_async_from_process` (`crates/aion/src/activity/bridge.rs:17-70`), invoked from `runtime/nif_activity_dispatch.rs:193` and `runtime/nif_concurrency.rs:209,317`. The Gleam SDK passes the retry POLICY in the config JSON (`gleam/aion_flow/src/aion/workflow/run.gleam:182-191`), but **no retry executor exists yet anywhere** — the server discards config (`bridge.rs:439 let _ = config;`) and no component re-dispatches on retryable failure. Each `dispatch` call is genuinely attempt 1 today.
- The workers design cluster already specifies the field: tasks carry "the attempt number" (`docs/design/aion-workers/DESIGN.md`, Protocol Semantics step 3) — the wire never implemented it; the SDKs papered over it with the parity hack.


---

## 2. The new wire contract (one break)

### 2.0 `crates/aion-proto/proto/worker.proto` — replacement definitions

```proto
service WorkerProtocol {
  rpc StreamWorker(stream WorkerToServer) returns (stream ServerToWorker);
}

// Messages sent by a worker to the server over the bidirectional stream.
// RegisterWorker MUST be the first frame; the server answers it with
// RegisterAck as the first frame on the response stream before any other
// server-to-worker message. (unchanged shape)
message WorkerToServer {
  oneof message {
    RegisterWorker register = 1;
    ActivityResult result = 2;
    Heartbeat heartbeat = 3;
  }
}

// Messages sent by the server to a worker over the bidirectional stream.
message ServerToWorker {
  oneof message {
    ActivityTask task = 1;
    DrainRequest drain = 2;
    RegisterAck register_ack = 3;   // NEW
    ResultAck result_ack = 4;       // NEW
  }
}

// Positive registration acknowledgement. Always the first frame on the
// response stream. There is no negative counterpart: a denied or invalid
// registration fails the RPC with a gRPC error status exactly as before
// (PermissionDenied = ungranted namespace, Unauthenticated = rejected
// credentials, InvalidArgument = malformed first frame).
message RegisterAck {
  // Server-assigned stream identifier for this registration. Stable for the
  // life of the stream; used in server logs ("worker_id=3 lost") so workers
  // can correlate their own logs with the server's.
  uint64 worker_id = 1;
  // The namespace the registration was authorized against (echo of the
  // resolved scope, which authorization may have derived rather than copied).
  string namespace = 2;
  // The operator-configured liveness window on THIS server: a worker whose
  // in-flight activity goes longer than this without a heartbeat will be
  // declared lost. Lets SDKs surface "heartbeat at least this often" to
  // activity authors instead of guessing.
  uint64 heartbeat_window_ms = 3;
}

// Per-result acknowledgement: the server has consumed the identified
// ActivityResult frame and the worker may stop re-reporting it. Sent for
// every well-formed result frame, including duplicates the engine could no
// longer apply (their obligation is equally discharged). NOT a durability
// receipt — the durable truth is the workflow's event history; an ack lost
// in transit merely costs one redundant re-report on the next session.
message ResultAck {
  WorkflowId workflow_id = 1;
  ActivityId activity_id = 2;
}

// Server-initiated drain: this server is going away (restart, deploy,
// rebalance). The worker finishes already-assigned work, reports what it
// can, stops expecting new tasks, and reconnects — to this address after the
// schedule's initial backoff. Receiving DrainRequest re-classifies this
// session's eventual stream end (clean or abrupt) as a DRAIN drop: the
// reconnect consumes NO drop budget. Distinct from denial (gRPC error
// status, terminal) and from an unannounced close (budgeted retryable drop).
message DrainRequest {}

// Activity invocation pushed by the server to a registered worker.
message ActivityTask {
  WorkflowId workflow_id = 1;
  ActivityId activity_id = 2;
  string activity_type = 3;
  Payload input = 4;
  // One-based delivery attempt stamped by the dispatching engine seam.
  // Zero is malformed: consumers MUST reject a task whose attempt is 0
  // (proto3 default = the producer failed to stamp it).
  uint32 attempt = 5;       // NEW
}
```

`RegisterWorker`, `ActivityResult`, `ActivityError(Kind)`, `Heartbeat` are unchanged. `ActivityResult` deliberately does NOT gain attempt: results correlate by `(workflow_id, activity_id)`, the same key every tracker uses; stale-attempt collisions are impossible once D12 lands because the dispatcher fabricates fresh ids per dispatch (bridge.rs:441-443) and the pending map keys on both ids.

Mirror updates (same wave): `crates/aion-proto/src/worker.rs` — add `ProtoRegisterAck` (tags 1-3 as above), `ProtoResultAck` (tags 1-2), add `#[prost(uint32, tag = "5")] pub attempt: u32` to `ProtoActivityTask`, rewrite the `ProtoDrainRequest` doc comment to the drain contract; extend the JSON+prost round-trip test set (:292-316 pattern) to the two new messages and the new field. Regenerate: tonic via `build.rs` (automatic), Python via `build_proto.py` (hatch hook; commit refreshed `worker_pb2*.py[i]`), TS by hand in `src/proto/worker.ts` (add `attempt` to `ActivityTask`, add `RegisterAck`/`ResultAck` interfaces, add `drain`/`registerAck`/`resultAck` to `ServerToWorker`).

### 2.1 Why an explicit RegisterAck improves on header receipt (#39 — the honest assessment)

The no-ack contract was the *correct minimal fix* for the efc24bbb deadlock (queue the register frame before awaiting the RPC), and within a direct worker↔tonic connection it is sound: the server demonstrably reads and authorizes the registration before returning its stream (`worker_grpc.rs:53-76`). Its weaknesses are real, not aesthetic:

1. **It is not intermediary-safe.** "Header receipt" is a transport artifact, not a protocol statement. An L7 proxy (Envoy, nginx-grpc, any mesh sidecar) may forward response headers before the upstream application has produced them or read the request stream's first frame. Behind such a hop, all three SDKs would report registration success for a worker the server has not registered — and the Python SDK's signal is even weaker (`wait_for_connection`, session.py:290-296, is connection readiness, not header receipt). A frame the application sends is end-to-end; nothing else on this path is.
2. **It carries no information.** The server assigns a `WorkerId` used in every lost-worker log line (`registry.rs:31`, `heartbeat.rs` reports) that the worker can never learn; the server enforces a heartbeat window (`config/mod.rs:210-216`) that activity authors must beat but cannot discover. Registration is the natural — and only — point to hand these over.
3. **It makes the registration state machine untestable against the real failure.** The scripted test server (`tests/grpc_registration.rs:37-82`) exists precisely because header-timing semantics are subtle; an explicit frame replaces timing semantics with data semantics.

What it does NOT need: a nack. The denial taxonomy (NamespaceDenied → `PermissionDenied`, auth → `Unauthenticated`, malformed → `InvalidArgument`) already flows as the RPC status (`worker_grpc.rs:243-250`) and every SDK already classifies it (`session.rs:246-254` registration_denial_error; `reconnect.py:29` NON_RETRYABLE_STATUS_CODES; `reconnect.ts:18-21`). A nack frame would duplicate that channel and the two would drift. **Decision D1: ack is positive-only; the error path is byte-for-byte unchanged.**

Worker between Register-sent and ack (D3): exactly what it does today between Register-sent and header-receipt — nothing. It does not open the serve loop, does not replay unacked results, does not process frames. The ack wait has a deadline (D4): `reconnect.max_backoff`. On deadline: tear the session down, classify as retryable `Registration` failure, consume one establishment attempt in `reconnect_with_backoff` exactly like a connect failure. A non-ack frame arriving first is a protocol violation → typed decode error → retryable budgeted drop (D3).

### 2.2 Result-ack + deadlines (#47)

**Server obligations.** In `process_inbound`'s Result arm (`worker_grpc.rs:138-149`):
1. Decode. On failure: `tracing::error!` with worker_id and the decode reason (replaces the silent `if let Ok` drop — D6), no ack (no key to ack with).
2. On success: `heartbeat.complete_task`, `drain.notify_activity_drained`, `pending.complete_activity` exactly as today, **then** push `ResultAck{workflow_id, activity_id}` onto the session's `task_tx` with `try_send`. `try_send`, not `send().await`: a worker that has stopped draining its receive side must not wedge the server's inbound loop; a dropped ack is recovered by the worker's next-session re-report (D5/F3). Log at warn on a full channel.
3. The ack is sent regardless of whether `pending.complete_activity` found a waiter (D6). The completion handoff result is logged (it currently is, via the sink's own error logs in `bridge.rs:73-108`) — never silently discarded.

**No server persistence (D5).** The tracker's durability story is unchanged and stated honestly: the worker tracker is in-memory; a worker process crash loses it. That does not break at-least-once *for the engine* — an in-flight task whose worker vanishes is failed back as a retryable `lost_worker_error` when the heartbeat window expires or the stream drops (`heartbeat.rs:213-241`), and the engine's (future) retry policy redelivers. The ack exists to bound the **live** worker's replay set, not to make the worker durable.

**Worker obligations — all three SDKs:**
- New session event `ResultAck{workflow_id, activity_id}` → `tracker.acknowledge(...)` (the method all three trackers already have and nothing calls: Rust `reconnect.rs:88-95`, Python `reconnect.py:129`, TS `reconnect.ts:144-153`). An ack with no matching entry is a no-op + debug log (F8 — acks can arrive for entries already acked on a previous session, or after a replace-on-rerecord).
- The serve loop consumes acks **without acquiring a concurrency permit** (they are bookkeeping, not work) — in Rust, ack events take the `deliver_cancellation`-style fast path in the select arm, not the permit-guarded `handle_session_event` path (`loop_.rs:201-237`).
- `re_report_unacked` keeps its replace-everything semantics per dial, but acks now drain the tracker mid-session, so the steady-state backlog is empty and the O(backlog)-per-dial replay decays to O(still-unacked). Entries are still only removed by explicit ack — a successful send proves nothing (the existing contract, `reconnect.rs:332-337`, stays).
- **Send deadlines (D4):** every report send — in the serve loop, the post-drop drain, and `re_report_unacked` — gets a per-send deadline of `reconnect.max_backoff`. Rust: `tokio::time::timeout` around `sender.send().await` inside `GrpcWorkerSession::send_to_server` (`session.rs:218-235`) mapping elapse to `WorkerError::Transport` (retryable; the session is dead by the operator's own definition). TS: `Promise.race` of the `write()` callback promise against a `maxDelayMs` timer in `GrpcWorkerSession.write` (`session.ts:235-245`), rejecting with a transport-shaped error. Python: `put_nowait` already cannot hang (session.py:304); its deadline goes on the register-ack wait and on `stream.read()` only where a bound is needed (the serve loop read is already shutdown-raced, `loop.py` `_receive_next_or_shutdown`).
- **Shutdown interrupts in-flight reports without losing tracked results:** Rust — `re_report_unacked` moves inside the shutdown race in `run_with_connector_until` (today it is unraced at `worker.rs:218`): `tokio::select! { biased; () = shutdown.wait() => …, result = re_report_unacked(...) => … }`. On shutdown-won: results remain tracked (record-before-send already guarantees no entry is removed, `report.rs:151`), session dropped, run returns per D11. The post-drop drain (`drain_remaining`, report.rs:41-83) keeps awaiting *handlers* (cooperative cancellation is the designed shutdown contract — `worker.rs` test `shutdown_waits_for_slow_in_flight_activity`), but its *sends* now carry the D4 deadline, so a dead server cannot wedge the drain. Python/TS: the equivalent replay calls are raced against the shutdown event/abort signal at their call sites (`loop.py` `connect_register_replay_and_serve` replay step; `loop.ts:268-270` replay step already observes the abort via the surrounding checks — add the race on the send future itself).

### 2.3 Attempt field (D7/D8)

**Producer.** `aion::activity::bridge::ActivityDispatcher` (the engine seam) is REPLACED with attempt threaded through (no default method shims left behind):

```rust
// crates/aion/src/activity/bridge.rs
fn dispatch(&self, name: &str, input: &str, config: &str, attempt: u32) -> Result<String, String>;
fn dispatch_from_process(&self, name: &str, input: &str, config: &str, attempt: u32, caller_pid: Option<u64>) -> Result<String, String>;
fn dispatch_async_from_process<'a>(&'a self, …, attempt: u32, caller_pid: Option<u64>) -> BoxFuture<'a, Result<String, String>>;
```

Call sites updated: `runtime/nif_activity_dispatch.rs:193`, `runtime/nif_concurrency.rs:209,317` — each passes `1` with one shared doc comment: *"First delivery: every dispatch issued from workflow code is attempt 1. The AT retry executor (unbuilt; retry POLICY rides in `config` JSON, `run.gleam:182-191`, and is consumed by nothing yet) re-invokes with the incremented attempt when it lands — the wire and seam are ready for it."* This is one documented producer-side constant replacing three consumer-side guesses; when redelivery machinery exists (engine retry executor, or server-side redelivery on lost worker) the value flows end to end with **zero further wire changes**. The recorded-event side already accepts attempt (`recorder.rs:495-506`).

**Server.** `WorkerActivityDispatcher::dispatch_blocking` (`bridge.rs:438-472`) receives `attempt` and stamps it into `activity_task()` (:484-499). The push-path `ScheduledActivity` (`dispatch.rs:16-40`) gains a `pub attempt: u32` field stamped into `to_task()`.

**Consumers (all three SDKs).** Decode reads the wire value; `attempt == 0` is a malformed-task decode error (the same class as a missing workflow_id):
- Rust: delete `WIRE_DEFAULT_ATTEMPT` (`task.rs:8`), `ActivityTask::try_from` takes `value.attempt`, rejecting 0 via a new `MalformedActivityTask::MissingAttempt` variant (`task.rs:70-84`).
- Python: delete `WIRE_DEFAULT_ATTEMPT` (`context.py:12`); `ActivityTask.from_proto` (session.py:128-137) carries `attempt: int`, validates ≥ 1; `loop.py:318` builds the context from `task.attempt`. `ActivityContext.attempt`'s default parameter is removed — the loop always supplies it.
- TS: delete `WIRE_DEFAULT_ATTEMPT` (`session.ts:302`); `decodeTask` (:304-324) reads `task.attempt`, throws on 0/undefined.

**What workers do with it (D8):** expose via `ActivityContext` (already present in all three), include in the task-receipt log line (Rust already logs `task.attempt`, `loop_.rs:328-334`; Python `_log_fields`, TS `loop.ts:427-432` gain it where absent). Heartbeat frames and all tracker keys are untouched.

### 2.4 Drain signal (#46 — D9/D10/D11)

The frame already exists on the wire (`ServerToWorker.drain = 2`); what this wave replaces is its **meaning and the workers' classification of it**, per the approved policy record (`docs/briefs/worker-reconnect-policy.md`, "The proper fix"). The clean-close *heuristic* (clean close = budgeted retryable drop) is NOT removed — it remains the contract for **unannounced** closes, exactly as the record specifies. What changes: a close *announced* by a drain frame stops costing budget.

**Server behavior (mostly already correct):** `broadcast_drain` on the first termination signal (`shutdown.rs:123`), `ensure_accepting` rejects new dispatch during drain (`shutdown.rs:69-83`), stream closes when the server exits. One addition: when the drain timeout expires and the server force-fails leftovers (`shutdown.rs:156-162`), the worker streams are torn down abruptly — workers that already saw the drain frame classify that abrupt end as drain-class (latched, D9), which is the honest reading: the server told them it was leaving.

**Worker classification — the per-SDK divergence this kills (D10):**

| SDK | Today @ HEAD | After this wave |
|---|---|---|
| Rust | Drain frame → serve loop `break` → `ServeEnd::StreamClosed` → **budgeted** clean-close drop (`loop_.rs:207-209`, `worker.rs:236-240`) | Drain frame sets a latched drain flag, serve loop finishes in-flight + returns new `ServeEnd::Drained` → new `DropCause::Drain`: no budget consumed, redial after `initial_backoff` |
| Python | Drain frame → `TransportError("unsupported server-to-worker message 'drain'")` → **error-class budgeted** drop (`session.py:326-338`) | `decode_server_message` yields new `DrainReceived` event; `serve` finishes in-flight + returns new `Drained` sentinel; loop treats it as unbudgeted drop |
| TS | Drain frame silently skipped (`session.ts:169-176`; field not even decoded, `proto/worker.ts:49-51`) → eventual close → **budgeted** clean-close drop | `receiveTasks` yields `{kind:"drained"}`; `runWorkerLoop` classifies the session end as drain: no `droppedAttempts` increment |

**Precise loop semantics (identical in all three):**
1. On drain event: stop reading new task frames, finish/report in-flight work (the existing post-drop drain machinery), close the session.
2. Classify the drop as DRAIN. Do **not** increment the drop budget. Do apply the health-reset rule first if the session qualifies (served ≥ 1 task or outlived max backoff) — a healthy drained session clears prior debt, consistent with #46's "demonstrably healthy" definition.
3. Sleep `initial_backoff` (derived — the schedule's own first pause; no new constant), racing shutdown as backoff sleeps already do.
4. Re-enter establishment via the normal `reconnect_with_backoff` machinery, which has its own budgeted attempts — a server that stays down through the establishment schedule still terminates the worker with the establishment-exhaustion error, exactly as today (the drain signal never grants infinite patience).
5. **Latching:** once the drain event is observed on a session, that session's end is drain-class even if the stream subsequently errors or closes abruptly (F6). The latch resets on the next established session.

**Budget interaction by phase (the #46 record's three cases, spelled out):**
- **Drain during establishment** (after RegisterAck, before any task): drain-class, unbudgeted; the redial's establishment attempts are budgeted as always.
- **Drain mid-serve:** drain-class, unbudgeted; health-reset applies independently per step 2.
- **Drain during replay** (`re_report_unacked` in flight): the replay runs before the stream is read in every SDK (Rust `worker.rs:218-231`, Python `loop.py` replay-then-serve, TS `loop.ts:268-270`), so a drain frame queued during replay is observed as the serve loop's first event → drain-class. If the draining server instead closes before the replay's sends flush, those send failures are an **unannounced** drop → budgeted retryable (the record's stated network ambiguity — the worker never saw the announcement).

**Shutdown-during-backoff alignment (D11):** `DropCause::into_shutdown_result` (Rust `worker.rs:368-373`) becomes the cross-SDK rule: pending `Drain` or `CleanClose` → `Ok`/clean return; pending `Failure(e)` → surface `e` (Rust already does; Python raises the pending drop exception from `connect_register_replay_and_serve` instead of returning when `shutdown` wins `_sleep_or_shutdown` after an error-class drop; TS `runWorkerLoop` throws the pending `dropError` at the `:239-244` shutdown check when it is error-class). Doc blocks describing the divergence (`crates/aion-worker/src/config.rs:66-71`, `session.py` ReconnectConfig docstring, `session.ts:38-42`) are rewritten to describe the aligned rule — no zombie divergence notes.


---

## 3. Server-side implementation (`crates/aion-server` + `crates/aion` seam)

### 3.1 `src/api/worker_grpc.rs`

- **RegisterAck ordering guarantee:** after `accept_registration` succeeds (:71-76) and `worker_id` is extracted (:81-83), push the ack **directly onto `task_tx` before spawning the write forwarder** (:85-96). The forwarder is what copies dispatched tasks onto `task_tx`; nothing can enqueue a task frame ahead of an ack that is written before the forwarder exists. This is a structural ordering proof, not a timing hope. Ack contents: `worker_id` (needs a `pub const fn value(self) -> u64` accessor on `WorkerId`, `registry.rs:31` — currently opaque), `registration.namespace()` (registry.rs:307-310), `heartbeat_window_ms` from `self.state.runtime_config().worker.heartbeat_window` (:50).
- **Result arm** (:138-149): per §2.2 — decode-failure logging, completion handoff, then `task_tx.try_send(Ok(result_ack_frame))` with warn-on-full. `encode_server_to_worker` (:259-271) is extended for the two new variants only if they route through `WorkerMessage` — they do not (see below); the ack frames are constructed inline as `generated::ServerToWorker` values in the stream task. `WorkerMessage` (`registry.rs:17-23`) stays exactly `{ActivityTask, DrainRequest}`: acks are session-local responses, not dispatchable messages, and widening the registry channel type for them would let unrelated server code "dispatch" acks — wrong layer.
- **Registration race note:** the registry insert (inside `accept_registration`) makes the worker dispatch-eligible before the ack is written. A dispatch in that window lands in `worker_rx` and is forwarded only after the forwarder starts — i.e. after the ack. No task can beat the ack onto the wire; no change needed beyond the ordering above.

### 3.2 `src/worker/bridge.rs`

- `PendingActivities` re-keyed `(WorkflowId, ActivityId)` (D12): `pending: Arc<DashMap<(WorkflowId, ActivityId), SyncSender>>`; `insert`/`complete`/`cleanup_activity` (:57-69, :302-313, :350) take both ids. `ActivityCompletionSink::complete_activity` already receives both (:73-108).
- `dispatch_blocking` signature gains `attempt: u32` from the replaced engine trait (§2.3) and stamps it via `activity_task()` (:484-499). The hardcoded fabricated-id scheme (:441-443) is unchanged — it is the dispatch-correlation contract, and D12 makes it collision-safe across restarts.
- The hardcoded 30s dispatch timeout (:158) is out of scope here (it predates this wave and `with_timeout` exists); do not touch it in this wave — flag-only.

### 3.3 `src/worker/dispatch.rs`, `src/worker/heartbeat.rs`, `src/shutdown.rs`

- `ScheduledActivity` gains `attempt` (§2.3). `handle_activity_result` (:227-232) unchanged. Heartbeat tracker unchanged (D8). Shutdown flow unchanged — the drain broadcast and timeout machinery already match the new contract's server half (§2.4).

### 3.4 `crates/aion` (engine seam — same wave, atomic with the server)

- `activity/bridge.rs` trait replacement (§2.3) + call sites `runtime/nif_activity_dispatch.rs:193`, `runtime/nif_concurrency.rs:209,317` + the trait's test dispatchers (`bridge.rs:73+` test mod, `runtime/nif_*` test fakes). One documented `attempt = 1` producer constant at the NIF entry; no other hardcoding remains on the path.

---

## 4. Per-SDK implementation

### 4.1 Rust (`crates/aion-worker`)

| Concern | File / change |
|---|---|
| Session events | `src/protocol/session.rs` — `WorkerSessionEvent` gains `ResultAck { workflow_id, activity_id }` and renames the semantics of `Drain` (variant stays; doc rewritten to the D9 contract). `RegisterAck` never surfaces as an event: it is consumed inside `register()`. `decode_server_message` (:372-386) maps the two new oneof arms; a `register_ack` arriving mid-stream (after registration) is a protocol error → decode error. |
| Register state machine | `open_registered_stream` (:183-216): after `stream_worker` returns, `tokio::time::timeout(config.reconnect.max_backoff, receiver.message())` and demand `RegisterAck`; store `worker_id`/`namespace`/`heartbeat_window` on the session (expose getters); only then set `self.receiver`. Timeout/wrong-frame → retryable `Registration` error. Trait doc (:55-104) and the efc24bbb no-ack comments (:67-71, :172-182) rewritten — no zombie contract text. |
| Send deadline | `send_to_server` (:218-235): `tokio::time::timeout(self.config.reconnect.max_backoff, sender.send(..))` → elapse maps to `WorkerError::Transport` (retryable). Covers serve-loop reports, drain-phase reports, and replay sends uniformly. |
| Serve loop | `src/runtime/loop_.rs` — ack events call `tracker.acknowledge` on the fast path (no permit); drain event sets latched flag + breaks with new `ServeEnd::Drained`; `handle_session_event` (:276-314) drops its now-dead `Drain` arm. `SessionHealth` unchanged. |
| Run loop | `src/worker.rs` — `re_report_unacked` raced against shutdown (:218); `DropCause` gains `Drain` (no budget increment, `initial_backoff` redial delay, `into_shutdown_result → Ok`, `into_recovery_error → None`); budget block (:253-281) skips increment for drain-class while still applying the health reset; doc blocks (:111-183) rewritten. |
| Task decode | `src/protocol/task.rs` — attempt from wire, 0 rejected, constant deleted (§2.3). |
| Config docs | `src/config.rs:41-71` — clean-close/drain/shutdown-outcome paragraphs rewritten to the final contract. |
| Tracker | `src/protocol/reconnect.rs` — `acknowledge` becomes production-load-bearing (no signature change); `re_report_unacked` doc updated (acks now clear entries; sends carry deadlines via the session). |

### 4.2 Python (`sdks/python/aion-worker`) — rebase on the in-flight `loop.py`/`reconnect.py` edits first

| Concern | File / change |
|---|---|
| Session events | `session.py` — new frozen dataclasses `ResultAcknowledged(workflow_id, activity_id)` and `DrainReceived()`; `WorkerSessionEvent` union extended; `decode_server_message` (:326-338) maps `result_ack`/`drain`; the `raise TransportError("unsupported …")` arm remains only for genuinely unknown oneofs. |
| Register state machine | `register()` (:215-220): replace `_wait_for_connection()` with reading the first response frame and demanding `register_ack` under `asyncio.timeout(config.reconnect.max_backoff_seconds)`. Because grpc.aio forbids mixing `read()` with `async for` on one call, `_receive_from_stream` (:307-314) is rewritten as a `read()` loop terminating on `grpc.aio.EOF` — register consumes frame 1 via `read()`, the serve iterator continues with `read()`. Store `worker_id`/`namespace`/`heartbeat_window_ms` on the session. `ConnectableStream`/`_wait_for_connection` (:290-296) deleted — no zombie code. |
| Serve loop | `loop.py` — `serve()` handles `ResultAcknowledged → tracker.acknowledge` and `DrainReceived → finish in-flight, return Drained()` (new sentinel beside `ShutdownRequested`/`StreamFinished` :44-56). `_run_and_report` (:318) takes `task.attempt`; `WIRE_DEFAULT_ATTEMPT` import (:13) and `context.py:12` constant deleted. |
| Run loop | `loop.py::connect_register_replay_and_serve` (:189-258): `Drained` end → no `dropped_attempt` increment (health reset still evaluated), redial after `initial_backoff_seconds` via the existing `_sleep_or_shutdown`; shutdown-during-error-backoff now re-raises the pending drop (D11) while drain/clean-close pending returns cleanly; replay step raced with shutdown. |
| Tracker | `reconnect.py` — `UnackedResultTracker.acknowledge` (:129) becomes production-load-bearing; docstrings (:115-122 "until AW adds an explicit ack frame") rewritten — the frame exists now. |
| Proto | regenerate `aion_worker/proto/worker_pb2*` via `build_proto.py`. |

### 4.3 TypeScript (`sdks/typescript/aion-worker`)

| Concern | File / change |
|---|---|
| Stubs | `src/proto/worker.ts` — `ActivityTask.attempt: number`; new `RegisterAck`/`ResultAck` interfaces; `ServerToWorker` gains `drain?`, `registerAck?`, `resultAck?` (:49-51 today has only `task`). |
| Session events | `session.ts` — `WorkerSessionEvent` becomes `task \| resultAck \| drained \| closed`; `receiveTasks` (:169-176) yields all three frame kinds (a mid-stream `registerAck` is a thrown protocol error). |
| Register state machine | `GrpcWorkerSession` creates ONE stream iterator in the constructor; `register()` (:154-167) writes the frame then awaits `iterator.next()` raced against a `reconnect.maxDelayMs` timer, demanding `registerAck`; `receiveTasks` continues the same iterator. Store ack fields on the session. |
| Send deadline | `write()` (:235-245): race the flush callback against `maxDelayMs`; elapse rejects with a transport-shaped `Error` (retryable by `isRetryableSessionError` since it carries no gRPC denial code). |
| Loop | `loop.ts` — `receiveUntilClosed` (:406-443) handles `resultAck → tracker.acknowledge` (note: `options.tracker` must be the loop-owned tracker — it already is, :125-126) and `drained → return` with a latched drain flag; the post-stream classification (:182-198) adds the drain class: no `droppedAttempts` increment, redial delay `initialDelayMs`; shutdown-during-error-backoff (:239-244) throws the pending error-class `dropError` (D11). `decodeTask` attempt validation (§2.3). |
| Worker | `worker.ts` — no structural change; `LiveSessionRouter` untouched (acks ride the loop's session, not the router). |


---

## 5. Failure-mode table

| # | Failure | Behavior under the new contract | At-least-once honest? |
|---|---|---|---|
| F1 | **RegisterAck lost / never sent** (proxy buffering, server stall, version-skewed peer — unsupported but physically possible) | Worker's ack wait times out at `reconnect.max_backoff` → retryable `Registration` error → one establishment attempt consumed → budgeted redial. Server side: its registration is live until the worker's teardown ends the stream → `deregister` via the registration drop guard (`worker_grpc.rs:111`, `registry.rs:331-344`). | ✓ — tasks dispatched into the doomed window are failed back as lost-worker retryables (`heartbeat.rs:234-241`). |
| F2 | **Task dispatched before ack written** | Structurally impossible: ack is written to `task_tx` before the forwarder that carries tasks exists (§3.1). | ✓ |
| F3 | **ResultAck lost** (stream drops between server ingest and worker receipt) | Tracker entry survives → re-reported on next session → server finds no pending waiter → acks again, drops the duplicate (`PendingActivities::complete` returns false, bridge.rs:63-69). | ✓ — engine saw the result exactly once; worker stops re-reporting after the second ack. |
| F4 | **Worker crashes after computing a result, before/after reporting, before ack** | In-memory tracker is lost — stated plainly: the worker SDK has no durable store by design. Engine-side: if the report landed, the workflow proceeded; if not, the heartbeat window expires → retryable lost-worker failure → (future) retry redelivers with incremented attempt. | ✓ — the activity may execute twice; that is the at-least-once contract activities already sign up for. |
| F5 | **Ack for an unknown tracker entry** (already acked on a prior session; entry replaced) | No-op `acknowledge` + debug log in all three SDKs (F8 rule, §2.2). | ✓ |
| F6 | **Server crashes mid-drain, drain frame delivered** | Drain latched → the abrupt stream end is drain-class → unbudgeted redial after `initial_backoff`; while the server is down, establishment attempts consume their own budget and can exhaust (the drain signal never grants infinite patience, §2.4 step 4). | ✓ |
| F7 | **Server crashes mid-drain, drain frame NOT flushed** | Unannounced close → budgeted retryable drop — the policy record's explicitly retained ambiguity (`worker-reconnect-policy.md`, "unannounced close"). | ✓ |
| F8 | **Drain raced by an in-flight dispatch** (task queued onto the stream before `ensure_accepting` flipped) | Worker that saw drain first stops reading; the unread task's blocked dispatch waiter times out server-side (30s, bridge.rs:158) → retryable error to workflow code. Bounded by the broadcast race window only. | ✓ |
| F9 | **Malformed result frame** (missing ids) | Server logs at error with worker context; no ack possible (no key). The worker's entry re-reports each session and re-fails — loud, attributable, and impossible by construction from these SDKs (they always set both ids). | ✓ — visible, never silent (D6). |
| F10 | **`attempt = 0` on the wire** (producer failed to stamp) | Consumer decode error → `pending_error` → budgeted retryable drop. A server stamping bug surfaces in worker logs within one task. | ✓ |
| F11 | **Worker stops draining its receive side** (acks/tasks back up) | Server ack `try_send` drops with a warn (D5 recovery applies); dispatched tasks back up in the 32-slot channel until dispatch fails with channel-full (`bridge.rs:323-340`) — existing behavior. | ✓ |
| F12 | **Stale re-report after server restart colliding with a fresh dispatch id** | Closed by D12: `(WorkflowId, ActivityId)` keying — the fabricated workflow uuid from the old server life can never equal a fresh `new_v4`. Regression-tested (§6.2). | ✓ |

---

## 6. Test plan

Gates everywhere: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test -p aion-proto -p aion -p aion-server -p aion-worker`; `pytest` + `ruff` + strict `mypy` (Python); `npm test` + lint (TS). No `#[allow]`/`#[ignore]`/`_var` bypasses (CLAUDE.md).

### 6.1 `aion-proto`

1. JSON+prost round-trips for `ProtoRegisterAck`, `ProtoResultAck`, and `ProtoActivityTask` with `attempt` (extend `src/worker.rs:292-316` suite).
2. Tag-stability test: encode the new `ServerToWorker` arms and assert oneof tags 3/4 decode (pins the wire numbers).

### 6.2 Server (`crates/aion-server`)

3. `worker_grpc`: RegisterAck is the first response frame — real tonic loopback (pattern of `tests/worker_dispatch_delivery.rs`): register, read frame 1, assert `register_ack` with the configured `heartbeat_window_ms` and the authorized namespace, **then** dispatch and assert the task arrives second.
4. Denied registration still fails the RPC with `PermissionDenied` and sends no frames (taxonomy unchanged).
5. ResultAck per well-formed result: dispatch → report → worker stream receives `result_ack{wf, act}`.
6. Duplicate result (no pending waiter) still acked; nothing delivered to the sink twice.
7. Malformed result (missing activity_id): error logged (capture via tracing subscriber), no ack frame, stream stays healthy.
8. D12 regression: insert waiter for (wfA, act5); deliver result (wfB, act5) → waiter NOT completed; deliver (wfA, act5) → completed. Verified to fail under the old bare-`ActivityId` keying before fixing.
9. Attempt stamping: engine-seam dispatch with attempt N → wire task carries N (both `WorkerActivityDispatcher` and `ScheduledActivity::to_task`).
10. Drain ordering: `broadcast_drain` → worker stream sees drain frame; post-drain dispatch rejected by `ensure_accepting` (existing coverage extended).

### 6.3 Rust worker

11. `tests/grpc_registration.rs` REWRITTEN: the scripted server now models the ack contract — register completes only after the ack frame; a server that never acks (the old script, kept as a scripted variant) now causes a `Registration` timeout error at `max_backoff`, not a hang; denial-before-ack still maps `PermissionDenied`/`Unauthenticated` correctly.
12. Ack-wait protocol violation: first frame is a task → typed decode/Registration error, retryable.
13. Serve loop: `ResultAck` event clears exactly its tracker entry (two workflows, colliding positions — both keys exercised); unknown ack is a no-op.
14. Steady-state replay decay: session 1 serves+reports two tasks, acks arrive, drop, session 2 → `re_report_unacked` sends nothing.
15. Ack lost: session 1 reports, NO ack, drop, session 2 → exactly one re-report, then ack on session 2 clears it.
16. Send deadline: a session whose report send never completes (scripted never-resolving send) → run errors retryably at `max_backoff` on a paused clock, never hangs.
17. Shutdown during `re_report_unacked`: scripted hung replay send + shutdown → run returns promptly per D11; tracker still contains the entry.
18. Drain: drain frame mid-serve → in-flight finishes and reports → reconnect after `initial_backoff` with **no budget consumed** (paused clock: `max_attempts = 2`, N drain cycles, still running; mirrors `clean_close_loop_exhausts_drop_budget_with_classified_error` inverted).
19. Drain latch: drain frame followed by an abrupt stream error → still unbudgeted.
20. Unannounced clean close still budgeted (existing `clean_close_*` tests stay green — the heuristic is retained for the unannounced case).
21. Drain + shutdown during the redial backoff → `Ok` (D11); error-class pending drop + shutdown → that error (existing test `shutdown_during_mid_run_drop_backoff_returns_promptly` stays).
22. Attempt: wire task with attempt 3 → context exposes 3; attempt 0 → decode error → budgeted drop.

### 6.4 Python worker (`sdks/python/aion-worker/tests/`, beside `test_drop_budget_policy.py`)

23. `register()` consumes the ack via `read()`; success requires the frame, not connection readiness; timeout at `max_backoff_seconds` → retryable registration error.
24. `decode_server_message`: `result_ack` → `ResultAcknowledged`; `drain` → `DrainReceived` (regression: the old `TransportError("unsupported …'drain'")` path is the bug being killed — assert it is gone); unknown oneof still raises.
25. Mirrors of 13–15, 18–22 (ack clears tracker; replay decay; ack-lost single re-report; drain unbudgeted + latch; shutdown-outcome alignment — **new**: error-class pending drop + shutdown now raises; drain/clean pending returns cleanly).
26. Attempt: context built from wire attempt; 0 rejected; `WIRE_DEFAULT_ATTEMPT` no longer importable.

### 6.5 TypeScript worker (`src/*.test.ts`)

27. Session: `receiveTasks` yields `drained` and `resultAck` events (regression for the silent-skip); register awaits ack with deadline; write deadline rejects on a never-flushing stream.
28. Loop mirrors of 13–15, 18–22 including the D11 shutdown-outcome change (error-class pending drop + abort → `runWorkerLoop` rejects).
29. Attempt decode validation; `WIRE_DEFAULT_ATTEMPT` export removed (compile-level).

### 6.6 Cross-stack integration + conformance implications

30. End-to-end (Rust worker ↔ real `aion-server` over loopback, `worker_dispatch_delivery.rs` pattern): register→ack→dispatch(attempt=1)→execute→report→result-ack→tracker empty; then server `broadcast_drain` → worker redials unbudgeted → re-registers → ack → serves again.
31. **Conformance:** there is no formal cross-SDK worker conformance harness; parity is enforced by the mirrored test matrix above — every numbered behavior (ack-first ordering, ack-clears-tracker, replay decay, drain-unbudgeted, drain-latch, D11 outcome, attempt validation) MUST exist in all three SDK suites under recognizably parallel names, as the #46 wave did (`worker.rs` tests / `test_drop_budget_policy.py` / `reconnect.test.ts`+`loop.test.ts`). The `aion-store` conformance suite is unaffected (no store surface touched). The reviewer (Wave V) checks the matrix for holes per SDK.
32. Docs: rewrite the protocol narrative in `worker.proto` comments (they ARE the wire contract); update `docs/design/aion-workers/DESIGN.md`'s "reconnect/resume frames" reference only if Tom wants the design JSON touched — flag, don't self-author cluster JSON (design-pipeline rule); add this brief to `docs/briefs/README.md` table; update `docs/API.md` worker section if it documents the stream (coordinate — file is mid-edit by the #37 wave).

---

## 7. Wave structure & sequencing

Prereq 0: rebase on landed #37/#46-follow-up work; resolve O1–O3 with Tom before Wave P freezes the proto.

- **Wave P — contract (1 agent):** `worker.proto`, `aion-proto/src/worker.rs` mirrors + round-trip tests, tonic regen (build.rs), Python `worker_pb2*` regen, TS `src/proto/worker.ts` hand stubs. *Exit: aion-proto tests green; stubs committed in all three SDK trees.* (~400 LoC)
- **Wave S — server + engine seam (1 agent, after P):** `aion::activity::bridge` trait replacement + call sites; `worker_grpc.rs` ack sends + decode-failure logging; `bridge.rs` pending re-key (D12) + attempt stamping; `ScheduledActivity.attempt`; `WorkerId::value`; server tests 3–10. The three SDKs are broken against HEAD during this wave (the contract break) — Wave S and the SDK waves land as one reviewed train, not piecemeal onto main. *Exit: server + engine tests green.* (~700 LoC)
- **Wave W — three SDK agents in parallel (after S):** W-rs (Rust §4.1, tests 11–22), W-py (Python §4.2, tests 23–26 — REBASE on the in-flight loop.py/reconnect.py edits first), W-ts (TS §4.3, tests 27–29). Each exits with its full gate set green against the Wave S server. (~600–800 LoC each incl. tests)
- **Wave E — cross-stack e2e (1 agent):** test 30 + doc updates (item 32). 
- **Wave V — review:** Fable-level rigorous review per CLAUDE.md (brief + intent + files, free to explore); explicit checks: parity matrix completeness (item 31), no zombie no-ack contract text anywhere (`git grep -i "no registration-ack\|header receipt\|WIRE_DEFAULT_ATTEMPT\|until AW adds an explicit ack"` must come back empty), failure-table rows F1–F12 each traceable to a test, single-break discipline (no compat shims).

Per the no-stash rule: each wave commits immediately on green gates; the P→S→W train coordinates through commits on a feature branch if main must stay releasable during the break window (Tom's call at dispatch time).

---

## Appendix: key file:line index (all @ 682f6356)

| Concern | Location |
|---|---|
| Wire contract | `crates/aion-proto/proto/worker.proto` (oneofs :20-34; task :45-50; DrainRequest :14-17) |
| Proto mirrors + round-trip tests | `crates/aion-proto/src/worker.rs:43-115, 292-316`; `src/generated.rs:14`; `build.rs` |
| Server stream handler | `crates/aion-server/src/api/worker_grpc.rs` (first-frame register :53-66; forwarder :85-96; process_inbound :128-179; result arm :138-149; denial mapping :243-250) |
| Registry / WorkerMessage / drain broadcast | `crates/aion-server/src/worker/registry.rs:17-23, 31, 107-170, 206-221, 331-344` |
| Engine-seam dispatcher (fabricated ids, pending keying, 30s, task encode) | `crates/aion-server/src/worker/bridge.rs:52-69, 155-158, 438-472, 484-499` |
| Push dispatcher / completion contract | `crates/aion-server/src/worker/dispatch.rs:16-40, 213-246` |
| Heartbeat tracker keying / lost-worker | `crates/aion-server/src/worker/heartbeat.rs:59-60, 213-292` |
| Graceful drain coordinator | `crates/aion-server/src/shutdown.rs:69-83, 113-164` |
| Server wiring / config | `crates/aion-server/src/state.rs:91-130`; `src/config/mod.rs:210-216, 236-263, 499` |
| Engine dispatch trait + call sites | `crates/aion/src/activity/bridge.rs:17-70`; `runtime/nif_activity_dispatch.rs:193`; `runtime/nif_concurrency.rs:209, 317` |
| Attempt in domain events | `crates/aion-core/src/event.rs:129-143`; `crates/aion/src/durability/recorder.rs:495-506` |
| Rust session (no-ack contract, decode, sends) | `crates/aion-worker/src/protocol/session.rs:55-104, 183-235, 246-254, 372-386` |
| Rust tracker / replay / backoff | `crates/aion-worker/src/protocol/reconnect.rs:59-125, 184-192, 338-378` |
| Rust run loop (#46 budget, DropCause, shutdown outcomes) | `crates/aion-worker/src/worker.rs:184-298, 349-391` |
| Rust serve loop (drain handling, health) | `crates/aion-worker/src/runtime/loop_.rs:56-78, 146-264, 304-314`; `runtime/report.rs:134-172` |
| Rust attempt hack | `crates/aion-worker/src/protocol/task.rs:8, 60-67` |
| Rust registration deadlock pin | `crates/aion-worker/tests/grpc_registration.rs:37-82`; commit `efc24bbb` |
| Python session (register wait, decode raise, queue) | `sdks/python/aion-worker/aion_worker/session.py:215-220, 269, 290-304, 326-338` (HEAD) |
| Python tracker / replay | `sdks/python/aion-worker/aion_worker/reconnect.py:81, 115-145, 280-289` (HEAD) |
| Python run loop / attempt | `sdks/python/aion-worker/aion_worker/loop.py:44-56, 189-258, 318` (HEAD); `context.py:12` |
| Python proto regen | `sdks/python/aion-worker/build_proto.py` |
| TS stubs (no drain field) | `sdks/typescript/aion-worker/src/proto/worker.ts:49-51` |
| TS session (silent skip, write, attempt) | `sdks/typescript/aion-worker/src/session.ts:154-176, 235-245, 294-324` |
| TS tracker / loop / worker | `sdks/typescript/aion-worker/src/reconnect.ts:129-181, 301-326`; `loop.ts:129-244, 406-443`; `worker.ts:53-88` |
| #46 decision record | `docs/briefs/worker-reconnect-policy.md` |
| Design statement that tasks carry attempt | `docs/design/aion-workers/DESIGN.md` (Protocol Semantics, step 3) |
