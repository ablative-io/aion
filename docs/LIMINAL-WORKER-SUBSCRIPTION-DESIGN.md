# Aion #13 — the liminal worker-pool SUBSCRIPTION (DESIGN + DECOMPOSITION)

Read-only analysis. No implementation. This is a build plan + risk map for the
**remaining gap** that makes aion's `(namespace, task_queue, node)` routing work
ACROSS the cluster over liminal, not just locally over gRPC.

## TL;DR (read this first)

- The DISPATCHER side is done: `LiminalOutboxDispatch::dispatch`
  (`crates/aion-server/src/worker/liminal_transport.rs:373`) derives a channel
  per row via `channel_for_row` → `dispatch_channel_name(ns, tq, Option<node>)`
  (`liminal_transport.rs:286`) = `aion.dispatch.{ns}.{tq}[.{node}]` (per-segment
  percent-encoded, injective) and publishes with a per-attempt idempotency key
  via `RemoteChannelHandle::publish_with_idempotency_key` (liminal
  `remote/handles.rs:214`). Wired in `run.rs:372` behind the `liminal-transport`
  feature + `outbox.transport=liminal`.
- The SUBSCRIBER side does not exist. 13-0 uses liminal's **in-server**
  `EchoBehaviour` responder (`liminal/src/conversation/participant.rs:47`); no
  remote aion-worker process joins a channel, receives a `DispatchRequest`,
  executes, and returns a `DispatchResponse`.
- **The hard blocker is a LIMINAL capability gap, not an aion wiring gap.**
  Liminal today gives a remote process: (1) synchronous request/reply over TCP
  via the conversation path (`remote/tcp/mod.rs:132`); (2) a `publish`
  delivery-ack (`PUBLISH_DELIVERED_FLAG`, `process.rs:353`); (3) channel
  **broadcast** fan-out to ALL subscribers with a `delivered_count`
  (`channel/actor/queue.rs:84-94`). It does NOT give a remote process: (a) an
  inbound subscription-delivery stream over the socket — the connection process
  is strictly request→response, no async server→client push
  (`process.rs:300-321`); (b) competing-consumer / one-of-N (pg-group) delivery
  — fan-out is broadcast, so N worker subscribers each execute the dispatch N
  times; (c) a way to register an EXTERNAL connection as a conversation
  responder — `register_responder` (`services.rs:336`) only installs an
  in-process `ParticipantBehaviour` running on liminal's own beamr scheduler
  (`participant.rs:36-39`, `242-256`).
- Therefore the worker-pool subscription requires **new liminal wire
  capability** (LSUB-L1/L2 below) before the aion worker-side transport
  (LSUB-2/3) can be real. The aion-side seam is clean: a `LiminalWorkerSession`
  implementing the existing `WorkerSession` trait
  (`crates/aion-worker/src/protocol/session.rs:66`), reusing `WorkerConfig`
  (`crates/aion-worker/src/config.rs:114`) and the existing serve/report loop.

---

## 1. The subscription model — exact channel-set a worker subscribes to

### 1.1 The single derivation function (already pinned)

Both sides MUST use `dispatch_channel_name(namespace, task_queue, node)`
(`liminal_transport.rs:286`) so dispatcher and subscriber strings cannot drift.
Format (`liminal_transport.rs:292-294`):

- unpinned: `aion.dispatch.{ns}.{tq}`
- node-pinned: `aion.dispatch.{ns}.{tq}.{node}`

Each segment is independently `encode_segment`'d (`liminal_transport.rs:216`):
`.` → `%2E`, `%` → `%25`, escape-first, making the triple→string map injective
across segment counts (proven by `encoding_is_injective_over_reserved_char_triples`,
`liminal_transport.rs:611`). This is aion's own derivation; it is NOT liminal's
`aion::channels::dispatch_channel` (`liminal/src/aion/channels.rs:34`), which
rejects dots and is not used by this path.

### 1.2 The cross-product rule

A worker is configured (`WorkerConfig`, `config.rs:114-139`) with:

- `namespaces: Vec<String>` — the SET of correctness boundaries it serves
  (`config.rs:120`). A worker registered for `{A, B}` is reachable for dispatch
  in BOTH (gRPC analogue: `register_namespaces` indexes one key per namespace,
  `registry.rs:308-342`; test `worker_serving_a_namespace_set_is_reachable_in_each`,
  `registry.rs:922`).
- `task_queue: String` — ONE pool/flavour selector within each namespace
  (`config.rs:127`).
- `node: String` — its locality, default = hostname (`config.rs:130`,
  `default_node()` `config.rs:154`).

The dispatcher publishes a row to its node-pinned channel when `row.node` is
`Some` and to the unpinned channel when `None` (`channel_for_row`,
`liminal_transport.rs:307`; rows: `OutboxRow.node` per NODE-2). The unpinned
channel NEVER carries a node-pinned dispatch and vice versa — they are distinct
strings (`node_pin_separates_channels`, `liminal_transport.rs:538`).

So a worker serving namespace-set `{A, B}`, task_queue `tq`, node `N` must
subscribe to, for EACH namespace `ns` in `{A, B}`, BOTH:

1. `dispatch_channel_name(ns, tq, None)` — to receive unpinned dispatches.
2. `dispatch_channel_name(ns, tq, Some(N))` — to receive dispatches pinned to
   its own node.

Concrete: worker `{A, B} × tq=gpu × node=box-7` subscribes to the 4-channel set:

```
aion.dispatch.A.gpu
aion.dispatch.A.gpu.box-7
aion.dispatch.B.gpu
aion.dispatch.B.gpu.box-7
```

General cardinality: `|namespaces| × 1 × 2` channels (the `×2` is `{unpinned,
own-node}`; node is NOT a cross-product over other nodes — a worker only ever
joins its OWN node's pinned channel, never another node's). This exactly mirrors
the gRPC registry, where `node` is a within-pool filter
(`worker_matches_node`, `registry.rs:570`) and `None`/`Some(N)` resolve against
the same pool key.

### 1.3 Why both (the NODE-5 distinction)

The node-pinned channel is a DISTINCT string, so node isolation is enforced by
the channel name itself (module docs `liminal_transport.rs:73-79`,
`245-261`). A worker that joined only the unpinned channel would never serve
pinned work for its node; a worker that joined only its node channel would miss
unpinned work it is eligible for. Both joins are required for full eligibility.

---

## 2. Liminal capability gap — exists vs needs-building (file:line)

### 2.1 EXISTS today (verified firsthand)

| Capability | Where | What it gives |
|---|---|---|
| Synchronous request/reply over TCP | `liminal/.../remote/tcp/mod.rs:132` `request_reply_conversation` → `connection.conversation_request_reply` | A client can send a conversation request on a subject and block for ONE correlated reply (used by 13-0 against `EchoBehaviour`). |
| Genuine delivery ack on publish | `liminal/.../server/connection/process.rs:349-358` sets `PUBLISH_DELIVERED_FLAG` iff `outcome.delivered`; SDK reads it in `publish_delivery_response` (`remote/tcp/mod.rs:244`) | Dispatcher learns "reached ≥1 subscriber" vs "empty channel". Aion already uses this (`liminal_transport.rs:400`). |
| Dedup-on-delivery by idempotency key | `services.rs:514-556` `claim_delivery` → `dedup.claim_or_get`; namespace `liminal:delivery-dedup` (`services.rs:448`) | A re-publish of the same key is delivered at most once. Aion keys per-attempt (`attempt_idempotency_key`, `liminal_transport.rs:191`). |
| In-process responder seam | `services.rs:336` `register_responder(subject, Arc<dyn ParticipantBehaviour>)`; resolved in `responder_for` (`services.rs:366`) | Replaces echo for a subject with a custom behaviour — **but it runs in the liminal-server process** (`participant.rs:242` `NativeHandler`, on the beamr scheduler). |
| Channel broadcast fan-out + count | `channel/actor/queue.rs:40-94` `Publish` → `PublishOutcome.delivered_count` = number of subscribers it reached | Pub/sub to all matching subscribers; predicate-gated (`subscription.rs:110`). |
| Subscribe frame round-trip | server `apply_frame` Frame::Subscribe → `subscribe_response` (`process.rs:255`); SDK `RemoteChannelHandle::subscribe` → `WireSubscribeRequest` (`remote/handles.rs:165`, `remote/tcp/mod.rs:102`) | A client can REGISTER a subscription and get a `SubscribeAck`. |

### 2.2 The GAP — needs building in liminal

**Gap A — no inbound subscription-delivery over the socket.** The SDK's remote
`subscribe<M>()` returns `SdkSubscription::empty()` (`remote/handles.rs:185`) —
the registration goes out, but there is no inbound stream to receive delivered
messages. The TCP transport is strictly call/response: `subscribe` does one
`round_trip` and returns the ack (`remote/tcp/mod.rs:102-118`); there is no
background reader. Server-side the connection process loops decode→apply→write a
SYNCHRONOUS response (`process.rs:300-321`); there is NO async server→client
push of delivered envelopes to a remote socket. The subscriber inbox
(`subscription.rs:32`) is only drained in-process / in tests
(`subscribe_handle_for_test`, `services.rs:388`). **A remote worker cannot
receive a published dispatch today.**

**Gap B — broadcast, not competing-consumer (one-of-N / pg-group).** Channel
fan-out delivers to ALL matching subscribers (`queue.rs:40`, `delivered_count`
counts every subscriber reached, `queue.rs:84-93`). If a pool has N worker
subscribers on `aion.dispatch.A.gpu`, ONE dispatch would be delivered to all N →
N-fold duplicate execution. Aion's terminal dedup (`record_fan_out_completion`,
idempotent on dispatch_key/ordinal) makes this CORRECT but enormously wasteful
(every worker runs every activity, one result wins). The worker pool needs
ONE-OF-N delivery (a process-group / consumer-group competing-consumer
semantic): each dispatch goes to exactly one ready worker in the pool.

**Gap C — no external-connection responder.** `register_responder`
(`services.rs:336`) takes an `Arc<dyn ParticipantBehaviour>` whose `process`
runs on liminal's scheduler (`participant.rs:38`, `run_slice` `participant.rs:124`).
There is no wire frame by which a remote connection CLAIMS a subject and serves
its requests over the socket. The liminal module doc itself flags this as the
unbuilt seam (`services.rs:329` "the liminal-side seam aion #13 plugs a remote
worker into").

### 2.3 Two liminal shapes that close the gap (pick one — Fork 1)

Both are genuine remote-worker request/reply; they differ in which liminal
primitive grows.

- **Shape P (pull / claim-reply over the conversation path).** Add a wire
  capability for a remote connection to register as the responder for a subject
  (or to long-poll/claim the next pending request for a subject), receive the
  `DispatchRequest`, and send back the correlated `DispatchResponse`. This reuses
  the EXISTING conversation request/reply correlation + delivery-ack +
  dedup-on-delivery the dispatcher already drives — the dispatcher path is
  literally unchanged. It closes Gap A and Gap C together, and Gap B falls out
  naturally because a claim/responder semantics is inherently one-of-N (the
  request is handed to exactly one claimant). This is the smaller liminal change
  and keeps the dispatcher byte-identical. **This is LSUB-L1.**

- **Shape S (push subscription + consumer-group + ack).** Add (1) inbound
  subscription delivery over the socket (a server→client push path) and (2) a
  consumer-group attribute on subscribe so a channel's subscribers form a group
  with one-of-N delivery + per-message ack + redelivery on no-ack. This closes
  Gap A and Gap B, but is a larger protocol change (async push, ack frames,
  in-flight tracking, redelivery timers) and the worker still needs a separate
  reply channel for the result. **This is the heavier LSUB-L2 (deferred).**

**Recommendation: Shape P.** It composes with the work the dispatcher already
does (request/reply + delivery-ack + dedup are live), is the minimum new liminal
surface, and its claim-one-of-N is exactly the worker-pool semantic. Shape S is
the right long-term substrate for high-fan-out pub/sub but is out of scope for
making routing cross-cluster.

---

## 3. Worker-side transport (aion-worker) — the clean seam

The aion-worker already abstracts its transport behind the `WorkerSession`
trait (`protocol/session.rs:66`): `handshake` / `register` / `receive_tasks`
(yields `WorkerSessionEvent::Task(ProtoActivityTask)`, `session.rs:28`) /
`report_result` / `report_failure` / `send_heartbeat`. The serve loop
(`runtime/loop_.rs`, driven via `run_worker_with_session`) and the report path
(`runtime/report.rs:170` `report_outcome` → `session.report_result/failure`) are
transport-agnostic. The gRPC impl is `GrpcWorkerSession` (`session.rs:160`).

**The worker gains a liminal mode by adding a `LiminalWorkerSession` that
implements `WorkerSession`**, parallel to `GrpcWorkerSession`:

- `handshake` / `register`: open the liminal connection (`RemoteChannelHandle`
  /conversation over TCP) and, via the LSUB-L1 capability, register/claim the
  channel-set from §1.2 derived by `dispatch_channel_name` for the config's
  `(namespaces × task_queue × {None, node})`. The same derivation function used
  by the dispatcher — imported, never re-implemented.
- `receive_tasks`: a `WorkerTaskStream` (`session.rs:23`) that yields
  `WorkerSessionEvent::Task` for each received `DispatchRequest`, mapped into a
  `ProtoActivityTask`-shaped task. `DispatchRequest` (`liminal_transport.rs:112`)
  carries `activity_type`, `workflow_id`, `ordinal`, `run_id`, `input`; the
  worker matches `activity_type` against its registered handlers AFTER receipt
  (exactly as the registry pushes type in the task body, not as a routing key —
  `liminal_transport.rs:281`). `ActivityId::from_sequence_position(ordinal)`
  reconstructs the activity id (mirrors `LiminalCompletionSource::deliver`,
  `liminal_transport.rs:448`).
- `report_result` / `report_failure`: serialize a `DispatchResponse`
  (`liminal_transport.rs:142`: `workflow_id`, `ordinal`, `run_id`,
  `outcome: Result<String, String>`) and send it back as the correlated reply
  (Shape P) so `LiminalCompletionSource` (`liminal_transport.rs:418`) re-enters
  it via `OutboxDeliveryCallback::deliver_completion`/`deliver_failure`.

`WorkerConfig` already carries everything needed (`namespaces`, `task_queue`,
`node`); no config change beyond an endpoint/transport selector. `node` defaults
to hostname so a worker self-pins correctly (`config.rs:154`). Heartbeats
(`send_heartbeat`) can be a no-op or a liminal conversation message in the spike
(Fork 4).

Reuse-or-extend choice (Fork 3): the worker `endpoint`/transport-mode selection
mirrors the server's `OutboxTransport` enum (`run.rs:342`). Recommend a small
worker-side `WorkerTransport::{Grpc, Liminal}` selector behind a worker
`liminal-transport` feature, so the default worker build stays byte-identical
gRPC.

---

## 4. Delivery / ack / failover semantics (how it composes)

- **At-least-once + per-attempt dedup (13-1/13-2, LANDED).** The dispatcher
  returns `Ok(())` ONLY on a genuine `DeliveryAck::is_accepted`
  (`liminal_transport.rs:400`); a non-accept (empty channel / no worker) returns
  `ServerError::WorkerDispatch` and the outbox's existing
  retry/backoff/dead-letter drives the row — identical to the gRPC path's
  behaviour on a failed push. Under Shape P, "accepted" = "a worker claimed and
  is processing it", which is the honest one-of-N delivery signal. The
  per-attempt key `{dispatch_key}#{attempt}` (`liminal_transport.rs:191`) keeps a
  legitimate retry from self-suppressing while still deduping a true same-attempt
  resend (`liminal:delivery-dedup`, `services.rs:448`).
- **Empty-channel handling (the no-subscriber case).** Today the
  delivery-ack reports `delivered=false` when the publish reached no subscriber
  (`process.rs:353`, `queue.rs:87-88`); the dispatcher converts that to a
  retryable error (`liminal_transport.rs:403-407`). This is the cross-node
  analogue of the gRPC "no worker registered" wait. Under Shape P the same
  no-claimant condition must surface as a non-accept so the outbox waits/retries
  rather than dead-lettering — the worker subscription must NOT change this
  contract.
- **Completion path (unchanged).** `LiminalCompletionSource::deliver`
  (`liminal_transport.rs:447`) threads `run_id` and calls the SAME
  `ServerOutboxDeliveryCallback` the gRPC path uses, so continue-as-new run gates
  (13-4) and terminal dedup (`record_fan_out_completion`) apply unchanged. The
  worker just has to put the right `run_id`/`ordinal` on the `DispatchResponse`.
- **Failover (13-5).** A worker crash mid-dispatch under Shape P means no
  correlated reply arrives → the outbox row stays scheduled-no-terminal →
  `rearm_outbox_pending` / `OutboxReconciler` re-dispatch it, one-of-N to another
  ready worker. Liminal conversation durability (13-L2) only improves recovery
  latency; the outbox is the backstop. The doc must ensure the worker session's
  "no reply" is observable as a redeliverable/retryable condition, not a silent
  drop.

---

## 5. Spike-first decomposition (LSUB-0..N + liminal prereqs)

Numbering: `LSUB-L*` = liminal-repo prerequisite (must land + be verified in the
liminal repo first); `LSUB-N` = aion increment. Smallest-first; default build
(no `liminal-transport` feature) stays byte-identical at every step.

### LSUB-L1 — liminal: external-connection responder / claim-reply (PREREQUISITE)
- **Goal:** a remote connection can serve requests for a subject over the
  socket: register/claim → receive the request envelope → send the correlated
  reply. One-of-N (exactly one claimant per request). Closes Gap A + C (and B
  via claim semantics).
- **Seam:** extend the connection protocol + `services.rs` responder routing
  (`responder_for` `services.rs:366`) so a remote connection can BE the responder
  for a subject, reusing the existing dedup (`claim_delivery` `services.rs:412`)
  and delivery-ack (`process.rs:349`).
- **Verify (liminal repo):** an external test client claims a subject, a publish
  with an idempotency key is delivered to exactly that client, and its reply
  flows back correlated; a second concurrent claimant does NOT also receive the
  same request (one-of-N).
- **Risk:** HIGH — first async server→client path; the load-bearing prereq.
- **Depends on:** 13-L0/13-L1 (LANDED: request-reply + delivery-ack + dedup).

### LSUB-0 — aion: `LiminalWorkerSession` skeleton + channel-set derivation (spike)
- **Goal:** a `LiminalWorkerSession` implementing `WorkerSession`
  (`session.rs:66`) that, on `handshake`/`register`, computes the §1.2
  channel-set from `WorkerConfig` via `dispatch_channel_name`
  (`liminal_transport.rs:286`) and registers/claims each. No execution yet.
- **Seam:** new module in `crates/aion-worker/src/` behind a worker
  `liminal-transport` feature; imports `dispatch_channel_name` (do not
  re-derive).
- **Verify:** unit test asserts the exact 4-channel set for `{A,B} × gpu × box-7`
  matches the dispatcher's `dispatch_channel_name` output byte-for-byte
  (drift-proof: same function); default build unchanged.
- **Risk:** LOW — pure derivation + trait scaffolding.
- **Depends on:** LSUB-L1 (for the register/claim call).

### LSUB-1 — aion: receive one `DispatchRequest` → `WorkerSessionEvent::Task`
- **Goal:** `receive_tasks` yields a `Task` for a received `DispatchRequest`,
  reconstructing the activity (`ActivityId::from_sequence_position(ordinal)`,
  `run_id`, `payload_from_request` `liminal_transport.rs:472`).
- **Seam:** `LiminalWorkerSession::receive_tasks` → `WorkerTaskStream`
  (`session.rs:23`).
- **Verify:** the serve loop (`runtime/loop_.rs`) dispatches the decoded task to
  the matching registered handler; `activity_type` matched after receipt.
- **Risk:** MEDIUM — first real receive over liminal.
- **Depends on:** LSUB-0.

### LSUB-2 — aion: report result/failure as the correlated reply
- **Goal:** `report_result`/`report_failure` serialize a `DispatchResponse`
  (`liminal_transport.rs:142`) and send it back so `LiminalCompletionSource`
  (`liminal_transport.rs:447`) re-enters it; `run_id`/`ordinal` correct.
- **Seam:** `LiminalWorkerSession::report_*` ↔ Shape-P reply; existing
  `report_outcome` (`report.rs:170`) unchanged.
- **Verify:** end-to-end one-worker round trip: dispatcher publishes → real
  remote aion-worker executes → completion re-enters via the SAME callback the
  gRPC path uses; a fan-out workflow completes one ordinal.
- **Risk:** MEDIUM — the 13-0 echo responder is replaced by a real worker.
- **Depends on:** LSUB-1.

### LSUB-3 — aion: one-of-N across a multi-worker pool + empty-channel honesty
- **Goal:** N workers on the same `(ns, tq)` pool; each dispatch executes on
  exactly one (claim one-of-N from LSUB-L1); a pool with no ready worker yields a
  non-accept so the outbox retries (no dead-letter).
- **Seam:** relies on LSUB-L1 one-of-N; aion's `is_accepted` contract
  (`liminal_transport.rs:400`) unchanged.
- **Verify:** integration test — 3 workers, M dispatches, each activity runs
  once (assert via terminal dedup + per-worker counters); kill all workers →
  dispatch non-accepts and the row retries.
- **Risk:** MEDIUM — proves the wastefulness of broadcast (Gap B) is actually
  closed.
- **Depends on:** LSUB-2, LSUB-L1.

### LSUB-4 — aion: node-pinned + unpinned coexist without double-delivery
- **Goal:** a worker on node N joins both `…{ns}.{tq}` and `…{ns}.{tq}.{N}`; a
  pinned dispatch reaches only N's pinned channel, an unpinned reaches the pool —
  and a single worker joined to BOTH does not receive a given dispatch twice.
- **Seam:** the §1.2 dual-join + LSUB-L1 claim per channel (Fork 2 governs the
  no-double-claim guarantee).
- **Verify:** test — pinned dispatch executes only on the targeted node's worker;
  unpinned executes once somewhere in the pool; no ordinal executes twice on a
  dual-joined worker.
- **Risk:** MEDIUM — the channel-isolation property (`node_pin_separates_channels`,
  `liminal_transport.rs:538`) carried into live subscription.
- **Depends on:** LSUB-3.

### LSUB-5 — aion: worker crash / failover over the liminal boundary
- **Goal:** a worker crash mid-dispatch loses no work: no reply → outbox re-arms
  (`rearm_outbox_pending`, `OutboxReconciler`) → one-of-N re-dispatch to another
  worker; one terminal per ordinal.
- **Seam:** existing re-arm/reconciler, unchanged, exercised over liminal worker
  subscription.
- **Verify:** integration test — kill the executing worker; assert re-dispatch +
  exactly one terminal.
- **Risk:** MEDIUM — depends on "no reply" being a redeliverable condition.
- **Depends on:** LSUB-3 (LSUB-L2 durable conversations OPTIONAL, latency only).

### LSUB-6 — aion: full worker bootstrap + real-app cross-node demo (GATE)
- **Goal:** a real aion-worker binary boots the liminal transport, joins its
  channel-set, and runs a fan-out app cross-node with the server on a different
  machine, surviving a node failure (aligns with 13-6).
- **Verify:** live two-node demo.
- **Depends on:** LSUB-0..5, LSUB-L1 (ideally LSUB-L2).

**Optional, parallel:** **LSUB-L2 — liminal push-subscription + consumer-group
+ ack/redelivery (Shape S).** Only if the broadcast pub/sub substrate is wanted
beyond worker dispatch; not on the cutover path. Heavier (async push, ack
frames, redelivery timers).

**Ordering:** LSUB-L1 → LSUB-0 → LSUB-1 → LSUB-2 → LSUB-3 → LSUB-4 → LSUB-5 →
LSUB-6 (LSUB-L2 anytime after, off the critical path).

---

## 6. Open forks for Tom (with recommended defaults)

### Fork 1 — Delivery shape: claim-reply (P) vs push subscription + consumer-group (S)?
- **Recommend: Shape P (claim-reply, LSUB-L1).** Reuses the live request/reply +
  delivery-ack + dedup the dispatcher already drives; smallest new liminal
  surface; claim semantics IS one-of-N; dispatcher byte-identical. Shape S is the
  right long-term high-fan-out pub/sub substrate but overshoots the
  cross-cluster-routing goal.

### Fork 2 — How node-pinned + unpinned coexist without double-delivery?
- The risk: a worker joined to BOTH `…{ns}.{tq}` and `…{ns}.{tq}.{N}` must not
  receive the SAME dispatch twice. With Shape P this is naturally safe — a given
  dispatch is published to ONE channel (pinned XOR unpinned, decided by
  `row.node`, `channel_for_row` `liminal_transport.rs:307`), so the two channels
  never carry the same row. **Recommend: rely on the dispatcher's single-channel
  publish (no row goes to both channels) — the worker's dual-join is safe by
  construction; the per-attempt idempotency key is the backstop.** No
  cross-channel de-dup needed at the worker.

### Fork 3 — Worker transport selection: new enum/feature vs endpoint sniff?
- **Recommend: a worker-side `WorkerTransport::{Grpc, Liminal}` behind a
  `liminal-transport` worker feature, mirroring the server's `OutboxTransport`
  (`run.rs:342`).** Default build = gRPC, byte-identical. Explicit and symmetric
  with the server selector; no magic endpoint sniffing.

### Fork 4 — Heartbeats over liminal in the spike?
- **Recommend: no-op heartbeats for LSUB-0..3, revisit in LSUB-5.** The gRPC
  heartbeat (`send_heartbeat`, `session.rs:117`) guards long-running activity
  liveness; the spike's round-trip is short. Liminal conversation durability
  (LSUB-L2) is the better long-running-activity story than wiring heartbeats
  early.

### Fork 5 — Re-subscribe on namespace-set change?
- A worker's `namespaces` set is fixed at `WorkerConfig` build
  (`config.rs:120`); the gRPC registry has no live re-scope (a set change means a
  re-register). **Recommend: treat a namespace-set change as a worker restart /
  re-register (re-derive and re-join the §1.2 channel-set) — no live
  re-subscription API in the spike.** Matches the gRPC model and avoids a
  liminal live-rescope capability the routing goal does not need.

### Fork 6 — `delivered=false` vs explicit "no claimant" signal?
- Today empty-channel = `delivered=false` (`process.rs:353`). Under Shape P,
  LSUB-L1 must map "no ready claimant" to the same non-accept so the outbox
  waits/retries (`liminal_transport.rs:403`). **Recommend: reuse the existing
  delivery-ack `false` for no-claimant — do NOT invent a new frame — so the aion
  dispatcher contract stays unchanged.**
