---
type: vision
cluster: aion-dashboard
title: Aion Dashboard — Vision & Design Spec (full-strength end-state)
status: vision, pending review (Frodo)
captured: 2026-06-29
---

# Aion Dashboard — Vision & Design Spec

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system vision
> and `COMPONENT-ARCHITECTURE.md` for the crate map.
>
> **Scope of this document.** This specifies the *final, full-strength*
> dashboard — the complete end-state ambition, not an MVP. We are building
> ground-up and get to make every choice. Where this document phases work, the
> phasing is **build-order only**; the spec itself describes the whole. Where a
> capability is aspirational rather than already-true, this document says so
> explicitly and in line.
>
> This vision **supersedes** the early-June read-only MVP design
> (`DESIGN.md`, AU-001..009 briefs). It does not reconcile that artifact here;
> §11 records the reconciliation as a separate, named follow-on.

---

## 1. North star & design principles

The dashboard is the human-facing operational surface of a durable workflow
engine that now also runs a cross-node failover mesh. It is the thing an
on-call engineer opens at 3 a.m. when a payment flow is stuck, a worker has
died, or a node has fallen over and a survivor has adopted its shard. Its job
is **diagnosis under pressure**: tell the operator what is broken, where, why,
and what single action to take — faster and more truthfully than reading logs.

**The hand-plane principle.** A Stanley hand plane is beautiful *because* it is
functional: every line serves the cut. The dashboard's beauty must come from
the same source — the clarity of the visualization, the precision of the
diagnosis, the speed of the triage. Never from glass effects, bokeh particles,
gradients, or chrome. We draw from *ourselves* — our own design voice serving
our own concepts — not from any pre-existing aesthetic. Decorative flourishes
(an idle "constellation" screensaver, say) are permissible only as a nicety
*after* everything underneath is rock-solid, and never as a priority. If a
pixel does not aid reading, diagnosis, or action, it does not earn its place.

**Execution as readable shape.** The core conviction: a durable execution is a
partial order of concurrent events, and the right way to make it legible is to
render that *shape* — lanes, overlaps, correlations, branches — not a flat
chronological log. The research backing this is direct (§5.1): partial-order
visualizations beat linear logs at execution-comprehension tasks with a large
effect. Every primary view in this dashboard is a deliberate visualization of
structure, not a table of rows.

**Respect for the engine's invariants.** The dashboard is a *reader of
projections*, and it must encode the engine's five load-bearing invariants
rather than fight them:

1. **Type-erased events.** Payloads are opaque bytes + a content-type tag. The
   dashboard decodes JSON payloads for display *best-effort* and renders the
   raw envelope when it cannot — it never assumes a concrete payload schema the
   engine does not own.
2. **The determinism boundary.** Replay is deterministic and re-executed from
   the start; `workflow.now` is the recorded event timestamp, randomness is
   seeded. This is what makes the execution scrubber (§5.2) *possible* and
   *exact*: the same history replays to the same logical state every time. The
   dashboard surfaces recorded timestamps as truth and never invents wall-clock
   ordering.
3. **Single writer per workflow.** Exactly one `Recorder` is the sole append
   path for a live workflow. **The dashboard is strictly read-only against any
   live workflow's history.** It never appends, never holds a recorder, never
   becomes a second writer. Operator *actions* that mutate state (reopen,
   cancel) are issued through the server's command API as commands the engine's
   single writer enacts — the dashboard requests, the engine writes (§7).
4. **Status is a projection.** `WorkflowStatus` is derived from event history,
   never a stored mutable field; suspension (residency) is orthogonal. The
   dashboard computes and displays status *the same way the engine does* —
   last-lifecycle-event-wins, reset-aware for reopen — and treats residency
   (Resident / Suspended) as a separate axis, never as a status.
5. **Content-hash module namespacing.** Each `.aion` version is an immutable
   `logical_name$hash` module. The dashboard shows the *recorded* package
   version a run was pinned to (from `WorkflowStarted.package_version`), never a
   re-resolved "latest", because that pinned version is what replay uses.

**The professional-console bar.** Beyond the hand-plane aesthetic, five
operational disciplines separate a tool an on-call engineer trusts from a
workflow *viewer*. They are first-class, not polish, and each is recorded as a
binding decision (ADR-015..ADR-019). They are Phase-1 obligations (§9), not
deferred refinements:

- **Deep-linkable by construction (ADR-015).** Every meaningful view state — the
  selected workflow, the selected swimlane bar, the scrub position, the active
  search query, the namespace — is encoded in the URL. A URL *is* an app state:
  an operator pastes a link into Slack at 3 a.m. and the responder lands on the
  exact same bar at the exact same scrub point. State lives in the URL, not in
  ephemeral component memory. This is the single strongest "professional
  software" tell and the thing Temporal does worst. (§6.5)
- **The console never lies about itself (ADR-016).** It always surfaces its own
  data provenance and freshness: which node it is reading from, the last `seq`
  it has applied, and — after a read fails over to a survivor — an explicit "you
  are viewing a survivor" signal. Under pressure the operator must never have to
  wonder whether the screen is stale or post-failover. A silently stale view is
  a defect, not a degradation. (§6.3)
- **Keyboard-first and dense (ADR-017).** The consoles operators reach for under
  pressure — k9s, Linear, Superhuman — are dense and keyboard-driven; Temporal
  is sparse and mouse-bound. A command palette (jump to a workflow, run a
  search, issue an action) and real keyboard navigation are core interaction,
  not an accessibility afterthought. (§4.6)
- **Performance is a feature with a budget (ADR-018).** "Faster than logs" has
  teeth: the swimlane stays smooth at 10k+ events via virtualized rendering, the
  triage surface paints the one broken thing within a single screen without
  scrolling, and primary views reach useful content faster than opening a log
  tail. These budgets are stated and measured, not aspirational. (§6.6)
- **The calm state is designed too (ADR-019).** The console is triage-first, but
  the 99% case is an operator glancing at a healthy cluster. The ambient
  "everything is fine — here is the heartbeat" state earns the same design care
  as the incident surface; the tool must feel finished when nothing is on fire,
  not only when something is. (§4.4)

---

## 2. The two shifts

The dashboard's ambition is defined by two simultaneous capability shifts in
the stack, both rooted in beamr 0.11.0's cooperative wasm-runtime and its
hardened 3+-peer mesh handshake. These are the conceptual foundation; the core
concepts (§4) are how they become a tool.

### Shift 1 — A distributed-system ops console, not just a workflow viewer

The engine grew a **cross-node failover + exactly-once dispatch** capability
(the LSUB / `demo-failover-backend` line of work): a durable outbox, a
shard-owner that claims work and pushes it to registered remote workers over
Liminal, and a survivor that *adopts* a shard when the owner is `kill -9`'d —
the worker redials, pending work re-dispatches, and exactly one terminal is
recorded per ordinal.

That introduces an entirely **new observable surface** beyond the 29 workflow
event types: outbox row lifecycle, worker registry and connection topology,
node ownership and shard-adoption transitions, delivery acks, dedup hits, typed
fenced-CAS rejections, and typed disconnect-vs-timeout worker-death signals
(catalogued in full in §6). The dashboard is therefore no longer a workflow
viewer with a live feed bolted on — it is an **operations console for a
distributed durable-execution cluster**. The failover demo (boot a 3-node
cluster on one laptop, kill the owner, watch work survive) becomes a *live
visualization* rather than a log tail.

This shift is **shippable now**: every signal it needs is server-side and
already produced (or a small, named promotion of today's logs to events/metrics
— §8). It is Phase 1 (§9).

### Shift 2 — A browser-embeddable replay engine

beamr-wasm 0.5.0 exposes a full live-engine JS seam: `create_vm`,
`spawn_actor`, `call_async` with awaited replies, async host I/O (fetch/OPFS),
and a `requestAnimationFrame` pump — i.e. **real OTP supervision and timers
running in a browser tab**. Haematite storage (OPFS WAL with IndexedDB
fallback), the WebSocket sync transport, and the sync codec all compile to
`wasm32` with **byte-identical parity to native** (proven by native
byte-parity tests). Liminal's subscriber and routing actors run on the
`WasmScheduler`.

Because the engine *and its storage* run in WASM, the dashboard can host a real
deterministic replay core **in the tab**:

- the execution scrubber (§5.2) becomes *real deterministic replay run
  locally*, not a TypeScript reconstruction of state;
- the reopen diff (§5.3) can *simulate* the reopen locally before the operator
  commits the real one;
- OPFS-persisted history can be inspected *offline*, with no server.

No existing workflow engine — Temporal, Inngest, Restate — has a
browser-embeddable replay core. This is the genuinely novel bet, and it is
Phase 2 (§9).

> **Honest caveat, stated up front and repeated where it bites (§8): the WASM
> paths are currently compile-gated and proven only by native byte-parity. They
> have *not* yet been exercised in a real headless browser.** The in-browser
> engine is therefore a *direction we design toward*, not a shipped fact. Every
> Shift-2 capability in this document is marked aspirational, and Phase 1 must
> not depend on any of it.

---

## 3. The observable surface — the data contract

This is the catalogue of every signal the dashboard consumes, and where each is
sourced. It is the load-bearing data contract; if a source is not yet a clean
feed, §8 flags it.

### 3.1 Workflow events (already produced — the 29 `Event` variants)

Source: the engine's append broadcast, fanned out by the server (cluster AW)
over the WebSocket event stream, plus the per-workflow history REST API. Wire
types come from `aion-core`/`aion-proto` via the house generated-types pipeline
(`src/types/generated/index.ts`; never hand-edited).

- **Lifecycle:** `WorkflowStarted` (carries `workflow_type`, `input`, `run_id`,
  `parent_run_id`, pinned `package_version`), `WorkflowCompleted`,
  `WorkflowFailed` (`WorkflowError`), `WorkflowCancelled` (reason),
  `WorkflowTimedOut` (timeout descriptor), `WorkflowContinuedAsNew`,
  **`WorkflowReopened`** (`run_id` + `reopened: ActivityId[]` — the steps to
  re-dispatch; the data behind the reopen diff, §5.3), `SearchAttributesUpdated`.
- **Activity:** `ActivityScheduled` → `ActivityStarted` →
  `ActivityCompleted` / `ActivityFailed` (`ActivityError` with
  `Retryable`/`Terminal` kind + one-based `attempt`) / `ActivityCancelled`,
  correlated by `ActivityId`.
- **Timer:** `TimerStarted` (`fire_at`), `TimerFired`, `TimerCancelled`,
  `WithTimeoutCompleted` (`WithTimeoutOutcome` + optional result), correlated by
  `TimerId`.
- **Signal:** `SignalReceived` (name + payload), `SignalSent`
  (target + name + payload).
- **Child workflow:** `ChildWorkflowStarted` (child id, type, input, pinned
  version), `ChildWorkflowCompleted`, `ChildWorkflowFailed`,
  `ChildWorkflowCancelled` — each linkable to the child's own detail view.
- **Schedule:** `ScheduleCreated`/`Updated`/`Paused`/`Resumed`/`Deleted`/
  `Triggered`.

That is **29 variants** (8 lifecycle, 5 activity, 4 timer, 2 signal, 4 child, 6
schedule) — the full generated `Event` union. Every view that maps over events
must be **exhaustive** over all 29 with a compile-time guard (an `assertNever`
default that fails the build, not at runtime) so the next type regeneration
surfaces a new variant loudly rather than throwing in a render path.

Each carries an `EventEnvelope` (`seq`, `recorded_at`, `workflow_id`). `seq` is
the strict ordering key; `recorded_at` is the determinism timestamp and the
only legitimate time source. Status is **projected** from these, not read from a
field (§1, invariant 4).

### 3.2 Outbox / dispatch signals (Shift 1)

Source: the durable outbox (`crates/aion-store/src/outbox.rs`) and the
dispatch/delivery path (`crates/aion-server/src/worker/{outbox_dispatcher,
outbox_delivery,outbox_reconciler,liminal_transport}.rs`).

- **Row lifecycle:** `OutboxStatus` ∈ `{Pending, Claimed, Done, Failed}`, keyed
  by `(workflow_id, ordinal)` (the `dispatch_key`). `Pending` → `Claimed`
  (dispatcher holds it; `claimed_at` set) → `Done` (delivered + acked) /
  `Failed` (retry budget exhausted — a dead letter). The dashboard renders this
  as the dispatch state of *individual work items*, with **attempt count** and
  **backoff** visible.
- **Reclaim:** rows `Claimed` past a staleness window are returned to `Pending`
  (owner death / hung dispatch). Each reclaim is a topology event worth showing.
- **Delivery acks & dedup hits:** the exactly-once mechanism — the ack that
  flips a row `Done`, and the dedup rejection that proves a duplicate dispatch
  was suppressed (one terminal per ordinal). Both are diagnostic gold for the
  failover story.

### 3.3 Worker registry & connection topology (Shift 1)

Source: the worker registry (`crates/aion-server/src/worker/registry.rs`) and
Liminal's connection-notifier seam.

- **Worker identity & membership:** `WorkerId`, the set of `namespaces` a
  worker serves, its `task_queue` (pool/flavour, e.g. norn vs claude), and an
  optional `node` locality.
- **Transport:** `WorkerDelivery` ∈ `{Grpc, Liminal}` — gRPC streaming vs
  Liminal server-push. The topology view distinguishes them.
- **Connection lifecycle:** connect / disconnect, surfaced cleanly via
  Liminal's `ConnectionNotifier` seam (the intended read-model feed — no
  liminal→aion coupling).
- **Typed death signals:** `disconnect` vs `timeout` worker-death distinction
  (`HeartbeatTracker::fail_disconnected_worker`) — *why* a worker's in-flight
  tasks were failed, not just *that* they were.

### 3.4 Node ownership & failover transitions (Shift 1)

Source: shard ownership (`crates/aion-server/src/run.rs`,
`outbox_reconciler.rs`) and the store's fenced-CAS path.

- **Shard assignment:** which shards each node owns (static SS-1 assignment from
  `[store] owned_shards`; empty = own-all single-node).
- **Shard adoption (failover):** a survivor adopting a dead owner's shard and
  re-residencing from history — the moment the cluster heals.
- **Fenced CAS rejections:** typed `StoreError::NotOwner` / `DatabaseError::
  Fenced` quorum-write rejections — the mechanism that prevents a fenced
  ex-owner from corrupting an adopted shard. Surfacing these makes the safety
  property *visible*.

> **Honest note (§8):** today, shard-ownership and adoption transitions are
> **logs only**. Promoting them to first-class events or metrics is a small,
> named server-side change this dashboard *depends on* for the failover
> visualization to be live rather than log-scraped. The outbox/worker/registry
> signals above are already structured.

### 3.5 Derived / computed (dashboard-side, never persisted)

- Per-workflow status projection, correlation groupings, lane assignment.
- Topology graph (nodes ↔ shards ↔ workers ↔ in-flight workflows).
- Replay state at a scrub point — server-reconstructed (Phase 1) or
  WASM-replayed (Phase 2, §5.2).

---

## 4. The core concepts (specced as final form)

These are the validated centerpiece concepts. Each is specified here as its
*end-state*, not a first cut.

### 4.1 Swimlane timeline — the centerpiece

**What it shows.** A single workflow's execution as a **partial order**:
activities, timers, signals, and child workflows each in their **own horizontal
lane**, time flowing left-to-right by recorded sequence. Concurrency is
rendered as **overlapping bars across lanes**, not flattened into one
chronological column. An activity is one bar spanning `ActivityScheduled` →
`ActivityStarted` → terminal, with retries shown as segmented attempts within
the bar (driven by `ActivityFailed.attempt`). Timers are bars from
`TimerStarted.fire_at` to `TimerFired`/`TimerCancelled`. Child workflows are
bars that drill into the child's own swimlane. The lifecycle lane carries
start / terminal / **reopen** markers.

**How it behaves.** Bars are selectable; selection opens a slide-out panel with
the decoded envelope and payload (best-effort JSON, raw fallback). Lanes
collapse/expand. The horizontal axis is the scrub track (§4.2). Live events
append in `seq` order and extend or close bars in place — a running workflow's
shape *grows*. De-duplication against history is by `seq` (the same event can
arrive via both the history fetch and the live stream around the subscription
boundary).

**Data it reads.** §3.1 in full. Correlation by `ActivityId` / `TimerId` /
`child_workflow_id`. Ordering strictly by `seq`.

**Why it's novel.** This is the ShiViz partial-order insight (§5.1) applied to
durable-workflow execution. Temporal's history view is a linear list; this
renders the concurrency structure the linear list destroys.

### 4.2 Execution scrubber

**What it shows.** A handle on the timeline's time axis. Drag it to any
sequence point and the **entire view reconstructs the workflow's logical state
at that point** — which activities had resolved, which timers were pending,
what the workflow's local state would have been. It is a debugger's step-back
for a durable execution.

**How it behaves (Phase 1, already-true direction).** The dashboard
reconstructs displayed state up to `seq = N` from the event prefix — a
faithful projection of *recorded* outcomes, no engine required. This is honest
and useful immediately.

**How it behaves (Phase 2, aspirational — Shift 2).** The scrubber is backed by
**real in-browser deterministic replay**: the WASM engine replays the workflow's
history prefix to `seq = N` and the view reflects the *actual* logical state the
engine would have held — not a UI reconstruction but the real thing, exact by
the determinism boundary (§1, invariant 2). This is the capability no other
engine has.

**Data it reads.** §3.1 (Phase 1); plus the WASM replay core over
OPFS/streamed history (Phase 2).

### 4.3 Reopen diff

**What it shows.** Reopen is the scariest operation (`WorkflowReopened`
re-drives a failed run from the failed step; see
`docs/WORKFLOW-REOPEN-DESIGN.md`). This view makes it **legible before you pull
the trigger**: a before/after diff showing which activities will **re-dispatch**
(the `reopened: ActivityId[]` set — terminal-failed steps with no later
success), which completed steps will be **reused from recorded results**, and
how the reset cursor **supersedes** the recorded failure. Green = preserved
work; amber = will re-run; struck-through = superseded terminal.

**How it behaves (Phase 1).** Computed from history: the dashboard derives the
reopened set the same way the engine does and renders the projected effect.
Issuing the reopen is a *command to the server* (§7) — the dashboard never
writes the `WorkflowReopened` event itself (invariant 3).

**How it behaves (Phase 2, aspirational — Shift 2).** The dashboard
**simulates the reopen locally** in the WASM engine — append a candidate
`WorkflowReopened` to an in-tab copy of history, replay, and show the *actual*
resulting execution — *before* committing the real reopen on the server. The
operator sees the consequence, then commits.

**Data it reads.** The failed run's history, `WorkflowReopened` semantics, the
cursor reset rule (§3.2 of the reopen design).

### 4.4 Three-AM view — triage-first

**What it shows.** The triage surface for "something's broken, where." Not a
dashboard of everything — the *one thing* wrong, foregrounded: the failing
workflow, the failing step, the error (`ActivityError` / `WorkflowError`), the
retry history, **the one action you'd take** (reopen, cancel, inspect worker),
and everything else cleared away. It answers "what do I do" before "what
happened."

**How it behaves.** A ranked surface of incidents: failed/stuck workflows,
dead/disconnected workers, `Failed` (dead-letter) outbox rows, fenced
rejections, shard-adoption events. Each incident is a card that opens directly
into the relevant deep view (swimlane, worker topology, reopen diff) and offers
the single most-likely action inline. Cleared incidents leave the surface.

**Why it's far more powerful now.** Shift 1 gives it a *real cluster* to
diagnose. Before, "broken" meant a failed workflow. Now it can show "node A
died, survivor B adopted shard 3, worker redialed, 4 items re-dispatched, all
terminal-once" — the failover story as a live, legible incident, not a log
archaeology dig.

**The calm state.** Triage-first does not mean broken-only. When nothing is
wrong, this surface is the ambient cluster heartbeat — nodes live, shards owned,
workers connected, throughput nominal — designed with the same care as the
incident cards (ADR-019). The operator who glances at a healthy cluster sees a
legible "all clear", not an empty page; the tool feels finished when nothing is
on fire, not only when something is.

**Data it reads.** §3.1 (failures), §3.2 (`Failed` rows), §3.3 (worker death),
§3.4 (node/shard/fenced); the healthy-state heartbeat reads the same topology
and throughput signals, read positively.

### 4.5 Event-level search

**What it shows.** An index over events so an operator can ask cross-cutting
questions: *"every activity that failed with error X across these workflows,"*
*"every workflow that reopened in the last hour,"* *"every dispatch that hit a
dedup rejection."* Results link back into the swimlane at the matching event.

**How it behaves.** Field-aware queries over event type, status projection,
activity type, error message/kind, time range, namespace, and (Shift 1) outbox
state and worker/node. Server-indexed for the live corpus; (Phase 2) the WASM
core can also search OPFS-persisted history offline.

**Why it's novel.** Temporal *literally cannot filter event history* — an open
bug for 20+ months. Because Aion's events are uniform, envelope-keyed, and
server-queryable, event-level search is essentially free for us and a genuine
differentiator.

### 4.6 Command palette & keyboard control

**What it shows.** A single keyboard-summoned palette (the universal `⌘K` / `/`
entry) that is the fastest path to everything: jump to a workflow by id or type,
run an event search, switch namespace, open the three-AM view, or issue an
operator action (reopen / cancel) on the focused workflow. Every primary surface
is reachable and operable without the mouse.

**How it behaves.** The palette is fuzzy, recency-aware, and context-sensitive —
its action list reflects the current selection (a focused failed workflow offers
"reopen"/"cancel"; a worker offers "inspect"). Keyboard navigation moves between
lanes and bars in the swimlane, between incidents in triage, and between results
in search, with consistent bindings surfaced inline. Mouse stays fully
supported; keyboard is simply never a second-class path.

**Why it's an outclass.** The consoles operators trust under pressure are
keyboard-first; Temporal's is mouse-driven and sparse. Making the whole console
operable from the keyboard, with a palette as the spine, is a direct
speed-of-triage advantage at 3 a.m. (ADR-017).

**Data it reads.** No new sources — it is an interaction layer over §3.1–§3.4
and the existing views.

### 4.7 Cluster map — the live work view

> **Status marker (read this first).** This concept is specified at its
> end-state. Its **observe/navigate layer (Tier 0)** and **cluster time-scrub
> (Tier 1)** are an *already-true direction* that rides directly on the Shift-1
> push channels (§8 / the cluster-event promotion); they need no new engine
> capability beyond that promotion. Its **operator-action layer (Tier 2)** is
> **aspirational**: every action below that mutates cluster state requires a
> *new, named server command that does not exist today* (drain, planned shard
> handoff, dead-letter redrive). Tier 2 is a designed direction, not a shipped
> fact, and Phase 1 must not depend on it. The decision to build the map and to
> design its command seam alongside the event seam is recorded as ADR-020.

**What it shows.** The cluster as a **deliberate map** (not a force-directed
hairball): nodes as fixed positions, the shards each owns as tiles, workers
docked to their node (tagged by `namespace × task_queue × node` and transport),
in-flight workflows as **tokens flowing along the edges** that are the real
Liminal dispatch channels. The exactly-once fan-out *spreads* before your eyes;
a dedup-suppressed duplicate is rendered as a token *absorbed at the boundary*
(the safety property made visible, not asserted). The failover beat becomes
**motion**: kill a node, its shard tile slides to the survivor, the in-flight
tokens re-route to the new owner and complete. This elevates today's
single-purpose `/failover` view into the general cluster view; `/failover`
becomes a focused preset of it.

**Tier 0 — navigate & drill (already-true direction).** Every element is a
deep-linked doorway (ADR-015): a workflow token → its swimlane (§4.1) at the
matching event; a node → its shards/workers/load/adoption history; a shard →
owner + adoption timeline; a worker → affinity, transport, in-flight tasks, and
*why* it died (the typed disconnect-vs-timeout signal, §3.3); an edge → the
channel + throughput + dedup-rejection count. Plus filter/heat overlays (load,
error-rate, dispatch latency; scope by namespace). All fed by the §3.2–§3.4
signals once they are pushed (§8).

**Tier 1 — scrub the cluster through time (direction).** The execution scrubber
(§4.2) applied to the whole cluster: drag back to *replay a failover* — node
dies, shard adopts, work re-routes — in the past. Leverages the recorded event
history; honest by the determinism boundary (§1, invariant 2).

**Tier 2 — act from the map (aspirational; engine-gated).** Operator actions,
strictly as *commands* the engine's single writer enacts (invariant 3 / §7),
each behind an explicit confirm and a **blast-radius preview** (hovering "drain
node 0" lights up exactly what will happen — which shard hands where, which
in-flight workflows re-dispatch — *before* you commit; the reopen-diff idea
(§4.3) applied to cluster ops). Spanning: cancel/reopen a workflow (§7, already
spec'd); **redrive** a dead-letter outbox row; **drain** a node (stop new work,
let in-flight finish, then safe to take down — graceful vs `kill -9`); **planned
shard handoff** (deliberately move a shard A→B using the existing epoch-fence /
`AcquireShard` machinery, rather than waiting for a crash); and a chaos
**kill-node** control for failover validation/demos. Drain and planned handoff
are the **control-plane north-star** — they turn the map from a monitor into the
operator surface for durable agents as infrastructure — but each is gated on a
new server command and is therefore *not* part of the Phase-1 baseline.

**Data it reads (and the seam this creates).** Tier 0/1 read §3.2–§3.4 over the
cluster-event push channel (§8). Tier 2 additionally requires a **command seam**
on the server. Because the push channels are being built now, the cluster-event
*event seam* and the operator-action *command seam* are designed **together**
(ADR-020) so the map is fed and operable from one coherent contract rather than
retrofitted later. The command seam itself ships incrementally as each named
command lands; until then Tier 2 surfaces are spec'd-but-disabled (§8).

**Why it's an outclass.** No existing engine console gives the operator a live,
legible model of the whole distributed system *with the consequence of an action
shown before it is taken*. Temporal et al. are observe-mostly; a map that
previews blast radius and drives drain/handoff is a different class of tool.

---

## 5. Why these concepts (the evidence)

### 5.1 Partial-order visualization beats linear logs

The swimlane bet is not aesthetic preference. The ShiViz line of research on
distributed-execution comprehension found partial-order (lane/branch)
visualizations significantly outperform linear logs on execution-understanding
tasks — reported at p = 0.00002 with a large effect size. A durable workflow is
exactly a partial order of concurrent activities, timers, signals, and
children; rendering that structure is the highest-leverage clarity decision the
dashboard makes.

### 5.2 The competitive gap

Temporal, Inngest, and Restate all render history as a linear list; none can
filter event history (Temporal's gap is a long-standing open bug); none has a
browser-embeddable replay core. The swimlane, event-search, and (Phase 2)
in-tab replay are not catch-up features — they are places we are ahead.

---

## 6. Architecture posture

### 6.1 Two execution modes, coexisting

- **Thin-client-over-WebSocket (Phase 1, the always-available mode).** The
  dashboard is a static SPA the server hosts (cluster AW). It reads list/query/
  history over REST and the live event + topology stream over WebSocket. It
  **computes projections** (status, lanes, correlation, reconstructed scrub
  state) but **reads all truth from the server**. This mode requires nothing
  from WASM and is the baseline for every deployment.

- **In-browser WASM engine (Phase 2, the novel mode).** The dashboard
  *additionally* hosts a real beamr engine + Haematite storage in the tab,
  enabling real-replay scrub, reopen simulation, and offline OPFS inspection
  (§5.2 capabilities). This mode is **additive**: it deepens the scrubber,
  reopen-diff, and search rather than replacing the thin-client paths. When the
  WASM core is unavailable (unverified-browser, no OPFS, etc.), the dashboard
  degrades cleanly to Phase-1 behavior — never a broken view.

The two modes share one data model (the generated wire types) and one set of
projections; the WASM core is a *more faithful* implementation of the same
scrub/diff semantics the thin client approximates.

### 6.2 Read vs compute vs command

- **Reads (truth):** event history, status projections' *inputs*, outbox/
  worker/topology signals — all from the server (or OPFS in Phase 2).
- **Computes (locally, derivable):** status projection, lane layout,
  correlation grouping, scrub-state reconstruction (Phase 1) / replay (Phase 2),
  reopen-diff preview, topology graph.
- **Commands (never direct writes):** reopen, cancel, and any future operator
  action are *requests to the server's command API* (AL's command surface). The
  engine's single writer enacts them. **The dashboard never appends to a live
  workflow's history — full stop (invariant 3).**

### 6.3 Live discipline

A single WebSocket manager with bounded-backoff reconnect; on drop, the active
view **resyncs** (re-fetch history / re-run query, or replay after last-seen
`seq` where the protocol supports an `after_seq` cursor). A persistent
connection indicator is always present; a dropped socket never leaves a
silently stale view. Namespace selection scopes every query and subscription.
These are non-negotiable: an operator who left the console open overnight must
wake to a live view.

Beyond liveness, the console is honest about its own **provenance**: it always
shows which node it is reading from and the last `seq` it has applied, and when
reads fail over to a survivor it says so explicitly ("viewing a survivor"). A
stale or post-failover view is never silent — the operator can always trust what
the screen claims to be (ADR-016).

### 6.4 House stack

React 19 + TypeScript + Vite + Tailwind v4 + Bun + Biome (100-char), shadcn/
Radix primitives, TanStack Query for server state, the singleton WebSocket
manager pattern — matching `apps/web` and the existing `apps/aion-dashboard`
scaffold, so the dashboard reads as part of the same codebase. Wire types are
generated, never hand-defined. The swimlane/scrubber rendering layer is the one
place that warrants bespoke, carefully-built visualization code; everything
around it stays house-conventional.

### 6.5 URL as state (deep-linking)

Every meaningful view state is URL-encoded so that a link reproduces it exactly:
namespace, selected workflow, selected event/bar, scrub `seq`, active search
query and filters, and the active view. Navigation is driven through the router;
component-local state holds only ephemeral UI (hover, transient focus), never
anything an operator would want to share or restore. This makes the console
shareable, bookmarkable, and back/forward-correct — the 3 a.m. handoff primitive
(ADR-015). Operator-action URLs (reopen/cancel) address the target but never
auto-execute; the action still requires an explicit confirm (§7).

### 6.6 Performance budgets

Performance is a committed contract, not best-effort. The swimlane renders large
histories (10k+ events) with virtualized lanes and stays interactive while live
events append; the triage surface paints the top incident within one screen on
first load; list and search results stream/virtualize rather than block. These
budgets are measured (not merely asserted) and a regression past them is treated
as a defect, not a tuning opportunity (ADR-018).

---

## 7. Operator actions (and the read-only boundary)

The original MVP was observability-only. The full-strength dashboard adds
**operator actions** — but strictly as *commands*, preserving invariant 3:

- **Reopen** a failed workflow (preceded by the reopen-diff preview, §4.3).
- **Cancel** a running workflow.
- (Future) signal a workflow; pause/resume a schedule.

Every action is issued to the server's command API and enacted by the engine's
single writer. The dashboard requests; it never writes history. The reopen-diff
*preview* and (Phase 2) *simulation* are read/compute-only; only the explicit
"commit reopen" button sends a command.

---

## 8. Honest risks, unknowns & dependencies

- **WASM substrate is incomplete, not merely browser-unverified (the headline
  risk).** All Shift-2 / Phase-2 capabilities (real-replay scrub, reopen
  simulation, offline OPFS inspection) rest on the WASM stack. A code audit
  (2026-06-29) found this is weaker than "compile-gated and native-parity-proven":
  the in-tab shard runtime (`WasmShardRuntime`) does **not currently compile** on
  its `wasm32` target, and the browser I/O backends (OPFS WAL, IndexedDB
  fallback, WebSocket sync transport) have **no executable tests** and carry
  known silent-failure defects (a WAL that acks writes before they are durable;
  a socket with no error/close handling). Only the codec / byte-parity layer is
  production-grade — and byte-parity proves the *framing* is identical to native,
  not that any browser I/O path works. Phase 1 must not depend on any of this.
  Phase 2 is gated on a genuine build-it-properly-and-exercise-live pass on the
  WASM stack (compile clean on-target, real headless-browser tests of every I/O
  path), not just a browser smoke test. Until then it is a designed direction,
  not a commitment.

- **Failover transitions are logs-only today.** Shard ownership and adoption are
  not yet first-class events/metrics (§3.4). The live failover visualization in
  the three-AM view depends on a small, named server-side promotion of those
  logs to a structured feed. Until then the dashboard can only show what the
  structured outbox/worker signals imply, not the ownership transition directly.

- **AW contract not fully pinned.** The exact REST paths, the WebSocket
  subscribe/resync protocol (including any `after_seq` cursor), the topology/
  outbox feed shape, and the asset base-path are AW's to define. The dashboard
  isolates each in one place (the api module) so a contract adjustment is a
  one-file change.

- **Command API surface for actions.** Reopen/cancel depend on AL's command
  endpoints existing and on a clear authorization model. Until then, actions are
  spec'd but disabled.

- **Topology feed coupling.** The intended clean feed is Liminal's
  `ConnectionNotifier` seam (no liminal→aion coupling). If a richer topology
  read-model is needed, it must preserve that boundary.

- **Payload decoding is best-effort.** Type-erased payloads mean the dashboard
  cannot guarantee a structured render; it must always fall back to the raw
  envelope without error.

- **Scrub/replay corpus size.** In-tab replay over a very long history has a
  cost; Phase 2 needs a strategy (prefix replay, checkpointing) — an open
  performance question, not a blocker for the design.

---

## 9. Phasing as build-order (not a reduction of ambition)

Phasing here is **sequencing**. The end-state is the whole of §1–§7; neither
phase narrows it.

**Phase 1 — Ops console (server-side, the always-available baseline).**
The swimlane timeline as the core view; the workflow list, detail, and live
feed; the full Shift-1 observable surface (outbox, worker topology, node/shard,
fenced/death signals — modulo the failover-event promotion of §8); the
three-AM triage view over a real cluster *including its designed calm state*
(§4.4); event-level search; the command palette and full keyboard control
(§4.6); the scrubber and reopen-diff in their *server-reconstructed* form;
reopen/cancel commands. The five professional-console disciplines of §1 —
deep-linking (§6.5), self-provenance (§6.3), keyboard-first interaction,
performance budgets (§6.6), and the designed calm state — are Phase-1
obligations, not later polish. The cluster map (§4.7) at its **Tier-0/1**
(observe, navigate, cluster time-scrub) is Phase 1 too — it rides on the §8
cluster-event push promotion and elevates the `/failover` view. This phase needs
no WASM. It showcases the failover work and is the baseline for every deployment.

**Phase 1.5 — Control plane (engine-gated, not WASM).**
The cluster map's **Tier-2** operator actions (§4.7) — drain, planned shard
handoff, dead-letter redrive — each gated on a *new server command* and shipped
incrementally as those commands land, behind confirm + blast-radius preview.
Distinct from Phase 1 only because it needs new engine surface, not because it is
less wanted; distinct from Phase 2 because it needs no WASM.

**Phase 2 — In-browser replay engine (the novel bet).**
Host the WASM engine + Haematite storage in the tab. Upgrade the scrubber to
real deterministic replay, the reopen-diff to local simulation-before-commit,
and search to include offline OPFS history. Gated on the WASM paths being
exercised live in a browser (§8). Additive — degrades to Phase 1 when
unavailable.

The build-order within each phase follows the engine's foundation-first
discipline; this document is the vision the per-cluster briefs will be authored
against.

---

## 10. Invariant compliance (recap)

1. **Type-erased events** — payloads decoded best-effort, raw fallback; no
   assumed payload schema. ✓
2. **Determinism boundary** — recorded `recorded_at`/`seq` are the only time/
   order truth; this is what makes exact replay-scrub possible. ✓
3. **Single writer** — read-only against live histories; all mutations are
   commands enacted by the engine's writer; the WASM tab engine operates on
   *copies* for simulation, never the live store. ✓
4. **Status is a projection** — computed last-event-wins, reset-aware; residency
   is a separate axis, never a status. ✓
5. **Content-hash namespacing** — displays the recorded pinned `package_version`,
   never a re-resolved latest. ✓

---

## 11. Relationship to existing artifacts

The early-June `DESIGN.md` and the **AU-001..009** briefs describe the
**stale, read-only monitoring MVP**: list + vertical-timeline detail + live
feed, observability-only, no ops console, no swimlane, no scrubber, no actions,
no WASM. This VISION supersedes that scope. **Reconciling the MVP design and
the AU briefs against this vision is a separate, named follow-on** — this
document deliberately does not rewrite them here; it sets the end-state they
will be re-aligned to.

The existing `apps/aion-dashboard` scaffold (React 19 / Vite 7 / Tailwind v4)
is **architecturally a valid Phase-1 foundation and is carried forward, not
discarded** — the WebSocket manager design, the contract-pinning pattern, the
skeleton/error/empty components, and the history+live `seq`-dedup are genuinely
good. A code audit (2026-06-29) found it was at that time **red across all three
gates** (`tsc`, Biome, tests): the timeline covered only 18 of 29 event variants
and threw at runtime on the rest; the generated types were truncated; and the
live-update path was dead-wired.

**Status update (2026-06-29, landed).** The named reconciliation pass has since
landed and is on `main`: the generated types are regenerated and the server now
serializes *exactly* the ts-rs-exported wire contract (list/detail/failover/
search/triage all render real data, no mocks); the live WebSocket feed works in a
real browser (the handshake-auth fix — credentials as query params, since
browsers can't set WS headers — ADR-016's provenance line and the `WsCaller`
server seam); a top-level/route error boundary + branded 404 exist; and the
dashboard gates are green (`tsc`/Biome/tests/build all pass). **One item from the
original pass remains open and is *not* yet done:** completing the timeline to all
29 event variants behind a compile-time `assertNever` exhaustiveness guard (§3.1)
— tracked in the console-disciplines work (ADR-018). Do not read the green gates
as proof of 29-variant exhaustiveness; that guard is still to be added.
