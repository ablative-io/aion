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

2. **Three-fd process contract (LOCKED).** A headless Norn run speaks on three file
   descriptors: **fd1/stdout = the RESULT and only the result** (one final structured
   value the worker captures as the activity output → replay-authoritative history);
   **fd3 = the EVENT stream**, NDJSON, one `ActivityEvent` envelope per line;
   **fd0/stdin = the CONTROL-COMMAND stream**, NDJSON, one intervention command per
   line into the running agent. **stderr stays human logs** — deliberately kept OUT
   of the structured stream so library noise can never pollute the durable store.
   Today Norn has *neither* fd3 nor a headless stdin control loop: stdin is fully
   consumed as the prompt ([orchestrator.rs:194-201](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L194)),
   one `run_agent_step` runs, then exit ([orchestrator.rs:383](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L383)),
   `cancel: None` ([orchestrator.rs:399](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L399)),
   and no `FromRawFd` path exists anywhere in the tree.

3. **Reuse Norn's native events; do not reinvent.** Norn already has a live event
   spine — a `broadcast::Sender<AgentEvent>` carrying `AgentEventKind`
   (`Provider | Subagent | Message | UsageEstimate`,
   [agent_event.rs:300](../../../norn/crates/norn/src/provider/agent_event.rs#L300))
   — and a working `AgentEvent → NDJSON` translator
   ([output.rs:317 `agent_event_to_ndjson`](../../../norn/crates/norn-cli/src/print/output.rs#L317)).
   The fd3 emitter is a near-clone of that translator pointed at fd 3, with an
   envelope mapping added. **Norn is the privileged first producer.**

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
   `t/p/r/o/n/d` tags ([keyspace.rs:14-27](../../../aion/crates/aion-store-haematite/src/keyspace.rs#L14)).
   The replay decoder only ever scans the `E`-stream and decodes `serde_json::<Event>`
   ([store.rs:1237-1258](../../../aion/crates/aion-store-haematite/src/store.rs#L1237)),
   so an `O`-region record is **structurally invisible** to replay. That byte-level
   disjointness is what makes "durable but non-authoritative" a guarantee, not a hope.

7. **Scope boundary (§2).** This is Norn-privileged fd3 + stdin-control, with the
   worker adapter supporting *other* harnesses via mixed-stdout demux and
   capability-gated intervention. It is NOT a generic distributed tracing system, NOT
   OpenTelemetry, and does NOT replace the Prometheus metrics surface
   ([metrics.rs](../../../aion/crates/aion-server/src/observability/metrics.rs)).

8. **Honesty caveat — intervention rides a HARNESS-NEUTRAL contract; Norn is merely the
   FIRST adapter.** Both *observability* and *intervention* are harness-neutral by design.
   The intervention **command vocabulary** is the complete set of five neutral semantic
   primitives — `InjectMessage`, `Cancel`, `PauseResume`, `UpdateBudget`, and
   `RespondToApproval` (§3.3) — spoken by the wire, server, and ops-console, none of
   which reference Norn types; ALL harness-specific translation lives in one place, the
   worker-side per-harness adapter (§3.4). A worker advertises **which neutral primitives its
   harness supports** (e.g. `{inject_message, cancel}`) in the `RegisterWorker` capabilities
   field, and **any harness that implements them is FIRST-CLASS, not second-class**. That the
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
activity-attempt → the worker-side per-harness adapter → agent stdin. Recorded as durable
observability events (so the transcript shows "operator steered here"), but NOT as
replay state.

The two share one envelope family, one liminal transport, one server bridge, one
haematite keyspace, and one ops-console feature. They differ only in direction (events
out, commands in) and in durability semantics (events are the primary durable artifact;
command *delivery* is live-only, command *record* is durable).

---

## 2. Scope boundary — what this is and is NOT

- **IS:** Norn-native fd3 events + stdin control as the privileged first-class **transport**;
  a **harness-neutral command vocabulary** (`InjectMessage`, `Cancel`, `PauseResume`,
  `UpdateBudget`, `RespondToApproval` — §3.3) spoken by the
  wire/server/console; a worker adapter that spawns the agent, tees fd3 → liminal, and
  translates neutral commands → the harness's native control channel; a server bridge that
  sequences + persists + fans out; a haematite `O`-region keyspace; ops-console transcript
  panel + intervention controls.
- **IS (harness-neutral, first-class-if-implemented):** the worker adapter ALSO supports other
  harnesses whose events come interleaved on stdout via a **mixed-stdout demux**, and
  intervention is **capability-gated on the neutral primitive set** — a harness whose adapter
  implements `{inject_message, cancel}` is first-class for those; a harness that cannot take
  control commands advertises an empty set and the ops console offers no controls for it. Norn
  is the FIRST adapter, not a privileged command shape (§3.4).
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

### 3.1 New crate: `aion-observability`

Shared types live in a **new `aion-observability` crate**, not in `aion-core`. Rationale:

- `aion-core` is the replay-authoritative type home (`Event`, `EventEnvelope`,
  `ActivityId`, [event.rs:14-25](../../../aion/crates/aion-core/src/event.rs#L14)).
  Putting transcript/command types there risks exactly the conflation §7.5 forbids —
  a future contributor adding a `Message` arm to `Event` would silently make transcript
  data replay-authoritative. Physical crate separation makes that a cross-crate change,
  not a one-line temptation.
- The envelope + command types are needed by aion-server, aion-worker, and (via
  `ts-rs`) the ops console. A leaf crate depended on by all three keeps the dep graph
  linear (worker → observability, server → observability), no diamond through core.
- `ts-rs` derives on the envelope/command/event-kind enums generate the ops-console
  TypeScript (the console types are already code-generated from Rust —
  [types/generated/index.ts:163](../../../aion/apps/aion-ops-console/src/types/generated/index.ts#L163)),
  so `Message | ToolCall | ToolResult | Progress | Stop | Raw` land in the generated
  union the same way `Event` does today.

**Does Norn depend on `aion-observability`?** **No.** Norn emits its NDJSON on fd3
using its *own* native shapes (a near-clone of `agent_event_to_ndjson`). The **worker
adapter** owns the translation from Norn's on-wire NDJSON into the `ActivityEvent`
envelope. This keeps Norn free of an aion dependency (it stays a standalone agent
harness) and keeps the envelope an aion-side contract the worker adapter enforces — which
is *required anyway* for the harness-agnostic path, where a non-Norn harness's stdout
events must be demuxed and mapped by the same adapter. Norn's fd3 schema is a stable,
documented NDJSON contract; the adapter is the single translation point.

### 3.2 The `ActivityEvent` envelope

```rust
// aion-observability
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
JSON is hand-built in `output.rs`. The fd3 emitter reuses/extends those hand-built
mappers — it CANNOT naively `serde_json::to_value` them (§9 risk).

### 3.3 The `InterventionCommand` enum — HARNESS-NEUTRAL SEMANTIC PRIMITIVES

The command vocabulary is defined in **harness-neutral semantic primitives**, NOT in any
harness's native terms. The enum lives in the shared `aion-observability` crate
(`ts-rs`-derived for the dashboard) and is spoken by **the wire, the server, and the
ops-console — none of which may reference Norn types**. Norn's `Steer`/`Update`/
`CancellationToken` appear NOWHERE in this enum; they live strictly in the worker-side
adapter mapping (§3.4 / §6).

**The design test (explicit):** a primitive belongs in the neutral enum ONLY if it can
plausibly map onto a **non-Norn** conversational-agent harness. Anything that only makes
sense as a Norn feature does not belong here — it belongs behind the adapter.

The complete neutral set is exactly five primitives — the whole universal agent-control
surface, each one gated by the harness's advertised capability set:

```rust
// aion-observability
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

## 4. The fd model + the Norn-repo changes

This is the load-bearing new surface in the **norn** repo. Today Norn has none of it.

### 4.1 fd1 (stdout) = the RESULT and only the result

Headless Norn already emits a final structured envelope in `-f json`/`-f text` modes;
the worker already parses child stdout as a JSON envelope and returns `output` on
`result == "completed"` ([norn-fan-worker main.rs:129-168](../../../aion/examples/norn-fan-worker/src/main.rs#L129)).
**Change:** in the fd3 mode, stdout carries *only* the single final result value —
the incremental event stream that today leaks onto stdout under `-f stream-json`
([spawn_stream_renderer output.rs:262](../../../norn/crates/norn-cli/src/print/output.rs#L262))
moves to fd3. This keeps stdout clean so the worker's activity-output capture is
unambiguous and becomes replay-authoritative history.

### 4.2 fd3 = the event stream (NEW emitter)

- Add a `--events-fd 3` (or `--emit-fd3`) flag to the CLI `Cli` struct
  ([args.rs:23](../../../norn/crates/norn-cli/src/cli/args.rs#L23)).
- In the print orchestrator, subscribe a **second receiver** off the existing
  `broadcast::channel::<AgentEvent>(N)` ([orchestrator.rs:371](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L371))
  — the broadcast fan-out means fd3 composes with any stdout renderer without
  interference. Point the subscriber at `std::fs::File::from_raw_fd(3)` (no
  `FromRawFd` path exists today — this is net-new).
- Reuse `agent_event_to_ndjson` ([output.rs:317](../../../norn/crates/norn-cli/src/print/output.rs#L317))
  but **ADD `agent_id`/`agent_role` to every emitted line** — the current translator
  DROPS them ([risk §9](#9-open-decisions--honest-risks)), which is fine for
  single-agent stdout but makes multi-agent events unattributable. This is a small,
  required change in the translator for the fd3 path.
- **Buffer/loss:** the broadcast channel is lossy under lag
  (`RecvError::Lagged`, [output.rs:284](../../../norn/crates/norn-cli/src/print/output.rs#L284)),
  and the 256 buffer ([orchestrator.rs:70](../../../norn/crates/norn/src/... "orchestrator buffer"))
  is tuned for a transient stdout renderer. The fd3 sink either needs a larger buffer
  or a non-broadcast tee. Because the server sequences and the keyspace is the durable
  store, **a dropped fd3 line is a gap in the transcript, not a correctness bug in
  workflow state** — but it is a visible transcript hole, so the buffer must be sized
  generously and lag surfaced as a `Raw`/gap marker.
- **Shutdown discipline (REQUIRED):** the `SharedAgentEventChannel` keeps an owned
  `Sender` clone so the channel never closes on its own
  ([wiring.rs:290](../../../norn/crates/norn-cli/src/runtime/wiring.rs#L290), REVIEW C1);
  the fd3 sink MUST use the explicit `finish()`/shutdown handshake
  ([output.rs:234](../../../norn/crates/norn-cli/src/print/output.rs#L234)) or it hangs
  forever awaiting closure.
- **TUI parity is out of scope** for the headless observability path — the TUI creates
  its own broadcast channel ([driver.rs:219](../../../norn/crates/norn-cli/src/tui/driver.rs#L219));
  fd3 is a headless-only emitter (§9 open decision).

### 4.3 fd0 (stdin) = the control-command stream (NEW loop)

Today headless stdin is fully consumed as the prompt
([orchestrator.rs:194-201](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L194)),
so it cannot double as control **as-is**. Two options (§9 open decision):

1. **Prompt via arg/file, stdin becomes control.** When `--events-fd 3` (driven mode)
   is set, the prompt comes from positional args or a `--prompt-file`, freeing stdin to
   be a line-framed NDJSON control channel read by a dedicated tokio reader task.
2. **Separate control fd.** Keep stdin as prompt, add `--control-fd 0`-style dedicated fd.

**LEAN: option 1** — the locked contract says fd0/stdin *is* the control stream, so in
driven mode the prompt moves off stdin. The reader task parses each line as a **neutral
`InterventionCommand`** and applies the Norn-adapter mapping (§3.4):
- `InjectMessage` → builds a `ChannelMessage` ([inbound.rs:72](../../../norn/crates/norn/src/loop/inbound.rs#L72))
  and sends on the root's registered `InboundSender`
  ([wiring.rs:211](../../../norn/crates/norn-cli/src/runtime/wiring.rs#L211) registers the
  root route). `priority: Interrupt` takes Norn's steer path; `priority: Normal` a queued
  `Update`. The frame-message security contract
  ([inbound.rs:125-148](../../../norn/crates/norn/src/loop/inbound.rs#L125)) must be
  preserved so an external injection cannot forge agent identity — the operator source
  is attributed as an operator, not as a peer agent.
- `Cancel` → trips a real `CancellationToken` threaded into `AgentStepRequest.cancel`
  ([runner.rs:163](../../../norn/crates/norn/src/loop/runner.rs#L163)) — today headless
  passes `cancel: None` ([orchestrator.rs:399](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L399)),
  so this is net-new wiring. This is the agent-run cancel (§7.5), not a workflow-visible cancel.
- The step runs under `tokio::select!` against the reader task.

**Single-run vs driven loop.** Headless runs ONE `run_agent_step` then exits
([orchestrator.rs:383](../../../norn/crates/norn-cli/src/print/orchestrator.rs#L383)).
That is sufficient for the complete intervention model: a single long activity attempt is
one step under `select!`, with control landing at tool boundaries. A multi-turn driven
daemon is NOT required by this design and is explicitly out of scope.

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
Norn (fd3 NDJSON, native shapes)
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

### 5.1 Worker adapter (aion-worker + norn-fan-worker)

The agent process is spawned **inside a user handler** today
([norn-fan-worker main.rs:86-169](../../../aion/examples/norn-fan-worker/src/main.rs#L86)),
using blocking `.output()` with `Stdio::null` stdin. The runtime never touches child
stdio. **The fd3-tee + stdin-forward must be introduced at the process-spawn boundary,
and it MUST be a shared `aion-worker` helper — not per-handler** (§9 open decision,
strongly leaned): otherwise every handler re-implements observability and the
harness-agnostic path has no home.

Concrete worker changes:
- **`spawn_agent` helper** in `aion-worker` that: spawns the child with fd3 as an extra
  pipe (via `CommandExt::pre_exec` / passed pipe fd) and `Stdio::piped()` on stdin
  (retaining `ChildStdin`); switches from blocking `.output()` to a streaming `spawn()`
  with concurrent async readers draining fd3 line-by-line while the child runs (today's
  single `tokio::time::timeout` at [main.rs:109](../../../aion/examples/norn-fan-worker/src/main.rs#L109)
  becomes a select over readers + timeout + cancellation).
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
  haematite-durable subsystem → route fd3 events straight to a liminal events channel,
  **zero worker-stream / proto changes for events**. The worker already holds a liminal
  connection (`serve_with_redial`, [main.rs:290-305](../../../aion/examples/norn-fan-worker/src/main.rs#L290));
  reuse it (confirm in the spike whether the event/control transport reuses that
  connection or opens its own — §9).
- **Capability advertisement:** at registration, the worker advertises **which of the five
  neutral primitives its harness's adapter supports** — the Norn adapter advertises
  `{inject_message, cancel}` and marks `{pause_resume, update_budget, respond_to_approval}`
  unsupported (§3.4) until the mechanisms exist. Today `RegisterWorker` has a fixed 4-field
  wire shape (`namespaces, activity_types, task_queue, node`,
  [worker.proto:94-109](../../../aion/crates/aion-proto-generated/proto/worker.proto#L94)) —
  add an `intervention_capabilities` field carrying the supported-primitive set (proto
  change, coordinated with the server; `aion-proto-generated` is generated). Any harness whose
  adapter implements a primitive is **first-class** for it; a harness that cannot take control
  commands advertises an empty set and the server/console never offers intervention for it.
  The server and console gate purely on **which of the five neutral primitives are in the
  advertised set** and never on harness identity.

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
handling/retrying `SequenceConflict`** — not on any magic in the store. This is exactly
why the server owns durability (below) and why NOI-4's negative control (colliding
sequences under a bad allocator) is **necessary, not optional**. The wrong approach — a
process-local `AtomicU64` à la `ClusterEventPublisher` — is called out above precisely
because it looks like it allocates but silently resets on failover.

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

## 6. The intervention flow — operator → server → worker → adapter → agent stdin

**Everything down to the worker adapter speaks ONLY the neutral primitives
(`InjectMessage`, `Cancel`, `PauseResume`, `UpdateBudget`, `RespondToApproval`) and is
harness-blind (§3.4).** The Norn-specific translation
happens in exactly one place — the worker-side adapter — and is described in §3.4. This
section describes the same boundary at the flow level.

```
ops-console / API
   │  POST (namespace-scoped, see §6.6)  OR  WS command frame        [NEUTRAL commands]
   ▼
aion-server                 resolve CURRENT owner of (workflow,activity,attempt)
   │  liminal PUSH to the owning worker's connection                 [NEUTRAL commands]
   ▼
aion-worker                 route by attempt -> in-flight handle -> control channel
   │  per-harness ADAPTER translates neutral -> native (§3.4)        [adapter boundary]
   ▼
Norn adapter -> agent stdin InjectMessage(Interrupt)->steer path; InjectMessage(Normal)->queued
                            ChannelMessage/Update; Cancel->CancellationToken
```

### 6.1 Best-effort / live-only (LOCKED)

Command delivery is **best-effort, live-only** — inherently real-time, NOT durably
queued or retried. Commands to a finished/migrated activity are **no-ops** (§6.4). This
is a deliberate, defensible asymmetry: the *event* stream is the durable artifact; a
*command* is a human real-time act. Durably queuing a steer would mean re-delivering it
on a retry — which is exactly the replay-contamination §7 forbids.

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
`ChildStdin`.

### 6.4 Attempt-scoped no-op

Every command carries `attempt`. If the server's back-index shows the attempt is finished,
migrated to a *different* attempt number, or unknown, the command is a **no-op** with an
honest NACK to the caller. This is what makes "commands to a finished/migrated activity
are no-ops" concrete — the `attempt` field is the guard. **BLOCKED on NOI-0:** the guard
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
to stop and await, or `Cancel` + resume (Norn's `CancellationToken` + `open_or_resume`), and
a literal `PauseResume` command returns a clean "capability not supported" rejection until the
mechanism exists. That a neutral primitive can be defined and gated ahead of any harness
implementing it is exactly the point of the capability contract (§3.4).

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

- The activity **RESULT** (fd1/stdout, captured by the worker as the activity output) is
  the **single replay-authoritative** value. It is the only thing that enters workflow
  history via `WritableEventStore::append`
  ([store.rs:257-313](../../../aion/crates/aion-store/src/store.rs#L257)).
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
- **NOI-1 (spike, norn only) — fd3 emitter round-trip.** Add `--events-fd 3`; subscribe a
  second broadcast receiver; write `agent_event_to_ndjson` (with `agent_id`/`agent_role`
  added) to fd 3 via `from_raw_fd`. **Gate / negative control:** a headless run with fd3
  redirected to a file yields well-formed NDJSON covering all four `AgentEventKind` arms AND
  stdout carries ONLY the final result (assert stdout has zero event lines — proves the
  fd1/fd3 split). Assert the shutdown handshake terminates (no hang, REVIEW C1).
- **NOI-2 (norn only) — stdin control loop + Norn adapter.** Driven mode: prompt off stdin
  (arg/file), stdin becomes NDJSON control carrying **neutral** `InterventionCommand`s; the
  Norn adapter (§3.4) maps `InjectMessage` → `ChannelMessage` (`Interrupt`→steer path,
  `Normal`→queued `Update`) and `Cancel` → a real `CancellationToken` threaded into
  `AgentStepRequest.cancel`. **Gate:** an `InjectMessage { priority: Interrupt }` sent on
  stdin mid-run is observed at the next tool boundary (drain at [runner.rs:939](../../../norn/crates/norn/src/loop/runner.rs#L939));
  a `Cancel` stops the step and yields `AgentStepResult::Cancelled`. **Negative control:** a
  forged-identity injection is attributed as operator, never as a peer agent
  (frame-message contract, [inbound.rs:125-148](../../../norn/crates/norn/src/loop/inbound.rs#L125)).
- **NOI-3 (aion-worker) — shared spawn helper + fd3 tee + stdin pipe.** `spawn_agent` in
  `aion-worker`; streaming `spawn()` replacing `.output()`; fd3 reader → `event_sender` on
  `ActivityContext`; `control_receiver` → `ChildStdin`. **Gate:** the norn-fan-worker example
  drives a real Norn run end-to-end, events drain live (not at exit), a command written to the
  worker reaches child stdin. **Negative control:** a handler that does NOT spawn an agent
  still compiles and runs (the helper is opt-in, not mandatory).
- **NOI-4 (liminal + server) — events out + sequencer + O keyspace.** *(Blocked on NOI-0 —
  the dedupe key needs a durable `attempt` on `ActivityStarted/Completed/Cancelled`.)* Worker
  publishes to a
  liminal events channel; server bridge (new `ActivityEventPublisher`) stamps commit-allocated
  `store_seq`, writes the `O` keyspace, fans out on a new transcript WS subscription. **Gate:**
  a live transcript streams to a WS client and resumes by `store_seq` after reconnect with no
  gap. **Negative control (THE key durability test):** kill-9 the worker mid-run; the adopting
  worker resumes the same session; two emitters for one `(wf,act,attempt)` **dedupe** and
  `store_seq` stays monotonic (mirror the #157 shard-fence test). A buggy process-allocated
  `store_seq` variant MUST be shown to produce colliding sequences and be DETECTED.
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

**Feature-gate** the fd3/observability path so a feature-off Norn build and a feature-off
worker can drop it before the on/off-by-default call is made (§9.3 open decision).

### 9.2 Open decisions

1. **fd3 envelope schema: mirror the existing stdout `stream-json` vocabulary or a new unified
   schema?** LEAN: reuse the `stream-json` `type` vocab as Norn's on-wire fd3 shape (maximizes
   `agent_event_to_ndjson` reuse); the *worker adapter* maps it to the `ActivityEvent` envelope.
2. **fd3 REPLACES the stdout `stream-json` renderer in driven mode, or runs alongside?** LEAN:
   in driven mode stdout carries only the result; the event renderer moves to fd3 (broadcast
   fan-out makes both cheap if we ever want them, but the locked contract says stdout = result
   only).
3. **fd3 = live `AgentEventKind` (lossy-under-lag) or durable `SessionEvent` (complete,
   persisted)?** LEAN: `AgentEventKind` — the locked `Message/ToolCall/ToolResult/Progress/Stop`
   kinds map most directly onto it. Loss under lag is a transcript gap (surfaced as a `Raw`
   marker), not a workflow-state bug, and the server+keyspace are the durable authority.
4. **Prompt-off-stdin vs a dedicated control fd.** LEAN: prompt-off-stdin (the locked contract
   says fd0/stdin IS the control stream).
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

### 9.3 Biggest risks (honest)

- **Commit-allocated `store_seq` is load-bearing and easy to get wrong.** A process-local
  `AtomicU64` (the `ClusterEventPublisher` pattern,
  [cluster_publisher.rs:61](../../../aion/crates/aion-server/src/cluster_publisher.rs#L61))
  resets on failover and two survivors collide. The seq MUST be allocated inside the haematite
  commit ([event_store.rs:132-157](../../../haematite/crates/haematite/src/api/event_store.rs#L132)).
  This is the #1 correctness risk and NOI-4's mandatory negative control targets it.
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
- **fd3 loss under broadcast lag.** The 256 broadcast buffer is tuned for a transient stdout
  renderer ([output.rs:284](../../../norn/crates/norn-cli/src/print/output.rs#L284)); an
  observability sink that lags drops lines → transcript gaps. Mitigation: larger buffer + a
  `Raw` gap marker; not a workflow-state bug but a visible UX hole.
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
  ([events.rs:114](../../../norn/crates/norn/src/session/events.rs#L114)); the fd3 contract is
  `AgentEventKind`, and it will NOT match on-disk `SessionEvent` JSON — the adapter, not a
  reader, is the single translation point.
- **Everything server/keyspace-side is greenfield.** No transcript event family exists anywhere
  today (not in `Event`, not in the WS protocol, not in `aion-core` —
  [cluster_event.rs:23](../../../aion/crates/aion-core/src/cluster_event.rs#L23) has only a
  deferred metrics note). This is a real cross-repo build, not a UI add.

---

## Appendix — one-paragraph summary for the impatient

A headless Norn run speaks three fds: **stdout = the result only** (worker captures it as the
replay-authoritative activity output), **fd3 = an NDJSON `ActivityEvent` stream** (near-clone
of the existing `agent_event_to_ndjson` translator, with `agent_id` added), **stdin = an NDJSON
stream of harness-neutral intervention commands** (the complete set — `InjectMessage`,
`Cancel`, `PauseResume`, `UpdateBudget`, `RespondToApproval`; the worker's Norn adapter maps
the ones Norn supports onto Norn's `Steer`/`Update` inbound + a real `CancellationToken` and
advertises the rest UNSUPPORTED until the mechanisms exist; no layer above the adapter names a
Norn type). The worker adapter spawns the agent, tees fd3 to a **liminal channel**,
and forwards commands via **liminal PUSH**; the **aion-server is the sequencer**, stamping a
**commit-allocated `store_seq`** and writing an append-only **haematite `'O'`-region keyspace**
per `(workflow, activity, attempt)` that is byte-provably disjoint from the `E`-stream replay
log, while fanning out to the ops-console WebSocket. Shared types live in a new
**`aion-observability`** crate (`ts-rs`-derived for the console); **Norn does not depend on
aion** — the worker adapter owns the translation, which is required anyway for the
capability-gated, mixed-stdout **harness-agnostic** path. The crux: **interventions are durable,
auditable observability records but NOT workflow replay state; the result is the single
authoritative output; retries re-run fresh without prior interventions** — defensible because an
intervention only "counts" when it lands in an accepted result, and an accepted result is
already durable, with the one residual line (state-affecting effects stay on the existing
`E`-stream engine paths) drawn explicitly in code and UI rather than papered over.
