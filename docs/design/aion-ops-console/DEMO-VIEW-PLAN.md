# Aion failover demo — dashboard view build plan

Status: build-ready. Target: investor demo, ~next day.
Bar: house stack (React 19 / Vite 7 / Tailwind 4 / TanStack Query 5 / shadcn-Radix /
biome / bun), hand-plane aesthetic (VISION §1), **no silent failures**, every new file
≤ 500 lines, generated types **never hand-edited** (regenerate via the Rust exporter).

This is a **Tier-2 real view** built on the existing `apps/aion-dashboard` scaffold,
reusing its sound bones (resilient WS manager, seq-dedup, REST client, SPA hosting). It
is NOT a throwaway and it does NOT depend on the broken 29-variant timeline engine. A
bulletproof **Tier-1 fallback** (§7) is kept in parallel as the floor.

---

## 0. The one-line verdict (read this first)

**NO server-side change is required.** Every demo beat is tellable from signals that
already exist on the wire. The single genuinely-missing structured beat — the *adoption
moment* — is recoverable by inference (the killed node goes dark on `/health/live`; the
survivor's `ActivityCompleted` tally resumes climbing) and, if we want the exact instant
labelled, by a one-line log-tail of `adopted a downed peer's shards (SS-5b auto-failover)`.
We do **not** block the demo on promoting that log to an event (VISION §3.4/§8).

Optional, explicitly NOT on the critical path: the smallest honest server improvement is
a `aion_shard_adoptions_total{shard,peer}` counter bumped beside the existing
`cluster.rs:178` log line + a `GET /cluster/shards` ownership read. ~Few lines, one call
site. Build the no-server-change path; treat the counter as post-demo polish.

---

## 1. The view — component design (hand-plane layout)

One screen, route `/failover`, namespace-scoped to the demo namespace (default). Single
column of structure, top to bottom, every pixel earning its place:

```
┌─────────────────────────────────────────────────────────────────────────┐
│  AION · FAILOVER                            [● connected]   ns: demo      │  HeaderBar
├─────────────────────────────────────────────────────────────────────────┤
│  ┌───────────┐   ┌───────────┐   ┌───────────┐                            │
│  │ node 0    │   │ node 1    │   │ node 2    │                            │  NodeGrid
│  │ ● LIVE    │   │ ● LIVE    │   │ ● LIVE    │     (3 cards; one goes      │  (NodeCard ×N)
│  │ shard 0 ◀ │   │ shard 1   │   │ shard 2   │      DARK on kill, then     │
│  │ owner     │   │           │   │           │      shard 0 lights up on   │
│  │ wk: 1     │   │ wk: 0     │   │ wk: 0     │      an adopter)            │
│  └───────────┘   └───────────┘   └───────────┘                            │
├─────────────────────────────────────────────────────────────────────────┤
│  SHARD 0 OWNERSHIP   node 0 ──✕──▶ node 1   adopted @ +2.1s               │  AdoptionStrip
├─────────────────────────────────────────────────────────────────────────┤
│  FAN-OUT  collect_four            ▓▓▓▓░  3 / 4 tasks                       │  FanOutBar
├─────────────────────────────────────────────────────────────────────────┤
│                                                                           │
│          EXACTLY ONCE                                                      │  ExactlyOnceCounter
│              ▶  3 / 4                                                      │  (the headline)
│        completed activities · zero duplicates across the kill             │
│                                                                           │
├─────────────────────────────────────────────────────────────────────────┤
│  EVENT LOG                                                                 │  EventLog
│  +0.0s  WorkflowStarted        collect_four                               │  (flat, seq-ordered,
│  +1.5s  ActivityCompleted      ordinal 0                                  │   newest at top,
│  +3.0s  ActivityCompleted      ordinal 1                                  │   bounded ring)
│  +4.2s  ⚠ node 0 went dark     (health poll)                              │
│  +6.3s  ★ shard 0 adopted      node 1 (SS-5b)                             │
│  +7.8s  ActivityCompleted      ordinal 2                                  │
│  ...                                                                       │
└─────────────────────────────────────────────────────────────────────────┘
```

Aesthetic rules applied (VISION §1, §6.4):
- Dark-default theme already in `src/index.css` (`--background: #121218`). No gradients,
  glass, bokeh, chrome. State is shown with **color + shape + position**, not decoration.
- LIVE = solid accent dot; DARK = desaturated/struck card with a single "no response"
  line. The kill is read as a card visibly losing its pulse — function carries the drama.
- The owner card carries a `◀ owner` marker on its shard row; adoption is the marker
  *moving* to the adopter card. That movement IS the failover story, rendered as shape.
- The exactly-once counter is the largest type on screen — it is the load-bearing proof.
- Flat seq-ordered event log (NOT `projectTimeline`) so we never hit the `assertNever`
  throw on the 11 unhandled variants. Unknown variants render as a generic typed row.

### 1.1 Degradation — what each component does when a signal is missing

Honesty under pressure is the hand-plane principle. No component lies; each shows a
visible "degraded" affordance rather than a stale or fabricated value.

| Component | Primary signal | If missing → |
|-----------|----------------|--------------|
| NodeCard liveness | `GET /health/live` poll per node | dot → `unknown` (hollow), label "no health response"; never silently "LIVE" |
| NodeCard worker count | `/metrics` `aion_connected_workers` | worker row hidden (not zeroed); card still shows liveness |
| NodeCard shard/owner | static `owned_shards` config (seeded) + adoption inference | shows configured owner with a "pre-kill" tag; if config absent, "owner: unknown" |
| AdoptionStrip moment | inference (owner dark + survivor tally resumes) OR log-tail label | if neither, strip reads "adoption inferred — instant unavailable" but still shows old→new owner from liveness delta |
| FanOutBar | `ActivityCompleted` count from firehose WS, fallback `/workflows/describe` poll | if WS down, falls to poll; if both down, bar greys + "progress stream lost" badge |
| ExactlyOnceCounter | same as FanOutBar (dedup by `seq`) | shows last-known value with a stale badge; never blanks |
| EventLog | firehose WS, plus synthesized health/adoption rows | if WS down, shows reconnect banner + keeps history; on resync, re-merges by `seq` |
| ConnectionIndicator (header + per-card) | WS `onStatusChange` / `onDisconnect` | this IS the degradation surface — bound to visible state, never `console.warn` only |

The whole view is wrapped in an **error boundary** (§3) so a single unexpected event
variant or bad frame during the kill cannot white-screen the demo.

---

## 2. Exact data binding (endpoints / ws / per-beat source)

All against the demo cluster (`scripts/demo/demo-failover.sh`, default 3 nodes). Node `i`
HTTP = `8090+i`, gRPC = `50051+i`. The dashboard talks **HTTP + WS only** (no gRPC). Dev:
point `baseUrl`/WS at node 0 (`http://127.0.0.1:8090`); after a kill, the view fails over
its *own* reads to a survivor (node 1) automatically (§2.2).

### 2.1 Per-beat signal source (decision: derive from existing signals)

| # | Beat | Source the view reads | Mechanism |
|---|------|------------------------|-----------|
| 1 | Node up/down | `GET http://127.0.0.1:{8090+i}/health/live` per node | TanStack Query poll, `refetchInterval` 1000ms, per-node query key; a node that stops answering (timeout/non-200) → `dark` |
| 2 | Shard ownership (pre-kill) | seeded `owned_shards` (node `i` owns shard `i`) | static config in the view (the demo is deterministic: node 0 owns shard 0). No endpoint exists; documented gap. |
| 3 | Adoption moment | **inference** + optional log-label | owner dark (beat 1) AND survivor tally resumes (beat 6) ⇒ adoption; instant = first survivor `ActivityCompleted` after owner went dark. (Exact label via log-tail only if a dev-mode log endpoint is wired — NOT required.) |
| 4 | Worker connect/redial | `GET /metrics` per node, scrape `aion_connected_workers` | poll 2000ms; survivor's gauge rises post-kill. Shown as the card's `wk:` row. Non-load-bearing. |
| 5 | Outbox lifecycle / dedup hit | — | **not exposed, not needed.** Exactly-once is proven at the outcome layer (beat 6), not the outbox layer. Omitted from the view. |
| 6 | Exactly-once tally | firehose WS `ws://…/events/stream` → count distinct `ActivityCompleted` by `seq`; fallback `POST /workflows/describe` poll | headline counter + fan-out bar. Dedup-by-seq IS the "exactly once" made visible. |

### 2.2 Concrete wire details (pinned to the server contract)

- **WS firehose:** first frame `{"firehose":{"namespace":"demo"}}` (per
  `ws_subscription.rs`; the client `buildSubscribeMessage` already emits this). Frame
  shape: `{"namespace":"…","event":<WireEnvelope>}`; the aion-core `Event` is
  `{"type":"ActivityCompleted","data":{…}}` inside `event.payload`. Firehose carries **no
  resume cursor** — on reconnect the manager re-firehoses live and the view back-fills via
  describe (§2.3).
- **Describe (fallback + back-fill):** `POST /workflows/describe`, body
  `{namespace, workflow_id, run_id:null, include_history:true}`. **HTTP nesting caveat:**
  over HTTP the event type is `history[i].payload.data.type` and status is
  `summary.payload.data.status` — NOT top-level. The existing `client.ts`
  `readEnvelopePayload` already decodes `payload.bytes` (base64/byte-array) into the inner
  JSON, so `getHistory` yields decoded `Event[]`; the counter reads `.type` off those.
- **Count cross-check (optional):** `GET /workflows/count?namespace=demo` → `{count}`.
  Add one `countWorkflows` method to `ApiClient` if we want a REST sanity number; not
  required for the headline.
- **List (to discover the workflow id):** prefer `GET /workflows?namespace=demo` (plain
  JSON `Vec<WorkflowSummary>`, engine-internal filtered out) over `POST /workflows/list`
  (encoded envelopes). One small `getWorkflowsPlain` method, or seed the workflow id from
  the demo (`demo.state` `WORKLOAD_ID`) via a query param `?workflow=<id>` on `/failover`.
- **Own-read failover:** node-target is a small piece of view state (default node 0). A
  `useClusterTarget` hook polls all nodes' `/health/live`; if the current target goes
  dark, it switches the REST/describe target and the WS `baseUrl` to the first live
  survivor and reconnects. This is why dropping node 0 doesn't blind the dashboard.

### 2.3 Resync discipline (VISION §6.3 — no silently stale view)

- WS drop → `onStatusChange('reconnecting')` flips the header + affected node cards to a
  visible reconnecting state (bounded backoff already in the manager: 250ms→5s, 5 tries).
- On reconnect, firehose has no cursor, so the view's `onResync` handler triggers a
  `/workflows/describe` re-fetch and re-merges by `seq` (`mergeEventsBySequence`) — gap-
  free, duplicate-free. The counter is derived from the merged set, so a reconnect can
  never double-count (this is the exactly-once guarantee surfaced honestly).
- If the manager exhausts reconnects → status `disconnected` → the header shows a hard
  "stream lost" state and the counter shows a stale badge. Never blank, never fake-live.

---

## 3. Exact file plan

All paths under `/Users/tom/Developer/ablative/aion/apps/aion-dashboard/`.

### 3.1 Reusable bones — build ON these, do not touch (KEEP)
- `src/lib/api/websocket.ts` (`AionEventWebSocketManager`, `aionEventSocket`) — transport.
- `src/lib/api/websocket-protocol.ts` — `buildSubscribeMessage` (firehose), `parseFrame`.
- `src/lib/api/websocket-types.ts` — `FirehoseEventSubscriptionFilter` confirmed present.
- `src/features/live-feed/hooks/useEventSubscription.ts` — namespace-scoped subscribe.
- `src/features/live-feed/hooks/useConnectionStatus.ts` + `components/ConnectionIndicator.tsx`.
- `src/features/workflow-detail/lib/timeline.ts` — REUSE `mergeEventsBySequence`,
  `eventSequence`, `payloadSummary`, `decodePayload`. DO NOT use `projectTimeline`.
- `src/lib/api/client.ts` (`ApiClient`/`createApiClient`) — REST + envelope decode.
- `src/components/` + `src/components/ui/` — `StatusBadge`, `EmptyState`, `ErrorState`,
  `LoadingSkeleton`, badge/button/select/tooltip/skeleton (shadcn).
- `src/features/namespace/` — `NamespaceContext`, `useNamespace`, selectors.
- `src/app/{App.tsx,providers.tsx,main.tsx}` — shell + singleton WS connect.

### 3.2 Minimal scaffold fixes ON the critical path (do FIRST, these are blockers)
These are the only pre-existing-code edits; each is small and clears tsc/biome/test.

1. **Regenerate broken generated types** (do NOT hand-edit — biome reformats it, CLAUDE.md
   forbids). Add `TimerIdKind` and `WithTimeoutOutcome` to the export list in
   `crates/aion-core/src/generated_types.rs` (push_type list, ~lines 40-62), then run
   `cargo test -p aion-core export_dashboard_wire_types`. This rewrites
   `src/types/generated/index.ts` with the two missing names defined → tsc unblocked.
2. **`src/components/StatusBadge.tsx`** — add the missing `ContinuedAsNew` entry to
   `STATUS_BADGE_METADATA` (it is now in `WorkflowStatus`). One line; prevents tsc error +
   runtime `undefined.className`. (Only strictly needed if the view renders `StatusBadge`;
   include it — it's free and clears the typecheck.)
3. **`src/features/live-feed/index.ts`** — collapse the duplicate
   `export { ConnectionIndicator }` (lines 1 and 3) to a single export → unblocks the 4
   failing test loads that gate the build.
4. **Surface WS drops (no silent failures):** the manager already exposes
   `onStatusChange`/`onConnect`/`onDisconnect`. The view consumes them (via
   `useConnectionStatus` + a new `useNodeLiveness`); this is *wiring*, not a manager fix.
   No `console.warn`-only paths reach the operator.

NOTE on the "dead-wired" live hooks (`useLiveWorkflowEvents`, `useLiveListUpdates`): the
failover view does **not** need them — it calls `useEventSubscription` with a firehose
filter directly. So "wire the dead hooks" reduces to: use the subscribe hook the view
actually needs. No fix to those two hooks required for this view.

### 3.3 New files for the view (each ≤ 500 lines; most ≪ 200)

Feature folder: `src/features/failover/`

| File | Purpose | Est. lines |
|------|---------|-----------:|
| `src/features/failover/FailoverView.tsx` | top-level view; composes the sections, owns the error boundary mount + namespace gate | ~120 |
| `src/features/failover/components/HeaderBar.tsx` | title + global ConnectionIndicator + namespace + active-node target | ~60 |
| `src/features/failover/components/NodeGrid.tsx` | lays out N `NodeCard`s | ~50 |
| `src/features/failover/components/NodeCard.tsx` | one node: liveness dot, shard row, owner marker, worker count | ~120 |
| `src/features/failover/components/AdoptionStrip.tsx` | old→new owner + adoption instant (or degraded label) | ~80 |
| `src/features/failover/components/FanOutBar.tsx` | progress bar `done/arity` | ~60 |
| `src/features/failover/components/ExactlyOnceCounter.tsx` | the big headline counter + stale badge | ~70 |
| `src/features/failover/components/EventLog.tsx` | flat seq-ordered bounded log incl. synthesized health/adoption rows | ~140 |
| `src/features/failover/hooks/useNodeLiveness.ts` | per-node `/health/live` poll → `live/dark/unknown` + active-target failover | ~110 |
| `src/features/failover/hooks/useNodeMetrics.ts` | per-node `/metrics` poll → parse `aion_connected_workers` | ~90 |
| `src/features/failover/hooks/useFanOutProgress.ts` | firehose subscribe + describe back-fill → `mergeEventsBySequence` → `{events, completedCount, arity, status, stale}` | ~180 |
| `src/features/failover/hooks/useAdoptionMoment.ts` | derive adoption from liveness-dark + tally-resume; expose instant + degraded flag | ~90 |
| `src/features/failover/lib/clusterConfig.ts` | static node/shard/owner seed + ports (8090+i); reads `?nodes=`/`?workflow=` query params | ~70 |
| `src/features/failover/lib/exactlyOnce.ts` | count distinct `ActivityCompleted` by ordinal/seq; pure, unit-tested | ~80 |
| `src/features/failover/index.ts` | barrel | ~10 |
| `src/components/FailoverErrorBoundary.tsx` | class error boundary → `ErrorState`, never white-screen | ~70 |
| `src/features/failover/lib/exactlyOnce.test.ts` | bun test: dedup-by-seq, no double-count on replay | ~120 |
| `src/features/failover/hooks/useFanOutProgress.test.ts` | bun test: firehose+describe merge, reconnect no double-count | ~140 |

Plus one tiny edit each to:
- `src/app/routes.tsx` — add `{ path: '/failover', element: <FailoverRoute /> }`.
- `src/lib/api/client.ts` — add `getWorkflowsPlain` (`GET /workflows`) and optional
  `countWorkflows` (`GET /workflows/count`). ≤ 30 lines, stays ≤ 500 total.

Largest new file ~180 lines — comfortably under the 500-line bar.

---

## 4. Ordered build sequence (each step has a real gate)

Each gate = `tsc --noEmit` + `biome check` + `bun test` green on the touched surface, and
where stated, a live run against `scripts/demo/demo-failover.sh`.

1. **Unblock the build (§3.2).** Regenerate types, fix StatusBadge + live-feed barrel.
   Gate: `bun run typecheck` and `biome check .` green; `bun test` loads all files.
2. **Cluster config + ports + REST additions.** `clusterConfig.ts`, `getWorkflowsPlain`,
   `countWorkflows`. Gate: typecheck + a `curl`/manual fetch against a booted node returns
   the workflow list.
3. **Node liveness + own-read failover.** `useNodeLiveness` + `NodeCard`/`NodeGrid` +
   `HeaderBar` + route. Gate: live run — 3 cards LIVE; `kill-owner.sh` → node 0 card goes
   DARK within ~1s; REST target auto-switches to a survivor.
4. **Fan-out progress + exactly-once (THE RISKIEST STEP, §4.1).** `exactlyOnce.ts` +
   `useFanOutProgress` + `FanOutBar` + `ExactlyOnceCounter`, firehose with describe back-
   fill and seq-dedup. Gate: unit tests green (dedup, reconnect no double-count) AND live
   run — counter climbs 0→4 and reaches exactly 4 *across a kill*, never 5.
5. **Adoption derivation + event log.** `useAdoptionMoment`, `AdoptionStrip`, `EventLog`
   with synthesized health/adoption rows. Gate: live run — strip shows node0→survivor and
   an adoption instant; log shows the dark + adoption + resumed-completion sequence.
6. **Resilience pass (no silent failures).** Error boundary mount; bind WS status to
   header + cards; verify reconnect resync re-merges by seq; degraded labels for every
   missing signal (§1.1). Gate: kill the WS mid-run (or kill node 0) → visible
   reconnecting state, no white screen, counter never double-counts on resync.
7. **Aesthetic + a11y polish.** Spacing, type scale (counter is the largest), dark-theme
   tokens, tooltips, focus states. Gate: `biome check` green; manual 3am-on-call read —
   can a stranger see what's broken and that work survived exactly once, in 5 seconds?
8. **Ship path.** `bun run build` → either populate `crates/aion-server/dashboard-embed/`
   from `dist/` (embedded hosting; currently a 66-byte stub) OR run `bun run dev` against
   node 0 with `[dashboard] source = FileSystem{asset_path=dist}`. For the demo, `bun run
   dev` pointed at the live cluster is the lowest-risk path. Gate: full rehearsal —
   `demo-failover.sh` then `kill-owner.sh` with the dashboard open; the screen tells the
   whole story unattended.

### 4.1 Riskiest step — Step 4 (exactly-once across the kill)
This is the headline and the only place correctness is load-bearing. Risks:
- Firehose has no resume cursor, so a reconnect during the kill must back-fill via
  describe and re-merge by `seq` — a naive append would double-count and show "5/4",
  which would be catastrophic on stage. Mitigation: counter derives ONLY from the
  seq-deduped merged set (`mergeEventsBySequence` + `exactlyOnce.ts`), proven by unit
  tests that replay overlapping history+live frames.
- HTTP describe nesting (`payload.data.type`) differs from the doc's top-level `type`;
  reading the wrong path yields 0/4. Mitigation: rely on `client.ts` envelope decode
  (already returns decoded `Event[]`), and a unit test pins the decoded shape.
- The workflow id must be known. Mitigation: seed via `?workflow=<WORKLOAD_ID>` (from
  `demo.state`) or discover via `getWorkflowsPlain`; fall back gracefully if absent.
Build and gate Step 4 with unit tests BEFORE the live run so the stage run only confirms.

---

## 5. Effort estimate (Tier-2) + the Tier-1-vs-Tier-2 question

**Estimate: ~1.5–2 focused engineering days** for the Tier-2 view to demo quality.
- Scaffold unblock (§3.2): ~1–2h (types regen is the fiddly bit, it's a Rust test run).
- Liveness + cards + route + own-read failover: ~3–4h.
- Fan-out + exactly-once with tested dedup (the risky core): ~4–5h.
- Adoption derivation + event log: ~2–3h.
- Resilience + error boundary + degraded states: ~2h.
- Aesthetic/a11y polish + ship/rehearsal: ~2–3h.

**Is Tier-2 ~the same effort as a throwaway Tier-1, and how much more planning?**
- **Build effort:** Tier-2 is modestly more than a throwaway — call it ~1.3–1.5×, not 2–3×.
  The reason is that the hard, expensive parts (resilient WS manager, seq-dedup, REST
  client + envelope decode, SPA hosting, design tokens) are **already built and good** in
  the scaffold. A throwaway Tier-1 that read the same status surface honestly (poll +
  reconnect + no-double-count) would have to re-implement most of that resilience anyway,
  or be visibly fragile on stage. Building on the bones gets the resilience for near-free,
  so Tier-2's marginal cost is mostly the failover-specific components + tests.
- **Planning:** Tier-2 needed meaningfully more up-front planning — this document. The
  extra planning is exactly: (a) confirming the wire contract so we don't discover the
  `payload.data.type` nesting on stage, (b) deciding no-server-change vs a named change,
  (c) the seq-dedup-on-reconnect correctness argument, and (d) the per-signal degradation
  table. That is ~half a day of scoping (already spent in the parallel scope phase + this
  doc). The payoff: the build is now mechanical and low-surprise, and the artifact is
  reusable as the Phase-1 ops-console slice (VISION), not thrown away.
- **Bottom line:** spend the planning, build Tier-2. It is the better investment *because*
  the scaffold already paid for the resilience a credible live demo demands.

---

## 6. Risk register (one-glance)

| Risk | Likelihood | Mitigation |
|------|-----------:|------------|
| Double-count on WS reconnect → "5/4" on stage | med | seq-dedup merged-set only; unit-tested replay (Step 4) |
| Generated-types regen fails / drifts | low | it's a Rust test; run early (Step 1); fall back to a clean regen, never hand-edit |
| Dashboard blinded when node 0 dies | med | own-read failover to survivor in `useNodeLiveness` (Step 3) |
| Adoption instant not crisp (logs-only beat) | med | inference label; honest "instant unavailable" degrade; optional counter as polish |
| Unexpected event variant white-screens | low | error boundary + flat log (no `projectTimeline`/`assertNever`) |
| embedded dashboard stub ships empty | low | demo via `bun run dev` against live cluster (Step 8) |

---

## 7. Tier-1 minimal fallback (the bulletproof floor — keep in parallel)

A single standalone screen, same status surface, that cannot fail to render. Build this
FIRST as a ~2-hour insurance policy; keep it even after Tier-2 lands.

- **One file**, plain React + the existing `ApiClient` + `aionEventSocket`, no new feature
  folder, no generated-types dependency on the broken variants (it reads only `health`,
  `metrics`, and `describe`/firehose `ActivityCompleted` count — a tiny typed subset).
- **Layout:** a stacked list — N node liveness rows (green/red from `/health/live` poll),
  one big `done/4` exactly-once number (from describe-poll `ActivityCompleted` count,
  deduped by ordinal), and a plain text event tail. No swimlanes, no shape, just legible
  truth.
- **Resilience floor:** pure polling (1s) for liveness + tally so it works even if the WS
  is entirely down; the WS is an enhancement, not a dependency. Polling describe and
  counting distinct ordinals can never double-count.
- **Hosting:** the same `bun run dev` path. If Tier-2 hits a wall the morning of the demo,
  this screen alone still shows: 3 nodes, one dies, the survivor finishes the work, count
  lands exactly on 4. That is the entire story, floor-guaranteed.

File: `src/features/failover/FailoverFallback.tsx` (~180 lines, self-contained) reachable
at `/failover?mode=fallback`. Shares `clusterConfig.ts` + `exactlyOnce.ts` with Tier-2 so
the floor and the real view agree on the numbers.

---

## 8. Acceptance bar (definition of done)
- `bun run typecheck`, `biome check .`, `bun test` green on the touched surface.
- Live rehearsal: `demo-failover.sh` then `kill-owner.sh` with the view open — node card
  goes dark, ownership marker moves, fan-out completes, counter lands **exactly 4** (never
  5), event log narrates the kill+adoption+completion, no white screen, no silent stale.
- House stack + hand-plane aesthetic respected; all new files ≤ 500 lines; generated types
  regenerated (never hand-edited); no server-side change required.
