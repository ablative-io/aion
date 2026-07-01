# Aion Outbox #13 — the liminal cross-node swap (DESIGN + DECOMPOSITION)

> ✅ LANDED (reconciled 2026-07-02). The liminal cross-node swap SHIPPED via the LSUB
> series (#101-112): real push-based cross-node dispatch is wired into the production boot
> (LSUB-PROD 13-6, `e02fbed8`/`c2aa2a22`), the OutboxDispatcher is ownership-gated
> (LSUB-4), and the cross-node owner-kill fan-out failover capstone + kill-9 worker
> reconnect-to-survivor landed (LSUB-5 `8a0bcb1b`, #112 `66c2e2db`). Symbol:
> `LiminalOutboxDispatch` / `channel_for_row` in `aion-server/src/worker/liminal_transport.rs`.
> FOLLOW-UP STILL OPEN: liminal-transport wire hardening remains a tracked follow-up.
> Design/decomposition record retained below.
>
> Status (original): **design + decomposition, not yet briefs.** Read-only analysis, 2026-06-27.
> Builds on the durable outbox (#9–12, landed) and is item 5 of "remaining work"
> in [AION-OUTBOX-CUTOVER-DECISION.md](./AION-OUTBOX-CUTOVER-DECISION.md):
> *"Replace the OutboxDispatcher's local `registry.dispatch` with a liminal
> cross-node send; `dispatch_key` → liminal per-channel idempotency key. No outbox
> schema change needed."* This doc makes that concrete, and also surfaces the
> blocking truth: **the aion seam is ready; liminal's wire transport is not.**

## TL;DR (read this first)

- **The aion seam is exceptionally clean.** The outbox already abstracts dispatch
  behind a trait (`OutboxRowDispatch`, `crates/aion-server/src/worker/outbox_dispatcher.rs:104`)
  and abstracts completion-return behind a trait (`OutboxDeliveryCallback`,
  `crates/aion-server/src/worker/bridge.rs:95`). #13 is, on aion's side, "write a
  `LiminalOutboxDispatch impl OutboxRowDispatch` + a liminal-side completion source
  that calls the existing `OutboxDeliveryCallback`, behind a feature flag." **No
  transport-trait extraction is needed for the outbox path.** This is much cleaner
  than swapping aion's raw gRPC, which has no server-side seam.
- **Liminal is not ready to carry it today.** The over-the-wire SDK is ~60%
  assembled: remote `subscribe` returns an empty stream
  (`crates/liminal-sdk/src/remote/handles.rs:184`), remote `request_reply` returns
  `Err("remote request/reply awaits protocol response integration")`
  (`remote/handles.rs:200`), remote `receive` returns
  `Err("remote receive awaits protocol inbox integration")` (`remote/handles.rs:267`).
  Conversations are in-memory and respawn empty on crash; there are no end-to-end
  delivery acks on the live path. **The single biggest risk is that #13's real cost
  is finishing liminal's transport, not wiring aion.**
- **Recommendation:** treat #13 as **spike-first, milestone-gated, augment-not-replace,
  cross-node-only**. Keep local/in-process gRPC dispatch exactly as it is. Build a
  flag-gated liminal path that proves one dispatch + result round-trip end-to-end
  (13-0/13-1), then only proceed to wider cutover as liminal's transport milestones
  (remote receive, request-reply, durable conversations, acks) actually land. Do not
  commit to full cutover until those exist.

---

## 1. Current-state map

### 1.1 What gRPC carries today

Aion has exactly one transport: tonic/gRPC (plaintext h2c; TLS is rejected,
`crates/aion-server/src/run.rs:64,330`). Three services, mounted in
`run.rs:137-157` (`serve_grpc`):

| Service (proto) | RPCs | Shape | Plane | Cross-node? |
|---|---|---|---|---|
| `WorkerProtocol` (`worker.proto:13`) | `StreamWorker(stream WorkerToServer)→(stream ServerToWorker)` | one bidi stream **per worker** | data plane (activity dispatch) | **YES — the only real cross-node path** |
| `WorkflowService` (`workflow.proto:8`) | Start/Signal/Query/Cancel/List/Count/Describe + 6 schedule RPCs | unary | control plane (client→server) | client↔server (mgmt) |
| `DeployService` (`deploy.proto:11`) | LoadPackage/ListVersions/RouteVersion/UnloadVersion | unary | operator plane (dark unless `deploy.enabled`) | operator↔server |

Event streaming is **not** gRPC — it is HTTP/WebSocket (`api/ws_subscription.rs`,
`run.rs:159-172`), out of scope for #13.

The `WorkerProtocol` bidi stream carries, per worker:
- **server→worker:** `ActivityTask` (the dispatch; `worker.proto:88`), `DrainRequest`,
  `RegisterAck`, `ResultAck`.
- **worker→server:** `RegisterWorker` (first frame, `worker.proto:82`),
  `ActivityResult` (result/failure report-back, `worker.proto:107`), `Heartbeat`
  (`worker.proto:133`).

### 1.2 The seam types (file:line)

**Worker (client) transport — a clean trait seam already exists:**
- `pub trait WorkerSession` — `crates/aion-worker/src/protocol/session.rs:67`
  (`handshake`, `register`, `receive_tasks() → WorkerTaskStream`, `report_result`,
  `report_failure`, `send_heartbeat`). The whole serve/reconnect stack is generic
  over `S: WorkerSession` (`runtime/loop_.rs:166`, `worker.rs:213`). Only production
  impl: `GrpcWorkerSession` (`session.rs:158`). **A new client transport = a new
  `WorkerSession` impl, nothing else changes.**

**Server transport — NO trait seam; tonic is wired into the handler:**
- The gRPC handler `WorkerGrpcService::stream_worker`
  (`crates/aion-server/src/api/worker_grpc.rs:43`) directly drives the tonic stream
  and encodes/decodes generated frames. There is no `Transport`/`WorkerBridge`
  trait. The decoupling is **channel-shaped, not interface-shaped**: dispatch logic
  talks to `WorkerTaskSender = mpsc::Sender<WorkerMessage>`
  (`crates/aion-server/src/worker/registry.rs:14`) and results return via the trait
  `ActivityCompletionSink` (`crates/aion-server/src/worker/dispatch.rs:237`). The
  transport-neutral core (`ConnectedWorkerRegistry`, `ActivityDispatcher`,
  `WorkerMessage`, `ScheduledActivity`, `ActivityCompletion`) is tonic-free, but the
  binding of those channels to gRPC is hardcoded in the handler.

**The OUTBOX path — TWO clean trait seams (this is the #13 swap surface):**
- **Dispatch-out seam:** `pub trait OutboxRowDispatch`
  (`crates/aion-server/src/worker/outbox_dispatcher.rs:104`):
  `async fn dispatch(&self, row: &OutboxRow) → Result<(), ServerError>`. Production
  impl `WorkerOutboxDispatch` (`outbox_dispatcher.rs:123`) maps an `OutboxRow` →
  `ScheduledActivity` (`to_scheduled`, line 152) and forwards to
  `ActivityDispatcher::dispatch` (line 169). The `OutboxDispatcher` task
  (line 178) is generic over `Arc<dyn OutboxRowDispatch>`. **Replacing this trait's
  impl is the dispatch half of #13.**
- **Completion-return seam:** `pub trait OutboxDeliveryCallback`
  (`crates/aion-server/src/worker/bridge.rs:95`):
  `deliver_completion(workflow_id, activity_id, run_id, result) → Result<bool, _>`
  and `deliver_failure(...)`. Production impl `ServerOutboxDeliveryCallback`
  (`crates/aion-server/src/worker/outbox_delivery.rs:31`) delegates to
  `engine.runtime().deliver_outbox_completion/failure`. Installed once via
  `set_outbox_delivery` (`bridge.rs:170`). **`Ok(true)` = delivered to a live run;
  `Ok(false)` = no live run (stale, correct to drop).** This is where a liminal
  result would re-enter aion. The exact return path becoming a network receive is
  the completion half of #13.

### 1.3 The exact point dispatch becomes a network send

Two hops, both in `aion-server`:
- **Seam A (the swap point):** `ActivityDispatcher::dispatch`
  (`crates/aion-server/src/worker/dispatch.rs:85`), specifically the
  `worker.sender().send(WorkerMessage::ActivityTask(activity.to_task())).await` at
  `dispatch.rs:121`. Picks a connected worker from `ConnectedWorkerRegistry` and
  pushes onto its in-process channel.
- **Seam B (channel → wire):** the per-worker forwarder in
  `crates/aion-server/src/api/worker_grpc.rs:106` drains `worker_rx` and writes the
  encoded frame to the tonic outbound stream (`task_tx.send`, returned as
  `ReceiverStream` at line 158).

**For #13, the swap happens at the `OutboxRowDispatch` trait (above Seam A), not at
the raw gRPC handler.** The outbox dispatcher already calls only the trait, so a
`LiminalOutboxDispatch` slots in with zero change to the dispatcher loop, retry/
backoff, dead-lettering, or reconciliation.

### 1.4 Local vs cross-node boundary

- **Genuinely cross-node (network):** the `WorkerProtocol` stream between
  `aion-worker` and `aion-server` (`GrpcWorkerSession::connect` dials
  `config.endpoint`, `session.rs:177`; server binds a TCP addr, `run.rs:153`). This
  is the data plane and the only horizontally-distributable seam.
- **Local / same-process (NOT a network):** every `mpsc` in the server —
  `worker_tx`/`task_tx` (`worker_grpc.rs:68`), `WorkerTaskSender` (`registry.rs:14`),
  the `std::sync::mpsc` completion channel in `PendingActivities` (`bridge.rs:160`).
  These connect the gRPC handler to the engine inside one process.
- **Outbox today is "cross-node-shaped, single workflow-node":** the workflow actor
  and the `OutboxDispatcher` run in the same server process; only the workers are
  remote (over the gRPC stream). #13 is what makes the *dispatch* genuinely
  cross-node-addressed instead of "whatever worker registered on my stream."

### 1.5 Outbox semantics today (what #13 must preserve)

From the cutover decision + code (verified):
- **At-least-once dispatch + idempotent terminal-write dedup** — NOT exactly-once.
  - Staging idempotency: `INSERT OR IGNORE` on `dispatch_key TEXT UNIQUE`
    (`crates/aion-store-libsql/src/schema.rs:97`), `dispatch_key = "{workflow_id}:{ordinal}"`
    (`crates/aion-store/src/outbox.rs:110`).
  - Dispatch is at-least-once: `mark_done` can fail after a successful dispatch
    (`outbox_dispatcher.rs:266`), reconciler re-arms stale `claimed` rows
    (`outbox_reconciler.rs:76` → `rearm_stale_claimed_outbox_rows`), crash recovery
    re-stages scheduled-no-terminal ordinals (`rearm_outbox_pending`,
    `crates/aion/src/durability/recorder/fan_out.rs:162`).
  - The real dedup chokepoint is `Recorder::record_fan_out_completion`
    (`fan_out.rs:217`): if the ordinal already has a terminal, returns `Dropped`
    without appending — single-writer, inside the workflow's actor turn.
- **RunId-on-the-wire (OBX-011, landed):** `run_id` is staged on the row
  (`fan_out.rs:126`), carried on `ScheduledActivity.run_id` (`dispatch.rs:28`) and
  onto the wire (`to_task`, `dispatch.rs:50`), echoed back by the worker, and gated
  on return by `outbox_delivery_pid` (`crates/aion/src/runtime/handle/delivery.rs:568`)
  and `record_fan_out_completion`'s run check. Closes the continue-as-new misroute
  window. **#13 must keep RunId on the wire through liminal.**
- All dormant unless `outbox.enabled = true` (libsql only), spawned at `run.rs:253`.

---

## 2. Liminal's SDK surface as a transport (honest assessment)

Liminal = a conversation-as-actor messaging bus on beamr
([VISION.md](../../liminal/VISION.md)). Conversations are supervised beamr
processes; backpressure is a wire primitive; durable conversations are *meant* to
resume from haematite-committed state.

### 2.1 The SDK API (real signatures)

`crates/liminal-sdk/src/lib.rs:14`. Two handle traits, two backends
(`Embedded` in-process, `Remote` over TCP), unified by `Sdk*Handle` enums.

- `trait ChannelHandle` (`crates/liminal-sdk/src/channel.rs:41`):
  - `publish<M>(&self, M) → Result<PressureResponse, SdkError>` (line 64)
  - `subscribe<M>(&self) → Subscription<M>` → `Stream<Item=Result<M,_>>` (line 72)
  - `request_reply<Req,Resp>(&self, Req) → ReplyFuture<Resp>` (line 87)
- `trait ConversationHandle` (`crates/liminal-sdk/src/conversation.rs:85`):
  - `send<M>(&self, M) → Result<(), SdkError>` (line 101)
  - `receive<M>(&self) → ReceiveFuture<M>` (line 114)
  - `lifecycle(&self) → LifecycleStream` → `Stream<Item=ConversationEvent>` (line 119)
- **Addressing:** string **channel name** + application **`ConversationId`** (a
  string, explicitly not a beamr pid — `conversation.rs:10`). Remote handles add a
  `ServerAddress` (`remote.rs:27`). Server-side: channel name → beamr pg group →
  subscriber pids (`SRV-005-PLAN.md:58`). So addressing is **topic/channel +
  conversation-id correlation**, not actor-id/node-id at the SDK level.

### 2.2 Delivery & durability guarantees — the critical truth

**The live delivery path is best-effort, in-memory, fire-and-forget, no acks.**
- Subscriber delivery is an in-memory inbox push (`channel/subscription.rs:91`),
  no per-message subscriber ack. Conversation sends over TCP are silent-on-success,
  reply only on error (`remote/tcp/connection.rs:173`) — no positive ack.
- The default embedded backend is a no-op shell: `publish` `black_box`es the message
  (`embedded.rs:201`), `subscribe` returns `empty()` (`embedded.rs:310`).
- **Conversations are NOT durable.** `conversation/types.rs:146`: "durable
  persistence is implemented elsewhere"; the live actor holds in-memory state and
  respawns empty on crash (`VISION.md:11`). An in-flight dispatch is lost on a
  liminal-server crash.

**Durable primitives are real but off the hot path.**
- Real haematite-backed `DurableStore`/`HaematiteStore` with `append(expected_seq)`,
  `cas`, `read_from` (`durability/store.rs:19`); real `DedupCache` with
  `claim_or_get → Claimed|Completed(receipt)|InFlight` (`durability/dedup.rs:104`);
  real cursors + replay (`durability/recovery.rs:87`). Durable channels persist
  before fanout (`channel/types.rs:271`). **But nothing on the live channel/
  conversation delivery path invokes dedup/cursors** — they are consumer/recovery
  utilities a caller must drive. (Note: liminal depends on real published
  `haematite 0.1.0` from crates.io, not the in-memory mock VISION.md claims —
  VISION.md is stale in liminal's favour here.)

**Verdict: no exactly-once on the live path; at-least-once is *constructible* from
the primitives but not *provided*; ordering is per-causal-chain, not global.**

### 2.3 Request-reply / correlation

- **Modeled well, but the wire path is a stub.** `DispatchRequest`/`DispatchResponse`
  carry a `conversation_id` correlation field (`aion/codec.rs`). There is even a
  real dispatch flow shape with retry-on-worker-crash:
  `aion/dispatch.rs:139-213` (open conversation → send `DispatchRequest` →
  `receive` → map to result; `WorkerExited` → exclude + retry, `dispatch.rs:208`).
- **BUT** the default `DispatchContext` is all no-op (`EmptyWorkerPool`,
  `NoopRouter`, `NoopConversationFactory`, `NoopRecorder` — `dispatch.rs:60`), there
  is **no `aion` crate dependency** (`VISION.md:92`), and **over the wire
  request-reply and receive are explicit `Err` stubs** (`remote/handles.rs:200,267`).
  You can `publish`/`send` over TCP but **cannot receive the correlated reply through
  the SDK today.**

### 2.4 Cross-node transport, backpressure, supervision, discovery

- **SDK↔server:** custom binary protocol over plain TCP, length-prefixed frames,
  5s timeouts, `Connect`→`ConnectAck` handshake (`remote/tcp/connection.rs:18`). No
  gRPC/QUIC.
- **server↔server:** beamr's BEAM distribution (channel name = pg group; publish
  fans out to remote pids via the scheduler) — depends directly on beamr 0.8.x
  distribution. Real two-node integration test exists (`cluster_two_node.rs`).
- **Discovery:** seed-list only (`ClusterConfig.seed_nodes: Vec<SocketAddr>`,
  `SRV-005-PLAN.md:42`); no dynamic registry/DNS.
- **Backpressure:** first-class (`PressureResponse::{Accept,Defer,Reject}`) — a real
  strength.
- **Supervision:** real — supervised beamr actors, microsecond crash detection via
  links/EXIT. But restart restores liveness, **not lost message/conversation state.**
- **Reconnect:** real lifecycle state machine (`ConnectionLifecycle`, `ReconnectConfig`,
  `SubscriptionRecovery`) — governs connection state, does not itself replay
  undelivered messages.

### 2.5 How liminal's semantics compose with the outbox

This is the good news: **the outbox is specifically designed to tolerate exactly
liminal's weakness.** The outbox already runs under "at-least-once dispatch +
idempotent terminal dedup." So:
- liminal's lack of exactly-once is **fine** — `dispatch_key` + `record_fan_out_completion`
  dedup absorb redelivery.
- liminal's lack of durable conversations is **fine for the dispatch side** — the
  outbox row is the durable record; if liminal loses an in-flight dispatch, the
  reconciler/recovery re-arms the row and re-dispatches.
- liminal's **best-effort live delivery with no acks is the actual problem.** The
  outbox needs the `OutboxRowDispatch::dispatch` call to return `Ok` **only when the
  send is genuinely accepted by a worker** (so a failed send drives retry, not a
  false `done`). liminal's `publish`→`Accept` is a backpressure ack, not a delivery
  ack; and the **result must come back** to call `OutboxDeliveryCallback`. Today the
  remote receive/reply path that would carry that result is unimplemented.

**The clean composition target:** map `dispatch_key` → liminal's per-channel
idempotency key (the `DedupCache.claim_or_get` mechanism, `dedup.rs:104`), so even
if both aion and liminal retry, the worker executes at-most-once-ish and the
terminal records once. This is exactly the H1 design in
[AION-DISTRIBUTION-DESIGN.md](./AION-DISTRIBUTION-DESIGN.md) §H1.4. **But that
requires liminal to actually wire dedup into the live delivery path, which it does
not today.**

---

## 3. Target design

### 3.1 Shape: augment, cross-node-only, behind a flag

Keep the gRPC worker transport entirely. Add a second, flag-gated dispatch path that
the outbox dispatcher selects when configured for cross-node liminal dispatch. The
swap is **at the `OutboxRowDispatch` trait**, not the raw gRPC handler.

```
                 outbox.enabled = true
                          |
   OutboxDispatcher (unchanged: claim/retry/backoff/dead-letter/reconcile)
                          |
            Arc<dyn OutboxRowDispatch>   <-- the seam
                 /                  \
   WorkerOutboxDispatch        LiminalOutboxDispatch   (NEW, flag: outbox.transport=liminal)
   (today: gRPC registry)      (liminal send + dedup key = dispatch_key)
                                          |
                                  liminal channel/conversation
                                          |  (cross-node)
                                  remote aion-worker subscribed to the activity channel
                                          |  result
                          LiminalCompletionSource  (NEW)  --calls-->  OutboxDeliveryCallback
                                                                       (existing, bridge.rs:95)
```

### 3.2 Each gRPC interaction, mapped

| gRPC interaction (today) | #13 target | Replace or augment? |
|---|---|---|
| Activity dispatch (`ActivityTask` over `StreamWorker`) | `LiminalOutboxDispatch::dispatch` → liminal `publish`/`send` on a channel keyed by `(namespace, task_queue)` (`dispatch_channel_name`, §5 13-3), `activity_type` carried INSIDE the payload, idempotency key = `dispatch_key`, carrying the full `ScheduledActivity` incl. `run_id` | **Augment** (cross-node only; local keeps gRPC) |
| Result/failure report-back (`ActivityResult`) | worker publishes `DispatchResponse` (carries `conversation_id`/correlation + `run_id`); `LiminalCompletionSource` receives and calls `OutboxDeliveryCallback::deliver_completion/failure` | **Augment** |
| Worker registration (`RegisterWorker`) | worker pool subscribes to its `(namespace, task_queue)` channel(s) on the liminal cluster; registration = subscription presence in the beamr pg group | **Augment** |
| Heartbeat (`Heartbeat`) | liminal supervision (process links, microsecond EXIT) replaces app-level heartbeat for liminal workers | **Replace (for liminal workers)** |
| Deploy (`DeployService`) | unchanged — operator plane, not worker fan-out | **Neither (out of scope)** |
| `WorkflowService` (start/signal/query/...) | unchanged — client control plane | **Neither (out of scope)** |
| Event streaming (HTTP/WS) | unchanged | **Neither (out of scope)** |

### 3.3 Exactly-once + idempotency composition

- `dispatch_key = "{workflow_id}:{ordinal}"` (`outbox.rs:110`) is passed as liminal's
  per-message idempotency key into `DedupCache.claim_or_get` on the worker channel.
  A re-dispatched row (aion retry, reconciler re-arm, crash recovery) reuses the same
  key → liminal returns `Completed(receipt)` / dedups → the worker does not re-run a
  completed activity (when liminal wires dedup to delivery).
- The **terminal-write dedup stays in aion** (`record_fan_out_completion`,
  `fan_out.rs:217`) — it is the single-writer source of truth and is unchanged. So
  even if liminal redelivers a *result*, aion drops the second terminal. **The
  correctness backstop never moves to liminal.**
- `run_id` rides through liminal end-to-end (request and response), and the existing
  `outbox_delivery_pid` run gate (`delivery.rs:568`) + `record_fan_out_completion`
  run check enforce continue-as-new safety unchanged.
- Net guarantee is identical to today: **at-least-once delivery, effectively-once
  recording.** #13 does not improve or weaken the guarantee; it changes the wire.

### 3.4 Dependency direction & avoiding "inbreeding"

- Direction is **aion → liminal** (aion gains a dep on `liminal-sdk`). This matches
  the stack order (beamr → haematite → liminal → aion). liminal must **never** depend
  on aion. liminal's existing `aion/` module is deliberately host-agnostic (no aion
  crate dep, all-Noop defaults, `VISION.md:92`) — the aion-specific glue
  (`ScheduledActivity` ⇄ `DispatchRequest`, `ActivityCompletion` ⇄ `DispatchResponse`)
  lives in **aion**, in a new `LiminalOutboxDispatch`/`LiminalCompletionSource`, not in
  liminal. liminal stays a generic bus; aion adapts to it.
- **Version-skew watch (real, active):** liminal pulls beamr 0.8.x transitively via
  haematite + directly; aion/haematite are moving to beamr 0.9.0
  (AION-DISTRIBUTION-DESIGN.md build-step 0). **A shared beamr version is a
  prerequisite** — two beamr versions in one binary is the literal "inbreeding"
  failure. Align beamr across haematite/liminal/aion before aion links liminal.
- Keep the liminal dep behind a Cargo feature (`liminal-transport`) so the default
  aion build does not pull liminal (and its beamr distribution) at all.

### 3.5 Failure/retry/reconnect ownership

- **aion outbox owns durability + retry + dead-lettering + reconciliation.** The
  outbox row is the source of truth. `LiminalOutboxDispatch::dispatch` returns `Err`
  on a non-accepted send → existing backoff/retry/dead-letter applies unchanged.
- **liminal owns connection lifecycle + backpressure + supervision** of the
  worker-side conversation. A worker crash surfaces as `WorkerExited`
  (`dispatch.rs:208`) → liminal can retry within the cluster, but the **authoritative
  retry budget stays in the outbox** (don't double-count attempts; prefer liminal
  fast-retry-once then surface failure to the outbox, or disable liminal-internal
  retry and let the outbox own it — a 13-x decision).
- **The `Ok` contract is load-bearing:** `LiminalOutboxDispatch::dispatch` must
  return `Ok(())` only when delivery is genuinely accepted by a worker, never on a
  best-effort fire-and-forget publish. Until liminal provides a delivery ack (not
  just a backpressure `Accept`), this contract cannot be honestly met — see 13-1.

---

## 4. KEY DECISIONS for Tom (with recommendations)

### (a) Does liminal REPLACE gRPC entirely, or only cross-node (keep local gRPC)?
**Recommendation: AUGMENT — cross-node only; keep local/in-process gRPC.** Reasons:
(1) liminal's transport is unfinished; a full replacement bets the whole worker path
on it. (2) Local same-node dispatch over gRPC is fine and fast; there is no win in
routing it through a liminal cluster. (3) The outbox already isolates the swap to one
trait, so a per-transport selection (`outbox.transport = grpc | liminal`) is cheap.
Revisit "replace entirely" only after liminal is production-proven and ADR-010's
"embedded zero-hop, same code path distributed" actually holds end-to-end.

### (b) Delivery / exactly-once semantics reconciliation
**Recommendation: keep aion as the correctness authority; use liminal only as the
wire + a redundant idempotency layer.** Map `dispatch_key` → liminal idempotency key,
but never move the terminal-write dedup (`record_fan_out_completion`) out of aion.
The guarantee stays "at-least-once + effectively-once recording." Do **not** rely on
liminal's "exactly-once three-party commit" (ADR-006) — it is designed, not built,
and the outbox doesn't need it.

### (c) Does a transport trait need extracting first?
**Recommendation: NO for the outbox path; the trait already exists** (`OutboxRowDispatch`,
`outbox_dispatcher.rs:104`; `OutboxDeliveryCallback`, `bridge.rs:95`). #13 implements
two new types behind existing traits. **Do NOT** attempt to also abstract aion's raw
gRPC worker server (`worker_grpc.rs`) for #13 — that has no seam and is a much larger,
separate refactor that #13 does not need. Scope #13 strictly to the outbox traits.

### (d) Failure/retry/reconnect ownership: aion outbox vs liminal
**Recommendation: aion outbox owns the authoritative retry/dead-letter/recovery;
liminal owns connection lifecycle, backpressure, and worker supervision.** Disable or
cap liminal-internal dispatch retry so attempts aren't double-counted; the outbox row's
`attempt`/`max_attempts`/backoff remain the single budget. This keeps durability and
the dead-letter inspection surface where operators already look.

### (e) Anything that forces a beamr or liminal change?
**YES — liminal changes are the gating prerequisite, and there is a beamr alignment
prereq.** This is the honest core of #13:
- **liminal MUST gain (does not have today):** (1) working remote **receive** and
  **request_reply** over the wire (`remote/handles.rs:200,267` are stubs) — without
  the reply path there is no result return, so #13 is impossible; (2) a real
  **delivery ack** so `dispatch` can return `Ok` truthfully (live path is currently
  ack-less); (3) **dedup wired into live delivery** (the primitive exists,
  `dedup.rs:104`, but is not on the path) for `dispatch_key` idempotency to mean
  anything; (4) ideally **durable conversations** (`conversation/types.rs:146` says
  "implemented elsewhere" — it isn't) so an in-flight dispatch survives a
  liminal-server crash (the outbox re-arm backstops this, so this is "ideally," not
  "must"). These map to liminal's own backlog: SDK remote integration, AION-002/004/005,
  DUR-005/006, SRV-003/005.
- **beamr:** align to a single shared version across haematite/liminal/aion before
  aion links liminal (AION-DISTRIBUTION-DESIGN.md build-step 0). No new beamr feature
  is strictly required for #13; the wasm port is **not** a prerequisite (#13 is
  native-process dispatch, unrelated to the WASM runtime track).

**Decision to make explicit:** does Tom want #13 to (i) **wait** until liminal lands
the receive/reply/ack/dedup milestones via its own dispatch waves, or (ii) **drive**
those liminal milestones as part of the #13 effort (aion's needs become liminal's
priority order)? Given the stack is Tom's to sequence, **recommend (ii) but staged**:
the 13-0/13-1 spike forces exactly the minimal liminal surface (one dispatch + one
correlated reply + one ack), which is the smallest useful slice of liminal's transport
and the right forcing function — but do it as explicit, separately-verified liminal
briefs, not smuggled into aion PRs.

---

## 5. Decomposition into SPIKE-FIRST incremental briefs

Each increment is independently implementable + verifiable, smallest-first, behind the
`liminal-transport` Cargo feature and an `outbox.transport = liminal` runtime flag
(default `grpc`, so a default build/server is byte-identical to today). Increments
13-L* are **liminal-side prerequisites** — they live in the liminal repo and must land
(and be verified there) before the aion increment that depends on them. This makes the
liminal dependency explicit rather than discovering it mid-build.

> Numbering: 13-L* = liminal prerequisite (other repo); 13-N = aion increment.

### 13-L0 — liminal: remote request-reply round-trip (PREREQUISITE)
- **Goal:** make `RemoteChannelHandle::request_reply` / `RemoteConversationHandle::receive`
  actually carry a correlated reply over TCP (replace the `Err("awaits...")` stubs).
- **Seam:** `liminal-sdk/src/remote/handles.rs:200,267`; protocol inbox/response path.
- **Verify:** a liminal-repo integration test sends a request over a socket and
  receives the correlated response (extends `sdk_tcp_e2e.rs`, which today asserts only
  publish/subscribe-ack).
- **Risk:** HIGH — this is the missing 60% of liminal's wire SDK; it is real protocol
  work, not glue. Everything else in #13 is blocked on it.

### 13-L1 — liminal: delivery ack + dedup-on-delivery (PREREQUISITE)
- **Goal:** a publish/send returns a genuine delivery ack (worker accepted), and the
  live delivery path consults `DedupCache.claim_or_get` keyed by an idempotency key.
- **Seam:** `channel/subscription.rs` delivery, `durability/dedup.rs:104`, the ack
  frame in the protocol.
- **Verify:** liminal-repo test: duplicate send with the same idempotency key delivers
  once; a non-accepted send returns a non-ack the caller can see.
- **Risk:** MEDIUM-HIGH — primitives exist but aren't on the path; touches the hot path.

### 13-0 — aion: spike — ONE dispatch over liminal, result back, behind the flag
- **Goal:** with `outbox.transport=liminal`, a single fan-out member dispatches over
  liminal to one remote worker and its result returns through `OutboxDeliveryCallback`;
  the workflow completes. Hard-coded addressing, one worker, happy path only.
- **Seam:** new `LiminalOutboxDispatch impl OutboxRowDispatch` (slots into
  `OutboxDispatcher`) + new `LiminalCompletionSource` calling `OutboxDeliveryCallback`
  (`bridge.rs:95`); `liminal-transport` Cargo feature; `ScheduledActivity` ⇄
  `DispatchRequest`, `DispatchResponse` ⇄ `ActivityCompletion` mappers (incl. `run_id`).
- **Verify:** an aion integration test (model on the existing outbox bootstrap test,
  OBX-012) with one liminal worker: row `pending→claimed→done`, exactly one terminal in
  history, correct collected result. Fails if the liminal path is off/broken.
- **Risk:** HIGH — first real aion↔liminal link; depends on 13-L0. Keep it one worker,
  one activity, no retry, to isolate the wire.
- **Depends on:** 13-L0; beamr version alignment (build-step 0).

### 13-1 — aion: honest `Ok` contract + retry/backoff through liminal
- **Goal:** `LiminalOutboxDispatch::dispatch` returns `Ok` only on a real delivery ack;
  a non-accepted send drives the existing outbox backoff/retry/dead-letter unchanged.
- **Seam:** `LiminalOutboxDispatch` ↔ 13-L1's ack; `OutboxDispatcher` retry path
  (unchanged) (`outbox_dispatcher.rs:259-316`).
- **Verify:** aion test: a send to a down/absent worker returns `Err`, the row retries
  with backoff and dead-letters after `max_attempts` (mirror existing dispatcher tests).
- **Risk:** MEDIUM — depends on 13-L1. Without a true ack, this can't be honest, so
  it gates the real cutover.
- **Depends on:** 13-0, 13-L1.

### 13-2 — aion: dispatch_key → liminal idempotency key
- **Goal:** pass `dispatch_key` as liminal's per-message idempotency key; a re-dispatched
  row (retry/reconciler/recovery) does not cause a second worker execution.
- **Seam:** `LiminalOutboxDispatch::dispatch` → liminal idempotency-key arg → 13-L1 dedup.
- **Verify:** aion test: force a duplicate dispatch (reconciler re-arm path), assert the
  worker executes once and exactly one terminal records.
- **Risk:** MEDIUM — correctness-relevant; relies on 13-L1 dedup being real.
- **Depends on:** 13-1.

### 13-3 — aion: namespace/task-queue addressing over liminal channels (LANDED, corrected)
- **CORRECTION:** the liminal channel key is **`f(namespace, task_queue)`**, NOT
  `(namespace, activity_type)`. `activity_type` is *what to run*, matched by the worker
  AFTER delivery (it rides inside the `DispatchRequest` payload), not a routing/pool
  dimension. See [NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md](./NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md)
  §4.2 for the full rationale. The worker-pool address is `(namespace, task_queue)`.
- **Goal:** route a dispatch to the right worker pool by `(namespace, task_queue)` as the
  liminal channel/group, instead of "whatever worker is on my stream."
- **Depends on NSTQ-2** (the landed `namespace` + `task_queue` columns on `OutboxRow`)
  for the channel-derivation input — that is the small additive schema change this
  consumes, not one this increment introduces.
- **Seam:** the single `dispatch_channel_name(namespace, task_queue)` function in
  `LiminalOutboxDispatch` (`crates/aion-server/src/worker/liminal_transport.rs`),
  forming `"aion.dispatch.{namespace}.{task_queue}"`, derived per-row at dispatch time
  from the row's `(namespace, task_queue)`. A worker pool MUST subscribe to the channel
  this same function produces (the worker-subscription transport is a documented seam,
  still to be built — see the module docs).
- **Verify:** unit test pins the exact channel string; a `(remote, gpu)` row and a
  `(local, norn)` row derive distinct channels matching the format; the row derivation
  routes through the one function so dispatcher and subscriber cannot drift.
- **Risk:** MEDIUM — interacts with the routing model; keep aligned with
  NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md.
- **Depends on:** 13-2, NSTQ-2.

### 13-4 — aion: continue-as-new safety over liminal (RunId end-to-end)
- **Goal:** prove `run_id` survives the liminal round-trip and the existing run gates
  (`outbox_delivery_pid` `delivery.rs:568`, `record_fan_out_completion` run check) drop a
  late completion from a superseded run.
- **Seam:** `run_id` in `DispatchRequest`/`DispatchResponse` mapping; existing gates
  (unchanged).
- **Verify:** aion test: continue-as-new mid-flight; a late liminal completion for the
  old run is dropped, the new run completes correctly (mirror the OBX-011 CAN test over
  the liminal transport).
- **Risk:** LOW-MEDIUM — gates already exist; this just proves they hold over the new wire.
- **Depends on:** 13-3.

### 13-5 — aion: crash recovery across the liminal boundary
- **Goal:** a liminal-server crash mid-dispatch loses no work: the outbox re-arms the
  scheduled-no-terminal rows and re-dispatches over liminal; history is byte-identical
  to a no-crash run.
- **Seam:** existing `rearm_outbox_pending` (`fan_out.rs:162`) + `OutboxReconciler`
  (`outbox_reconciler.rs:76`), unchanged, exercised over liminal.
- **Verify:** aion integration test: kill liminal mid-flight, assert re-arm + completion,
  one terminal per member (mirror OBX-012 R2 over liminal).
- **Risk:** MEDIUM — depends on whether liminal conversations are durable (13-L2 below);
  if not, the outbox re-arm is the only backstop (acceptable, but slower recovery).

### 13-L2 — liminal: durable conversations (OPTIONAL hardening, parallel)
- **Goal:** in-flight conversation state survives a liminal-server crash
  (`conversation/types.rs:146` → real persistence via the existing durable primitives).
- **Seam:** liminal `conversation/actor` ↔ `durability/conversation`, `DUR-005/006`.
- **Verify:** liminal-repo crash test: a conversation resumes mid-exchange after restart.
- **Risk:** MEDIUM — improves recovery latency; **not strictly required** because the
  aion outbox already backstops loss. Sequence after 13-5; do not block cutover on it.

### 13-6 — aion: full bootstrap + real-app cross-node failover demo (CUTOVER GATE)
- **Goal:** `run_server` boots the liminal transport end-to-end (mirror OBX-012's full
  boot test) and a real fan-out app runs cross-node with a worker on a *different
  machine*, surviving a node failure.
- **Seam:** `run.rs` wiring (`maybe_spawn_outbox_dispatcher` selecting the liminal
  transport); integration of all prior increments.
- **Verify:** a live two-node demo (aligns with the "wants real-app sanity checks soon"
  memory): fan-out completes with a remote worker; kill a node, work completes.
- **Risk:** MEDIUM — integration risk; the prior increments de-risk it. This is the flag
  flip from "shaped" to "actually cross-node."
- **Depends on:** 13-0..13-5, 13-L0/L1 (and ideally 13-L2).

**Ordering summary:** 13-L0 → 13-L1 → 13-0 → 13-1 → 13-2 → 13-3 → 13-4 → 13-5 → 13-6,
with 13-L2 parallel-after-13-5. The liminal prereqs (13-L0/L1) are the long pole.

---

## 6. Risks / open questions / prerequisites

### Biggest risk (the single unknown)
**liminal's over-the-wire receive/request-reply is unimplemented** (`remote/handles.rs:200,267`
are `Err("awaits ... integration")` stubs) and the live delivery path has no acks.
Without the reply path, a result cannot return, so #13 is **not buildable today** — its
real cost is finishing ~60% of liminal's wire transport (13-L0/13-L1), not wiring aion.
**Do not schedule the aion increments until 13-L0/13-L1 have landed and been verified in
the liminal repo.**

### Other risks / open questions
- **beamr version skew** (liminal on 0.8.x via haematite; aion/haematite → 0.9.0). Must
  converge to one beamr in the binary before aion links liminal — this is the literal
  "inbreeding" failure mode. (AION-DISTRIBUTION-DESIGN.md build-step 0.)
- **Delivery-ack honesty.** liminal's `publish→Accept` is backpressure, not delivery.
  The outbox's retry correctness depends on `dispatch` returning `Ok` only on real
  acceptance. If liminal can't provide that, the liminal path must stay experimental.
- **Retry double-counting.** If both liminal and the outbox retry, attempt budgets and
  dead-letter semantics get muddy. Decide (13-1/13-d) that the outbox owns the budget;
  cap or disable liminal-internal retry.
- **Addressing model gap.** liminal addresses by channel-name + conversation-id, not
  node/actor. Node affinity (ROUTING-MODEL Tier 3) — needed for "reopen on the device
  holding the files" — has no liminal primitive yet beyond the global-name registry idea
  in AION-DISTRIBUTION-DESIGN §Affinity. Out of scope for #13 but flagged.
- **Discovery is seed-list only** (`SRV-005-PLAN.md:42`). Fine for a fixed cluster;
  insufficient for dynamic worker fleets. Out of scope for #13.
- **The `namespace` + `task_queue` columns (RESOLVED).** NSTQ-2 added durable
  `namespace` + `task_queue` columns to `OutboxRow` (additive libSQL `ADD COLUMN`
  migrations). 13-3 *consumes* these as the channel-derivation input
  (`dispatch_channel_name(namespace, task_queue)`); it introduces no further schema
  change. The earlier note (channel keyed by `(namespace, activity_type)`, schema add
  owned by 13-3) is superseded — see §5 13-3 and NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md §4.2.

### Prerequisites (gating)
1. liminal 13-L0 (remote request-reply over the wire) — **hard blocker**.
2. liminal 13-L1 (delivery ack + dedup-on-delivery) — **hard blocker for honest retry**.
3. beamr single-version alignment across haematite/liminal/aion.
4. `liminal-transport` Cargo feature + `outbox.transport` runtime flag so default builds
   never link liminal.

### Not prerequisites (explicitly)
- The **beamr WASM port is NOT required** — #13 is native-process dispatch.
- The **haematite active-active foundation (H2 quorum/fencing) is NOT required** for #13.
  #13 is the H1 (durable-outbox cross-node dispatch) wire swap; it is "single workflow-
  node, cross-node workers/dispatch." Active-active workflow ownership is a later track
  (AION-DISTRIBUTION-DESIGN §H2) and orthogonal to the transport swap.
