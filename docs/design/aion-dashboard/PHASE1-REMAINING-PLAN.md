# Phase-1 Aion Ops Console — Remaining Build Plan

Status: **build-ready**, fan-out-ready. Authoritative plan for COMPLETING the full
Phase-1 ops console, excluding the failover view (slice 1) and the scaffold
reconciliation, which are landing in a separate build right now.

Date: 2026-06-29
Owner: Tom (orchestrating; fan-out to per-view subagent builds)
Bar (every slice must honor): house stack (React 19 / Vite 7 / Tailwind 4 /
TanStack Query 5 / shadcn-Radix / biome / bun); hand-plane aesthetic (VISION §1);
**no silent failures** (VISION §6.2/§6.3); generated types **never hand-edited**
(regenerate via the Rust ts-rs exporter, CO2); every new file ≤ 500 lines (CO,
DEMO-VIEW-PLAN convention); feature-folder convention (CO4); server state via
TanStack Query, live via the single WS manager (CO5).

---

## 1. Framing

### 1.1 What "full Phase-1" is

Phase-1 is the **server-side ops console** (thin-client-over-WebSocket): everything in
SPEC §`phase1Views` items 1–12 that ships without the WASM engine. The in-browser WASM
engine, real deterministic replay, OPFS offline inspection — all Phase-2, all OUT
(SPEC `inOutBoundary`). Phasing is sequencing only; the end-state ambition is unchanged.

### 1.2 What is ALREADY covered (do not rebuild)

Two builds are in flight and OWN these files. This plan must not touch them:

- **Reconciliation pass** (landing now) — makes the scaffold green. Owns the
  post-reconcile baseline of: `src/features/workflow-detail/lib/timeline.ts`
  (generic-entry fallback, no `assertNever` throw — verified at lines 53–58),
  `src/features/workflow-detail/hooks/useLiveWorkflowEvents.ts`,
  `src/features/workflow-list/hooks/useLiveListUpdates.ts`,
  `src/features/live-feed/hooks/useEventSubscription.ts` (afterSeq/lastSeenSequence
  resumption + onResync threading), `src/types/generated/index.ts` (all 29 variants —
  confirmed present, incl. `WorkflowReopened`, `WorkflowContinuedAsNew`,
  `SearchAttributesUpdated`, the 6 `Schedule*`, `WithTimeoutCompleted`), and minor
  cosmetic touches to `StatusBadge.tsx`, `EventIcon.tsx`, `LoadingSkeleton.tsx`,
  `ConnectionIndicator.tsx`, `client.ts`, `websocket.ts`, plus all `*.test.*` snapshots.
  **Treat every reconciliation-touched file as the new baseline — NOT a stub.**

- **Failover view (slice 1)** (landing now, per `DEMO-VIEW-PLAN.md`) — owns the entire
  `src/features/failover/` folder (already on disk: `lib/clusterConfig.ts`,
  `lib/exactlyOnce.ts` + tests) and will add the `/failover` route + its NodeGrid /
  AdoptionStrip / FanOutBar / ExactlyOnceCounter / EventLog components. It is a
  self-contained Tier-2 view that deliberately does NOT use the 29-variant timeline.

### 1.3 What REMAINS (this plan)

Verified against the live tree (`src/` walk 2026-06-29):

1. **Live-path wiring** — `useLiveWorkflowEvents` and `useLiveListUpdates` are DEAD-WIRED
   (confirmed: `WorkflowDetail.tsx` line 20 calls only `useWorkflowHistory` then
   `projectTimeline(history)` at line 76; never imports the live hook. `WorkflowList`
   never imports `useLiveListUpdates`). The hooks are reconciled and type-safe but have
   **zero consumers**. This is audit finding C4.
2. **Workflow DETAIL completion** — currently renders a flat chronological
   `projectTimeline` list, not the swimlane. Needs the live hook wired, selection/detail
   panel, and the 29-variant exhaustiveness made first-class (today only ~15 variants get
   a dedicated arm; signals partial, the 6 schedule variants + Reopened + ContinuedAsNew +
   SearchAttributesUpdated + WithTimeoutCompleted all fall through to the generic arm).
3. **SWIMLANE partial-order viz (§4.1, the centerpiece)** — does not exist. The detail
   view today is a linear `<ol>`.
4. **Execution Scrubber (§4.2, server-reconstructed form)** — does not exist.
5. **Reopen Diff (§4.3, computed form)** — does not exist; command API gated (§8).
6. **Three-AM Triage view (§4.4)** — does not exist; partially server-gated (§8).
7. **Event-Level Search (§4.5)** — does not exist; depends on server query surface.
8. **Workflow LIST completion** — list renders + filters + paginates; needs the live
   hook wired (C4) and pagination/live-insert interaction hardened.
9. **App SHELL / NAV / routing** — only 2 routes (`/`, `/workflows/:id`). Needs
   `/search`, `/incidents`, and a persistent nav surface; error boundary exists
   (`RootErrorBoundary.tsx`) — verify it's wired in `App.tsx` (M2).
10. **Live discipline hardening** — WS-drop resync per active view, surface WS errors to
    UI not `console.warn` (M1), shared-socket survives route churn (M3).

### 1.4 The hard dependency

**Nothing in this plan starts until the reconciled foundation verifies green.**
The gate is: on the reconciliation branch merged to the dashboard baseline,
`bun run typecheck` clean, `bun run check` (biome) clean, `bun test` green. The live
hooks, the 29-variant generated types, and the WS resync threading are the substrate
every slice below binds to. Building against a pre-reconcile tree guarantees rework.

---

## 2. Slice list (each = self-contained vertical slice)

Slices are sized to ≤ ~5 files of new/edited code, each file ≤ 500 lines, each with its
own gate. Shared-file edits are isolated into **two serialized integration slices** (S0,
S1) so the per-view slices can run fully parallel.

Gate vocabulary (every slice):
- **TC** = `bun run typecheck` (`tsc --noEmit`) clean.
- **LINT** = `bun run check` (biome lint+format) clean.
- **TEST** = `bun test` green (slice ships its own tests).
- **LIVE** = manual/headless run against a local Aion server: WS frame observed in
  DevTools Network, live append/patch observed without refetch (where relevant).

---

### S0 — Foundation verify + Shell/Nav scaffold (SERIAL, runs first)

**Scope.** Confirm the reconciled green bar; establish the app shell, nav surface, and
route table that the parallel view-slices plug into. This slice OWNS all shared shell
files so no later slice contends on them.

**Files to edit/create (shared — owned only here):**
- `src/app/routes.tsx` (edit, ~90 lines) — add `/search` → `SearchRoute`,
  `/incidents` → `IncidentsRoute`; keep `/`, `/workflows/:id`; export `searchPath`,
  `incidentsPath`, and `*Href` helpers. Route components stay thin (read `useNamespace`,
  render the feature root). Lazy-import view roots so a feature build failure cannot break
  unrelated routes.
- `src/app/App.tsx` (edit, ≤ 120 lines) — verify `RootErrorBoundary` wraps
  `RouterProvider` (M2); add persistent left nav / breadcrumb surface (List / Search /
  Incidents / Failover links + namespace selector + connection indicator already in
  header). Hand-plane: text-link nav, no chrome.
- `src/app/providers.tsx` (edit, ≤ 80 lines) — verify single `AionEventWebSocketManager`
  instance is provided once and NOT torn down on route change (M3); add no new providers.
- `src/components/AppNav.tsx` (create, ≤ 90 lines) — presentational nav list, active-route
  highlight, keyboard-navigable. Pure; route paths injected.

**Data binding.** None new; consumes existing `NamespaceContext` + WS provider.

**Live-wiring.** Asserts the WS manager singleton survives `react-router` navigation
(M3 regression test).

**Gate.** TC + LINT + TEST (route-render test for all 4 routes mounting without crash;
nav active-state test; M3 socket-survives-navigation test). No LIVE needed.

**Why serial / first.** It is the only slice that edits `routes.tsx`, `App.tsx`,
`providers.tsx`. Every parallel slice imports route helpers from `routes.tsx`; they need
the path constants to exist. S0 publishes the contract; the rest consume it.

---

### S2 — Workflow LIST: complete + wire live (PARALLEL)

**Scope.** Wire the dead `useLiveListUpdates` hook into the list; harden pagination ×
live-insert; ensure loading/empty/error states are exhaustive.

**Files (disjoint folder `src/features/workflow-list/`):**
- `src/features/workflow-list/components/WorkflowList.tsx` (edit, ~180 lines) — after the
  existing `useWorkflowQuery` call (lines 38–42) add:
  ```tsx
  import { useLiveListUpdates } from '../hooks/useLiveListUpdates';
  // ...
  useLiveListUpdates({ filter, page: { cursor: pagination.cursor, limit: normalizedPageLimit } });
  ```
  No change to `WorkflowListBody` — the hook patches the React Query cache for
  `workflowListQueryKey(ns, filter, page)` in place; the table re-renders automatically.
- `src/features/workflow-list/hooks/useLiveListUpdates.ts` (edit, small) — only if the
  pagination-interaction fix (below) requires it: on a non-first page, suppress top-insert
  of new `WorkflowStarted` rows (or mark them as "new above" rather than mutating the
  visible page), avoiding the cursor-boundary UX break (risk register R3).
- `src/features/workflow-list/index.ts` (edit) — export `useLiveListUpdates` +
  `LiveListUpdatesOptions` for testability.
- `src/features/workflow-list/components/WorkflowList.test.tsx` (edit) — add: live patch
  updates a row's status without refetch; new `WorkflowStarted` inserts at top on page 1;
  on page 2 it does NOT corrupt the cursor view.

**Data binding.** REST `POST /workflows/list` (`client.ts` `queryWorkflows`,
request `{namespace, filter:{workflow_type,status,started_after,started_before,parent},
cursor, limit:50}`; response normalized by `normalizeWorkflowPage` — handles direct-array
and `{items,next_cursor,has_more}` envelope, plus the `payload.bytes` nesting via
`readEnvelopeArray`). WS filtered subscription `{kind:'filtered', namespace, workflow_type,
status, after_seq}` via `useEventSubscription`.

**Live-wiring.** `useLiveListUpdates` → `useEventSubscription(filter='filtered')` →
`patchWorkflowPage` patches existing summaries (status/timing) and inserts new
`WorkflowStarted`; `onResync` invalidates the query.

**Gate.** TC + LINT + TEST + LIVE (open `/`, start a workflow on the server, see the row
appear and transition Running→Completed live without refresh; confirm one `filtered`
subscribe frame in Network).

---

### S3 — Workflow DETAIL + 29-variant timeline: complete + wire live (PARALLEL)

**Scope.** Wire the dead `useLiveWorkflowEvents` hook; make the timeline rendering
first-class over all 29 variants with a compile-time exhaustiveness guard; add the
selection slide-out detail panel (decoded envelope + best-effort payload). This slice
delivers the **non-swimlane** linear-timeline completion; S4 layers the swimlane on top of
the same projection engine.

**Files (disjoint folder `src/features/workflow-detail/`):**
- `src/features/workflow-detail/components/WorkflowDetail.tsx` (edit, ~110 lines) —
  add `useLiveWorkflowEvents({ enabled: historyQuery.isSuccess, history: historyQuery.data
  ?? [], workflowId })` after the history query; replace
  `<EventTimeline entries={projectTimeline(history)} />` (line 76) with
  `<EventTimeline entries={liveEvents.timeline} />`; surface `liveEvents.isTerminal` /
  `terminalOutcome` in the header.
- `src/features/workflow-detail/lib/timeline.ts` (edit, ≤ 500; currently ~524 — keep under
  by extracting helpers to a new file if needed) — promote the 14 currently-generic
  variants to dedicated arms where they carry a lane (SignalSent, the 4 Child arms already
  patched, WithTimeoutCompleted → timer lane close, WorkflowReopened/ContinuedAsNew/
  SearchAttributesUpdated → lifecycle markers). The 6 `Schedule*` variants project to
  typed `GenericTimelineEntry` rows with a schedule sub-kind. CRITICAL: keep the
  reconciliation's safe default arm (no runtime throw) BUT add a **compile-time**
  `assertNever` in a separate exhaustiveness checker so a NEW server variant fails
  `tsc`, not runtime (SPEC handPlane rule 4): a `function assertAllVariantsHandled(e:
  never)` referenced from a `switch` over a `KnownEventType` union that is derived from
  the generated `Event['type']`. Build breaks on next regeneration if a handler is missing.
- `src/features/workflow-detail/components/DetailPanel.tsx` (create, ≤ 160 lines) —
  slide-out for a selected entry: full decoded envelope (seq, recorded_at, workflow_id,
  type) + `PayloadView` (best-effort JSON, raw-bytes fallback). Selection state lifted to
  `WorkflowDetail` (selected seq).
- `src/features/workflow-detail/lib/timelineVariants.ts` (create, ≤ 120 lines) — the
  `KnownEventType` union + `assertNever` exhaustiveness checker, isolated so the
  build-guard is unit-testable and timeline.ts stays under 500 lines.
- `src/features/workflow-detail/__tests__/timeline.test.ts` (edit) — add one assertion per
  variant family confirming each of the 29 produces a typed entry (no generic-fallback for
  the variants that now have lanes); add a "decode failure → raw envelope, no throw" test.

**Data binding.** REST `POST /workflows/describe` (`getHistory`, request
`{namespace, workflow_id, run_id:null, include_history:true}`; response normalized by
`normalizeHistory`, sorted by seq, `payload.bytes` decoded). WS per-workflow subscription
`{kind:'workflow', namespace, workflow_id, after_seq}`.

**Live-wiring.** `useLiveWorkflowEvents` → `useEventSubscription(filter='workflow')` →
`mergeEventsBySequence(history, live)` (dedup by `envelope.seq`) → `projectTimeline` →
re-render; subscription disabled once `terminalOutcomeForEvents !== null`.

**Gate.** TC + LINT + TEST + LIVE (open `/workflows/:id` on a running workflow; new
ActivityScheduled/Completed events append in seq order without refresh; one `per_workflow`
subscribe frame; kill+regenerate-types dry-run: temporarily add a fake variant to the
union and confirm `tsc` fails at `assertAllVariantsHandled`).

---

### S4 — SWIMLANE partial-order visualization (§4.1) (PARALLEL, depends on S3 projection)

**Scope.** The centerpiece. Render the workflow as concurrent horizontal lanes keyed on
`seq`, not a linear list. Reuses S3's `projectTimeline` output (TimelineEntry union) —
does NOT re-parse events. Bars selectable → reuse S3 `DetailPanel`. Lanes collapse/expand.

**Files (new subfolder `src/features/workflow-detail/swimlane/`):**
- `src/features/workflow-detail/swimlane/Swimlane.tsx` (create, ≤ 220 lines) — lane
  container; computes lane assignment from TimelineEntry union (lifecycle lane top;
  per-ActivityId / per-TimerId / per-child / signal lanes); x-position by seq rank;
  collapse/expand per lane type; horizontal axis is the scrubber mount point (S5).
- `src/features/workflow-detail/swimlane/LaneBar.tsx` (create, ≤ 160 lines) — one bar:
  activity bars segmented by attempt (from `ActivityFailed.attempt`, one-based); timer bars
  span TimerStarted→Fired/Cancelled; child bars drill into `/workflows/:childId`. Color =
  event kind (consistent with `EventIcon` tone map); selectable.
- `src/features/workflow-detail/swimlane/laneLayout.ts` (create, ≤ 180 lines) — pure
  lane-assignment + x-rank-by-seq math; fully unit-testable; no DOM.
- `src/features/workflow-detail/swimlane/Swimlane.test.tsx` (create) — lane assignment,
  attempt segmentation, concurrent overlap rendering, child drill-link, live-append extends
  a bar in place.

**Integration into the detail view.** `WorkflowDetail.tsx` gains a view toggle (List ⇄
Swimlane) — this is the ONE shared edit S4 makes inside S3's `WorkflowDetail.tsx`; to
avoid contention, S3 lands first and exposes a documented `view` prop / slot, OR S4's
toggle is added as a wrapper component `WorkflowDetailView.tsx` that S0's route renders.
**Decision: S4 ships `WorkflowDetailView.tsx` (create, ≤ 80 lines) wrapping S3's
`WorkflowDetail`, and S0's `/workflows/:id` route renders the wrapper.** Zero edit to
S3 files. (Coordination note in §3.)

**Data binding.** None new — consumes S3's projected timeline + live events.

**Live-wiring.** Lives entirely on S3's `useLiveWorkflowEvents` output; bars extend/close
as seq grows.

**Gate.** TC + LINT + TEST + LIVE (swimlane shows concurrent activity + timer lanes
overlapping; a retried activity shows segmented attempts; child bar links to child detail;
live event extends a bar without refresh).

---

### S5 — Execution Scrubber (§4.2, server-reconstructed) (PARALLEL, depends on S4)

**Scope.** Drag-handle on the swimlane time axis; scrubbing to `seq=N` reconstructs the
displayed state from the event prefix (faithful projection of recorded outcomes — no
engine replay; that's Phase-2).

**Files (new, `src/features/workflow-detail/swimlane/`):**
- `src/features/workflow-detail/swimlane/Scrubber.tsx` (create, ≤ 140 lines) — draggable
  handle bound to seq; position is the source of truth; emits `scrubSeq`.
- `src/features/workflow-detail/swimlane/scrub.ts` (create, ≤ 60 lines) — pure
  `prefixUpTo(events, seq)` slice + re-project helper.
- `Swimlane.tsx` (edit, S4-owned) — accept optional `scrubSeq` to filter the projected
  entries to the prefix. Because S4 and S5 share `Swimlane.tsx`, **S5 serializes after S4**
  (same-folder, same-file edit).
- `Scrubber.test.tsx` (create) — handle maps to correct seq; prefix reconstruction hides
  later events; releasing returns to live.

**Data binding.** None new (operates on already-fetched/merged event prefix).

**Gate.** TC + LINT + TEST + LIVE (drag handle back; later bars disappear; state at seq=N
matches recorded prefix; release → live resumes).

---

### S6 — Reopen Diff (§4.3, computed form) (PARALLEL, command-gated)

**Scope.** Before/after preview of a reopen: green (preserved), amber (will re-run),
struck (superseded terminal). Computed from history the same way the engine derives the
reopened set + cursor reset. Issuing reopen is a SERVER COMMAND (invariant 3) — the
**commit button is built but disabled** until the command API exists (§8); the preview is
fully shippable now.

**Files (new subfolder `src/features/workflow-detail/reopen/`):**
- `src/features/workflow-detail/reopen/ReopenDiff.tsx` (create, ≤ 180 lines) — modal /
  slide-out; before/after columns; commit button disabled with an explicit "command API
  not yet available" affordance (no silent dead button).
- `src/features/workflow-detail/reopen/computeReopen.ts` (create, ≤ 160 lines) — pure:
  given failed-run history, derive re-dispatched vs reused set + cursor reset rule. Mirror
  engine semantics; documented against `WorkflowReopened` spec.
- `computeReopen.test.ts` (create) — preserved/re-run/superseded classification over a
  fixture history.

**Data binding.** Reads existing history (no new endpoint). Command path: reserved
`apiClient.reopen()` in `client.ts` is OUT of scope until §8 lands (do not add a live
write).

**Gate.** TC + LINT + TEST (no LIVE write — preview only; LIVE optional: open the diff on a
failed workflow and eyeball classification).

---

### S7 — Three-AM Triage view (§4.4) (PARALLEL, partially server-gated)

**Scope.** Ranked incident cards: failed/stuck workflows, plus (when the feeds exist)
dead workers, `Failed` outbox rows, fenced rejections, shard adoptions. Each card opens
the relevant deep view; one inline action.

**Files (new folder `src/features/triage/`):**
- `src/features/triage/components/TriageView.tsx` (create, ≤ 200 lines) — ranked card
  list; each card → deep-link (swimlane / failover / reopen-diff).
- `src/features/triage/components/IncidentCard.tsx` (create, ≤ 120 lines) — one card; rank,
  one action.
- `src/features/triage/hooks/useIncidents.ts` (create, ≤ 160 lines) — ranks failed/stuck
  workflows from the LIST query (status=Failed + stuck-Running heuristic by age). Worker /
  outbox / node-shard / fenced incidents are **gated behind a feed-availability check**:
  the topology/outbox subscription kinds are NOT in `websocket-types.ts`
  `AionEventSubscriptionFilter` (confirmed: only per_workflow/filtered/firehose). Those
  incident classes render only when the server promotes them (§8); until then the hook
  emits workflow-failure incidents only and the card surface degrades cleanly.
- `src/features/triage/index.ts` (create) + tests for ranking + degradation.

**Data binding.** REST list query (status=Failed). Future: outbox/worker/node feeds —
require §8 server promotion + new subscription kinds; spec'd, not built.

**Live-wiring.** Live failover incidents are already covered by the separate failover view;
S7 links INTO `/failover` rather than duplicating its feed.

**Gate.** TC + LINT + TEST (ranking; degradation when no topology feed). LIVE: fail a
workflow on the server → incident card appears and deep-links to its swimlane.

---

### S8 — Event-Level Search (§4.5) (PARALLEL, server-query-gated)

**Scope.** Field-aware query over event type, status projection, activity type, error
message/kind, time range, namespace; results link into the swimlane at the matching event.

**Files (new folder `src/features/search/`):**
- `src/features/search/components/SearchView.tsx` (create, ≤ 200 lines) — query form +
  results list; each result → `/workflows/:id` with `?seq=N` deep-link into the swimlane.
- `src/features/search/components/SearchForm.tsx` (create, ≤ 140 lines) — field-aware
  inputs.
- `src/features/search/hooks/useEventSearch.ts` (create, ≤ 140 lines) — TanStack Query
  against the server's event-search endpoint. **Server dependency:** `client.ts` has no
  search method today; this slice adds `apiClient.searchEvents()` pinned to the AW contract
  (CO3, one-file edit in `client.ts`). If the server endpoint is not yet pinned, S8 ships
  the form + a client-side filter over the current namespace's list as the floor, and the
  server-backed path is feature-flagged.
- `src/lib/api/client.ts` (edit, small — **shared file; see §3**) — add `searchEvents`
  method + contract entry only.
- `src/features/search/index.ts` + tests (query build, deep-link href, decode-failure
  fallback).

**Data binding.** New REST search endpoint (AW contract). Deep-link consumes S4 swimlane +
S5 scrub (`?seq=`).

**Gate.** TC + LINT + TEST. LIVE (server-backed) only when the endpoint is pinned.

---

### S9 — Live-discipline hardening (SERIAL, last; touches shared api/ + live-feed)

**Scope.** Cross-cutting no-silent-failure pass (SPEC handPlane). Surface WS errors to UI
(M1, replace `console.warn`); per-active-view resync on reconnect; connection indicator
states verified everywhere; payload decode best-effort everywhere.

**Files (shared — owned only here, runs after all parallel slices):**
- `src/lib/api/websocket.ts` (edit) — emit a typed error to listeners instead of swallowing
  to `console.warn` (M1).
- `src/features/live-feed/hooks/useEventSubscription.ts` (edit, reconciliation-owned
  baseline) — only if a resync hook-point is missing; otherwise no-op.
- `src/features/live-feed/components/ConnectionIndicator.tsx` (edit) — ensure
  Connected/Reconnecting/Disconnected + error message surface (CO7).
- Tests: WS error surfaces to UI; reconnect triggers per-view resync; indicator reflects
  all three states.

**Why serial / last.** It edits shared `lib/api/` and `live-feed/` that parallel slices
read; doing it last avoids churning their baseline mid-build.

**Gate.** TC + LINT + TEST + LIVE (kill the server socket; indicator → Reconnecting; view
does not blank; restore → resync; force give-up → Disconnected + error message).

---

## 3. PARALLELIZATION MAP (the fan-out contract)

**Headline: after S0 lands, S2–S8 run fully in parallel (7-wide) on disjoint feature
folders; S5 serializes behind S4 (same `Swimlane.tsx`); S9 serializes last. S8 makes one
tiny additive edit to the shared `client.ts` — coordinate or land first.**

### Serialized (shared-file contention):

| Slice | Shared files it owns | Must run |
|------|----------------------|----------|
| **S0** | `routes.tsx`, `App.tsx`, `providers.tsx`, new `AppNav.tsx` | FIRST (before all) |
| **S9** | `lib/api/websocket.ts`, `live-feed/*`, `useEventSubscription.ts` | LAST (after all) |

### Parallel cohort (launch together once S0 is green):

| Slice | Folder (disjoint) | Shared-file edits | Notes |
|------|-------------------|-------------------|-------|
| **S2** LIST | `features/workflow-list/` | none | independent |
| **S3** DETAIL+29 | `features/workflow-detail/` (top-level + `lib/`) | none | S4 depends on its projection |
| **S4** SWIMLANE | `features/workflow-detail/swimlane/` (new) + own `WorkflowDetailView.tsx` | **none** (wraps S3, S0 route renders wrapper) | depends on S3 landing |
| **S6** REOPEN | `features/workflow-detail/reopen/` (new) | none | independent of S4/S5 |
| **S7** TRIAGE | `features/triage/` (new) | none | links into `/failover`, `/workflows` |
| **S8** SEARCH | `features/search/` (new) | **`client.ts` (+1 method)** | tiny additive; see below |

### Same-folder serialization inside workflow-detail:

`S3 → S4 → S5` form a chain on `features/workflow-detail/`:
- S3 owns top-level + `lib/`. S4 owns the NEW `swimlane/` subfolder + a NEW wrapper file —
  **no edit to S3 files** by design (the `WorkflowDetailView.tsx` wrapper rule). So S3 and
  S4 are file-disjoint and could even overlap, but S4's lane layout consumes S3's
  TimelineEntry shape, so **S4 starts when S3's `types.ts`/`timeline.ts` are stable.**
- S5 edits S4's `Swimlane.tsx` → **S5 strictly after S4.**
- S6 (reopen) is a third disjoint subfolder → parallel with S3/S4/S5.

### Shared-file contention call-outs (the only ones):

1. **`routes.tsx`** — ONLY S0 edits it. Every other slice IMPORTS path/href helpers from it
   (read-only). This is why S0 must publish first.
2. **`App.tsx` / `providers.tsx`** — ONLY S0. Nav + boundary + WS singleton live here.
3. **`client.ts`** — S8 adds one `searchEvents` method. Recommended: fold S8's client edit
   into S0 (add the empty contract entry there) so S8 touches only its own folder. If not
   folded, S8 must land its `client.ts` edit on a tight window or rebase.
4. **`websocket.ts` / `useEventSubscription.ts` / `live-feed/`** — ONLY S9 (last).
5. **`types/generated/index.ts`** — NOBODY hand-edits (CO2); regenerate via Rust exporter
   only. If a slice needs a new server-side field, that is a server change + regeneration,
   not a dashboard edit.

### Fan-out recipe:
1. Land S0. Verify green.
2. Launch S2, S3, S6, S7, S8 concurrently (5-wide). (S8 client edit folded into S0.)
3. When S3's timeline/types are stable, launch S4.
4. When S4 lands, launch S5.
5. After all parallel slices merge, run S9.

---

## 4. Effort estimate + build order

Estimates are agent-build units (one focused subagent build + review pass each).

| Slice | Scope | Est. |
|------|-------|------|
| S0 Shell/Nav + foundation verify | shell, 4 routes, nav, M2/M3 | 0.5 unit |
| S2 LIST live-wire + pagination fix | wire C4, harden | 0.5 unit |
| S3 DETAIL + 29-variant + detail panel | wire C4, exhaustiveness guard, panel | 1.0 unit |
| S4 SWIMLANE | lane layout + bars + tests | 1.5 units |
| S5 Scrubber | handle + prefix reconstruct | 0.5 unit |
| S6 Reopen diff (preview) | compute + diff UI | 0.75 unit |
| S7 Three-AM triage | ranking + cards + degradation | 0.75 unit |
| S8 Event search | form + query + deep-link | 0.75 unit |
| S9 Live-discipline hardening | M1, resync, indicator | 0.5 unit |
| **Total** | | **≈ 6.75 units** |

**Wall-clock with the fan-out:** critical path is
`S0 (0.5) → S3 (1.0) → S4 (1.5) → S5 (0.5) → S9 (0.5) ≈ 4.0 units`, with S2/S6/S7/S8
absorbed in parallel against that path. So ~6.75 units of work compress to ~4 units of
wall time at 5–7-wide.

**Recommended ORDER (dependency-correct):**
1. **S0** (serial, unblocks everything).
2. **S2, S3, S6, S7, S8** (parallel cohort).
3. **S4** (after S3 timeline/types stable).
4. **S5** (after S4).
5. **S9** (serial, last — hardening over the merged tree).

---

## 5. Risk register (honest)

1. **Timeline-variant correctness (S3).** All 29 variants are present in generated types,
   but the post-reconcile default arm is a *safe* generic fallback, not a build guard. If a
   future server regeneration adds a variant, the view silently renders it generic. **Fix
   in S3:** a separate compile-time `assertAllVariantsHandled(e: never)` over a
   `KnownEventType` union derived from `Event['type']` so the build *fails* on an
   unhandled variant (SPEC handPlane rule 4). Risk: MEDIUM if the guard is skipped; LOW
   with it. Also: out-of-order live events on resync could mis-attribute an activity
   attempt — mitigated by seq-ordered merge (`mergeEventsBySequence`), but resync must be
   atomic server-side.

2. **Per-workflow vs firehose subscription scaling (S3/S4).** Detail opens one
   `per_workflow` subscription per workflow; a tab that opens 20 detail views accumulates
   20 subscriptions. Server rate-limit behavior is not in the pinned contract. Risk:
   LOW–MEDIUM. Mitigate: unsubscribe on unmount (verify in S9), and consider a shared
   firehose+client-filter fallback if the server caps subscriptions.

3. **List pagination × live insert (S2).** `useLiveListUpdates` inserts new
   `WorkflowStarted` rows at index 0, which corrupts the "page moves forward" expectation
   on page ≥ 2 (new item appears on the wrong page boundary). Risk: MEDIUM. **Fix in S2:**
   suppress top-insert off page 1 (mark "new above" instead), and invalidate the full query
   on filter change.

4. **Dead-path wiring regressions (S2/S3).** The whole point is wiring C4; the risk is
   re-introducing a dead path or double-subscribing on filter/namespace change. Risk: LOW
   with the LIVE gate (observe exactly one subscribe frame per active view in DevTools) and
   a unit test asserting unsubscribe-on-cleanup. Namespace switch must re-scope BOTH query
   and subscription (CO10) — assert in tests.

5. **Server-gated slices (S6 commit, S7 topology, S8 search).** Reopen commit, the
   worker/outbox/node-shard incident classes, and server-backed search all depend on §8
   server promotion / command API / pinned search endpoint that are NOT on the wire today
   (confirmed: `websocket-types.ts` exposes only per_workflow/filtered/firehose; no
   topology/outbox/shard kinds). Risk: these slices ship their *computed/preview/floor*
   forms now and the server-backed paths are feature-flagged + disabled-with-affordance
   (never a silent dead button). No slice blocks on a server change.

6. **Swimlane file-size + perf (S4).** A long-running workflow (thousands of events) could
   stress lane rendering. Risk: LOW–MEDIUM. Mitigate: keep `laneLayout.ts` pure +
   virtualize the time axis if needed; cap initial render and lazy-expand lanes.
