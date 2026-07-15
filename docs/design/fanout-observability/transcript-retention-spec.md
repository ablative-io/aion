# Implementation Spec — Lane #229: Durable transcript retention + replay

Repository: `/Users/tom/Developer/ablative/aion` (base: `main`).
Worktree (create first, work ONLY inside it):
`git -C /Users/tom/Developer/ablative/aion worktree add /Users/tom/Developer/ablative/aion/.worktrees/t229-transcript-retention -b lane/t229-transcript-retention main`
All cargo commands: `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target`. Gate outputs → `<worktree>/t229-gates/` (untracked) with an exit-code manifest.

---

## 0. Reality audit — what ALREADY exists on main (verified by reading; do NOT rebuild)

The lane brief describes transcripts as live-only. That is stale: NOI-5/5b/7 landed a durable spine (commit `bdeb3375`). Verified inventory:

1. **Durable `O`-keyspace store contract** — `crates/aion-store/src/observability.rs`: `ActivityStreamKey` (workflow, activity, attempt), `ActivityRecord`, `ObservabilityStore` trait (`append_activity_event`/`activity_head`/`read_activity_events_from`), `InMemoryObservabilityStore` reference impl. Re-exported at `crates/aion-store/src/lib.rs`.
2. **Haematite durable impl** — `crates/aion-store-haematite/src/store.rs` (append/head/read over the 29-byte `O || uuid(16) || activity_seq_be(8) || attempt_be(4)` key from `crates/aion-store-haematite/src/keyspace.rs`).
3. **Server sequencer + live fan-out** — `crates/aion-server/src/activity_publisher.rs`: `ActivityEventPublisher::publish` (commit-allocated `store_seq`, conflict-retry loop), `replay_from`, `subscribe` (splice/dedup). Ephemeral `Delta`s are WS-forward-only, never persisted.
4. **The one worker→server ingress** (the tee already exists) — liminal `OBSERVABILITY_CHANNEL` tap at `crates/aion-server/src/worker/liminal_transport.rs` decodes `ActivityEvent` and calls `publisher.publish`. Installed with the durable publisher on a haematite boot via `crates/aion-server/src/state.rs` (haematite leaf captured as `ObservabilityStore`; memory/libSQL boots fall back to `InMemoryObservabilityStore` — retention then lives only for the process lifetime).
5. **WS read path (history + live tail in one)** — transcript subscription on `/events/stream`: `crates/aion-server/src/stream/transcript_stream.rs` (durable replay then live splice), namespace gate.
6. **Console cold-load for finished runs** — the durable attempt navigator derives selectable attempts from durable history (`apps/aion-ops-console/src/features/workflow-detail/swimlane/AttemptNavigator.tsx`, `.../lib/attemptNavigator.ts`) and `useTranscript` loads the durable `O` tail over the WS (`apps/aion-ops-console/src/features/transcript/hooks/useTranscript.ts`). The live-only `POST /workflows/attempts` enumeration (`useActivityAttempts.ts`) is retained only to gate intervention controls.

**What is genuinely MISSING (this lane's work):**
- **(A)** No REST read API mirroring the events pair (`getHistory` → `POST /workflows/describe`): transcripts are reachable only via the WS. No socket-free "fetch full transcript history".
- **(B)** No enumeration of retained transcript streams for a workflow (neither store primitive nor endpoint) — `POST /workflows/attempts` enumerates LIVE attempts only (`crates/aion-server/src/api/http/intervene.rs`).
- **(C)** Injected operator messages are NOT guaranteed in the retained record: nothing server-side records an applied `InjectMessage` (`crates/aion-server/src/worker/intervention.rs` routes and returns the ack only), and the CLI harness adapter maps every harness `message` line to `MessageRole::Assistant` (`crates/aion-integration-cli/src/demux.rs`), so even a harness echo is misattributed.
- **(D)** No size/truncation bounds anywhere (publisher, tap, store): a hostile/verbose harness can grow the `O` keyspace without limit.
- **(E)** No execution test of the acceptance case: write chunks → drop ALL subscribers → fetch later over HTTP → full ordered transcript.

"Old runs simply have no retained transcripts" falls out naturally: an unwritten stream reads empty.

---

## 1. Work package A — store enumeration primitive `list_activity_streams`

### 1.1 `crates/aion-store/src/observability.rs`
Add:
```rust
/// One retained transcript stream of a workflow: its key and its head
/// (the number of durably retained records / the next `store_seq`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityStreamSummary {
    /// The stream's `(workflow, activity, attempt)` key.
    pub key: ActivityStreamKey,
    /// Next `store_seq` to be written == count of retained records.
    pub head: u64,
}
```
Trait method on `ObservabilityStore` (after `read_activity_events_from`):
```rust
/// Enumerate every retained transcript stream of `workflow_id`, ordered by
/// `(activity_id, attempt)` ascending. A workflow with no retained
/// transcript reads empty (old runs simply have none).
///
/// # Errors
/// A backend or serialization error.
async fn list_activity_streams(
    &self,
    workflow_id: &WorkflowId,
) -> Result<Vec<ActivityStreamSummary>, StoreError>;
```
`InMemoryObservabilityStore` impl: lock `streams` (map keyed by `(Uuid, u64, u32)`), filter on `key.0 == workflow_id.as_uuid()`, build summaries with `head = stream_head(records)`, sort by `(activity_seq, attempt)`.

Export `ActivityStreamSummary` from `crates/aion-store/src/lib.rs` alongside the existing observability exports.

Tests (in the existing `#[cfg(test)] mod tests`): `list_activity_streams_orders_by_activity_then_attempt` (two activities × two attempts for wf-1 plus one stream for wf-2 → wf-1 lists 3 summaries in order with correct heads), `list_activity_streams_is_empty_for_unknown_workflow`.

### 1.2 `crates/aion-store-haematite/src/keyspace.rs`
Add beside `observability_stream_key`:
```rust
/// The 17-byte `O || workflow_uuid` prefix every observability stream key of
/// one workflow shares (see `observability_stream_key` for the full layout).
pub(crate) fn observability_workflow_prefix(workflow_id: &WorkflowId) -> Vec<u8>
/// Decode a full 29-byte `O`-region stream key into `(activity_seq, attempt)`.
/// Returns `None` for any other region/length (scan safety).
pub(crate) fn decode_observability_stream_key(key: &[u8]) -> Option<(u64, u32)>
```
Decode: require `key.len() == 29` and `key[0] == OBSERVABILITY_TAG`, big-endian decode bytes 17..25 (activity_seq) and 25..29 (attempt) via `try_into()` (no `as` casts — pedantic deny). Unit tests next to the existing key tests: prefix is a strict prefix of the full key; encode→decode round-trips; an `E`-tagged 17-byte key and a truncated key decode `None`.

### 1.3 `crates/aion-store-haematite/src/store.rs`
Add a blocking helper next to `observability_read_blocking`:
```rust
/// Enumerate the retained `O`-streams of one workflow via haematite's
/// intentionally unindexed stream scan (O(total streams) — an operator read,
/// never on the hot publish path).
fn observability_list_blocking(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
) -> Result<Vec<ActivityStreamSummary>, StoreError>
```
Body: `let prefix = keyspace::observability_workflow_prefix(workflow_id);` then `store.scan(|meta| meta.stream_key.starts_with(&prefix)).map_err(|error| api_error(&error))?` (haematite 0.4.1 `EventStore::scan`; `ScanResult { stream_key: Vec<u8>, next_seq: u64 }`). For each result, `decode_observability_stream_key`; skip `None` (foreign region can't match the prefix, but stay defensive); build `ActivityStreamSummary { key: ActivityStreamKey::new(workflow_id.clone(), ActivityId::from_sequence_position(activity_seq), attempt), head: next_seq }`. Sort by `(activity_seq, attempt)`.
Wire into `impl ObservabilityStore for HaematiteStore` with the same `self.blocking(move |store| …)` shape as the other three methods.

Test in `crates/aion-store-haematite/tests/observability.rs` (extend the existing file): `list_activity_streams_enumerates_only_the_workflows_streams_and_survives_reopen` — append events to two streams of wf-A (different activity/attempt) and one stream of wf-B; list wf-A → exactly the two summaries, ordered, heads correct; drop and reopen the store from the same dir; list again → identical (durability across restart, the acceptance's "hour later" at the store layer).

---

## 2. Work package B — publisher: enumeration passthrough + retention bounds

### 2.1 `crates/aion-server/src/activity_publisher.rs`
- Add passthrough (beside `replay_from`):
```rust
/// Enumerate the retained transcript streams of `workflow_id` from the
/// durable `O` keyspace (empty for a workflow with none).
pub async fn list_streams(
    &self,
    workflow_id: &aion_core::WorkflowId,
) -> Result<Vec<aion_store::ActivityStreamSummary>, StoreError>
```
- Add a `bounds: TranscriptBounds` field to `ActivityEventPublisher`. Keep `new(store, capacity)` signature (defaults bounds), add `#[must_use] pub fn with_bounds(mut self, bounds: TranscriptBounds) -> Self`.
- In `publish`, for the NON-ephemeral arm only:
  1. First bound the event: `let event = crate::activity_bounds::bound_event(event, self.bounds.max_event_bytes)?;` and use it for key/append/fan-out.
  2. Add the stream cap INSIDE the conflict-retry loop (re-evaluated each iteration, since `expected_seq` moves on conflict):
     - `expected_seq > self.bounds.max_stream_events` → fan the (bounded) event out live with `store_seq: None` (same shape as the ephemeral fan-out, but `ephemeral` stays `false`) and return `Ok(None)` — live streaming continues past the cap; persistence stops.
     - `expected_seq == self.bounds.max_stream_events` → append a MARKER instead: same identity fields as the incoming event, `kind: ActivityEventKind::Progress { detail: ProgressDetail::Note { text: format!("transcript retention cap reached ({cap} events); further events are live-only and not persisted") } }`. On success fan out the marker (with its `store_seq`) AND the original event live-only (`store_seq: None`), return `Ok(None)`. On `SequenceConflict` adopt `found` and re-loop (the cap re-check then routes to the drop arm).

`activity_publisher.rs` breaches the ≤500-code-line law with the additions. Move the existing `#[cfg(test)] mod tests` into `crates/aion-server/src/activity_publisher_tests.rs` using the established pattern `#[cfg(test)] #[path = "activity_publisher_tests.rs"] mod tests;` (exactly as `crates/aion-server/src/worker/intervention.rs` does).

New publisher tests (in the relocated tests file, reusing its `publisher(cap)`/`event(...)` helpers):
- `stream_cap_appends_one_marker_then_stops_persisting`: bounds `max_stream_events = 3`; publish 6 messages; `replay_from(0)` → exactly 4 records, seqs `[0,1,2,3]`, record 3 is the `Progress`/`Note` marker containing `"retention cap"`; publishes 4..6 returned `Ok(None)`.
- `capped_stream_still_fans_out_live_without_store_seq`: subscriber attached first; past-cap events arrive live with `store_seq: None` and `ephemeral == false`.
- `oversized_event_is_truncated_before_persist`: bounds `max_event_bytes = 512`; publish a `Message` with a 10_000-byte text; replay → one record, text ends with the truncation marker, serialized record ≤ 512 + marker slack (assert `serde_json::to_vec(&record.event)?.len() <= 1024` and original text absent in full).

### 2.2 New file `crates/aion-server/src/activity_bounds.rs` (+ `pub(crate) mod activity_bounds;` in `crates/aion-server/src/lib.rs` beside `pub mod activity_publisher;`)
```rust
/// Operator-tunable transcript retention bounds (see `[observability]` config).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptBounds {
    /// Ceiling on one persisted event's serialized size (bytes).
    pub max_event_bytes: usize,
    /// Ceiling on retained events per `(workflow, activity, attempt)` stream.
    pub max_stream_events: u64,
}
impl Default for TranscriptBounds { /* the two DEFAULT_* consts from §3 */ }

/// Bound one non-ephemeral event to `max_event_bytes`, deterministically.
pub(crate) fn bound_event(event: &ActivityEvent, max_event_bytes: usize)
    -> Result<ActivityEvent, StoreError>
```
`bound_event` algorithm (deterministic, two-stage):
1. `let size = serde_json::to_vec(event).map_err(|e| StoreError::Serialization(e.to_string()))?.len();` if `size <= max_event_bytes` → return clone unchanged.
2. Per-kind reduction:
   - `Message { text }`, `Progress { Note { text } }`, `Stop { Error { message } }`, `Stop { Other { reason } }`: budget = `max_event_bytes.saturating_sub(size - field_len)` then truncate the string on a `char` boundary to `budget.saturating_sub(marker.len())` and append marker `format!(" …[truncated {omitted} bytes by observability.max_event_bytes]")` (walk `char_indices()` for the boundary; `floor_char_boundary` is nightly — do not use).
   - `ToolCall { input }`, `ToolResult { output }`, `Raw { value }`: replace the JSON value with `serde_json::json!({"truncated": true, "original_bytes": original_json_len, "reason": "observability.max_event_bytes"})`, keeping `tool`/`call_id`/`source`/`is_error` intact.
   - `Delta`: unreachable here (ephemeral events are never passed) — return the clone unchanged (do NOT panic; law: no panics).
3. Re-measure; if STILL over (pathological non-text overhead), replace the kind wholesale with `Progress { Note { text: format!("event truncated: {kind_tag} of {size} bytes exceeded observability.max_event_bytes={max_event_bytes}") } }` where `kind_tag` is a static str per variant.

Unit tests in-module: `undersized_event_passes_through_unchanged`, `message_text_truncates_on_a_char_boundary_with_marker` (use multi-byte chars straddling the cut), `tool_result_output_is_replaced_with_truncation_stub`, `raw_value_is_replaced_with_truncation_stub`, `pathological_event_falls_back_to_note`, and a proptest-free exhaustive loop over every kind asserting `bound_event` output re-serializes ≤ `max_event_bytes` for a generous max (execution proof of the invariant).

---

## 3. Work package C — `[observability]` config (operator-tunable bounds)

- `crates/aion-server/src/config/sections.rs`: new section modeled on `DeployConfig`:
```rust
/// Agent-observability transcript retention bounds from `[observability]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObservabilityConfig {
    /// Ceiling on one persisted transcript event's serialized size, bytes.
    pub max_event_bytes: usize,
    /// Ceiling on retained events per `(workflow, activity, attempt)` stream.
    pub max_stream_events: u64,
}
```
`impl Default` from the new consts; `validate()` rejecting zero for either with operator-facing `OBSERVABILITY_MAX_EVENT_BYTES_REQUIRED` / `OBSERVABILITY_MAX_STREAM_EVENTS_REQUIRED` messages (mirror `WebSocketConfig::validate`).
- `crates/aion-server/src/config/defaults.rs`: `pub const DEFAULT_OBSERVABILITY_MAX_EVENT_BYTES: usize = 256 * 1024;` and `pub const DEFAULT_OBSERVABILITY_MAX_STREAM_EVENTS: u64 = 20_000;` (+ the two `*_REQUIRED` messages). Values are proposals — flagged for Tom (see open questions).
- `crates/aion-server/src/config/env.rs`: `AION_OBSERVABILITY_MAX_EVENT_BYTES` / `AION_OBSERVABILITY_MAX_STREAM_EVENTS` in a new `overlay_observability` chained from `overlay_websocket`'s fallthrough (chain before `overlay_outbox`), using `parse_positive_usize`/the u64 twin.
- `crates/aion-server/src/config/load.rs`: `observability: ObservabilityConfig` field on `ServerConfig` (serde-default), carried through `into_parts`, `self.observability.validate()?` beside the websocket validation. Add load tests mirroring the websocket ones: omitted → defaults; explicit zero → the operator message.
- `crates/aion-server/src/config/runtime.rs`: `pub observability: ObservabilityConfig` on `RuntimeConfig`. Re-export `ObservabilityConfig` from `config/mod.rs` with the other sections.
- `crates/aion-server/src/state.rs`: in `build_real_time_publishers` pass bounds: `build_transcript_publisher(observability_store, capacity).with_bounds(TranscriptBounds { max_event_bytes: runtime.observability.max_event_bytes, max_stream_events: runtime.observability.max_stream_events })` — restructure so the runtime is in reach; the four `from_parts*` fallback sites keep defaults (embedder states).
- **Mechanical fan-out**: every literal `aion_server::config::RuntimeConfig { … }` gains `observability: ObservabilityConfig::default(),` (or a tuned value in the new e2e). Verified literal sites: `crates/aion-server/src/run.rs`, `src/config/load.rs`, `src/state.rs`, `src/api/deploy_grpc.rs`, `src/api/http/test_support.rs`, `src/api/grpc/mod.rs`, and tests `deploy_api_e2e.rs`, `worker_sdk_e2e.rs`, `deploy_restart_e2e.rs`, `dev_ui_e2e.rs`, `transcript_stream_e2e.rs`, `websocket_resume.rs`, `routing_nstq_node_compose_e2e.rs`, `authoring_e2e.rs`, `routing_forward_e2e.rs`, `graceful_shutdown_park_e2e.rs`, `heartbeat_sweeper_e2e.rs`, `deploy_audit_log.rs`, `outbox_transport_e2e.rs`, `awl_deploy_direct_e2e.rs`, `bridge_liminal_dispatch_e2e.rs`, `worker_dispatch_delivery.rs`, `noi5b_noi6_live_agent_e2e.rs`, plus `crates/aion-cli/tests/deploy_commands.rs`. CAUTION: `crates/aion/src/runtime/config.rs` and `crates/aion/src/engine/builder.rs` define/construct a DIFFERENT `RuntimeConfig` (the engine's) — do not touch.

---

## 4. Work package D — REST read API mirroring the events pair

### 4.1 Gate refactor — `crates/aion-server/src/stream/transcript_stream.rs`
Extract the namespace gate body of `authorize_transcript` into:
```rust
/// Per-workflow transcript gate shared by the WS subscription and the REST
/// fetch/enumeration endpoints — byte-identical to the workflow event
/// subscription's authorization (anti-leak `not_found`).
pub(crate) async fn gate_transcript_workflow(
    state: &ServerState,
    caller: &CallerIdentity,
    namespace: &str,
    workflow_id: &aion_core::WorkflowId,
) -> Result<(), ServerError>
```
(builds the `PerWorkflowSubscription` + `WorkflowTarget::workflow` + `SubscriptionScope::PerWorkflow` + `EventFilter` + `NamespaceOperation::subscribe` and calls `state.namespace_guard().scope(...)` exactly as today). `authorize_transcript` becomes a thin caller of it. Re-export via `crates/aion-server/src/stream/mod.rs` (mod.rs is re-exports only — add `pub(crate) use transcript_stream::gate_transcript_workflow;`).

### 4.2 New file `crates/aion-server/src/api/http/transcripts.rs` (modeled on `intervene.rs`)
DTOs (serde; `ActivityId` serializes as a plain number):
```rust
pub(crate) struct TranscriptFetchRequest { namespace: String, workflow_id: WorkflowId, activity_id: ActivityId, attempt: u32, from_seq: Option<u64> }
pub(crate) struct TranscriptFetchResponse { events: Vec<ActivityEvent> }
pub(crate) struct TranscriptStreamsRequest { namespace: String, workflow_id: WorkflowId }
pub(crate) struct TranscriptStreamEntry { activity_id: ActivityId, attempt: u32, head: u64 }
pub(crate) struct TranscriptStreamsResponse { streams: Vec<TranscriptStreamEntry> }
```
Handlers (both `HttpCaller` + `HttpWireError`, exactly the `intervene` shape):
- `POST /workflows/transcript` → `fetch_transcript`: gate via `gate_transcript_workflow`; `let key = ActivityStreamKey::new(request.workflow_id, request.activity_id, request.attempt);` `state.transcript_publisher().replay_from(&key, request.from_seq.unwrap_or(0)).await` → `events: records.into_iter().map(|r| r.event).collect()`. An unknown/old stream returns `200 { "events": [] }` — the honest answer, never an error.
- `POST /workflows/transcripts` → `list_transcript_streams`: gate; `state.transcript_publisher().list_streams(&request.workflow_id).await` → summaries mapped to `TranscriptStreamEntry` (activity_id via `ActivityId::from_sequence_position` is already inside the key). Empty list for a workflow with no retained transcripts.
Doc comments must state the mirror relationship: this pair is the transcript twin of `POST /workflows/describe` history (fetch) and the `/events/stream` transcript subscription remains the live-tail attach; a client that wants both does REST-fetch then WS-attach with `after_seq` = last fetched `store_seq` (dedup contract already in `subscribe`).
In-module serde round-trip tests mirroring `intervene.rs`.

### 4.3 Wiring
- `crates/aion-server/src/api/http/mod.rs`: add `mod transcripts;`.
- `crates/aion-server/src/api/http/router.rs`: after `.route("/workflows/attempts", post(list_attempts))` add:
```rust
.route("/workflows/transcript", post(fetch_transcript))
.route("/workflows/transcripts", post(list_transcript_streams))
```
(both in the same always-mounted chain, so they ride `workflow_router` AND `http_router`).

---

## 5. Work package E — injected messages guaranteed in the retained record

Tee at the ROUTER (covers every transport, testable without HTTP):
- `crates/aion-server/src/worker/intervention.rs`: add `transcript: Option<crate::activity_publisher::ActivityEventPublisher>` to `InterventionRouter` + `#[must_use] pub fn with_transcript_publisher(mut self, publisher) -> Self`. In `route`: before the command is moved into `transport.push`, capture `let injected = match &command.kind { InterventionKind::InjectMessage { text, .. } => Some((text.clone(), command.issued_at)), _ => None };` (the `key: AttemptKey` already holds workflow/activity/attempt). After `Ok(outcome)` from the transport, if `outcome.is_applied()`, `injected` is `Some`, and a publisher is installed → publish:
```rust
ActivityEvent { workflow_id: key.workflow_id.clone(), activity_id: key.activity_id.clone(),
    attempt: key.attempt, agent_id: Uuid::nil(), agent_role: "operator".to_owned(),
    emitted_at: issued_at, worker_seq: 0, store_seq: None, ephemeral: false,
    kind: ActivityEventKind::Message { role: MessageRole::User, text } }
```
A publish failure is `tracing::warn!`-logged and the `Applied` ack still returned (the intervention DID apply; retention is best-effort at this seam, same doctrine as the tap). Note in the doc comment: `agent_id` nil = server-origin operator record; nil never collides with a real agent's delta-stream coalescing in the console (`useTranscript.ts` joins on `agent_id`).
- `crates/aion-server/src/state.rs` (`intervention_router()`): append `.with_transcript_publisher(self.inner.transcript_publisher.clone())`.
- Tests in `crates/aion-server/src/worker/intervention_tests.rs` (reuse `RecordingTransport`, `inject`, `register_worker`):
  - `an_applied_inject_is_retained_as_an_operator_user_message`: router built with `.with_transcript_publisher(ActivityEventPublisher::new(Arc::new(InMemoryObservabilityStore::default()), NonZeroUsize::new(8)…))`; route an advertised inject → `Applied`; `replay_from(key, 0)` → exactly one record: `Message { role: User, text }`, `agent_role == "operator"`, `store_seq == Some(0)`.
  - `gated_stale_and_cancel_outcomes_retain_nothing`: unadvertised primitive, unowned attempt, and an applied `Cancel` each leave the durable stream empty.
- `crates/aion-server/tests/noi5b_noi6_live_agent_e2e.rs` — the tee shifts the live-path store_seqs (GATE 2's applied inject now persists at seq 1, between "thinking…"=0 and m-1/m-2):
  - threshold `< 3` → `< 4` (and the message "3 records" → "4 records").
  - resume-from-1 expectation `vec![1, 2]` → `vec![1, 2, 3]`; ADD an assertion that the record at `store_seq == 1` is `Message { role: User, text }` with `text == "stop editing that file"` and `agent_role == "operator"` — this is the live-path wiring proof.
  - Run the file's second test (`serve_with_redial_installs_the_composed_harness`); if it also routes an applied inject, adjust its expectations the same way (semantics-preserving only; the file's existing top-level `#![allow…]` is legacy — do not add new allow attributes anywhere).

---

## 6. Work package F — acceptance execution test

New file `crates/aion-server/tests/transcript_retention_e2e.rs`. Harness: copy the `TranscriptServer`/`runtime_config` shape from `crates/aion-server/tests/transcript_stream_e2e.rs` but drive HTTP via `tower::ServiceExt::oneshot` on `workflow_router(state.clone())` (the deploy e2e pattern — real axum stack, no TCP listener needed) with headers `x-aion-subject: alice`, `x-aion-namespaces: tenant-a`. Make `runtime_config` take the `ObservabilityConfig` so tests tune bounds. Keep the file ≤500 code lines; split a second file if needed.

Tests:
1. `retained_transcript_is_fetchable_in_order_after_all_subscribers_drop` — **THE ACCEPTANCE CASE**: attach a live subscriber (`state.transcript_publisher().subscribe(key, None)`); publish messages `m0..m2` + one ephemeral delta through the publisher (the exact ingress seam); `drop(live)` (all subscribers gone); publish `m3`, `m4`; then `oneshot POST /workflows/transcript` → 200 with exactly 5 events, `store_seq` `[0,1,2,3,4]`, texts `m0..m4` in order, and NO `Delta` kind present. Then `oneshot POST /workflows/transcripts` → `[{activity_id: 3, attempt: 0, head: 5}]`. (Fetching later with zero subscribers IS the "open it an hour later" proof — retention is store-backed, time- and subscriber-independent.)
2. `a_run_with_no_retained_transcript_reads_empty` — second workflow id recorded under tenant-a with nothing published: `/workflows/transcripts` → `{"streams": []}`; `/workflows/transcript` → `{"events": []}`.
3. `transcript_fetch_denies_a_foreign_namespace_caller` — same requests with `x-aion-namespaces: tenant-b` → non-200 wire error, body contains no `events`/`streams` (anti-leak parity with the WS test).
4. `configured_bounds_flow_from_runtime_config_to_the_wire` — boot with `max_event_bytes: 512, max_stream_events: 3`; publish one oversized message then 5 normal ones; fetch → seq 0 truncated (marker suffix), seqs 1-2 normal, seq 3 the retention-cap `Progress`/`Note` marker, nothing after; enumeration head == 4. (Proves config plumbing end-to-end; the truncation matrix itself is §2 unit tests.)
5. `fetch_from_seq_resumes_mid_stream` — `from_seq: 3` returns exactly seqs `[3,4]` (REST twin of GATE 4).

---

## 7. Console: NO changes in this lane (deliberate)

The console already satisfies the operator acceptance flow: finished-run attempts stay selectable from durable history and their transcripts cold-load over the WS durable replay. The new REST pair is additive (automation/CLI parity + the socket-free acceptance proof) and nothing in `apps/aion-ops-console` changes — therefore the committed embed at `crates/aion-server/ops-console-embed` must NOT be regenerated (regenerate only if console files change). Do not run `biome`; run `cargo fmt` over the workspace.

---

## 8. Gates (redirect FULL output to `<worktree>/t229-gates/*.log`; never pipe through grep/tail/head; record every exit code in `t229-gates/manifest.txt`)

1. `cargo fmt` (workspace-wide, in the worktree).
2. `cargo clippy --workspace --all-targets` → `t229-gates/clippy.log`.
3. `cargo clippy -p aion-server --all-targets --features liminal-transport` → `t229-gates/clippy-liminal.log` (compiles the noi5b edits).
4. `cargo test -p aion-store` → `t229-gates/test-store.log`.
5. `cargo test -p aion-store-haematite` → `t229-gates/test-store-haematite.log`.
6. `cargo test -p aion-server` → `t229-gates/test-server.log` (includes `transcript_retention_e2e`, `transcript_stream_e2e`, config load tests, intervention tests).
7. `cargo test -p aion-server --features liminal-transport --test noi5b_noi6_live_agent_e2e` → `t229-gates/test-noi5b.log`.
8. `cargo test --workspace` → `t229-gates/test-workspace.log` (the RuntimeConfig fan-out touches many crates' tests).

Workspace laws throughout: no `unwrap`/`expect`/`panic` in NEW code (tests use `?` + typed errors); no new `#[allow]`/`#[expect]`/`#[ignore]`; files ≤500 code lines; `mod.rs` re-exports only; backticked identifiers in all doc comments (doc_markdown is DENY).

Commit on `lane/t229-transcript-retention`, staging EXPLICIT paths only, message ending with the trailer:
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
Do NOT push/merge/deploy/restart anything.

---

## 9. Ordered checklist

1. Create the worktree/branch (§ header).
2. A: `aion-store` `ActivityStreamSummary` + trait method + in-memory impl + tests + lib re-export (§1.1).
3. A: haematite keyspace prefix/decode + tests (§1.2); store scan impl + reopen test (§1.3).
4. B: `activity_bounds.rs` (`TranscriptBounds`, `bound_event`) + unit tests; register module in `lib.rs` (§2.2).
5. B: publisher `list_streams` + bounds field/builder + capped `publish` loop; relocate publisher tests to `activity_publisher_tests.rs`; add the three new publisher tests (§2.1).
6. C: config section/defaults/env/load/runtime + validation + load tests; re-export (§3).
7. C: `state.rs` bounds wiring in `build_real_time_publishers`; mechanical `observability: ObservabilityConfig::default(),` fan-out to every `aion_server` `RuntimeConfig` literal (§3 list).
8. D: `gate_transcript_workflow` extraction (§4.1); `transcripts.rs` DTOs+handlers+tests (§4.2); `mod.rs` + router routes (§4.3).
9. E: router tee + state install + `intervention_tests.rs` additions (§5).
10. E: noi5b seq-expectation updates + the new operator-message assertion (§5).
11. F: `transcript_retention_e2e.rs` with tests 1–5 (§6).
12. `cargo fmt`; run gates 2–8 in order, fixing failures; write `t229-gates/manifest.txt` (gate name → exit code, all zero).
13. Commit on the lane branch with explicit paths (all touched files under `crates/aion-store`, `crates/aion-store-haematite`, `crates/aion-server`) + the trailer. No push, no merge.

---

## Implementation deviations (recorded at build time)

- **Worktree branch name**: the orchestrator's lane header mandated `.worktrees/transcript-retention -b transcript-retention`; this spec's header mandated `.worktrees/t229-transcript-retention -b lane/t229-transcript-retention`. The spec's names were used (the more specific instruction).
- **`from_parts*` bounds (§3)**: the spec said the four `from_parts*` sites keep default bounds; they instead derive bounds from the `RuntimeConfig` they are handed (`transcript_bounds(&runtime)`). Required for §6 test 4 (`configured_bounds_flow_from_runtime_config_to_the_wire`), whose harness — per §6's own instruction to copy the `transcript_stream_e2e` shape — builds state through `ServerState::from_parts`; with hardcoded defaults the configured bounds would never reach the publisher and the test would fail. Deriving from the runtime is also the more truthful contract: an embedder's `[observability]` settings are honored.
- **`with_bounds` visibility (§2.1)**: `pub(crate)` rather than `pub`, since `TranscriptBounds` lives in a `pub(crate)` module (a `pub fn` exposing it would trip the `private_interfaces` lint and export an unnameable type).
- **Haematite enumeration scan (§1.3)**: deliberately NOT `EventStore::scan` as the spec sketched — that walks only shards materialised this process lifetime, so on a freshly reopened database it would enumerate nothing until each stream's shard was touched. `observability_list_blocking` instead uses `database().scan_sequence_keys_for_shards(..)` over every shard (with the same `stream_has_live_events` guard `EventStore::scan` applies), which materialises/WAL-recovers on demand and makes enumeration restart-correct — proven by the reopen leg of `list_activity_streams_enumerates_only_the_workflows_streams_and_survives_reopen`.
- **`crates/aion-cli/tests/new_agent_e2e.rs` (out of spec scope)**: the scaffolded-agent e2e spawns `cargo build` inside a generated project and asserts the binary at the project-local `target/` path; the lane's mandated shared `CARGO_TARGET_DIR` is inherited by that child and strands the binary elsewhere, failing the test for an environmental reason. The child now runs with `env_remove("CARGO_TARGET_DIR")` — semantics-preserving, and the test passes both with and without a shared target dir.
