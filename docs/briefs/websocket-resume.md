# Implementation Brief: WebSocket Subscription Resumption (task #37)

Produced by Plan agent against committed HEAD 75a81df4; verified findings. Saved 2026-06-12 session for fan-out after Tom signs off on open decisions T1–T7 (section 5).

## DECISIONS — SIGNED OFF (Tom + external reviewer + orchestrator amendments, 2026-06-12)

- **T1 CONFIRMED**: per-workflow-only resume v1. MANDATORY companion: filtered/firehose SDK disconnect after >=1 delivered event surfaces Unavailable — never silent gapped reattach. Ships in the same wave, not later.
- **T2 AMENDED**: engine builder keeps `event_streaming(capacity: NonZeroUsize)` as explicit opt-in (embedded users don't pay for unused broadcast). aion-server makes `event_broadcast_capacity` REQUIRED config — startup validation FAILS with a precise message if absent. No default, no streaming-dark half-state (the server unconditionally mounts /events/stream; an unconfigured-but-mounted endpoint is the current bug). Config break is loud and once.
- **T3 CONFIRMED**: reserve not_found + error_type="HistoryCompacted" in proto comment + CLIENT-CONTRACT only. No code, no new wire variant.
- **T4 CONFIRMED**: explicit `stream_endpoint` builder option on Rust client; subscribe without it → InvalidArgument with precise message.
- **T5 RESOLVED: GLOBAL_WEBSOCKET_CLEAN** (spike verdict, empirical on Node 22.0/22.4/22.22/24.12): `new WebSocket(url, { headers })` is a DOCUMENTED undici extension (WebSocketInit.headers, typed in @types/node@22, source comment cites whatwg/websockets#42); the test server received x-aion-* upgrade headers in every run; aion-server reads headers only (no subprotocol path exists). Node 20 is EOL (2026-04-30). IMPLEMENT: engines >=22.4.0 (where the experimental warning vanished), zero runtime deps, AND two companion changes in the TS client: drop "DOM" from tsconfig lib (DOM typing shadows Node's WebSocketInit signature → TS2353) and bump @types/node ^20 → ^22. Spike artifacts: /tmp/aion-ws-spike/.
- **T6 CONFIRMED**: Gleam live WS transport stays a documented conformance divergence; stub cursor protocol aligned (cursor 0 = absent). Live conformance run = 3 SDKs + documented divergence.
- **T7 CONFIRMED as follow-up WITH a tracked task**: v1 uses read_history + slice (O(full history) per reconnect, consistent with describe path). read_history_from(workflow_id, from_seq) trait addition + both stores + conformance suite = its own task so it can't evaporate.
- Worker proto additions (#39 register-ack, #47 result-ack, attempt field) are a SEPARATE second proto wave after the #43 register-hang root cause lands — not bolted onto this one.

## 0. Context and verified findings

The client contract (docs/design/aion-clients/CLIENT-CONTRACT.md, "Subscribe resumption", lines 63-67 and 139-147) promises gap-free, duplicate-free resumption from the last delivered EventEnvelope.seq. Verified state of the world at HEAD:

- **Proto**: PerWorkflowSubscription (crates/aion-proto/proto/events.proto, mirrored hand-written prost types in crates/aion-proto/src/events.rs:33-42) has namespace (tag 1) and workflow_id (tag 2). No resume field on any variant. StreamedEvent carries the serde-encoded aion_core::Event whose envelope (crates/aion-core/src/event.rs:16-17) holds the per-workflow monotonic seq.
- **Seq semantics**: seq is per-workflow monotonic and contiguous starting at 1, enforced at append (crates/aion-store/src/memory.rs:103-135 rejects non-contiguous batches; expected_seq head check). Continuous across runs of the same workflow — continue-as-new runs share one history ordered by seq (crates/aion-store/src/run_chain.rs:18-36). There is NO global ordering across workflows anywhere in the store schema. read_history returns full history sorted by seq; no range-read primitive (crates/aion-store/src/store.rs:55-95).
- **Server subscription path**: WS only (/events/stream in crates/aion-server/src/api/http.rs, workflow_router); first client frame is JSON SubscriptionRequest (read_subscription_request / decode_subscription_value, http.rs ~805+); stream/subscribe.rs::subscribe_events runs the namespace guard then Engine::subscribe(filter) — always live, never replays. stream/socket.rs forwards frames through a bounded mpsc with try_send (full ⇒ terminal lagged error frame + close).
- **CRITICAL LATENT GAP — production live streaming is empty.** Engine::subscribe delegates to the EventPublisher seam (crates/aion/src/engine/delegated.rs:93-96, 260-264). The only non-test implementation is DeferredEventPublisher, which returns stream::empty(). ServerState::build_with_store_arc (crates/aion-server/src/state.rs) never calls .event_publisher(...). Nothing in the append path (crates/aion/src/durability/recorder.rs) publishes. The only working live stream today is the TestEventPublisher inside http.rs tests. Resumption's "live half" does not exist in production; building it is in scope.
- **Namespace guard**: every subscription verified pre-engine (crates/aion-server/src/namespace/guard.rs: SubscriptionScope::verify → verify_workflow_ownership, resolver.rs:303-316 — unknown and foreign yield identical NotFound, anti-existence-leak).
- **WireError taxonomy** (crates/aion-proto/src/error.rs): closed set not_found, namespace_denied, sequence_conflict, unknown_query, query_timeout, not_running, lagged, invalid_input, backend, plus optional error_type discriminator string. WS error frames are {"error": <WireError>} + close.
- **SDKs**:
  - Python (sdks/python/aion-client/aion_client/stream.py): real websockets transport; tracks last_seq; _subscription_request rejects any non-None resume_from (HEAD: InvalidArgument; working tree converts to honest Unavailable on reconnect-after-delivery via transport_supports_resume). Ready to map a cursor the moment the wire supports it.
  - TypeScript (sdks/typescript/aion-client/src/stream.ts, handle.ts:138-155): ResumingEventStream computes resumeFrom = lastDeliveredSeq + 1 and passes it to an INJECTED SubscribeTransport; there is NO built-in WS transport (zero runtime deps; handle.subscribe() throws UnavailableError unless caller injects one). Its seq <= lastDeliveredSeq skip removes duplicates only — against today's live-only server a reconnect silently GAPS.
  - Gleam (gleam/aion_client/src/aion_client/stream.gleam): subscribe returns [StreamError(Unavailable)]; only a stub transport exercises the cursor protocol (open(cursor), dedupe, gap ⇒ Unavailable). No live transport (acknowledged divergence in conformance/aion-clients/README.md).
  - Rust (crates/aion-client/src/stream.rs, transport.rs): ResumingEventStream fully built and tested against stubs (tracks last_seq, passes resume_from_sequence = last+1, dedupes, retries only Unavailable), but GrpcWorkflowTransport::subscribe (transport.rs:198-204) is a stub returning Err(ClientError::Unavailable). The embedded transport subscribes live and IGNORES resume_from. Also: its dedupe compares seqs across workflows, incoherent for Filtered/Firehose targets.
- **Conformance** (conformance/aion-clients/scenarios.json): disconnect-resume scenario and sequenceContiguousUnique vocabulary already exist; Python harness currently stubs harness.forceDisconnect (returns success without disconnecting) and — bug — harness.assertStream opens a NEW subscription instead of asserting on the stream from the subscribe step.

## 1. Design decisions (recommendations)

### D1. Wire shape: resume_from_seq on PerWorkflowSubscription only

```proto
// crates/aion-proto/proto/events.proto
message PerWorkflowSubscription {
  string namespace = 1;
  WorkflowId workflow_id = 2;
  // First per-workflow sequence number the caller wants. When present, the
  // server replays recorded history events with seq >= resume_from_seq in
  // order, then splices into the live stream with no gaps and no duplicates.
  // Sequence numbers start at 1; 0 is invalid_input. Absent = live tail only
  // (current behaviour). resume_from_seq = 1 replays the full history.
  optional uint64 resume_from_seq = 3;
}
```

Semantics: "first seq wanted", not "last seq seen" — all four SDKs already compute last + 1. proto3 optional gives presence detection so absent ≠ 0.

Filtered/Firehose get NO cursor field in v1 — resumption is structurally unrepresentable for them (seq is per-workflow; a faithful cursor would be an unbounded per-workflow seq map or a new store-global ordering = schema change to every backend). The contract's own wording is per-workflow. Document: filtered/firehose streams are live-only; on transient disconnect after ≥1 delivered event SDKs must surface Unavailable (honest) instead of silently reattaching a gapped stream.

### D2. Server replay/live splice: publish-after-commit + subscribe-then-snapshot-then-dedupe

**D2a — real EventPublisher (fixes the production empty-stream gap).** New module crates/aion/src/publish/:
- PublishingEventStore: wraps Arc<dyn EventStore>; append delegates and, ONLY on success, sends each appended event into a tokio::sync::broadcast channel. Reads delegate untouched. Recorder is single writer per workflow (invariant 3) and publish strictly follows commit, so broadcast order per workflow equals seq order.
- BroadcastEventPublisher: implements EventPublisher; subscribe(filter) returns a BroadcastStream filtered by EventFilter::matches.
- Seam change (no back-compat shim): EventPublisher::subscribe and Engine::subscribe return BoxStream<'static, Result<Event, EventStreamLagged>> where EventStreamLagged { skipped: u64 } is new in delegated.rs. A lagging broadcast receiver yields Err(EventStreamLagged) — never a silent skip, never a silent end. Update DeferredEventPublisher, delegated.rs tests, embedded client transport, server socket.
- EngineBuilder::event_streaming(capacity: NonZeroUsize): wraps the store in PublishingEventStore BEFORE recorders/recovery are constructed and installs the publisher seam. Explicit opt-in; capacity from the caller — no assumed default.
- crates/aion-server/src/config/mod.rs: WebSocketConfig.event_broadcast_capacity (validated non-zero, same pattern as outbound_buffer_bound); state.rs calls .event_streaming(capacity).

**D2b — the splice (new crates/aion-server/src/stream/resume.rs).** In subscribe_events, after guard scope succeeds, only for per-workflow requests carrying resume_from_seq = R:
1. let live = engine.subscribe(filter) — subscribe to broadcast FIRST (time T0).
2. let history = engine.store().read_history(&workflow_id).await? — snapshot after T0 (T1). head = history.last().map(Event::seq).unwrap_or(0).
3. Validate cursor: R == 0 → invalid_input("resume_from_seq must be >= 1"); R > head + 1 → invalid_input, error_type = "ResumeCursorAheadOfHistory".
4. replay = history[partition_point(seq < R)..]. replay_head = head in both empty/non-empty cases.
5. Combined: stream::iter(replay.map(Ok)).chain(live.filter(|item| match item { Ok(e) => e.seq() > replay_head, Err(_) => true })).

Duplicate-free: replay delivers exactly [R ..= head]; live filter drops seq <= head, so (T0,T1) double-arrivals emit once, from the snapshot.
Gap-free: seq order = commit order (single writer); any seq > head committed after T1 > T0, so the T0 receiver observes it. Broadcast overflow during replay drain → Err(EventStreamLagged) → existing terminal lagged frame → client reconnects with higher cursor; guaranteed progress (each attempt delivers at least the replay slice).

Socket changes (stream/socket.rs): reader consumes Result<Event, EventStreamLagged>; on Err send lagged frame (existing path). Replace try_send with send().await on the per-connection mpsc (broadcast channel now decouples engine; otherwise any replay longer than outbound_buffer_bound spuriously lags). Existing is_terminal_workflow_event break applies unchanged to the combined stream (replay containing terminal/ContinuedAsNew closes at run boundary; callers walk CAN chains by resubscribing with cursor, gap-free). Belt-and-braces per-workflow contiguity guard: after first emitted frame, a live event with seq > expected_next treated as lag (should be unreachable).

Ordering requirement (D6): history read only after guard.scope succeeds — cursor validation must never leak existence across namespaces.

### D3. Invalid cursors / deleted history (all within the closed WireErrorCode set, discriminated by error_type)

| Condition | Wire result | SDK taxonomy | Retry? |
|---|---|---|---|
| Workflow unknown or foreign-owned | not_found (from guard, BEFORE any cursor inspection — identical either way) | NotFound | No |
| resume_from_seq == 0 | invalid_input | InvalidArgument | No |
| resume_from_seq > head + 1 | invalid_input, error_type = "ResumeCursorAheadOfHistory" | InvalidArgument | No |
| Cursor older than earliest retained event (compaction — cannot occur in v1) | reserved in proto comment + CLIENT-CONTRACT: not_found, error_type = "HistoryCompacted" | NotFound; caller restarts without cursor | Only as fresh subscription |

No new WireErrorCode variant for v1; the compile-breaking successor chain stays untouched.

### D4. SDK behaviour matrix

**Rust (crates/aion-client)** — needs a real subscription transport. Recommend WebSocket (tokio-tungstenite, matching the server's only streaming surface; do NOT invent a gRPC streaming RPC — workflow.proto has none, AW owns it):
- Split transport.rs (640 lines, over budget) into transport/{mod,grpc,embedded,ws}.rs.
- transport/ws.rs: connect to explicit stream endpoint, send JSON SubscriptionRequest (prost types already derive serde; new field rides along), decode StreamedEvent via decode_event(), map {"error":...} frames (lagged → Unavailable so resume loop fires; namespace_denied/not_found/invalid_input terminal). Transient socket failure → Err(ClientError::Unavailable) item.
- ClientBuilder::stream_endpoint(url): required for subscribe (gRPC and HTTP/WS listeners are separate addresses; deriving one from the other would be an assumed default). subscribe without it → InvalidArgument, precise message.
- GrpcWorkflowTransport::subscribe (stub at transport.rs:198-204): build request with resume_from_seq and delegate to WS connector.
- EmbeddedWorkflowTransport::subscribe: real resume with the same subscribe-then-snapshot-then-dedupe splice against engine.subscribe + engine.store().read_history; map Err(EventStreamLagged) → Err(ClientError::Unavailable) item.
- stream.rs::ResumingEventStream: restrict cursor/dedupe to SubscribeTarget::Workflow; Filtered/Firehose transient failure after ≥1 delivered event → terminal Unavailable; reconnect-live-only allowed only when nothing delivered yet.

**Python**: _subscription_request emits "resume_from_seq": resume_from inside per_workflow instead of raising; built-in websocket transport becomes resume-capable (transport_supports_resume flips to true for built-in; stays as guard for injected transports that disclaim). Cursor tracking, dedupe, lagged→reconnect already exist.

**TypeScript**: add built-in SubscribeTransport (src/ws.ts) used when none injected: WS URL derived from the client's HTTP endpoint (same axum listener serves /events/stream — scheme swap is protocol mapping, not a default), auth header forwarded, sends {"per_workflow": {namespace, workflow_id: {uuid}, resume_from_seq}}, terminal-error frames thrown as mapped errors, socket drop → UnavailableError. stream.ts already threads resumeFrom and dedupes. Remove the "no transport configured" throw in handle.ts:141-145. Runtime: recommend global WebSocket with engines >= 22 — see T5.

**Gleam**: align stub cursor protocol — open(cursor) where cursor 0 = "no resume field", cursor >= 1 maps to resume_from_seq; keep dedupe and gap⇒Unavailable. Live Erlang-target WS transport is decision T6.

### D5. Conformance

- forceDisconnect: each harness runs a small local TCP relay (listen 127.0.0.1:0, pipe to AION_SERVER_URL host) and points the SDK stream endpoint at it; forceDisconnect closes currently-piped sockets without touching the listener (reconnect succeeds through same relay). Python: asyncio streams; TS: node:net; Rust: tokio TcpListener/copy_bidirectional; Gleam: gen_tcp via FFI or inherits documented divergence until transport exists.
- Same-stream assertion: subscribe step (with collect) starts ONE background collector storing the live stream + delivered list in scenario context; assertStream awaits THAT collector (honoring minimumEventsBeforeDisconnect/minimumEventsAfterReconnect) and computes sequenceContiguousUnique over the single accumulated list. Fix Python harness's re-subscribe in _collect_stream; implement equivalently in TS/Rust.
- No scenarios.json change required beyond optionally dropping the "required server capability" caveat once the server lands.

### D6. Namespace re-verification on resume

A resume is a brand-new WS connection with a full SubscriptionRequest; it traverses caller_from_headers → SubscriptionScope::from_request → NamespaceGuard::scope → verify_workflow_ownership before any engine/store access. The cursor adds no bypass PROVIDED the order is: guard scope first, history read second, cursor validation third. Pin with a test: foreign-namespace resume with any cursor yields the same not_found as a foreign fresh subscribe — no invalid_input leak confirming existence.

## 2. Changes by file

1. crates/aion-proto/proto/events.proto — optional uint64 resume_from_seq = 3 on PerWorkflowSubscription + D1 doc comment (incl. live-only filtered/firehose + reserved HistoryCompacted).
2. crates/aion-proto/src/events.rs — mirror field (#[prost(uint64, optional, tag = "3")]); round-trip test.
3. crates/aion/src/engine/delegated.rs — EventStreamLagged; subscribe seam returns BoxStream<'static, Result<Event, EventStreamLagged>>; update DeferredEventPublisher + tests.
4. crates/aion/src/publish/{mod,store,publisher}.rs (new) — PublishingEventStore (publish-after-commit) + broadcast EventPublisher with explicit lag items.
5. crates/aion/src/engine/builder.rs — event_streaming(capacity) wraps store before recovery/recorder construction.
6. crates/aion-server/src/config/mod.rs — WebSocketConfig.event_broadcast_capacity + non-zero validation.
7. crates/aion-server/src/state.rs — .event_streaming(...) in build_with_store_arc (every recorder append must flow through the publishing wrapper).
8. crates/aion-server/src/stream/subscribe.rs — MappedSubscription.resume_from: Option<u64> (per-workflow only); guard-then-snapshot-then-splice.
9. crates/aion-server/src/stream/resume.rs (new) — cursor validation + splice_resume; unit tests here.
10. crates/aion-server/src/stream/socket.rs — Result items, send().await, lag mapping, contiguity guard.
11. crates/aion-server/src/api/http.rs — decode_per_workflow_subscription parses resume_from_seq (presence only; range checks in resume.rs).
12. Rust client: transport/{mod,grpc,embedded,ws}.rs split + WS transport + real embedded resume; client.rs stream_endpoint; stream.rs per-workflow-only resume.
13. Python: stream.py emits cursor; built-in transport resume-capable. Final state subsumes the in-flight working-tree diff.
14. TS: src/ws.ts (new), client.ts default transport, handle.ts drop throw, package.json engines.
15. Gleam: stream.gleam cursor-0-means-absent alignment.
16. Conformance harnesses (py/ts/rust): TCP relay + real forceDisconnect + same-stream assertStream.
17. conformance/aion-clients/README.md + CLIENT-CONTRACT.md — replace AW-gap caveats (lines 65-67, 144-147) with landed cursor semantics + D3 table.

## 3. Test plan

- Engine unit (publish/): append publishes after commit in seq order; failed append publishes nothing; filter matching; overflow yields Err(EventStreamLagged{skipped}) then resumes; builder wiring test proving Engine::subscribe delivers Recorder-appended events (kills the production empty-stream bug permanently).
- Server unit (resume.rs): cursor 0 → invalid_input; head+2 → ResumeCursorAheadOfHistory; head+1 → empty replay live-only; overlap dedupe (history 1..5 snapshot, live re-emits 4,5,6 → delivered 1..6 contiguous unique); replay containing terminal closes after it; lag mid-splice → lagged frame. subscribe.rs: cursor carried per-workflow only; guard-before-cursor pin test (foreign ns + absurd cursor ⇒ not_found, not invalid_input).
- Server e2e WS (http.rs, tokio-tungstenite pattern): connect, N events, drop socket, reconnect resume_from_seq = lastSeq+1, frames resume contiguous no dups; cursor beyond head → invalid_input frame + close.
- Per-SDK: Python test_stream.py — built-in emits cursor; resume gap/dup-free against cursor-honoring fake; honest-Unavailable stays for disclaiming transports. TS — ws.ts unit + integration against local WS server; stream.ts resume loop with cursor-honoring stub. Rust — WS transport integration runtime-gated on AION_SERVER_URL (no #[ignore]); filtered/firehose honest-terminal tests. Gleam — stub-protocol cursor-0 tests.
- Conformance: disconnect-resume on all four harnesses against one live fixture server; sequenceContiguousUnique on the single accumulated stream spanning the forced disconnect.

## 4. Sequencing

1. Proto field. 2. Engine seam + publish/ + builder. 3. Server config + state wiring. 4. Server resume splice + socket + JSON decode + tests. 5. SDKs in parallel. 6. Conformance + four-SDK run. 7. Docs.

## 5. Open decisions for Tom

- **T1 — Filtered/firehose resume**: confirm per-workflow-only v1 (recommended). Alternative = store-global ordering, schema change across all backends; defer to own design.
- **T2 — event_broadcast_capacity value** (pattern matches outbound_buffer_bound: 32; number needs Tom's call per no-arbitrary-defaults), and whether event_streaming stays explicit builder call (recommended) or unconditional in build().
- **T3 — Compaction signal**: confirm reserving not_found + error_type = "HistoryCompacted" now (documentation only).
- **T4 — Rust stream endpoint**: explicit stream_endpoint builder option (recommended) vs deriving WS from configured HTTP base.
- **T5 — TS WebSocket runtime**: global WebSocket + engines >= 22 (recommended, zero runtime deps) vs ws package + >= 20.
- **T6 — Gleam live WS transport**: in scope for #37 (pick an Erlang-target WS hex dep) or remains documented conformance divergence.
- **T7 — Store range-read**: v1 uses full read_history + slice. Adding read_history_from(workflow_id, from_seq) to ReadableEventStore + both stores + conformance suite is a real optimization for long histories — now or follow-up?

## Critical files
- crates/aion-proto/src/events.rs (+ proto/events.proto)
- crates/aion/src/engine/delegated.rs
- crates/aion-server/src/stream/subscribe.rs
- crates/aion-server/src/stream/socket.rs
- crates/aion-client/src/transport.rs

## 6. Requirements carried from the publisher release review (4f9fcd96, verdict APPROVE)

The Fable review of the engine publisher wave approved the commit and flagged four items the remaining waves MUST absorb:

1. **Namespace-aware filtering at the server splice seam (MEDIUM, splice-wave REQUIREMENT).** The broadcast channel is engine-global: one shared `Engine` serves all namespaces, `EventFilter` has no namespace dimension, and `encode_frame` labels every frame with the *authorized* namespace. Inert today (the server never calls `event_streaming` yet), but the moment the splice wave installs the publisher behind the server, an unfiltered firehose is a cross-tenant leak with mislabeled frames. The splice seam must filter events to the authorized namespace before encoding — for per-workflow (covered by guard + workflow filter), filtered, and firehose alike.
2. **Embedded `ResumingEventStream` silently gaps on lag until the embedded splice lands (MEDIUM, Rust-client-wave scope).** `aion-client`'s embedded transport ignores `resume_from_sequence` and maps engine lag to retryable `Unavailable`, so the resume loop reattaches with a silent gap. Until the Rust client wave implements the embedded resume splice, embedded consumers (Meridian) must use `Engine::subscribe` directly, where lag surfaces as `Err(EventStreamLagged)`. Recorded in the yggdrasil pin notes.
3. **Zero-subscriber clone waste + retention note (LOW, fixed post-review).** `PublishingEventStore` cloned every event before `send` even with no receivers; guarded with `receiver_count() == 0`. Doc note added: once a subscriber has existed, up to `capacity` events stay resident in channel slots.
4. **Cancellation-safety assumption on `PublishingEventStore::append` (LOW doc, fixed post-review).** If an append future were dropped between the inner store's durable commit and the broadcast, events would be committed-but-never-published — a silent gap defeating the splice proof. No production append site is cancellable today (verified: no timeout/select wraps an append; NIF bridges use `block_on`); documented as a contract on `append` so the splice wave's gap-free argument has a stated precondition.

Also noted (INFO, pre-existing): `EventFilter.run` is not applied (Event carries no RunId; documented on `matches`); lag errors are filter-blind, so capacity must be sized for global event volume, not per-subscription volume — feed that into the `event_broadcast_capacity` guidance in the server config docs.
