# Norn Agent Observability + Mid-Run Intervention (DESIGN)

> Status: **design pass, read-only analysis. No production code changed by this doc.**
> 2026-07-01. Cross-repo: **norn**, **aion** (worker + server + ops-console),
> **liminal**, **haematite**. Written in the lineage of the other design passes
> ([CONTROL-PLANE.md](./CONTROL-PLANE.md), [CLUSTER-AUTODISCOVERY.md](./CLUSTER-AUTODISCOVERY.md),
> [HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md](./HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md)).
>
> Companion to the Aion observability-layer direction (auto-memory
> `aion-observability-layer-direction.md`) and the ops-console-out-of-box thread
> (`ops-console-out-of-box.md`). This doc makes a **specific, locked set of
> decisions** concrete across the five repos and defends the one crux that governs
> the whole design: **an intervention is a durable observability record but NOT
> part of the workflow replay log.**

## TL;DR (read this first)

1. **The story.** You open the ops console on a running Norn agent activity, watch
   its transcript stream live — messages, tool calls, tool results, token deltas —
   and, mid-run, you type a steer ("stop editing that file, use the other module")
   that lands in the agent's next tool boundary. When the agent finishes, its
   **result** is the single authoritative activity output the workflow durably
   records. Your steer is preserved in the durable transcript (auditable, replayable
   as a transcript), but it is **not** workflow state: on a retry the activity
   re-runs fresh without it.

2. **JSON-RPC 2.0 stdio-duplex process contract (LOCKED — SUPERSEDES the former three-fd
   model).** A headless Norn run driven in the new `--protocol jsonrpc` mode speaks a
   **single bidirectional JSON-RPC 2.0 channel over child stdin + child stdout**, with an
   **LSP-style `initialize`/capabilities handshake** and our own method namespace
   (`initialize`, `run/*`, `event/*`, `intervene/*`, `approval/*`). Result-vs-events
   separation — Tom's original linchpin insight — is **PRESERVED, now expressed by JSON-RPC
   MESSAGE TYPING instead of fd separation**: the **RESULT is the `run/*` RESPONSE** (a
   message with an `id`; the single final structured value the worker captures as the
   activity output → replay-authoritative history), **events are `event/*` NOTIFICATIONS**
   (no `id`; never enter history), **interventions are `intervene/*` REQUESTS with acks**,
   and **approvals are agent-initiated `approval/*` REQUESTS correlated by `id`**.
   **stderr stays human logs** — deliberately kept OUT of the structured JSON-RPC stream so
   library noise can never pollute the durable store (the tracing subscriber already targets
   stderr, [main.rs:21-27](../../../norn/crates/norn-cli/src/main.rs#L21)). This byte-level
   discipline — only the `run/*` Response feeds workflow history, Notifications never do —
   is what fd disjointness bought before, now made structural by the message kind. Today
   Norn has *no* JSON-RPC driven mode: stdin is fully consumed as the prompt
   ([orchestrator.rs:194-201](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L194)),
   one `run_agent_step` runs, then exit ([orchestrator.rs:383](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L383)),
   `cancel: None` ([orchestrator.rs:399](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L399));
   the JSON-RPC 2.0 stdio framing itself is proven prior art in-tree
   ([mcp_server.rs::serve_stdio:206-227](../../../norn/crates/norn/src/integration/mcp_server.rs#L206)).

   **Additive — removes nothing.** The JSON-RPC mode is a **NEW driven mode gated behind
   `--protocol jsonrpc`**. Norn's existing `-p`/headless stdout `stream-json`, `-f json`
   envelope, `-f text`, and the TUI are **UNTOUCHED**; the driven branch is entered only when
   the flag is set. The worker gains a **NEW additive `spawn_agent` JSON-RPC mode** beside its
   current blocking one-shot capture. See §4.0.

3. **Reuse Norn's native events; do not reinvent.** Norn already has a live event
   spine — a `broadcast::Sender<AgentEvent>` carrying `AgentEventKind`
   (`Provider | Subagent | Message | UsageEstimate`,
   [agent_event.rs:300](../../../norn/crates/norn/src/provider/agent_event.rs#L300))
   — and a working `AgentEvent → NDJSON` translator
   ([output.rs:317 `agent_event_to_ndjson`](../../../norn/crates/norn-cli/src/print/output.rs#L317)).
   The `event/*` notification emitter **reuses that translator's per-line JSON `Value`
   verbatim as the notification `params`** and derives the JSON-RPC `method` from the same
   `AgentEventKind` match arm, so payloads stay byte-identical — only the framing
   (`{"jsonrpc":"2.0","method":"event/...","params":<payload>}`) is new, plus the
   `agent_id`/`agent_role` the current translator drops. **Norn is the privileged first
   producer**, and JSON-RPC is its privileged rich path.

4. **Server is the SEQUENCER (LOCKED).** Workers stay thin: best-effort ordering +
   an `emitted_at` timestamp. The aion-server assigns a **monotonic `store_seq` at
   durable-commit time** — NOT an in-process counter, which resets on failover. The
   dashboard resume cursor is `store_seq`. Honest mechanism (§5.3): haematite
   `append_batch` / aion `WritableEventStore::append` take a **caller-supplied
   `expected_seq`** (optimistic concurrency, returning `next_seq` / `SequenceConflict`) —
   they do NOT auto-allocate a collision-proof id, so monotonicity depends on the server
   being the **single writer** to the `O` keyspace and on it serializing/handling
   `SequenceConflict`. This mirrors how `EventEnvelope.seq` is
   store-allocated per workflow today ([event_store.rs:132-157](../../../haematite/crates/haematite/src/api/event_store.rs#L132)),
   NOT how `ClusterEventPublisher.next_seq` is process-allocated (which resets,
   [cluster_publisher.rs:46](../../../aion/crates/aion-server/src/cluster_publisher.rs#L46)).

5. **The crux — intervention vs replay/retry.** Interventions ARE recorded as durable
   observability events (auditable, visible on transcript replay) but are NOT part of
   the workflow REPLAY log. The activity **result** stays the single replay-authoritative
   output. On RETRY the activity re-runs **fresh, without prior interventions**
   (interventions are ATTEMPT-SCOPED) because an intervention is a human real-time act,
   not deterministic workflow state. §7 states the strongest objection ("the retry then
   diverges from what you intervened to produce") and answers it. The answer holds
   because the intervened *result*, if accepted, is already the durable output and no
   retry occurs; a retry only happens when that attempt did NOT produce an accepted
   result, at which point re-running fresh is the correct, non-surprising behavior.
   There is a residual sharp edge (§7.4) which we name and mitigate rather than paper over.

6. **Keyspace guarantee.** Observability + intervention events live in a **new, third
   haematite keyspace family** — a general-KV region tag `'O'` that is provably
   `!= 0x00` and `!= 'E'` (the replay-log stream tag) and disjoint from the existing
   `t/p/r/o/n` tags ([keyspace.rs:14-27](../../../aion/crates/aion-store-haematite/src/keyspace.rs#L14)) —
   uppercase `'O'` (0x4F) is disjoint from every actual tag regardless.
   The replay decoder only ever scans the `E`-stream and decodes `serde_json::<Event>`
   ([store.rs:1237-1258](../../../aion/crates/aion-store-haematite/src/store.rs#L1237)),
   so an `O`-region record is **structurally invisible** to replay. That byte-level
   disjointness is what makes "durable but non-authoritative" a guarantee, not a hope.

7. **Scope boundary (§2).** This is Norn-privileged **JSON-RPC 2.0 stdio-duplex** (the
   rich path), with the worker adapter supporting *other* harnesses via mixed-stdout demux
   for observability (and no intervention) and capability-gated intervention. It is NOT a
   generic distributed tracing system, NOT OpenTelemetry, and does NOT replace the Prometheus
   metrics surface ([metrics.rs](../../../aion/crates/aion-server/src/observability/metrics.rs)).

8. **Honesty caveat — intervention rides a HARNESS-NEUTRAL contract; Norn is merely the
   FIRST adapter.** Both *observability* and *intervention* are harness-neutral by design.
   The intervention **command vocabulary** is the complete set of five neutral semantic
   primitives — `InjectMessage`, `Cancel`, `PauseResume`, `UpdateBudget`, and
   `RespondToApproval` (§3.3) — spoken by the wire, server, and ops-console, none of
   which reference Norn types; ALL harness-specific translation lives in one place, the
   worker-side per-harness adapter (§3.4). A harness advertises **which neutral primitives it
   supports** (e.g. `{inject_message, cancel}`) in its JSON-RPC `initialize` capabilities
   response, which the worker **forwards to the server in `RegisterWorker`**, and **any harness
   that implements them is FIRST-CLASS, not second-class**. That the
   FIRST adapter (Norn) does not yet implement every neutral primitive is the STRONGEST proof
   the contract is not Norn-shaped: capability-gating means the complete contract exists
   independent of any one harness's coverage. "Norn-only"
   is a **today-fact of adapter coverage** (there is, today, exactly one shipped adapter —
   the Norn one), **NOT a design limitation**: the contract is not Norn-shaped, and a second
   harness is "write an adapter," never "reshape aion-core." The `RegisterWorker` proto
   capabilities field this rides on is **genuine net-new work** (`worker.proto` today is a
   fixed 4-field shape,
   [worker.proto:94-109](../../../aion/crates/aion-proto-generated/proto/worker.proto#L94)),
   not decoration. A harness that genuinely cannot take control commands advertises no
   primitives and the console offers no controls for it — an absent tier, not a second-class
   contract.

---

## 1. The two things we are building

**Observability** — a live, durable, per-`(workflow, activity, attempt)` transcript
of what a Norn agent is doing inside an activity: its messages, tool calls, tool
results, progress, stop reasons, and (ephemeral) token deltas. Streamed to the ops
console in real time, persisted to a haematite keyspace that survives kill-9 and
failover, replayable as a transcript, and **never** mixed into workflow replay history.

**Intervention** — a live, best-effort, mid-run control channel INTO a running agent,
expressed in the complete set of **harness-neutral primitives** (§3.3): `InjectMessage` (an
out-of-band user turn, `Normal` or `Interrupt` priority — steering is just an `Interrupt`
injection), `Cancel` (stop the agent run), `PauseResume` (suspend/resume between steps),
`UpdateBudget` (adjust the run's resource limits mid-flight), and `RespondToApproval`
(answer a pending tool-use / permission gate). Routed operator → server → the worker currently owning the
activity-attempt → the worker-side per-harness adapter → the child as a JSON-RPC
`intervene/*` REQUEST that returns an ack. Recorded as durable
observability events (so the transcript shows "operator steered here"), but NOT as
replay state.

The two share one envelope family, one liminal transport, one server bridge, one
haematite keyspace, and one ops-console feature. They differ only in direction (event
notifications out, intervention requests in) and in durability semantics (events are the
primary durable artifact; command *delivery* is live-only, command *record* is durable).

---

## 2. Scope boundary — what this is and is NOT

- **IS:** a Norn-native **JSON-RPC 2.0 stdio-duplex** channel (`event/*` notifications +
  `intervene/*`/`approval/*` requests) as the privileged first-class **transport**;
  a **harness-neutral command vocabulary** (`InjectMessage`, `Cancel`, `PauseResume`,
  `UpdateBudget`, `RespondToApproval` — §3.3) spoken by the
  wire/server/console; a worker adapter that spawns the agent, tees `event/*` notifications
  → liminal, and translates neutral commands → the harness's native control channel; a
  server bridge that sequences + persists + fans out; a haematite `O`-region keyspace;
  ops-console transcript panel + intervention controls.
- **IS (harness-neutral, first-class-if-implemented):** JSON-RPC is the **privileged rich
  path** (events + acknowledged interventions + agent-initiated approvals). A **non-JSON-RPC
  harness falls back to mixed-stdout demux for observability only** and **offers no
  intervention**. Intervention is **capability-gated on the neutral primitive set** — a harness
  whose JSON-RPC adapter advertises `{inject_message, cancel}` at `initialize` is first-class
  for those; a harness that cannot take control commands advertises an empty set (or speaks no
  JSON-RPC at all) and the ops console offers no controls for it. Norn is the FIRST adapter,
  not a privileged command shape (§3.4).
- **IS NOT:** a generic OTel/Jaeger tracing backend; a replacement for the Prometheus
  `/metrics` surface (per-process aggregates,
  [instrumented_store.rs](../../../aion/crates/aion-server/src/observability/instrumented_store.rs));
  a new workflow-history event family (transcript events do NOT enter `Event` /
  `EventEnvelope`, [event.rs:14-25](../../../aion/crates/aion-core/src/event.rs#L14));
  a durable command queue (intervention delivery is live-only — see §6);
  a cross-node message bus beyond what liminal already provides.
- **NON-NEGOTIABLE INVARIANT (mirrors the CSOT "discovery is not a source of truth"
  rule):** the observability/intervention keyspace MUST NEVER become a second source
  of truth for workflow state. Replay reads only the `E`-stream. If an intervention's
  *effect* must change deterministic workflow state, that effect goes through the
  normal `WritableEventStore::append` `E`-stream path as a first-class `Event`; the
  `O`-region copy is a mirror/annotation, never the authority (§7.5).

---

## 3. Shared types — the envelope, the command, where they live

### 3.1 Shared types: new modules in `aion-core` (NOT a new crate)

Shared types live as **new modules in `aion-core`** — `activity_event.rs` (the envelope +
event kinds) and `intervention.rs` (the command enum + `ApprovalDecision`) — siblings of the
existing `cluster_event.rs`. **No new crate.** Rationale, grounded in the actual layout:

- **`aion-core` is already the ts-rs shared-DTO home, and the only crate that uses it**
  (`ts-rs = { workspace = true }`, [aion-core/Cargo.toml:24](../../../aion/crates/aion-core/Cargo.toml#L24)).
  `cluster_event.rs`, `status.rs`, `describe.rs` all derive `ts-rs` and land in the
  ops-console generated union
  ([types/generated/index.ts:163](../../../aion/apps/aion-ops-console/src/types/generated/index.ts#L163)).
  The envelope/command enums join that union exactly as `ClusterEvent` does today — a new
  crate would have to re-import `ts-rs` and re-wire codegen for no gain.
- **The precedent is exact: `aion-core` already coexists the replay-authoritative `Event`
  with a non-replay real-time DTO.** `cluster_event.rs` (`ClusterEvent`) is precisely that
  category — a live cluster-stream type that is *never* replayed. `ActivityEvent` and
  `InterventionCommand` are the same kind of thing and belong beside it.
- **worker and server already both depend on `aion-core`**, so there is no diamond and no
  new leaf crate to wire — putting the types in core is strictly *less* graph, not more.

**What about the §7.5 conflation risk** (a future contributor making transcript data
replay-authoritative)? A separate crate was floated to make that a cross-crate change. But
that guard is **structural and already in place**, not a function of crate boundaries: the
replay decoder reads *only* the E-stream
([store.rs:1237-1258](../../../aion/crates/aion-store-haematite/src/store.rs#L1237)) and the
observability records live under the byte-disjoint `O` keyspace tag (0x4F), so an `O`-record
is *undecodable* as an `Event` regardless of which crate defines the type. `cluster_event.rs`
already lives in `aion-core` under exactly this discipline. Module + type separation + the
keyspace disjointness IS the fence; a crate boundary would be redundant ceremony the project's
own precedent does not use.

**Does Norn depend on these `aion-core` types?** **No.** Norn emits its `event/*` JSON-RPC
notifications using its *own* native shapes (the same per-line JSON `agent_event_to_ndjson`
already produces, wrapped as notification `params`). The **worker adapter** owns the
translation from Norn's on-wire notification payloads into the `ActivityEvent` envelope. This
keeps Norn free of an aion dependency (it stays a standalone agent harness) and keeps the
envelope an aion-side contract the worker adapter enforces — which is *required anyway* for the
harness-agnostic path, where a non-Norn harness's stdout events must be demuxed and mapped by
the same adapter. Norn's `event/*` schema is a stable, documented JSON-RPC contract; the
adapter is the single translation point.

### 3.2 The `ActivityEvent` envelope

```rust
// aion-core::activity_event
pub struct ActivityEvent {
    // identity — the (workflow, activity, attempt) key + agent sub-identity
    pub workflow_id: WorkflowId,
    pub activity_id: ActivityId,
    pub attempt: u32,
    pub agent_id: Uuid,          // Norn AgentEvent.agent_id — REQUIRED for multi-agent attribution
    pub agent_role: Arc<str>,    // Norn AgentEvent.agent_role
    // ordering
    pub emitted_at: DateTime<Utc>,        // worker/producer clock, best-effort
    pub worker_seq: u64,                  // worker-local best-effort monotonic
    pub store_seq: Option<u64>,           // SERVER-STAMPED at commit; None until persisted
    pub ephemeral: bool,                  // true for token Deltas — WS-forward only, never persist
    // payload
    pub kind: ActivityEventKind,
}

pub enum ActivityEventKind {
    Message   { role: MessageRole, text: String },        // ProviderEvent::TextComplete/ThinkingComplete + AgentMessageLifecycle
    ToolCall  { tool: String, call_id: String, input: serde_json::Value },  // ProviderEvent::ToolCallComplete
    ToolResult{ call_id: String, output: serde_json::Value, is_error: bool },// ProviderEvent::ToolResult + SessionEvent::ToolResult
    Progress  { detail: ProgressDetail },                 // TextDelta/ThinkingDelta/ToolCallDelta + AgentUsageEstimate
    Stop      { reason: StopKind },                        // ProviderEvent::Done{stop_reason} + AgentStopReason
    Raw       { source: String, value: serde_json::Value },// passthrough fallback for unmapped/other-harness lines
    Delta     { message_id: String, text_fragment: String },// EPHEMERAL token deltas (ephemeral=true)
}
```

**Kinds are LOCKED:** `Message`, `ToolCall`, `ToolResult`, `Progress`, `Stop`, `Raw`,
plus `Delta` carried on the same channel but flagged `ephemeral` (forwarded to the WS,
never persisted). `Raw` is the passthrough fallback — critical for the harness-agnostic
path and for forward-compat when Norn adds an event shape the adapter does not yet map.

**Mapping from Norn native events (adapter-side).** The Norn→envelope mapping reuses the
exact categories the existing translator already computes
([output.rs:317](../../../norn/crates/norn-cli/src/print/output.rs#L317)):

| Envelope kind | Norn source |
|---|---|
| `Message` | `ProviderEvent::TextComplete`→text / `ThinkingComplete`→thinking ([output.rs:463-470](../../../norn/crates/norn-cli/src/print/output.rs#L463)); `AgentMessageLifecycle` for inter-agent ([output.rs:372](../../../norn/crates/norn-cli/src/print/output.rs#L372)) |
| `ToolCall` | `ProviderEvent::ToolCallComplete`→`tool_call` ([output.rs:471](../../../norn/crates/norn-cli/src/print/output.rs#L471)) |
| `ToolResult` | `ProviderEvent::ToolResult`→`tool_result` ([output.rs:487](../../../norn/crates/norn-cli/src/print/output.rs#L487)); durable form also `SessionEvent::ToolResult` |
| `Progress` | `TextDelta/ThinkingDelta/ToolCallDelta` (gated by `partial`, [output.rs:420](../../../norn/crates/norn-cli/src/print/output.rs#L420)); `AgentUsageEstimate`→`usage_estimate` ([output.rs:330](../../../norn/crates/norn-cli/src/print/output.rs#L330)) |
| `Stop` | `ProviderEvent::Done{stop_reason: StopReason}`→`done` ([output.rs:507](../../../norn/crates/norn-cli/src/print/output.rs#L507)); richer `AgentStopReason` ([agent/output.rs:35](../../../norn/crates/norn/src/agent/output.rs#L35)) via `SubagentLifecycle::Completed.stop` + terminal `completed` line ([output.rs:549](../../../norn/crates/norn-cli/src/print/output.rs#L549)) |
| `Delta` | `TextDelta` fragments, `ephemeral=true` |
| `Raw` | any line the adapter cannot classify |

Note: `ProviderEvent`, `AgentUsageEstimate`, `StopReason` are **NOT** serde-derived
([events.rs:24](../../../norn/crates/norn/src/provider/events.rs#L24),
[agent_event.rs:290](../../../norn/crates/norn/src/provider/agent_event.rs#L290)); their
JSON is hand-built in `output.rs`. The `event/*` notification emitter reuses/extends those
hand-built mappers as the notification `params` — it CANNOT naively `serde_json::to_value` them
(§9 risk).

### 3.3 The `InterventionCommand` enum — HARNESS-NEUTRAL SEMANTIC PRIMITIVES

The command vocabulary is defined in **harness-neutral semantic primitives**, NOT in any
harness's native terms. The enum lives in `aion-core` (`intervention.rs`, beside
`cluster_event.rs`; `ts-rs`-derived for the dashboard) and is spoken by **the wire, the server, and the
ops-console — none of which may reference Norn types**. Norn's `Steer`/`Update`/
`CancellationToken` appear NOWHERE in this enum; they live strictly in the worker-side
adapter mapping (§3.4 / §6).

**The design test (explicit):** a primitive belongs in the neutral enum ONLY if it can
plausibly map onto a **non-Norn** conversational-agent harness. Anything that only makes
sense as a Norn feature does not belong here — it belongs behind the adapter.

The complete neutral set is exactly five primitives — the whole universal agent-control
surface, each one gated by the harness's advertised capability set:

```rust
// aion-core::intervention
pub struct InterventionCommand {
    pub workflow_id: WorkflowId,
    pub activity_id: ActivityId,
    pub attempt: u32,             // commands to a stale attempt are no-ops (§6.4)
    pub issued_by: Option<Subject>, // auth subject when auth is ON
    pub issued_at: DateTime<Utc>,
    pub kind: InterventionKind,
}

pub enum InterventionKind {
    // An out-of-band user turn injected into the running agent (steer / redirect /
    // add context). SUBSUMES "steer": steering is just an Interrupt-priority injection.
    // There is NO separate Steer/Update variant in the neutral enum.
    // Interrupt = steer-now; Normal = queued turn.
    InjectMessage    { text: String, priority: InjectPriority },
    // Stop the AGENT RUN (this subprocess's current run). See §7.5:
    // this is DISTINCT from a workflow-visible cancel/signal, which stays
    // on the E-stream engine paths and is NOT an agent-stdin intervention.
    Cancel           { reason: String },
    // Suspend/resume the agent between steps. Capability-gated: harnesses that
    // cannot suspend mid-step advertise no support for it.
    PauseResume      { paused: bool },
    // Adjust the run's resource limits mid-flight.
    UpdateBudget     { max_tokens: Option<u64>, max_turns: Option<u32> },
    // Answer a pending tool-use / permission gate — human-in-the-loop approval of
    // the agent's next action. The highest-value watch-and-control primitive.
    RespondToApproval { call_id: String, decision: ApprovalDecision, note: Option<String> },
}

pub enum InjectPriority {
    Normal,     // a queued user turn (batches; may not wake an idle agent)
    Interrupt,  // act now — this is what "steer" was
}

pub enum ApprovalDecision {
    Approve,
    Deny,
}
```

**Every primitive passes the design test** (it can plausibly map onto a non-Norn
conversational-agent harness): `InjectMessage` and `Cancel` are universal; `PauseResume`
is the standard suspend/resume any stepped agent loop can expose; `UpdateBudget` maps onto
any harness with token/turn limits; `RespondToApproval` maps onto any harness with a
tool-use / permission gate. None is Norn-specific — anything that only made sense as a Norn
feature would belong behind the adapter, not here.

**`Cancel { reason }` cancels the AGENT RUN as an observability/control act — it does NOT
write workflow replay state** (§7.5). A workflow-visible cancel/signal is a different thing
entirely and stays on the `E`-stream engine paths; the neutral enum has no state-affecting
variant by construction. Likewise `RespondToApproval { decision: Deny }` is an agent-run
control act (it declines the agent's proposed next action); it does NOT write workflow
replay state (§7.5).

The harness capabilities the transport actually needs already exist in Norn — the ride-along
mechanics that the *adapter* (§3.4) maps the supported primitives onto are `MessageKind
{ Steer, Update }` ([inbound.rs:45](../../../norn/crates/norn/src/loop/inbound.rs#L45)), a
cloneable `InboundSender` ([inbound.rs:244](../../../norn/crates/norn/src/loop/inbound.rs#L244)),
drain at tool boundaries ([runner.rs:939](../../../norn/crates/norn/src/loop/runner.rs#L939)),
and a `CancellationToken` checked at boundaries
([runner.rs:470](../../../norn/crates/norn/src/loop/runner.rs#L470),
[:809](../../../norn/crates/norn/src/loop/runner.rs#L809)) — but these are adapter-internal
details, referenced only in §3.4 and never above the adapter.

### 3.4 The adapter boundary (LOCKED) — the single translation point

**Rule:** `aion-core` / the wire / `aion-server` / the ops-console speak **ONLY neutral
commands and are harness-blind.** ALL harness-specific translation lives in **ONE place —
the worker-side per-harness adapter.** Nothing above the adapter may name a Norn type.

The **Norn adapter** maps the neutral primitives it supports onto Norn's native control
channel, and advertises the rest as UNSUPPORTED until the underlying Norn mechanism exists:

| Neutral primitive | Norn native mapping |
|---|---|
| `InjectMessage { priority: Interrupt }` | Norn's steer/priority path — `ChannelMessage { kind: MessageKind::Steer }` (immediate, drains at the next tool boundary) |
| `InjectMessage { priority: Normal }` | a queued Norn `ChannelMessage`/`Update` (batches to stop-time; deliberately does NOT wake an idle agent, [inbound.rs:189](../../../norn/crates/norn/src/loop/inbound.rs#L189), DECISION M2) |
| `Cancel { reason }` | Norn `CancellationToken` (hard stop at next boundary) |
| `PauseResume { paused }` | **advertised UNSUPPORTED** — Norn has no cited suspend/resume-between-steps mechanism; the Norn adapter returns a clean "capability not supported" rejection until one exists |
| `UpdateBudget { .. }` | **advertised UNSUPPORTED** — no cited mid-flight budget-mutation surface on the headless run; advertised unsupported until the mechanism exists |
| `RespondToApproval { .. }` | **advertised UNSUPPORTED** — no cited pending-approval / permission-gate surface on the headless Norn run to answer; advertised unsupported until the mechanism exists |

This is the ONLY location where the Norn-specific `inbound.rs` types (`frame_message` /
`ChannelMessage` / `CancellationToken`) are referenced. A **future harness** is "write an
adapter mapping the neutral primitives it supports to its own control channel," **never**
"reshape `aion-core`." (Cross-referenced from §6, which describes the same boundary at the
flow level.)

**Why the UNSUPPORTED rows are a feature, not a gap.** Having neutral primitives that even
the FIRST adapter (Norn) does not yet implement is the STRONGEST possible proof the contract
is **not Norn-shaped**: if the neutral enum were merely Norn's control surface renamed, every
primitive would map. It does not. `PauseResume`, `UpdateBudget`, and `RespondToApproval` are
defined by what a *universal* agent-control surface must express, and capability-gating means
the **complete contract exists independent of any one harness's coverage** — Norn advertises
`{inject_message, cancel}`, the server/console gate on that set, and the other three light up
for whichever harness (Norn included, once the mechanism lands) advertises them. We do NOT
fabricate Norn internals to fill these rows; "advertised unsupported until the mechanism
exists" is the honest entry.

---

## 4. The JSON-RPC 2.0 stdio-duplex model + the Norn-repo changes

This is the load-bearing new surface in the **norn** repo. Today Norn has none of it. The
transport is a **single bidirectional JSON-RPC 2.0 channel over child stdin + child stdout**,
with stderr reserved for human logs. **Result-vs-events separation is now by MESSAGE TYPING,
not fd separation** — the crux Tom locked (result on its own channel, events on another) is
preserved structurally: only a `run/*` RESPONSE (a message with an `id`) is the
replay-authoritative result; `event/*` NOTIFICATIONS (no `id`) never enter history.

### 4.0 Additive — removes nothing

The JSON-RPC channel is a **NEW driven mode**, gated behind a **`--protocol jsonrpc`** flag on
the `Cli` struct ([args.rs:23](../../../norn/crates/norn-cli/src/cli/args.rs#L23)) — a
*transport* flag, deliberately NOT a fourth `OutputFormat` variant, so `-o file` redirection
and `--partial` (which are render concerns) do not implicitly apply to a duplex. The driven
branch is entered early in `execute`/`orchestrate` (before `read_stdin_if_piped`,
[orchestrator.rs:177](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L177)/
[:194](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L194)) and takes ownership of
stdin+stdout, running its own loop **instead of** the one-shot `write_output` path. Nothing
existing is touched:

- **`-f text` / `-f json` / `-f stream-json` stay byte-for-byte identical** — their branches
  are only reached when `--protocol jsonrpc` is absent. The stream-json renderer, JSON
  envelope, text renderer, and their `broadcast::channel` + `finish()` termination remain
  intact ([output.rs](../../../norn/crates/norn-cli/src/print/output.rs)).
- **The TUI is untouched** — it never inspects the protocol flag and is only reached when
  `detect_mode` returns Tui ([driver.rs](../../../norn/crates/norn-cli/src/tui/driver.rs)).
- **`detect_mode` purity is preserved** — the driven flag is honored inside Print mode
  (where a stdout pipe already lands, [mode.rs:32-38](../../../norn/crates/norn-cli/src/cli/mode.rs#L32));
  the flag must be explicit, like `-p`, to override mode inference cleanly.
- **The worker keeps its current blocking one-shot `.output()` capture as the default**; the
  JSON-RPC spawn is a NEW additive mode (§5.1).

**What stays working (explicit):** Norn `-f text`, `-f json`, `-f stream-json` (with/without
`--partial`), `--output`/`-o` redirection, the TUI, `mcp serve`'s existing JSON-RPC stdio
server ([mcp_server.rs](../../../norn/crates/norn/src/integration/mcp_server.rs)), the
`compose_prompt`/`read_stdin_if_piped` semantics for the non-driven formats, and the worker's
one-shot `run_norn_step`.

### 4.1 The `run/*` RESPONSE = the RESULT and only the result

Headless Norn already emits a final structured envelope in `-f json`/`-f text` modes;
the worker already parses child stdout as a JSON envelope and returns `output` on
`result == "completed"` ([norn-fan-worker main.rs:129-168](../../../aion/examples/norn-fan-worker/src/main.rs#L129)).
**Change:** in driven mode, the host issues a `run/execute` REQUEST and the **single final
structured value is the RESPONSE `result`** — the replay-authoritative activity output the
worker captures. Because it is a typed Response (has an `id`), it is unambiguously the result;
the incremental event stream that today would render under `-f stream-json`
([spawn_stream_renderer output.rs:262](../../../norn/crates/norn-cli/src/print/output.rs#L262))
becomes `event/*` NOTIFICATIONS on the same channel, distinguished by kind, never mistaken for
the result.

**The worker's capture rule (PRECISE — the near-STRUCTURAL discriminator, cross-ref §7.1).**
The result is captured **ONLY** as the JSON-RPC Response whose `id` **matches the exact
`run/execute` request `id` the worker sent**. This is not merely "typed differently" — it is
an id-match against a specific outstanding request:
- A message with **no `id`** (a Notification — every `event/*` is one) is **NEVER** a candidate
  for the result; the worker's demux routes it to the transcript sink and it can never reach the
  result slot.
- A Response with a **non-matching `id`** (e.g. an ack for an `intervene/*` request, or a stray
  correlation) is **rejected/logged, never captured** as the result.
- Only the Response carrying the `run/execute` `id` fills the result slot, exactly once.

This id-matching is what makes the result/event split near-structural rather than a naming
convention: mixing them requires not just a wrong `method` but a forged or absent `id`. It
replaces the former fd1/fd3 byte disjointness and MUST be enforced/tested (NOI-1): a bug that
emitted the result as a Notification, or an event as a Response, or captured a non-`run/execute`
Response as the result, would silently violate replay authority — so the negative control
asserts the result arrives ONLY as the id-matched `run/execute` Response (both directions,
§9.1 NOI-1).

### 4.2 `event/*` NOTIFICATIONS = the event stream (NEW emitter)

- In the driven-mode loop, subscribe a **second receiver** off the existing
  `broadcast::channel::<AgentEvent>(N)` ([orchestrator.rs:371](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L371))
  — the broadcast fan-out means the notification emitter composes with the run without
  interference. For each `AgentEvent`, wrap the existing per-line payload as a JSON-RPC
  notification: `{"jsonrpc":"2.0","method":"event/...","params":<payload>}`.
- Reuse `agent_event_to_ndjson` ([output.rs:317](../../../norn/crates/norn-cli/src/print/output.rs#L317))
  to produce the `params` **byte-identically**, deriving `method` from the same
  `AgentEventKind` match arm (`event/message`, `event/toolCall`, `event/toolResult`,
  `event/progress`, `event/stop`, `event/delta` (ephemeral), `event/raw`), and **ADD
  `agent_id`/`agent_role` to every notification's params** — the current translator DROPS
  them ([risk §9](#9-open-decisions--honest-risks)), which is fine for single-agent stdout
  but makes multi-agent events unattributable.
- **Ordering:** JSON-RPC gives no cross-notification ordering guarantee, so each `event/*`
  notification carries `worker_seq` in its params; the server sequencer re-stamps the
  authoritative `store_seq` at commit (§5.3).
- **Single writer on stdout (FIRST-CLASS REQUIREMENT, tested).** Because the channel is
  bidirectional-push (§4.3), the child's stdout is shared by *three* outbound frame sources:
  `run/*` responses, `intervene/*` acks, and `event/*` notifications — and `approval/request`
  requests once that lands, all child→host. Interleaved concurrent writes would corrupt
  JSON-RPC (newline) framing. The child (Norn adapter) therefore **MUST serialize ALL outbound
  frames through a single writer** — a mutex-guarded writer or a single dedicated writer task,
  **exactly as Norn's existing `mcp_server` serve loop already does**
  ([mcp_server.rs::serve_stdio:206-227](../../../norn/crates/norn/src/integration/mcp_server.rs#L206)).
  Do NOT also spawn the legacy `stdout().lock()`-per-event stream renderer in driven mode.
  A named negative control (NOI-1/NOI-2) drives a **burst of `event/*` notifications
  concurrently WHILE an approval/request (or `intervene/*` ack) is emitted** and asserts every
  emitted line is a complete, parseable JSON-RPC frame — no interleave corruption. **The same
  single-writer discipline applies host-side** for host→child requests
  (`initialize`/`run/execute`/`intervene/*`) sharing the child's `ChildStdin`.
- **Buffer/loss:** the broadcast channel is lossy under lag
  (`RecvError::Lagged`, [output.rs:284](../../../norn/crates/norn-cli/src/print/output.rs#L284)),
  and the 256 buffer ([orchestrator.rs:70](../../../norn/crates/norn/src/... "orchestrator buffer"))
  is tuned for a transient stdout renderer. The notification sink either needs a larger buffer
  or a non-broadcast tee. Because the server sequences and the keyspace is the durable
  store, **a dropped notification is a gap in the transcript, not a correctness bug in
  workflow state** — but it is a visible transcript hole, so the buffer must be sized
  generously and lag surfaced as an `event/raw`/gap marker.
- **Shutdown discipline (REQUIRED):** the `SharedAgentEventChannel` keeps an owned
  `Sender` clone so the channel never closes on its own
  ([wiring.rs:290](../../../norn/crates/norn-cli/src/runtime/wiring.rs#L290), REVIEW C1);
  the notification sink MUST use the explicit `finish()`/shutdown handshake
  ([output.rs:234](../../../norn/crates/norn-cli/src/print/output.rs#L234)) or it hangs
  forever awaiting closure. In JSON-RPC terms, the run's terminal `run/execute` Response is
  the natural close signal.
- **TUI parity is out of scope** for the headless observability path — the TUI creates
  its own broadcast channel ([driver.rs:219](../../../norn/crates/norn-cli/src/tui/driver.rs#L219));
  the `event/*` emitter is a headless driven-mode-only emitter (§9 open decision).

### 4.3 `intervene/*` / `approval/*` REQUESTS = the control channel (NEW loop)

In driven mode the prompt comes from `run/execute` params (or positional args / `--prompt-file`),
so stdin is **free to be the JSON-RPC read half**, bypassing `read_stdin_if_piped`
([orchestrator.rs:194-201](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L194)) which
otherwise slurps stdin to EOF before the turn. A dedicated tokio reader task reads
newline-delimited JSON-RPC messages (the exact framing `serve_stdio` already ships,
[mcp_server.rs:206-227](../../../norn/crates/norn/src/integration/mcp_server.rs#L206)) and
dispatches by method:

- **`intervene/*` REQUESTS** carry a **neutral `InterventionCommand`** in `params` and the
  Norn adapter (§3.4) maps the supported ones onto Norn's native control channel, **returning
  an ack RESPONSE** (this upgrades the former best-effort/no-op-on-miss stdin write to an
  **acknowledged request**; a finished/gone child cleanly errors the request):
  - `intervene/injectMessage` → builds a `ChannelMessage` ([inbound.rs:72](../../../norn/crates/norn/src/loop/inbound.rs#L72))
    and sends on the root's registered `InboundSender`
    ([wiring.rs:211](../../../norn/crates/norn-cli/src/runtime/wiring.rs#L211) registers the
    root route). `priority: Interrupt` takes Norn's steer path; `priority: Normal` a queued
    `Update`. The frame-message security contract
    ([inbound.rs:125-148](../../../norn/crates/norn/src/loop/inbound.rs#L125)) must be
    preserved so an external injection cannot forge agent identity — the operator source
    is attributed as an operator, not as a peer agent.
  - `intervene/cancel` → trips a real `CancellationToken` threaded into `AgentStepRequest.cancel`
    ([runner.rs:163](../../../norn/crates/norn/src/loop/runner.rs#L163)) — today headless
    passes `cancel: None` ([orchestrator.rs:399](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L399)),
    so this is net-new wiring. This is the agent-run cancel (§7.5), not a workflow-visible cancel.
  - `intervene/pauseResume`, `intervene/updateBudget` → **not advertised by the Norn adapter**,
    so under the LOCKED gate (§5.0 / §6.4 / §9.2 decision 14) the **server never sends them** —
    the console shows no control and the command is refused as "not supported" at the server on
    the advertised set. `-32601 Method not found` is reserved for the degenerate protocol-bug
    case where the server sends an unadvertised method anyway and the child rejects it; it is
    DISTINCT from the attempt-superseded no-op (§6.4), which is an application-range code
    (`-32001` = "this attempt is gone/superseded"). Three distinct classes: not supported
    (server gate) / too late (attempt no-op) / protocol bug (child `-32601`).
- **`approval/*` REQUESTS are agent-initiated** — the agent raises a pending tool-use /
  permission gate as an `approval/request` REQUEST toward the host (correlated by `id`); the
  host routes it to the ops-console; the console answers via `approval/respond` (the neutral
  `RespondToApproval` primitive). This makes the channel **bidirectional JSON-RPC** (both peers
  issue requests, like LSP): the child is the server for `run/*` and `intervene/*` but the
  client for `approval/request`, so the two directions use disjoint `id` spaces. The Norn
  adapter advertises `respond_to_approval` UNSUPPORTED today (no cited permission-gate surface),
  so it emits no `approval/request` yet.
- The step runs under `tokio::select!` against the reader task.

**Single-run vs driven loop.** Headless runs ONE `run_agent_step` then exits
([orchestrator.rs:383](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L383)).
A single `run/execute` REQUEST → one `run_agent_step` → its terminal RESPONSE is sufficient
for the complete intervention model: a single long activity attempt is one step under
`select!`, with `intervene/*` landing at tool boundaries. A multi-turn driven daemon (many
`run/execute` per process) is NOT required by this design and is explicitly out of scope.

### 4.4 Session ↔ (workflow, activity, attempt)

Norn's `SessionManager::open_or_resume` is the idempotent retry-safe primitive whose
docs already cite "workflow run + activity key"
([manager.rs:368](../../../norn/crates/norn/src/session/manager.rs#L368),
[:24-36](../../../norn/crates/norn/src/session/manager.rs#L24)). The worker maps
`(workflow, activity, attempt)` → a sanitized `--session-id` (validation rejects
path-capable ids, `[A-Za-z0-9._-]`,
[manager.rs:532](../../../norn/crates/norn/src/session/manager.rs#L532)).

**LINCHPIN (LOCKED): session-id = f(workflow, activity, ATTEMPT); resume-same-session
applies to WITHIN-attempt failover only; a retry is a new attempt and therefore a new
session.** The `attempt` component is NOT optional — it is what makes resume and retry
mean different things, and it is why §7's "retry re-runs fresh" and this section's
"resume the same session" do not contradict. Norn's shipped resume model is literal:
`open_or_resume` continues the predecessor's event history
([manager.rs:24-36](../../../norn/crates/norn/src/session/manager.rs#L24)), and a
persisted `UserMessage`/injected intervention frame is **replayed verbatim on resume**
([inbound.rs:125-148](../../../norn/crates/norn/src/loop/inbound.rs#L125)). So resuming
a session id genuinely re-injects that attempt's prior interventions — which is correct
in one case and forbidden in the other, and the `attempt`-keyed id is exactly what keeps
the two cases apart:

- **Consequence A — crash-failover of the SAME attempt.** Attempt N is still logically
  in-flight, its owning worker died, and a new worker adopts it. The adopter resumes the
  **same** session id (same `attempt`), so Norn resumes and **correctly keeps this
  attempt's prior interventions** — an operator's mid-attempt steer must survive a worker
  crash. This is exactly what `open_or_resume`'s idempotency buys, scoped to
  **within-attempt** failover. The event stream continues under the same key; `store_seq`
  stays monotonic (server-allocated, §5).
- **Consequence B — retry is a NEW attempt N+1.** A retry has a different `attempt`,
  therefore a **different session id**, therefore a genuinely fresh Norn session with **no
  prior intervention frames replayed**. This is precisely what §7 means by "fresh."

Intervention state is thus attempt-scoped: it survives within-attempt failover (same
session) and does NOT carry across a retry (new attempt → new session, §7).

---

## 5. The event flow — worker → liminal → server → both

```
Norn (event/* JSON-RPC notifications, native shapes)
   │
   ▼
aion-worker adapter          maps native → ActivityEvent envelope; stamps emitted_at + worker_seq
   │  (liminal Channel Publish, one topic per (workflow,activity,attempt) OR seq'd envelopes)
   ▼
liminal server (fan-out)
   │
   ▼
aion-server BRIDGE           SEQUENCER: stamps store_seq at commit; then BOTH:
   ├──► dashboard WebSocket   (live; ephemeral Deltas forwarded, never persisted)
   └──► haematite 'O' keyspace (append-only, per (workflow,activity,attempt), NOT the E replay log)
```

### 5.0 The `initialize` capability handshake (LSP-style, LOCKED)

Before any run, the host and child perform an **LSP-style `initialize` handshake** over the
JSON-RPC channel. The host sends `initialize` (host→child REQUEST) carrying the protocol
version + run identity (`workflow_id`, `activity_id`, `attempt`); the child RESPONDS with its
**capabilities**, advertising **which of the five neutral primitives it supports** (its
`interventions` set) and which `event/*` arms it emits. This response IS the source of the
capability set — the Norn adapter advertises `{inject_message, cancel}` and omits
`{pause_resume, update_budget, respond_to_approval}`.

**Capability-gating is enforced at the SERVER on the advertised set (LOCKED — does NOT rely on
`-32601`).** The authoritative gate is the `initialize`-advertised capability set (forwarded via
`RegisterWorker`, §5.1): the server/console offer only the advertised primitives and the server
**NEVER sends the child a primitive it did not advertise**. `-32601 Method not found` is the
idiomatic JSON-RPC code for "method not implemented by this peer," but here it is a *defence*,
not the gate: a `-32601` arriving *from the child* means the server sent an unadvertised method
— a **protocol bug**, logged/surfaced, not normal capability-gating (§6.4). This keeps three
outcomes unambiguous (§6.4): **not supported** (server-side capability gate), **too late / wrong
attempt** (the §6.4 application-range no-op, e.g. `-32001`), and **protocol bug** (a `-32601`
from the child) are three distinct classes.

**The worker forwards the capability set to the server in `RegisterWorker`.** The child's
`initialize` capabilities are read by the worker adapter and carried into registration via a
new `intervention_capabilities` field (§5.1); the server and ops-console gate purely on that
forwarded set, never on harness identity.

### 5.1 Worker adapter (aion-worker + norn-fan-worker)

The agent process is spawned **inside a user handler** today
([norn-fan-worker main.rs:86-169](../../../aion/examples/norn-fan-worker/src/main.rs#L86)),
using blocking `.output()` with `Stdio::null` stdin. The runtime never touches child
stdio. **The JSON-RPC duplex client must be introduced at the process-spawn boundary,
and it MUST be a shared `aion-worker` helper — not per-handler** (§9 open decision,
strongly leaned): otherwise every handler re-implements observability and the
harness-agnostic path has no home.

Concrete worker changes:
- **`spawn_agent` helper** in `aion-worker` — a **NEW additive spawn mode** beside the current
  one-shot `.output()` capture — that, when the JSON-RPC/driven capability is enabled: spawns
  the child with `--protocol jsonrpc`, `Stdio::piped()` on stdin (retaining `ChildStdin` to
  write `initialize`/`run/execute`/`intervene/*` REQUESTS) and stdout piped as the framed
  JSON-RPC duplex read half; switches from blocking `.output()` to a streaming `spawn()` with
  concurrent async readers demuxing `event/*` notifications live and correlating the terminal
  `run/execute` Response while the child runs (today's single `tokio::time::timeout` at
  [main.rs:109](../../../aion/examples/norn-fan-worker/src/main.rs#L109) becomes a select over
  the reader + timeout + cancellation). The final `run/execute` Response `result` is handed to
  the engine as the SAME `DispatchOutcome::Completed { output }` the one-shot capture produces
  today — only the *source* of the result bytes changes (framed Response vs post-exit stdout
  buffer). A handler that does NOT spawn an agent is unaffected (the helper is opt-in).
- **`ActivityContext` gains two fields** mirroring the existing outbound `heartbeat_sender`
  ([context.rs:12-19](../../../aion/crates/aion-worker/src/context.rs#L12)): an
  `event_sender` (handler → runtime → liminal) and a `control_receiver` (runtime →
  handler → child stdin). `ActivityContext::for_workflow`
  ([context.rs:111-131](../../../aion/crates/aion-worker/src/context.rs#L111)) and
  `spawn_activity` ([loop_.rs:437-442](../../../aion/crates/aion-worker/src/runtime/loop_.rs#L437))
  populate them; a new select arm in `serve_activity_tasks_until`
  ([loop_.rs:157-287](../../../aion/crates/aion-worker/src/runtime/loop_.rs#L157)) drains
  events, exactly as the heartbeat arm does ([loop_.rs:214-219](../../../aion/crates/aion-worker/src/runtime/loop_.rs#L214)).
- **Transport for events OUT: liminal Channel Publish, out-of-band from the worker gRPC
  stream** (§9 open decision, leaned). The norn-fan-worker header comment
  ([main.rs:20-23](../../../aion/examples/norn-fan-worker/src/main.rs#L20)) and the
  observability-as-separate-subsystem memory both say observability is a distinct
  haematite-durable subsystem → route the demuxed `event/*` notifications straight to a liminal
  events channel, **zero worker-stream / proto changes for events**. The worker already holds a liminal
  connection (`serve_with_redial`, [main.rs:290-305](../../../aion/examples/norn-fan-worker/src/main.rs#L290));
  reuse it (confirm in the spike whether the event/control transport reuses that
  connection or opens its own — §9).
- **Capability advertisement:** the worker reads the child's `initialize` capabilities (§5.0)
  and **forwards** them at registration — the Norn adapter advertises `{inject_message, cancel}`
  and marks `{pause_resume, update_budget, respond_to_approval}` unsupported (§3.4) until the
  mechanisms exist. Today `RegisterWorker` has a fixed 4-field wire shape (`namespaces,
  activity_types, task_queue, node`,
  [worker.proto:94-109](../../../aion/crates/aion-proto-generated/proto/worker.proto#L94)) —
  add an `intervention_capabilities` field carrying the supported-primitive set (proto
  change, coordinated with the server; `aion-proto-generated` is generated). Any harness whose
  adapter implements a primitive is **first-class** for it; a harness that cannot take control
  commands advertises an empty set and the server/console never offers intervention for it.
  The server and console gate purely on **which of the five neutral primitives are in the
  forwarded set** and never on harness identity.

### 5.2 liminal transport

Events OUT ride the **CHANNEL Publish** primitive
([Frame::Publish frame.rs:303](../../../liminal/crates/liminal/src/protocol/frame.rs#L303),
[services.rs:529](../../../liminal/crates/liminal-server/src/server/connection/services.rs#L529)) —
worker publishes to a well-known events channel (or one topic per attempt), server fans
out to observer subscribers. It exists and needs little protocol change. **Two gaps to
close** (both liminal-side):
- **Ordering:** fan-out is FIFO within one channel actor
  ([supervisor.rs:66](../../../liminal/crates/liminal/src/channel/supervisor.rs#L66)) but
  there is no cross-channel/global order and no envelope sequence number. → use **one
  channel per `(workflow,activity,attempt)`** (natural per-attempt ordering) OR carry
  `worker_seq` in the envelope. We already carry `worker_seq`; the server re-stamps
  `store_seq` at commit as the authority, so liminal ordering only needs to be
  best-effort — which it is. **No liminal ordering change strictly required.**
- **Backpressure:** the subscriber inbox is an unbounded `VecDeque`
  ([subscription.rs:33](../../../liminal/crates/liminal/src/channel/subscription.rs#L33))
  and `publish()` does NOT consult the pressure module
  ([pressure/mod.rs](../../../liminal/crates/liminal/src/pressure/mod.rs)). A high event
  rate to a slow observer grows memory unbounded. Wiring `PressureEnforcer` into the
  publish path is real work (§9 risk); the pipeline lands a bounded-channel cap that
  drops-to-`Raw`-gap under pressure first, with full `PressureEnforcer` wiring a later slice
  in the same pipeline.

Commands IN ride the **PUSH** primitive (LSUB-0,
[ConnectionSupervisor::push_to_connection supervisor.rs:177](../../../liminal/crates/liminal-server/src/server/connection/supervisor.rs#L177),
[Frame::new_push frame.rs:495](../../../liminal/crates/liminal/src/protocol/frame.rs#L495)) —
server-originated, addressed to one connection, correlated reply. This is purpose-built
for a targeted server→worker command. **Gaps:** (a) `push_to_connection` addresses a
beamr PID, not an activity-attempt → need an **activity-attempt → owning-connection
index** (§6.3); (b) Push payload is opaque `Vec<u8>` and `PushClient` only exposes
`recv_timeout`/`reply` ([push_client.rs:183](../../../liminal/crates/liminal-sdk/src/remote/tcp/push_client.rs#L183)) →
add a command taxonomy in the payload + SDK dispatch; (c) Push uses a hardcoded
`PUSH_STREAM_ID=1` ([process.rs:28](../../../liminal/crates/liminal-server/src/server/connection/process.rs#L28))
so concurrent commands to one worker serialize — acceptable for low-rate interventions,
noted as a scaling limit.

### 5.3 aion-server bridge — the SEQUENCER

A per-activity transcript stream slots in as a **fourth `SubscriptionRequest` variant**
alongside `per_workflow / filtered / firehose / cluster`. Decode: add a case in
`ws_subscription::decode_subscription_value`
([ws_subscription.rs:87-114](../../../aion/crates/aion-server/src/api/ws_subscription.rs#L87))
+ a proto variant. Dispatch: add an arm in `serve_subscription_socket`
([events.rs:52](../../../aion/crates/aion-server/src/api/http/events.rs#L52)) next to the
`Cluster` arm.

**The transcript stream does NOT ride the engine broadcast** (that carries `Event`s /
workflow history). Instead the bridge is a **new publisher modeled on `ClusterEventPublisher`**
([cluster_publisher.rs:46](../../../aion/crates/aion-server/src/cluster_publisher.rs#L46)) —
a `broadcast::Sender<ActivityEvent>` for the live WS tail + a durable append to the `O`
keyspace, with the same gap-free splice + typed-lagged terminal-frame contract the
existing streams use ([cluster_stream.rs:46](../../../aion/crates/aion-server/src/stream/cluster_stream.rs#L46),
[resume::splice](../../../aion/crates/aion-server/src/stream/resume.rs)).

**`store_seq` MUST be commit-allocated, not process-allocated.** The `ClusterEventPublisher`
stamper is a per-process `AtomicU64` ([cluster_publisher.rs:61](../../../aion/crates/aion-server/src/cluster_publisher.rs#L61))
that resets on restart/failover — copying it naively would make two survivors produce
colliding/non-monotonic sequences (§9 risk). Instead `store_seq` is allocated by the
haematite append inside the same atomic commit as the event, exactly how
`EventEnvelope.seq` is store-allocated ([event_store.rs:132-157](../../../haematite/crates/haematite/src/api/event_store.rs#L132)). The
dashboard resume cursor is this `store_seq`.

**Precise mechanism (honest — the store does NOT auto-allocate a collision-proof id).**
Neither haematite `append_batch` nor aion `WritableEventStore::append` mints a global id
for you: both take a **caller-supplied `expected_seq`** (optimistic concurrency —
they return the `next_seq` on success or a `SequenceConflict` on a stale expectation,
[event_store.rs:132-157](../../../haematite/crates/haematite/src/api/event_store.rs#L132)).
So "commit-allocated" here means: **monotonicity for the `O`-stream depends on the SERVER
being the single writer to the `O` keyspace AND on the server serializing writes and
handling/retrying `SequenceConflict`** — not on any magic in the store. **The
read-head → append(expected_seq) → on-`SequenceConflict`-re-read-head-and-retry loop is
CORRECTNESS-CRITICAL code, NOT an implementation detail:** it is the only thing that keeps
`store_seq` monotonic when two appends race for the same `(activity, attempt)` head (one wins,
the other must re-read the advanced head and retry, still landing monotonically). This is
exactly why the server owns durability (below) and why NOI-4's negative control must cover
**both** the wrong-allocator case AND the concurrent-writer-retry case (below). The wrong
approach — a process-local `AtomicU64` à la `ClusterEventPublisher` — is called out above
precisely because it looks like it allocates but silently resets on failover; it is the WRONG
pattern regardless of retry.

**Failover double-emit.** A dying worker AND an adopting worker can both emit for one
`(workflow, activity, attempt)`. The keyspace keys on the attempt and **dedupes** so two
emitters collapse; `store_seq` is commit-allocated so the surviving/adopting writer's
ordering is authoritative regardless of who emitted first. **BLOCKED on NOI-0:** this
dedupe is only well-defined once `attempt` is a durable field on
`ActivityStarted/Completed/Cancelled` (today only `ActivityFailed` carries it,
[event.rs:218-253](../../../aion/crates/aion-core/src/event.rs#L218)) — see the §9 NOI-0
prerequisite gate. This mirrors the
shard-adoption fence discipline the store forwards
([instrumented_store.rs:357-380](../../../aion/crates/aion-server/src/observability/instrumented_store.rs#L357), #157).

**Scope guard (LOCKED, and a real leak hazard):** the transcript is **namespace-scoped**
data → it MUST route through the `NamespaceEventGate`
([stream/namespace_filter.rs](../../../aion/crates/aion-server/src/stream/mod.rs)), NOT
the deploy gate. The cluster stream deliberately leaks topology across tenants to
deploy-granted callers ([cluster_stream.rs:9-21](../../../aion/crates/aion-server/src/stream/cluster_stream.rs#L9));
using that gate for a transcript would be a cross-tenant leak.

**Server owns haematite durability.** Remote/unknown workers hold NO haematite creds —
they publish to liminal; only the server writes the `O` keyspace. This is the LOCKED
"server owns durability" decision and it also means the failover-dedupe + commit-seq
logic lives in exactly one trusted place.

---

## 6. The intervention flow — operator → server → worker → adapter → child JSON-RPC request

**Everything down to the worker adapter speaks ONLY the neutral primitives
(`InjectMessage`, `Cancel`, `PauseResume`, `UpdateBudget`, `RespondToApproval`) and is
harness-blind (§3.4).** The Norn-specific translation
happens in exactly one place — the worker-side adapter — and is described in §3.4. This
section describes the same boundary at the flow level. At the child boundary each command is a
JSON-RPC **`intervene/*` REQUEST that returns an ack** (upgraded from the former best-effort
stdin write that no-op'd on a miss): a live child acks, and a finished/gone child cleanly
**errors** the request.

```
ops-console / API
   │  POST (namespace-scoped, see §6.6)  OR  WS command frame        [NEUTRAL commands]
   ▼
aion-server                 resolve CURRENT owner of (workflow,activity,attempt)
   │  liminal PUSH to the owning worker's connection                 [NEUTRAL commands]
   ▼
aion-worker                 route by attempt -> in-flight handle -> JSON-RPC control half
   │  per-harness ADAPTER translates neutral -> native (§3.4)        [adapter boundary]
   ▼
Norn adapter -> intervene/injectMessage(Interrupt)->steer path; intervene/injectMessage(Normal)
                ->queued ChannelMessage/Update; intervene/cancel->CancellationToken   -> ack RESPONSE
```

### 6.1 Best-effort / live-only (LOCKED)

Command delivery is **best-effort, live-only** — inherently real-time, NOT durably
queued or retried. As a JSON-RPC `intervene/*` REQUEST it now returns an **ack** on a live
child; commands to a finished/migrated activity are **no-ops** that cleanly error the request
(§6.4) rather than silently vanishing. This is a deliberate, defensible asymmetry: the *event*
stream is the durable artifact; a *command* is a human real-time act. Durably queuing a steer
would mean re-delivering it on a retry — which is exactly the replay-contamination §7 forbids.

### 6.2 Server routing

Today dispatch is strictly server→worker one-directional (`WorkerMessage` has only
`ActivityTask` + `DrainRequest`,
[worker/registry.rs:58](../../../aion/crates/aion-server/src/worker/registry.rs#L58)). The
registry resolves workers by `(namespace, task_queue, node, activity_type)` POOL, NOT by
a specific in-flight attempt ([round-robin cursor registry.rs:224](../../../aion/crates/aion-server/src/worker/registry.rs#L224)).
**New: an `attempt → owning-worker` back-index.** In the failover case the target may be
dead/adopted, so the router MUST resolve the CURRENT owner via durable shard-owner state
(`publish_shard_owner` / `is_current_owner` forwarded at
[instrumented_store.rs:357-380](../../../aion/crates/aion-server/src/observability/instrumented_store.rs#L357)),
NOT a stale in-memory registry entry ([lost_worker_error dispatch.rs:495](../../../aion/crates/aion-server/src/worker/dispatch.rs#L495)).

The command reaches the worker via **liminal PUSH** (§5.2), NOT the gRPC worker stream —
this keeps the intervention path on the same out-of-band transport as events and avoids a
`WorkerMessage`/proto change. (Alternative: a new `ServerToWorker` oneof + `WorkerSessionEvent`
variant mirroring the existing `Cancel` template
[session.rs:45-56](../../../aion/crates/aion-worker/src/protocol/session.rs#L45) — kept as
a fallback if PUSH addressing proves awkward; §9 open decision.)

### 6.3 Worker delivery

The Cancel machinery is the proven precedent: `WorkerSessionEvent::Cancel` routes via
`deliver_cancellation` to an `ActivityCancellationHandle` stored in `InFlightActivity`
([loop_.rs:469-482](../../../aion/crates/aion-worker/src/runtime/loop_.rs#L469),
[context.rs:133-141](../../../aion/crates/aion-worker/src/context.rs#L133)). A control
command follows the same shape but carries *data*, delivered to the handler via the new
`control_receiver` on `ActivityContext` (§5.1), which the handler writes to the child's
`ChildStdin` as an `intervene/*` JSON-RPC REQUEST and awaits the child's ack RESPONSE (the
ack surfaces back to the operator as the honest delivery confirmation, §6.4).

### 6.4 Attempt-scoped no-op

Every command carries `attempt`. If the server's back-index shows the attempt is finished,
migrated to a *different* attempt number, or unknown, the command is a **no-op** with an
honest NACK to the caller. This is what makes "commands to a finished/migrated activity
are no-ops" concrete — the `attempt` field is the guard.

**THREE distinct, unambiguous outcome classes (LOCKED — the `-32601` overload is resolved):**
capability-gating does **NOT rely on `-32601` at the server at all**. The server gates on the
**`initialize`-advertised capability set** (forwarded via `RegisterWorker`, §5.0/§5.1) and
**NEVER sends the child a primitive the child did not advertise** — so a `-32601` observed
*from the child* is a **real protocol bug** (the server sent an unadvertised method, or the
child mis-declared its own methods), logged/surfaced as such, NOT treated as normal
capability-gating. The three outcomes are then distinct, unambiguous codes:
- **Not supported** (capability) → gated at the SERVER on the advertised set; the console never
  offers the control and the server never emits the method. `-32601` is reserved for the
  degenerate case where the child itself rejects a method as unimplemented, and that is a
  **protocol bug**, not routine gating.
- **Too late / wrong attempt** (routing) → the attempt no-op: an **application-range** code
  (`-32000..-32099`, e.g. **`-32001 attempt superseded`**), returned by the server's back-index
  guard when the target attempt is finished/migrated/unknown.
- **Protocol bug** → **`-32601 Method not found`** arriving from the child (the server should
  have gated it), or any malformed frame — surfaced/logged, never silently swallowed.

A finished/gone *child*
that receives an `intervene/*` request while still attached likewise cleanly errors the request
rather than silently dropping it. **BLOCKED on NOI-0:** the guard
can only detect a finished/superseded attempt once `attempt` is durably stamped on the
terminal activity events (`ActivityStarted/Completed/Cancelled`), which today it is not
([event.rs:218-253](../../../aion/crates/aion-core/src/event.rs#L218)) — see the §9 NOI-0
prerequisite gate.

### 6.5 `PauseResume` — a neutral primitive, capability-gated

`PauseResume { paused }` IS a first-class neutral primitive (§3.3): suspend/resume the agent
between steps. It belongs in the neutral set because a plausible non-Norn harness with a
suspendable step loop can expose it. It is **capability-gated**: a harness whose adapter has
no suspend/resume mechanism advertises no support and the console offers no pause control for
it. The **Norn adapter advertises `PauseResume` UNSUPPORTED for now** (§3.4) — Norn has no
cited suspend/resume-between-steps mechanism, so on the Norn adapter a would-be pause is
approximated operationally by an `InjectMessage { priority: Interrupt }` that tells the agent
to stop and await, or `Cancel` + resume (Norn's `CancellationToken` + `open_or_resume`). Under
the LOCKED server-side gate (§5.0 / §6.4 / §9.2 decision 14) the console offers no pause control
for a Norn-owned attempt and the server never sends `intervene/pauseResume`; a child `-32601 Method
not found` would occur only as a protocol bug (server sent an unadvertised method), not as routine
gating, until the mechanism exists. That a neutral
primitive can be defined and gated ahead of any harness implementing it is exactly the point of
the capability contract (§3.4).

### 6.6 Auth (LOCKED)

Intervention is a **privileged mutating action**. When auth is OFF it is
**granted-by-default**, consistent with the deploy-grant model
([ops-console-out-of-box](../../../aion/docs/... "ops console memory")); when ON it is
**permission-gated**. Seam decision: intervention is **namespace-scoped**, mirroring
`/workflows/cancel` + `/workflows/signal` (`NamespaceGuard.scope()` with a
`WorkflowTarget` verifying durable ownership,
[namespace/guard.rs:372](../../../aion/crates/aion-server/src/namespace/guard.rs#L372)), NOT
the deploy-scoped `/cluster/command` seam (deploy-gated, all mutating variants currently
`Unimplemented`, [cluster_command.rs:49-57](../../../aion/crates/aion-server/src/api/http/cluster_command.rs#L49)).
Rationale: intervening in one activity should not require a deployment-wide deploy grant.
A new endpoint `POST /workflows/{id}/activities/{activity_id}/intervene` (namespace grant
via `x-aion-namespaces`) is the clean seam; the command result confirmation arrives back
as an `O`-region observability event on the same WS subscription, closing the loop
socket-first.

---

## 7. THE CRUX — intervention vs replay/retry

### 7.1 The statement (LOCKED)

- The activity **RESULT** (the `run/execute` RESPONSE, captured by the worker as the activity
  output) is the **single replay-authoritative** value. It is the only thing that enters
  workflow history via `WritableEventStore::append`
  ([store.rs:257-313](../../../aion/crates/aion-store/src/store.rs#L257)). Per §4.1, the worker
  captures it **only** as the Response whose `id` matches the `run/execute` request it sent — a
  Notification (no `id`) is never a candidate and a non-matching Response is rejected — so the
  "result vs events" split is an id-matched, near-structural discriminator, not a convention.
- **Interventions are durable OBSERVABILITY events** in the `O` keyspace — auditable,
  visible when replaying the transcript — but are **NOT part of the workflow REPLAY log**.
- On **RETRY**, the activity re-runs **fresh, WITHOUT prior interventions**. Interventions
  are **attempt-scoped**: a retry is a new attempt with a new (empty) intervention set.

**The mechanism that makes "fresh" true (cross-ref §4.4):** session-id = f(workflow,
activity, **attempt**); **resume-same-session applies to within-attempt failover only; a
retry is a new attempt and therefore a new session.** This matters because Norn's
`open_or_resume` continues the predecessor's event history and **replays a persisted
intervention frame verbatim on resume**
([inbound.rs:125-148](../../../norn/crates/norn/src/loop/inbound.rs#L125)). Resuming the
same session id would therefore re-inject that attempt's interventions — which is exactly
what we WANT for a mid-attempt crash-failover (§4.4 Consequence A) and exactly what we
must NOT do on a retry. The `attempt` component of the session id is what keeps those
apart: a retry gets a different `attempt`, a different session id, and hence no replayed
intervention frames. Without `attempt` in the session id, "retry re-runs fresh" would be
FALSE — the two sections would contradict.

### 7.2 Why this is the only coherent choice

An intervention is a **human real-time act**, not deterministic workflow state. Replay's
whole guarantee is determinism: given the same recorded history, the workflow reconstructs
the same state ([read_history → serde_json::<Event> store.rs:1237-1258](../../../aion/crates/aion-store-haematite/src/store.rs#L1237)).
A human steer typed at wall-clock T is not reproducible and not part of that determinism.
If interventions were replay state, replay would either (a) re-apply them (meaningless — the
agent that received them is gone) or (b) need to record their *effect* as authoritative,
which is precisely what the **result** already is. So the result already carries everything
authoritative; the intervention is annotation.

### 7.3 The strongest objection

> "I steered the agent mid-run and it produced result R *because of* my steer. The activity
> then fails a downstream check and Aion retries it. The retry re-runs **without** my steer,
> produces a *different* result R′, and now the workflow diverges from the result I
> intervened to shape. My intervention was silently discarded. That is surprising and wrong."

### 7.4 The answer

The objection conflates two cases. **Resolve on whether the intervened attempt produced an
ACCEPTED result:**

- **Case A — the intervened attempt SUCCEEDED (produced an accepted result R).** Then R is
  already the durable activity output on the `E`-stream. **No retry occurs.** The steer did
  its job: it shaped R, R is authoritative, and it is preserved forever. The transcript
  records "operator steered here → R". There is no divergence because there is no retry.
  This is the *common* case and it is fully correct.

- **Case B — the intervened attempt did NOT produce an accepted result** (it failed, timed
  out, was cancelled, or the worker died). Then by definition the steer did *not* produce a
  durable output — there is nothing authoritative to diverge *from*. The retry re-running
  fresh is the correct, least-surprising behavior: retrying with a stale human steer from a
  *dead* attempt would be re-applying a real-time act out of its moment, to a different agent
  process, possibly on a different node — which is neither reproducible nor what the operator
  meant. The operator, watching live, sees the retry start fresh and can steer again.

The crux is defensible because **an intervention only "counts" when it lands in an accepted
result, and an accepted result is already durable and authoritative.** The steer is never
"silently discarded" in the case that matters (Case A); in Case B there was no authoritative
outcome to discard.

**The external-side-effect objection, answered honestly (do NOT lean only on result
authority).** The argument above is about **workflow-state determinism** — the result is
the single replay-authoritative value. That is only half the answer, and it is not the
half that addresses external side effects. Here is the other half: agent activities are
**ALREADY at-least-once and nondeterministic for EXTERNAL side effects**. An ordinary
un-steered agent that sends an email, writes a file, or calls an API and then fails and
retries already re-does (or partially re-does) those effects — every failure path in the
worker returns `ActivityFailure::retryable` (verified: `norn-fan-worker` returns a
retryable failure on every failure path), so retry-and-redo is the baseline contract, not
a new hazard. A **steered** agent that does the same is therefore **no worse than any
ordinary agent activity**: intervention adds **NO NEW exactly-once hazard for external
effects**. So there are two distinct, honest claims and we state both — result-authority
(workflow-state determinism) AND at-least-once external effects (intervention introduces
no new external-effect hazard because agent activities were never exactly-once to begin
with). Neither claim is doing the other's job.

### 7.5 The residual sharp edge — state-affecting interventions (named, not papered over)

There is one class the clean story does NOT fully cover: an intervention whose **effect must
change deterministic workflow state** — e.g. injecting a *signal* the workflow must observe
on replay, or a forced cancel the workflow logic branches on. For those, the `O`-region record
is **insufficient**, because replay never scans `O` and could not decode it as an `Event`
even if it did (different region tag, different schema — the very guarantee in §0.6). Such an
intervention MUST ALSO be recorded as a first-class `Event` on the `E`-stream through the
normal append path; the `O`-region copy is then a mirror/annotation, not the authority.

**Mitigation (and honest limit):** we split interventions by semantics at design time:
- **Observational/operational** (`InjectMessage` into the agent, the agent-run `Cancel`,
  `PauseResume`, `UpdateBudget`, `RespondToApproval`, transcript annotations) → `O`-region
  ONLY. These are the neutral kinds. They perturb the *agent's* behavior within an attempt but
  never the *workflow's* deterministic state, so the crux holds cleanly. The neutral
  `Cancel { reason }` here is the AGENT-RUN cancel (stop this agent subprocess's current run),
  DISTINCT from a workflow-visible cancel; likewise `RespondToApproval { decision: Deny }` is
  an agent-run control act (it declines the agent's proposed next action) and does NOT write
  workflow replay state — a workflow-visible cancel still goes through the `E`-stream engine
  path.
- **State-affecting** (a signal, a workflow-visible cancel) → these are NOT modeled as
  agent-stdin interventions at all. They already have first-class engine paths
  (`/workflows/signal`, `/workflows/cancel`, [workflows.rs:57](../../../aion/crates/aion-server/src/api/http/... "workflows signal/cancel"))
  that go through `E`-stream append and are correctly replay-authoritative. **We deliberately
  do NOT let any of the neutral agent-run primitives (agent-stdin `InjectMessage`, agent-run
  `Cancel`, `PauseResume`, `UpdateBudget`, `RespondToApproval`) mutate workflow state.** If a future kind needs
  to, it must be a first-class `Event` variant in `aion-core`, reviewed as replay state — a
  cross-crate change (the §3.1 physical-separation rationale), not a quiet addition.

This is the one place the locked model has a residual boundary. It is **not** an unclosed
correctness hole — it is a deliberately drawn line: the neutral interventions are
agent-behavioral and attempt-scoped; workflow-state changes stay on the existing engine
paths. The line must be
**enforced in code** (the `InterventionKind` enum has no state-affecting variant) and **stated
in the operator UI** so an operator never believes an agent-steer is durable workflow state.

---

## 8. Ops-console panels

Two panels, both built on proven substrates
([workflow-detail view](../../../aion/apps/aion-ops-console/src/features/workflow-detail/swimlane/WorkflowDetailView.tsx),
[namespace registry hook](../../../aion/apps/aion-ops-console/src/features/namespace/hooks/useNamespaceRegistry.ts)).

### 8.1 TranscriptPanel (observability)

- **Mount point:** the existing per-activity `ActivityGroup`
  ([ActivityGroup.tsx](../../../aion/apps/aion-ops-console/src/features/workflow-detail/components/ActivityGroup.tsx))
  / slide-out `DetailPanel`
  ([DetailPanel.tsx](../../../aion/apps/aion-ops-console/src/features/workflow-detail/components/DetailPanel.tsx))
  already receive the selected activity entry + its correlated events. TranscriptPanel opens
  from there.
- **Subscription:** a new transcript filter kind (activity-scoped, carrying `activity_id` +
  `attempt`) added to `buildSubscriptionRequest`
  ([websocket-protocol.ts:44](../../../aion/apps/aion-ops-console/src/lib/api/websocket-protocol.ts#L44)),
  resuming by **`store_seq`** — a **distinct cursor axis** from the per-workflow
  `EventEnvelope.seq` ([index.ts:144](../../../aion/apps/aion-ops-console/src/types/generated/index.ts#L144)),
  because transcript events have their own sequence space. Reuse `ResyncContext` full-refetch
  on gap ([websocket-protocol.ts:108](../../../aion/apps/aion-ops-console/src/lib/api/websocket-protocol.ts#L108)).
- **Delta coalescing:** streaming token `Delta`s (`ephemeral`) must fold into one logical
  `Message` by `(activity_id, message_id)` via a latest-wins reducer analogous to
  `applyQuotaDelta`/`applyPlacementDelta`
  ([useNamespaceRegistry.ts:156](../../../aion/apps/aion-ops-console/src/features/namespace/hooks/useNamespaceRegistry.ts#L156)) —
  the current idempotent seq-keyed merge (`mergeEventsBySequence`) would duplicate/flicker on
  partial deltas (§9 risk). Ephemeral deltas render live but are dropped from the persisted
  replay (they are never in the `O` keyspace).
- **Render model:** a new `TranscriptPanel` rendering `Message`/`ToolCall`/`ToolResult`/`Stop`
  rows (compare `ActivityGroup` / `PayloadView`), with a `Raw` fallback row.

### 8.2 InterventionControls

- **Mount:** in/next to `TranscriptPanel`, gated on a runtime capability. When auth OFF,
  granted-by-default (like `Capabilities.deployGranted`,
  [client.ts:127](../../../aion/apps/aion-ops-console/src/lib/api/client.ts#L127), but a
  namespace-`intervene` grant per §6.6); when ON, permission-gated. The panel surfaces **all
  five neutral controls** — inject-message (steer / queued), cancel, pause-resume,
  update-budget, and respond-to-approval — but **each control is shown/enabled ONLY when the
  owning worker's advertised capability set includes that primitive** (§5.1); a primitive the
  worker did not advertise renders as a disabled/absent control, never a live-but-failing one.
  A non-interveneable harness (empty advertised set) shows the transcript with NO controls.
  Because the Norn adapter today advertises only `{inject_message, cancel}` (§3.4), a
  Norn-owned attempt shows those two enabled and pause-resume / update-budget /
  respond-to-approval disabled — the console gates on the advertised set, never on harness
  identity.
- **Action:** a mutation hook mirroring `useStartWorkflow`/`useDeployPackage` posting to the
  namespace-scoped intervene endpoint (§6.6), surfacing honest loading/confirmed/error state,
  showing success ONLY on server ACK. The confirmation arrives as an `O`-region event on the
  live subscription.
- **Honesty rule:** the UI MUST label these as agent-behavioral, attempt-scoped acts — NOT
  durable workflow state (§7.5). A steer must never be presented as "this will persist across
  a retry."

---

## 9. Spike-first slice pipeline + open decisions + honest risks

Smallest-first, full clippy/biome bar, no shims, each slice with a **falsifiable negative
control** (the `engine/fence.rs` discipline,
[fence.rs:129](../../../aion/crates/aion/src/engine/fence.rs#L129)).

### 9.1 Pipeline (explicit gates)

- **NOI-0 (aion-core) — DURABLE ATTEMPT IDENTITY (FOUNDATIONAL PREREQUISITE — nothing
  else can be built until this lands).** The entire design keys on
  `(workflow, activity, ATTEMPT)`: the `O`-keyspace key layout (§9.2 decision 10), the
  §5.3 failover dedupe, the §6.4 intervention attempt-guard no-op, and the §4.4 session-id
  mapping (session-id = f(workflow, activity, attempt)). But **today only `ActivityFailed`
  carries `attempt: u32`; `ActivityStarted`, `ActivityCompleted`, and `ActivityCancelled`
  carry NO attempt** (verified [event.rs:218-253](../../../aion/crates/aion-core/src/event.rs#L218)).
  So the dedupe/guard/session keys are ambiguous for every non-failure event — the design
  cannot be built on them. **This gate requires landing a durable `attempt` field on
  `ActivityStarted` / `ActivityCompleted` / `ActivityCancelled` in `aion-core`**, with the
  engine populating it and replay/history round-tripping it. **Gate / negative control:** a
  workflow whose activity started, was adopted, and completed has a single consistent
  `attempt` readable off `ActivityStarted` AND `ActivityCompleted` (not just
  `ActivityFailed`); a history that predates the field decodes without panic (schema
  back-compat). **Everything below is BLOCKED on NOI-0** — most sharply §5.3 (dedupe) and
  §6.4 (attempt-guard), realized in NOI-4 (events/dedupe) and NOI-5 (intervention routing).
- **NOI-1 (spike, norn only) — JSON-RPC channel: `initialize` + `run/*` + `event/*`
  round-trip.** Add `--protocol jsonrpc`; on the duplex, answer `initialize` with the child's
  capabilities (`interventions: {inject_message, cancel}`); serve one `run/execute` REQUEST
  whose RESPONSE `result` is the final value; subscribe a second broadcast receiver and emit
  each `AgentEvent` as an `event/*` NOTIFICATION reusing `agent_event_to_ndjson`'s payload
  verbatim (with `agent_id`/`agent_role` added). **Gate / negative control — MANDATORY,
  NAMED: the id-matched result/event discriminator (both directions).** A driven run yields
  well-formed JSON-RPC covering all four `AgentEventKind` arms as `event/*` notifications, and
  the gate asserts BOTH directions explicitly:
  - **(a) result-capture fidelity** — the result arrives **ONLY** as the Response whose `id`
    matches the `run/execute` request, and it is **byte-identical** to what the one-shot
    `.output()` capture path would have returned for the same run (proving the framed Response
    and the post-exit stdout buffer carry the same result bytes, §4.1).
  - **(b) no cross-contamination** — **NO `event/*` notification is ever a Response** (every one
    lacks an `id` and is routed to the transcript sink) and **NO Response is ever mistaken for
    an event**; a non-`run/execute` Response (e.g. an `intervene/*` ack) is rejected/logged,
    never captured as the result.

  This is the gate that CLOSES the §9.3 "Result/event message-typing is now load-bearing" risk
  (it is the message-typed, id-matched replacement for the old fd1/fd3 disjointness check).
  Assert the shutdown handshake terminates on the terminal Response (no hang, REVIEW C1).
  **Additive control:** `-f text`/`-f json`/`-f stream-json` and the TUI are unaffected (flag
  absent → legacy path byte-identical).
- **NOI-2 (norn only) — `intervene/*` request loop + Norn adapter.** Driven mode: prompt off
  stdin (via `run/execute` params / arg / file), the JSON-RPC read half carries **neutral**
  `intervene/*` REQUESTS; the Norn adapter (§3.4) maps `intervene/injectMessage` →
  `ChannelMessage` (`Interrupt`→steer path, `Normal`→queued `Update`) and `intervene/cancel` →
  a real `CancellationToken` threaded into `AgentStepRequest.cancel`, **each returning an ack
  RESPONSE**. **Gate:** an `intervene/injectMessage { priority: Interrupt }` mid-run is observed
  at the next tool boundary (drain at [runner.rs:939](../../../norn/crates/norn/src/loop/runner.rs#L939))
  and acked; an `intervene/cancel` stops the step and yields `AgentStepResult::Cancelled`; a
  method the child does not implement (`intervene/pauseResume`) returns native **`-32601 Method
  not found`** from the child — which in production the SERVER-side capability gate (§6.4 / §9.2
  decision 14) prevents from ever being sent, so a child `-32601` is a protocol-bug signal, not
  routine gating; the routine gate is the server refusing the unadvertised primitive. **Negative
  control:** a forged-identity injection is attributed as operator, never as a peer agent
  (frame-message contract, [inbound.rs:125-148](../../../norn/crates/norn/src/loop/inbound.rs#L125)).
  **Negative control (single-stdout-writer serialization, §4.2):** drive a burst of `event/*`
  notifications concurrently WHILE an `intervene/*` ack (and, once it lands, an
  `approval/request`) is emitted, and assert every outbound line is a complete, parseable
  JSON-RPC frame — proving the single serializing writer prevents interleave corruption of the
  shared child stdout.
- **NOI-3 (aion-worker) — shared JSON-RPC spawn helper.** `spawn_agent` in
  `aion-worker` — a NEW additive spawn mode beside the one-shot `.output()`; streaming
  `spawn()` with `--protocol jsonrpc`, `Stdio::piped()` stdin (`ChildStdin` retained); reader
  demuxes `event/*` notifications → `event_sender` on `ActivityContext` and correlates the
  terminal `run/execute` Response as the `DispatchOutcome::Completed { output }`;
  `control_receiver` writes `intervene/*` REQUESTS to `ChildStdin` and awaits acks. **Gate:**
  the norn-fan-worker example drives a real Norn run end-to-end over JSON-RPC, events drain live
  (not at exit), a command reaches the child and is acked. **Negative control:** a handler that
  does NOT spawn an agent still compiles and runs, and the **default one-shot `run_norn_step`
  path is unchanged** (the JSON-RPC mode is opt-in, not mandatory).
- **NOI-4 (liminal + server) — events out + sequencer + O keyspace.** *(Blocked on NOI-0 —
  the dedupe key needs a durable `attempt` on `ActivityStarted/Completed/Cancelled`.)* Worker
  publishes to a
  liminal events channel; server bridge (new `ActivityEventPublisher`) stamps commit-allocated
  `store_seq`, writes the `O` keyspace, fans out on a new transcript WS subscription. **Gate:**
  a live transcript streams to a WS client and resumes by `store_seq` after reconnect with no
  gap. **Negative control (THE key durability test):** kill-9 the worker mid-run; the adopting
  worker resumes the same session; two emitters for one `(wf,act,attempt)` **dedupe** and
  `store_seq` stays monotonic (mirror the #157 shard-fence test). This gate covers TWO distinct
  failure modes, both mandatory:
  - **Wrong-allocator case:** a buggy process-local `AtomicU64` (`ClusterEventPublisher`
    pattern) `store_seq` variant MUST be shown to produce colliding/non-monotonic sequences and
    be DETECTED — proving the counter belongs in the commit, not the process.
  - **Concurrent-writer-retry case (§5.3):** two racing `O`-appends for the same
    `(activity, attempt)` head — one wins with its `expected_seq`, the other gets
    `SequenceConflict`, **re-reads the now-advanced head and retries**, and still lands a
    strictly-monotonic `store_seq`. Assert no lost/duplicated/out-of-order `store_seq` results
    from the race — proving the read-head/retry loop (not the store) is what enforces
    monotonicity, and that the server serialization + retry is exercised, not assumed.
- **NOI-5 (server + liminal PUSH) — intervention routing.** *(Blocked on NOI-0 — the
  attempt-guard no-op needs a durable `attempt` on the terminal activity events.)*
  `attempt → owning-worker`
  back-index resolving via durable shard-owner state; namespace-scoped intervene endpoint;
  liminal PUSH to the owning worker. This slice exercises the **full neutral command set**
  through the Norn adapter: the ones Norn supports (`InjectMessage`, `Cancel`) drive the agent
  end-to-end, and the ones the Norn adapter advertises unsupported (`PauseResume`,
  `UpdateBudget`, `RespondToApproval`) each return a clean "capability not supported"
  rejection rather than silently succeeding. **Gate:** an operator `InjectMessage { priority:
  Interrupt }` posted to the API lands in the running agent and appears as an `O`-region event
  on the WS; a `PauseResume`/`UpdateBudget`/`RespondToApproval` posted for the Norn-owned
  attempt is cleanly rejected as unsupported (the capability gate). **Negative control:** a
  command to a finished/migrated attempt is a no-op with an honest NACK (the `attempt` guard,
  §6.4); after failover the command routes to the CURRENT owner, never a stale/dead worker.
- **NOI-6 (ops-console) — TranscriptPanel + InterventionControls.** Transcript render + delta
  coalescing + capability-gated controls. **Gate:** the panel shows a live transcript with
  token deltas coalescing into messages (no flicker), and the intervention control is HIDDEN
  for a harness that did not advertise the capability.
- **NOI-7 (harness-agnostic) — SECOND adapter, both directions.** A mock/non-Norn harness
  gets its own worker-side adapter: outbound, its interleaved-stdout events demux into
  `Raw`/mapped envelopes; inbound, it advertises at least `{inject_message, cancel}` and its
  adapter maps those **neutral** primitives onto its own control channel. **Gate / negative
  control (the real proof the contract is neutral):** the mock adapter actually DRIVES at
  least `InjectMessage` + `Cancel` through the neutral contract end-to-end — operator command
  → server → PUSH → worker → mock adapter → mock agent — AND correctly REJECTS at least one
  primitive it advertises unsupported (e.g. `RespondToApproval`) with a "capability not
  supported" NACK, proving **both** the contract neutrality and the capability gate in one
  test. All of this with `aion-core`/wire/server code UNCHANGED and naming ZERO harness types.
  This is what forces those layers to stay harness-blind; if adding a second working adapter
  required touching them, the contract was Norn-with-a-flag, not neutral. (A harness that
  genuinely cannot take control commands advertises nothing and the console offers no controls
  — a separate, weaker case than the driven mock above.)

**Feature-gate** the JSON-RPC/observability driven mode so a feature-off Norn build and a
feature-off worker can drop it before the on/off-by-default call is made (§9.3 open decision).

### 9.2 Open decisions

1. **`event/*` params schema: mirror the existing stdout `stream-json` vocabulary or a new
   unified schema?** LEAN: reuse the `stream-json` `type` vocab as the `event/*` notification
   `params` shape (maximizes `agent_event_to_ndjson` reuse, byte-identical payloads); the
   *worker adapter* maps it to the `ActivityEvent` envelope.
2. **JSON-RPC driven mode REPLACES the stdout `stream-json` renderer, or runs alongside?** LEAN:
   in JSON-RPC driven mode the run's result is the `run/execute` Response and events are
   `event/*` notifications on the same stdout writer; the legacy `stream-json` renderer is NOT
   spawned in this mode (single-writer discipline, §4.2) but is entirely intact when the flag is
   absent (additive, §4.0).
3. **`event/*` = live `AgentEventKind` (lossy-under-lag) or durable `SessionEvent` (complete,
   persisted)?** LEAN: `AgentEventKind` — the locked `Message/ToolCall/ToolResult/Progress/Stop`
   kinds map most directly onto it. Loss under lag is a transcript gap (surfaced as an
   `event/raw` marker), not a workflow-state bug, and the server+keyspace are the durable authority.
4. **Prompt source in driven mode: `run/execute` params vs positional arg / `--prompt-file`.**
   LEAN: `run/execute` params (the request that starts the run carries the prompt), freeing
   stdin to be the JSON-RPC read half; arg/file remain accepted fallbacks.
5. **Events transport: out-of-band liminal channel vs a new `WorkerToServer` gRPC variant.**
   LEAN: out-of-band liminal (observability is a separate subsystem; zero proto churn for
   events). Confirm the worker reuses its existing liminal connection vs opens its own (NOI-3/4).
6. **Intervention transport: liminal PUSH vs a new `ServerToWorker` oneof.** LEAN: PUSH (keeps
   the intervention path on the same out-of-band transport, no proto change); the `Cancel`
   template is the fallback if PUSH addressing is awkward.
7. **`store_seq` granularity: per-attempt, per-activity, or per-workflow counter?** LEAN:
   per-attempt cursor with `(activity, attempt)` in the key — cheapest attempt-scoped range
   scan (the §5.3 / haematite `O`-key layout).
8. **Shared spawn helper vs per-handler spawn.** STRONG LEAN: shared `aion-worker` helper —
   per-handler does not generalize and has no home for the harness-agnostic path.
9. **Feature-gate on/off-by-default.** Genuine tension: the ops-console-out-of-box philosophy
   argues on-by-default; the lean-binary posture argues gated. LEAN: on-by-default for the
   shipped binary, feature-gated so a feature-off build can drop it.
10. **`O`-keyspace append mechanism: KV `put_routed` with caller-owned `store_seq` vs a native
    non-`E` haematite event stream.** LEAN: general-KV `'O'` region with an explicit
    big-endian `store_seq` suffix ([keyspace.rs:14-27](../../../aion/crates/aion-store-haematite/src/keyspace.rs#L14),
    layout `O || wf(16) || 0x1f || activity_id || 0x1f || attempt_be || 0x1f || store_seq_be`),
    co-located on the workflow shard via `put_routed`/`range_routed`
    ([kv.rs:169-238](../../../haematite/crates/haematite/src/api/kv.rs#L169)) — keeps it
    byte-provably outside the `E` decoder. The native-stream alternative reuses `append_batch`'s
    atomic seq counter ([event_store.rs:132-157](../../../haematite/crates/haematite/src/api/event_store.rs#L132))
    but reintroduces the `0x00` stream encoding; acceptable only because the `'O'` prefix keeps
    it disjoint from `E` ([workflow_id_from_event_stream_key keyspace.rs:59-64](../../../aion/crates/aion-store-haematite/src/keyspace.rs#L59)).
11. **Retention/TTL for the `O` keyspace.** Observability streams grow unbounded; haematite
    supports `put_with_ttl`/`append_with_ttl` ([kv.rs:78-101](../../../haematite/crates/haematite/src/api/kv.rs#L78)).
    OPEN: should transcript events carry a TTL while replay-log events never expire?
12. **`O`-event distributed durability.** Same quorum `replicate_append` as replay
    ([store.rs:69-76](../../../aion/crates/aion-store-haematite/src/store.rs#L69)) or local-only
    like outbox rows ([store.rs:1264-1266](../../../aion/crates/aion-store-haematite/src/store.rs#L1264))?
    OPEN — affects whether a transcript survives a node loss vs just a process restart.
13. **JSON-RPC wire framing: newline-delimited JSON vs LSP-style `Content-Length` headers.**
    LEAN: **newline-delimited** — it is what Norn's in-tree JSON-RPC stdio already ships and
    proves ([mcp_server.rs::serve_stdio:206-227](../../../norn/crates/norn/src/integration/mcp_server.rs#L206),
    [mcp_client.rs](../../../norn/crates/norn/src/integration/mcp_client.rs)), and `serde_json::to_string`
    emits no interior newlines so generated frames are safe. Caveat: an external peer that
    pretty-prints multi-line JSON would corrupt a newline-framed stream — if strict LSP interop
    is ever required, swap to `Content-Length` framing in ~30 LOC localized to two read/write
    helpers. Both peers must support requests in **both directions** (`approval/request` is
    child→host) with **disjoint `id` spaces per direction** (bidirectional JSON-RPC, like LSP).
14. **`-32601` overload: gate on it, or gate at the server? (LOCKED — server-side gate,
    `-32601` reserved for protocol bugs.)** Capability-gating does **NOT** rely on `-32601` at
    the server: the server gates on the `initialize`-advertised capability set (forwarded via
    `RegisterWorker`, §5.0/§5.1) and **never sends a primitive the child did not advertise**, so
    a `-32601` *from the child* is treated as a real protocol bug (logged/surfaced), not routine
    gating. This makes three outcomes distinct and unambiguous (§6.4): **not supported** (server
    capability gate) vs **too late / wrong attempt** (application-range no-op, `-32000..-32099`,
    e.g. `-32001 attempt superseded`) vs **protocol bug** (`-32601 Method not found` from the
    child). No `error.data` disambiguation is needed — the three are already distinct
    codes/classes by construction.

### 9.3 Biggest risks (honest)

- **Commit-allocated `store_seq` is load-bearing and easy to get wrong.** A process-local
  `AtomicU64` (the `ClusterEventPublisher` pattern,
  [cluster_publisher.rs:61](../../../aion/crates/aion-server/src/cluster_publisher.rs#L61))
  resets on failover and two survivors collide. The seq MUST be allocated inside the haematite
  commit ([event_store.rs:132-157](../../../haematite/crates/haematite/src/api/event_store.rs#L132)).
  But since the store takes a caller-supplied `expected_seq` and does NOT auto-allocate (§5.3),
  the **server-single-writer + `SequenceConflict` read-head/retry loop is itself
  correctness-critical code**, not an implementation detail: on a concurrent-append race the
  loser must re-read the advanced head and retry to stay monotonic. This is the #1 correctness
  risk and NOI-4's mandatory negative control targets **both** the wrong-allocator case AND the
  concurrent-writer-retry case (§9.1 NOI-4).
- **Durable attempt identity is a FOUNDATIONAL PREREQUISITE, not an open question
  (NOI-0).** Dedupe requires a stable `(workflow, activity, attempt)` key, but **today only
  `ActivityFailed` carries `attempt: u32`; `ActivityStarted`, `ActivityCompleted`, and
  `ActivityCancelled` carry NONE** (verified
  [event.rs:218-253](../../../aion/crates/aion-core/src/event.rs#L218)). Every part of this
  design keys on `attempt` — the `O`-keyspace layout, §5.3 dedupe, §6.4 attempt-guard, §4.4
  session-id — so without a durable attempt on the terminal activity events the keys are
  ambiguous and **nothing else can be built**. This is no longer flagged as "open": it is
  the hard gate **NOI-0** (land a durable `attempt` field on
  `ActivityStarted/Completed/Cancelled` in `aion-core`), and NOI-4/NOI-5 (and §5.3, §6.4)
  are explicitly **BLOCKED on it**.
- **The intervention-vs-retry residual (§7.5).** The clean crux covers agent-behavioral
  interventions; state-affecting effects are deliberately excluded from agent-stdin
  intervention and kept on the existing `E`-stream engine paths. The mitigation is an enum
  that *cannot* express a state-affecting agent intervention + a UI honesty rule. This is a
  drawn line, not a closed hole — if a future requirement needs an agent-steer to change
  workflow state, it needs a first-class `Event` variant and a fresh design pass. Named here
  so it is never discovered by surprise.
- **`event/*` loss under broadcast lag.** The 256 broadcast buffer is tuned for a transient
  stdout renderer ([output.rs:284](../../../norn/crates/norn-cli/src/print/output.rs#L284)); an
  observability sink that lags drops notifications → transcript gaps. Mitigation: larger buffer
  + an `event/raw` gap marker; not a workflow-state bug but a visible UX hole.
- **Result/event message-typing is now load-bearing.** Collapsing the fd1 result into a
  `run/execute` Response means the result no longer has a dedicated fd — a bug emitting the
  result as a Notification (or an event as a Response) would silently violate replay authority.
  The typing discipline MUST be enforced/tested. **This risk is CLOSED by the named, mandatory
  NOI-1 gate** (§9.1), which makes the guarantee near-structural (id-match, not convention) and
  asserts both directions: (a) the result is captured only as the id-matched `run/execute`
  Response and is byte-identical to the one-shot `.output()` capture, and (b) no event is ever a
  Response and no Response is ever mistaken for an event — the message-typed analogue of the old
  "stdout carries ONLY the result" check.
- **`-32601` overload — RESOLVED (LOCKED, §6.4 / §9.2 decision 14).** JSON-RPC reserves
  `-32601` for "method does not exist," which would overload "primitive not advertised" vs
  "typo/bug" if the server leaned on it for gating. It does not: capability-gating is enforced
  at the **server** on the `initialize`-advertised set (forwarded via `RegisterWorker`), which
  **never sends the child an unadvertised primitive**, so a `-32601` *from the child* is a real
  **protocol bug** (logged/surfaced), never routine gating. The attempt-superseded no-op stays
  on a distinct **application-range** code (`-32001`, §6.4). Three classes — **not supported**
  (server capability gate), **too late / wrong attempt** (`-32001`), **protocol bug**
  (`-32601` from the child) — are distinct and unambiguous, no `error.data` needed.
- **Bidirectional-push framing.** `approval/request` is child→host while `run/*`/`intervene/*`
  are host→child; the single stdout writer must serialize all outbound frames or interleaving
  corrupts newline framing — the one piece the in-tree JSON-RPC prior art does not already cover
  (§9.4). This is now a **first-class tested requirement** (§4.2): the child serializes every
  outbound frame through one mutex/single-task writer (as `mcp_server`'s serve loop already
  does), the host applies the same discipline to `ChildStdin`, and a **named negative control
  (NOI-1/NOI-2)** bursts `event/*` notifications concurrently with an ack/approval and asserts
  every line is a complete, parseable frame.
- **liminal backpressure unwired.** Unbounded subscriber `VecDeque`
  ([subscription.rs:33](../../../liminal/crates/liminal/src/channel/subscription.rs#L33)) +
  `publish()` not consulting the pressure module means a slow observer can grow server memory.
  Mitigation: a bounded cap that drops-to-gap lands first in the pipeline; full
  `PressureEnforcer` wiring is a later slice in the same pipeline.
- **The agent-process spawn lives in USER handler code today** (norn-fan-worker), not the
  reusable runtime. Any design that only edits the example does NOT generalize — the shared
  `spawn_agent` helper (NOI-3) is mandatory, not optional.
- **Two Norn taxonomies must not be conflated.** The live `AgentEventKind` stream and the
  durable `SessionEvent` timeline overlap but differ in shape/field names
  ([events.rs:114](../../../norn/crates/norn/src/session/events.rs#L114)); the `event/*` contract
  is `AgentEventKind`, and it will NOT match on-disk `SessionEvent` JSON — the adapter, not a
  reader, is the single translation point.
- **Everything server/keyspace-side is greenfield.** No transcript event family exists anywhere
  today (not in `Event`, not in the WS protocol, not in `aion-core` —
  [cluster_event.rs:23](../../../aion/crates/aion-core/src/cluster_event.rs#L23) has only a
  deferred metrics note). This is a real cross-repo build, not a UI add.

### 9.4 Transport dependency decision (LOCKED — lightest high-quality approach)

**Decision: hand-roll a tiny (~120–180 LOC) JSON-RPC 2.0 framing layer over `serde_json`.
Add ZERO new crates.** This is the lightest high-quality approach per the dep-hygiene audit:

- `serde` + `serde_json` (1.0.150) + `tokio` + `async-trait` are ALREADY present in every
  affected workspace, so the hand-rolled layer adds **no new `[[package]]`** to any Cargo.lock —
  the whole point versus the alternatives. (In the lean crates the only Cargo.toml change would
  be appending `io-std`/`process` to the existing tokio feature array — a feature flag, not a
  new crate; norn already has `process`/`io-std` in use.)
- **Complete, tested prior art exists in-tree to lift almost verbatim:** the client
  ([mcp_client.rs](../../../norn/crates/norn/src/integration/mcp_client.rs)) and server
  ([mcp_server.rs](../../../norn/crates/norn/src/integration/mcp_server.rs)) already hand-roll
  JSON-RPC 2.0 over child/process stdio — `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError`
  envelopes, id correlation via `Mutex<u64>`, notification-vs-request discrimination on
  `Option<id>`, and the `-32700/-32600/-32601/-32603` error codes. Extract the generic envelope
  from the MCP-specific payload types; reuse, don't reinvent.
- **Rejected — `lsp-server`** (rust-analyzer's): a NEW top-level crate plus `crossbeam-channel`,
  and it hard-codes `Content-Length` framing / the LSP `Message` shape — dep-tree cost with
  little payoff over the ~150 lines we already have working. (Only reuse-justify it if strict LSP
  framing interop becomes a hard requirement — decision 13.)
- **Rejected — `jsonrpsee`**: heaviest; its stdio story is weak and it drags in tower/hyper/http/
  futures. This directly violates the no-heavy-async-framework / no-inbreeding hygiene rule and
  would balloon the lean trees. (The only LSP-adjacent crate present anywhere is `lsp-types` in
  norn, a pure serde type library pulled by an unrelated git dep — irrelevant to transport.)
- **Bidirectional push is the only net-new piece** the prior art does not cover: extend the loop
  with a single serializing stdout writer (mutex or single writer task) so responses and
  outbound notifications never interleave-corrupt framing.

### 9.5 Code-quality bar (the implementation MUST clear this)

The JSON-RPC transport is added on aion (worker/server), norn (CLI), and shared crates whose
lint regimes are the strictest in the stack. The implementation MUST:

- Compile clean under **clippy `all` + `pedantic` at DENY** with **`unwrap_used` / `expect_used`
  / `panic` DENIED** (aion/liminal/haematite bar; haematite also `nursery` DENY). On norn the
  levels are WARN but the CLAUDE.md/CONVENTIONS.toml no-bypass process still applies.
- Add **NO new `#[allow]` / `#[expect]` / `#[deny]` / `#[cfg(any())]` / `_var` / `#[ignore]`
  bypasses on production items** — fix the code, not the lint (aion & norn CLAUDE.md). The only
  exception is `#[allow]` on `#[cfg(test)]` items (norn).
- Keep functions under clippy's default **`too_many_lines` = 100** (no custom threshold exists
  on aion/liminal/haematite to lean on) and files under **500 LOC** (norn hard-caps 500 general
  / 200 for mod|lib|main); `mod.rs` = re-exports only, thin `lib.rs`/`main.rs`.
- Use **`thiserror` typed errors** in library code (`anyhow` only in the binary), propagate with
  `?`, and **map lock poison to a typed error** — never `.unwrap()`/`.expect()` in library code.
- Keep `unsafe_code` at **deny/forbid** (no new `unsafe`).
- Honor the **"replace, don't add alongside" no-compat rule** by ensuring JSON-RPC is a
  genuinely NEW capability (a driven transport), not a parallel duplicate of an existing RPC
  surface — the §4.0 additive framing is additive *capability*, not a compat shim.
- Run `cargo fmt` over the whole tree (never a format-check command).

---

## Appendix — one-paragraph summary for the impatient

A headless Norn run driven with **`--protocol jsonrpc`** speaks a **single bidirectional
JSON-RPC 2.0 channel over stdin+stdout** (stderr = human logs), with an **LSP-style
`initialize`/capabilities handshake** and our own namespace: the **`run/execute` RESPONSE = the
result only** (worker captures it as the replay-authoritative activity output), **`event/*`
NOTIFICATIONS = the transcript stream** (payloads produced byte-identically by the existing
`agent_event_to_ndjson`, with `agent_id` added), **`intervene/*` REQUESTS = harness-neutral
intervention commands that return acks** (the complete set — `InjectMessage`, `Cancel`,
`PauseResume`, `UpdateBudget`, `RespondToApproval`; the worker's Norn adapter maps the ones Norn
supports onto Norn's `Steer`/`Update` inbound + a real `CancellationToken`, advertising the rest
UNSUPPORTED so the SERVER gates them on the advertised set and never sends them (a `-32601` from
the child is a protocol bug, not routine gating, §6.4); no layer above the adapter names a Norn type),
and **`approval/*` = agent-initiated approval requests** correlated by `id`. Result-vs-events
separation — Tom's linchpin — is preserved **by message TYPING, not fd separation**: only the
`run/*` Response feeds history, Notifications never do. **This is additive — it removes nothing:**
Norn's `-f stream-json`/`-f json`/`-f text` and the TUI stay untouched, and the worker keeps its
one-shot capture as the default beside a new JSON-RPC spawn mode. The worker adapter spawns the
agent, tees `event/*` to a **liminal channel**, and forwards commands via **liminal PUSH**; the
**aion-server is the sequencer**, stamping a **commit-allocated `store_seq`** and writing an
append-only **haematite `'O'`-region keyspace** per `(workflow, activity, attempt)` that is
byte-provably disjoint from the `E`-stream replay log, while fanning out to the ops-console
WebSocket. Shared types live as new **`aion-core`** modules (`activity_event.rs` +
`intervention.rs`, beside `cluster_event.rs`; `ts-rs`-derived for the
console); **Norn does not depend on aion** — the worker adapter owns the translation, which is
required anyway for the capability-gated **harness-agnostic** path (non-JSON-RPC harnesses fall
back to mixed-stdout demux for observability and offer no intervention). The transport is a
**hand-rolled ~150-LOC JSON-RPC 2.0 layer over `serde_json`, zero new crates**, lifted from
Norn's in-tree MCP prior art. The crux: **interventions are durable, auditable observability
records but NOT workflow replay state; the result is the single authoritative output; retries
re-run fresh without prior interventions** — defensible because an intervention only "counts"
when it lands in an accepted result, and an accepted result is already durable, with the one
residual line (state-affecting effects stay on the existing `E`-stream engine paths) drawn
explicitly in code and UI rather than papered over.
