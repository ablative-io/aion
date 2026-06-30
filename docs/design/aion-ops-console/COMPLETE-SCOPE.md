# Aion Ops Console — Complete Scope to Ship (Highest Standard)

## Executive summary

The Aion ops console is the real-time operations UI for a distributed durable-execution cluster, built to outclass Temporal: a swimlane partial-order timeline, an execution scrubber, a three-AM triage view, event-level search, a keyboard-first command palette, and a live cluster map — all real data, no mocks, Matt-Pocock-strict TypeScript on the house stack (React 19 / Vite 7 / Tailwind 4 / TanStack Query / Bun / Biome / shadcn-Radix), served as an embedded single binary from `aion-server`. The architecture is sound and the design intent (VISION.md §1–7, ADR-015..021) is complete and binding. What remains is substantial: completing the Phase-1 feature surface, wiring the WS3 cluster push channel end-to-end (server emits + client consumes), building the cross-cutting professional-console disciplines (deep-linking, provenance, keyboard, performance budgets, calm states, hand-plane), and standing up the test/CI/perf/security/docs scaffolding that turns a built UI into a shippable product. This document is the definitive, deduplicated, prioritized backlog.

**DONE (landed or landing):** scaffold + reconciliation (generated wire types, 29 variants present, error boundary, browser-WS auth via query-param credentials), failover demo view (slice 1), single-binary embed pipeline with three-layer CI guard.

**IN FLIGHT (assume it LANDS; scope around/after it):** WS3 — server-side cluster-event push channel (`ClusterEvent`/`ClusterSnapshot` typed, broadcast plumbing partly present) + the ADR-020 command seam contract. Treat WS3 as INCOMING; do not re-scope it. Note: cluster wire types exist but `emit()` is not yet called from the supervisor/registry, and the client WS manager has no `cluster` subscription arm — both are in-scope work that depends on WS3 landing.

**REMAINS:** everything in Milestones M1 (full Phase-1 console), M2 (Phase 1.5 control plane), M3 (Phase 2 WASM) below.

---

## Milestones

### M1 — Complete Phase-1 ops console (the shippable baseline)

The whole of VISION §1–§7 minus Tier-2 cluster actions and WASM. Ordered workstreams:

1. **Reconciliation close-out** — land the green gate: regenerate types (fix `TimerIdKind`/`WithTimeoutOutcome`/`ContinuedAsNew`), fix half-applied live-feed `index.ts` exports, wire `RootErrorBoundary`, surface WS errors to UI. `bun typecheck`/`check`/`test` green. (Blocks everything.)
2. **Foundational live-path bug fixes** — kill the per-event resubscribe storm (P1), bound the live buffer (P2), make merge incremental (P3), coalesce live renders (P5). These make every later measurement meaningful and fix a suspected live correctness bug.
3. **App shell + routes + nav (S0)** — 5 routes, route-path helpers, singleton WS manager surviving nav, persistent connection indicator.
4. **Core feature slices** — list live-wiring (S1/S2), 29-variant timeline + exhaustiveness guard + detail panel (S2/S3), live-detail wiring (S3), swimlane (S4), scrubber (S5), reopen-diff preview (S6), triage + calm state (S7), event search (S8).
5. **WS3 integration (server + client)** — server emit-site wiring per `ClusterEvent` variant + cluster subscription socket routing; client `cluster` subscription arm, snapshot/delta seq-gating, `ClusterLagged` handling; cluster map Tier-0/1 (S10) consuming the feed; triage topology/worker/outbox/shard incident classes.
6. **WS4 console disciplines** — command palette + keyboard nav (ADR-017), deep-linking URL-state (ADR-015), provenance/freshness (ADR-016), performance budgets + virtualization (ADR-018), calm/empty/loading/error states (ADR-019).
7. **Cross-cutting hardening (S9)** — per-view resync, freshness watchdog, stale-data veils, subscription lifecycle correctness, hand-plane remediation.
8. **Quality scaffolding** — interactive DOM test harness, frontend CI gates, Playwright e2e (incl. failover-in-UI), perf benchmarks, security headers + version/skew contract, operator + developer docs, embed/asset hygiene.

### M2 — Phase 1.5 control plane (Tier-2 cluster actions)

Each action is gated on a **new named server command** (does not exist today) and ships incrementally behind confirm + blast-radius preview. Ordered workstreams:

1. **Command transport + round-trip discipline** — client command seam (idempotency key, typed ack, wait-for-effect-event reconciliation, never optimistic history write), typed rejection rendering (Fenced/NotOwner/unauthorized).
2. **RBAC / capability model** — split read vs per-workflow command vs cluster-control authority (today only `deploy: bool` exists); per-workflow commands reuse the namespace ownership guard.
3. **Operator-action audit log** — durable, structured, audited-on-denial record of who-did-what-to-what.
4. **The commands themselves** — Cancel workflow (server endpoint exists), Reopen workflow (`POST /workflows/:id/reopen`), Redrive dead-letter outbox row, Drain node, Planned shard handoff, Chaos kill-node (compiled out / highest-grant only in prod).
5. **Blast-radius preview** — server-computed, provenance-stamped, optimistic-concurrency-gated on `cluster_seq`.

### M3 — Phase 2 in-browser WASM replay engine (the novel bet, BLOCKED)

Gated on the haematite WASM stack being built-properly and exercised in a real headless browser. Ordered workstreams:

1. **WASM substrate remediation (haematite, upstream)** — `WasmShardRuntime` compiles clean on `wasm32`; OPFS WAL, IndexedDB fallback, WebSocket sync each have real headless-browser tests; fix known silent-failure defects (WAL acks-before-durable, socket no error/close handling).
2. **In-browser engine host** — beamr engine + haematite storage in the tab; degrades cleanly to Phase-1 when unavailable.
3. **Capability upgrades** — scrubber → real deterministic replay; reopen-diff → local simulation-before-commit; search → offline OPFS history; long-history replay strategy (prefix/checkpoint).
4. **A11y/perf for the WASM path** — non-motion equivalents, replay-corpus perf budget.

---

## Work items by dimension

Effort: S (<0.5d) / M (0.5–2d) / L (2d+). Severity: Critical / High / Medium / Low. Deps note other items or external workstreams (WS3 = cluster push channel; WS4 = console disciplines; CMD = server command API; AW = REST/WS contract pinning; FEP = failover-event promotion; WASM = haematite browser I/O).

### Reconciliation & foundational correctness (blocks all)

- **R1 — Regenerate wire types, fix truncation.** AC: `TimerIdKind`, `WithTimeoutOutcome` defined; all 29 variants named; `StatusBadge` gains `ContinuedAsNew` (+`Reopened`). `tsc` clean. Effort S. Severity Critical. Deps none.
- **R2 — Complete live-feed `index.ts` refactor.** AC: no duplicate `ConnectionIndicator` export; the three phantom re-exports resolved; 4 failing test files load. Effort S. Severity Critical. Deps none.
- **R3 — Wire `RootErrorBoundary` into `App.tsx`; surface WS errors to UI (no `console.warn` swallow).** AC: a visible alert/toast surface for transient WS errors; error path test. Effort S. Severity High. Deps none.
- **R4 — Fix per-event WebSocket resubscribe storm.** `useEventSubscription` effect deps include `lastSeenSequence`+`onEvent`, which churn on every event → unsubscribe/resubscribe + server resync per event. AC: exactly one subscribe frame per (namespace, workflowId) lifetime; `lastSeenSequence` tracked via ref/manager, not effect dep; `onEvent`/`onResync` stabilized; test asserts one subscribe across N events. Effort L. Severity Critical. Deps none. **Do first among feature work.**
- **R5 — Bound the detail-view live buffer.** AC: rendering window bounded or folded into query cache; soak shows bounded heap. Effort M. Severity High. Deps R4.
- **R6 — Incremental `mergeEventsBySequence`; stop redundant re-merge in `terminalOutcomeForEvents`; memoize `projectTimeline`.** AC: append 10k events stays sub-quadratic (stated ms budget); zero redundant merges. Effort M. Severity High. Deps none.
- **R7 — Coalesce live re-renders (rAF batch + `useDeferredValue`).** AC: 500 events/sec holds 60fps, long-tasks <50ms, scrubber/keyboard stay responsive. Effort M. Severity High. Deps R4.

### Feature slices (Phase-1 surface)

- **F1 — App shell + 5 routes + nav + path helpers (S0).** AC: `/workflows`, `/workflows/:id`, `/search`, `/incidents`, `/failover`, `*`; nav active-state; WS singleton survives nav (regression test). Effort M. Severity Critical. Deps R1–R3.
- **F2 — Workflow list live-wiring + pagination×live-insert (S1/S2).** AC: live row patch; page-1 top-insert; page ≥2 "new above" pill not cursor mutation; namespace switch re-scopes query+subscription; `WorkflowRow` memoized. Effort M. Severity High. Deps F1, R4.
- **F3 — 29-variant timeline + compile-time exhaustiveness guard + detail panel (S2/S3).** AC: all 29 promoted to dedicated/typed arms; `assertAllVariantsHandled(never)` makes a new variant fail `tsc` (negative-compile test proves the failure mode); decode failure → raw envelope, never throws; detail panel opens/closes with full envelope + best-effort JSON. Effort L. Severity Critical. Deps F1, R1.
- **F4 — Live-detail wiring (S3).** AC: history+live merge in seq order, no refetch; terminal disables subscription; **reopen re-enables it** (terminal-gating must not strand a `WorkflowReopened` workflow — A3 below). Effort M. Severity High. Deps F3, R4.
- **F5 — Swimlane partial-order visualization (S4).** AC: lanes (lifecycle/activity/timer/signal/child), concurrency overlap, attempt segmentation (one-based), child drill-link, live-append extends bars; pure `laneLayout.ts` unit-tested. Effort L. Severity Critical (centerpiece). Deps F3.
- **F6 — Execution scrubber (S5).** AC: draggable handle maps to seq; `prefixUpTo` reconstructs prior state; ghosted (not removed) future bars + "viewing seq N of M (historical)" banner + return-to-live; suppresses live announcements while scrubbing. Effort M. Severity Medium. Deps F5.
- **F7 — Reopen-diff preview (S6).** AC: `computeReopen` matches engine derivation (cross-checked against a Rust-exported fixture); green/amber/strikethrough with text labels; commit disabled-with-honest-affordance until CMD. Effort M. Severity Medium. Deps F3; commit gated on CMD.
- **F8 — Three-AM triage + calm state (S7).** AC: ranked workflow-failure/timeout/stuck incidents now; worker/outbox/fenced/shard incident classes render when WS3 lands, else honest "awaiting server support" with reason; deep-link actions; designed calm "all clear, cluster heartbeat" state (ADR-019), not an empty box. Effort L. Severity High. Deps F1; topology classes gated on WS3+FEP.
- **F9 — Event-level search (S8).** AC: field-aware form (event type, status, activity type, error kind, time range, namespace-scoped); virtualized/streamed results; deep-link result → swimlane at matching seq; client-filter floor until server endpoint, disabled-with-affordance for server-backed. Effort M. Severity Medium. Deps F1; server endpoint gated on AW.

### Real-time / data integrity

- **A1 — seq-collision dedup policy (authoritative-source-wins).** AC: history (REST) supersedes buffered live for overlapping seqs; no two distinct payloads at one seq both invisible; documented + tested. Effort S. Severity Medium. Deps none.
- **A2 — Gap detection in per-workflow stream.** AC: missing seq between min(history) and max(live) raises "incomplete — refetching" and triggers refetch; never a silent hole. Effort M. Severity High. Deps none (AW for density guarantee).
- **A3 — Reopen does not strand the live view.** AC: a `WorkflowReopened` after terminal re-enables the per-workflow subscription and clears cached terminal outcome; test goes live again. Effort M. Severity High. Deps F4.
- **A4 — Per-view resync parity (S9).** AC: every live-derived view (list, detail, **search, triage, cluster**) registers `onResync` re-running its authoritative query; no subscribe without a resync handler (review gate). Effort M. Severity High. Deps F8, F9.
- **A5 — Reconnect-exhausted recovery.** AC: visible "Reconnect" action re-arms backoff, clears exhausted error, resets attempts; integration test. Effort S. Severity Medium. Deps none.
- **A6 — `after_seq` past compacted history.** AC: resync reply carries server earliest-available seq; client raises "history truncated, showing from seq X" + bounded refetch; never silent jump. Effort M. Severity High. Deps AW (resync reply must carry floor seq — likely missing server-side).
- **A7 — Reconnect/resync race (resubscribe before refetch completes).** AC: buffer reset keyed so mid-flight resync can't drop new-history events; test across the boundary, no dup/drop. Effort M. Severity Medium. Deps R4.
- **A8 — Coalesce high-rate frames at the manager (rAF drain).** AC: socket buffer never backs up; folds into R7. Effort M. Severity Medium. Deps R7.
- **A9 — Dispatch fan-out ceiling.** AC: index per-workflow routing or documented+tested max-concurrent-subscription policy with shared-firehose fallback. Effort S. Severity Medium. Deps AW (server cap behavior).
- **A10 — Subscription lifecycle correctness.** AC: balanced subscribe/unsubscribe on mount/unmount; no double-subscribe on filter/namespace change; ref-counted shared subscription for two views of same workflow; `sendJson`-while-disconnected reconciles unsubscribe on reconnect (no orphaned server stream). Effort M. Severity High. Deps R4.
- **A11 — Reconnect/resume integration harness.** AC: deterministic fake socket drives drop→reconnect→resync, exhausted→manual, decode-error→recover, namespace-switch-mid-stream, terminal→reopen; each asserts no dup/gap/silent-stale. Effort L. Severity High. Deps none.

### Cluster channel (WS3 surface — server + client)

- **C1 — Server emit-site wiring per `ClusterEvent` variant.** `emit()` is currently called from nowhere; the broadcast channel is silent. AC: supervisor tick + worker registry + outbox reconciler emit each of the 10 variants with correct payload, monotonic `cluster_seq`, edge-triggered (no event without mutation), correct `WorkerDeathReason`; one Rust test per variant. Effort L. Severity Critical. Deps WS3.
- **C2 — Cluster subscription socket routing (server).** `ws_subscription.rs` has no `cluster` branch. AC: `cluster` subscribe → `StreamedClusterSnapshot` priming then `StreamedClusterEvent` deltas in seq order; `after_seq` contiguous+dup-free; slow consumer gets typed `ClusterLagged` terminal frame; namespace isolation (no foreign workers). Effort M. Severity Critical. Deps WS3, C1.
- **C3 — Client `cluster` subscription arm.** AC: filter variant + subscribe builder + parser branch for snapshot/delta/`ClusterStreamError`; `matchesSubscription` cluster routing; protocol round-trip test against captured frame fixture. Effort L. Severity High. Deps WS3, C2.
- **C4 — Cluster snapshot/delta seq-gating reducer.** AC: holds `appliedSeq`; drops deltas ≤ appliedSeq, applies > in order, snapshot resets to `as_of_seq`; scrambled-order replay → deterministic final state. Effort M. Severity High. Deps C3.
- **C5 — `ClusterLagged` handling.** AC: lag frame → "fell behind, re-syncing (N dropped)" banner + re-request snapshot; never extrapolate from incomplete deltas. Effort M. Severity High. Deps C3.
- **C6 — Worker death-reason fidelity.** AC: exhaustive `WorkerDeathReason` switch with `assertNever`; renders typed reason + transport; never inferred. Effort S. Severity Medium. Deps C3.
- **C7 — Shard-ownership degradation guard.** AC: until FEP, render shard ownership "last-known (not live)" with provenance note; no fabricated adoption animation; animates once events land. Effort S. Severity Medium. Deps WS3 + FEP (server shard-event promotion missing).
- **C8 — Cluster snapshot data scoping.** AC: tenant-a cluster subscription never sees tenant-b workers; documented decision on peers/shards visibility. Effort M. Severity Medium. Deps C2.

### Cluster map & failover visualization (UX)

- **C9 — Real cluster map (S10), not the bespoke `/failover` demo.** AC: deterministic fixed layout (not force-directed), node containers, shard tiles, worker docks (namespace×task_queue×transport), animated workflow tokens on Liminal edges; `/failover` reduced to a preset; deep-link node/shard/worker/edge (ADR-015). Effort L. Severity High (headline outclass). Deps WS3, C3.
- **C10 — Failover-as-motion.** AC: shard tile animates dead→adopter on real `ShardAdopted`; tokens re-route; dedup hit shows token absorbed at boundary; replaces synthetic-row demo. Effort L. Severity High. Deps C9, WS3, FEP.
- **C11 — Tier-1 cluster time-scrub.** AC: scrub back replays a failover from recorded history, honest by determinism boundary. Effort M. Severity Medium. Deps C9.
- **C12 — Migrate failover view off HTTP polling to push.** AC: liveness/metrics from WS3 in the primary path; polling retained only as explicitly-named `FailoverFallback`. Effort M. Severity Medium. Deps WS3.

### Professional-console disciplines (WS4)

- **D1 — Command palette + keyboard control (ADR-017).** AC: ⌘K / `/` palette, fuzzy + recency + context-sensitive actions; per-view arrow nav (list rows, swimlane lanes/bars, triage cards, search results); bindings surfaced inline; mouse never required, keyboard never second-class. Effort L. Severity High. Deps F1+ views; actions gate on CMD.
- **D2 — Deep-linking URL-state (ADR-015).** AC: every shareable state (namespace, workflow, selected event/bar, scrub seq, search query+filters, active view) in URL; router-driven; component-local state only for ephemeral UI; copy→reload→back/forward round-trip per view. Effort M (spread per slice). Severity High. Deps per-view.
- **D3 — Provenance/freshness (ADR-016).** AC: per-view footer with source node, last-applied seq, relative freshness; explicit "viewing a survivor" signal post-failover generalized beyond the failover view; goes amber when stale. Effort M. Severity High. Deps server stamping responses/frames with node+seq (AW — likely missing).
- **D4 — Freshness watchdog (connected-but-silent socket).** AC: server keepalive (preferred) or client last-frame-time threshold downgrades freshness to "stale?" + offers refetch; never a green indicator over frozen data. Effort M. Severity High. Deps AW/WS3 keepalive (likely missing server-side).
- **D5 — Stale-data per-view veil/banner.** AC: view-level "stale, reconnecting, last updated Xs ago" the moment liveness degrades, clears on resync. Effort M. Severity High. Deps D3, A4.
- **D6 — Performance budgets written + benchmark harness (ADR-018).** AC: budgets committed (swimlane ≤16ms/frame at 10k; triage top-incident <1s/one-screen; list/search FMP <200ms; initial JS gzip ceiling; sustained event-rate floor); benchmark mounts 10k-event swimlane and asserts; micro-bench for merge/layout append; regression fails CI ("defect, not tuning"). Effort L. Severity High. Deps R6, F5, virtualization.
- **D7 — Swimlane virtualization.** AC: horizontal (time-axis) + vertical (lane) windowing; `LaneBar`/`LaneRow` memoized; live append doesn't re-layout all events; DOM nodes bounded to viewport at 10k events. Effort L. Severity Critical (centerpiece perf). Deps F5.
- **D8 — List + search virtualization.** AC: `@tanstack/react-virtual` for >100-row lists/results (or explicit server-paging decision with memoized rows); 1000 rows / 5000 results at 60fps, first screen <200ms. Effort M. Severity Medium. Deps F2, F9.
- **D9 — Calm/empty/loading/error states per surface (ADR-019).** AC: distinct designed states (triage calm = positive vitals; empty list = onboarding; empty search = suggestions); shape-matched skeletons for swimlane/triage/search/cluster; severity-aware opaque error with retry. Effort M. Severity High. Deps F8, views.

### UX / visual / design system

- **U1 — Load the web fonts.** DM Sans / JetBrains Mono are referenced but never loaded (no `@font-face`/link/dep) → system fallback ships today. AC: self-hosted woff2, `font-display: swap`, subset, verified rendered face. Effort S. Severity High (visual identity bug). Deps none.
- **U2 — Define `--surface-elevated` (and full elevation ramp).** Referenced in `Swimlane.tsx` but undefined → transparent container. AC: ramp defined both themes; grep asserts every `var(--…)` resolves. Effort S. Severity High (live bug on centerpiece). Deps none.
- **U3 — Hand-plane remediation: eliminate opacity/glow/shadow.** Grep finds ~58 opacity-modifier hits (5× the surveyed 11) across LaneBar, ConnectionIndicator, FirehoseFeed, ExactlyOnceCounter, FanOutBar, FailoverView, NodeCard, ErrorState, StatusBadge, ActivityGroup; plus `opacity-40/70` conveying state and an `accent-cyan-glow` token. AC: fixed opaque status palette (solid fill+border, both themes); zero `/[0-9]` color modifiers; glow token deleted; collapsed/dark via solid token + glyph; CI grep gate. Effort M. Severity High (binding gate). Deps none.
- **U4 — Light/dark theme toggle.** `.light` token set exists but is never applied; `:root` hard-pins dark. AC: system/light/dark controller, persisted, FOUC-safe inline apply, header control; audit all `var(--…)` in light. Effort M. Severity Medium. Deps none.
- **U5 — Unify token vocabulary + status color language.** Bespoke `--text-*`/`--surface-*` and shadcn `--foreground`/`--card` coexist inconsistently; `StatusBadge`/`LaneBar`/`NodeCard`/failover each define colors independently. AC: one canonical token layer (other aliased); one shared state→color table (workflow status + node liveness + outbox status); documented; review gate. Effort M. Severity Medium. Deps U3.
- **U6 — Swimlane temporal axis mode.** x-axis is seq-rank only; duration is meaningless. AC: toggleable logical (seq) vs temporal (`recorded_at`-scaled with ruler/gridlines/relative ticks), deep-linked; temporal tooltip on every bar. Effort L. Severity High. Deps F5.
- **U7 — In-flight bar now-anchor + retry segment legibility.** Open activities have no "still running to now" treatment; attempt dividers are 1px hairlines. AC: animated open trailing edge for running bars; each attempt a distinct segment with index badge. Effort M. Severity Medium. Deps F5, U3.
- **U8 — Swimlane lane grouping + minimap.** AC: deterministic sections with sticky group headers, child lanes indented under parent, collapse/expand-all, max-lanes-before-fold; slim minimap with viewport window + incident markers, click-to-jump. Effort M. Severity Medium. Deps F5.
- **U9 — Root overview/home (calm-state landing).** No "is the cluster healthy" glance home. AC: `/` overview = designed calm state doubling as incident roll-up. Effort M. Severity Medium. Deps WS3 (degrades to workflow-only).
- **U10 — Header IA: breadcrumbs + workflow identity + provenance integration.** AC: Namespace › Workflows › id trail; detail header always shows identity; provenance line in chrome. Effort M. Severity Medium. Deps D3.
- **U11 — Timestamp formatting utility.** Raw ISO strings rendered today. AC: shared relative ("2m ago") + absolute-on-hover, tabular-nums, tz-aware; applied list/detail/axis/event-log/triage. Effort S. Severity Medium. Deps none.
- **U12 — Payload/detail panel quality.** AC: collapsible JSON tree, copy-to-clipboard, raw-bytes fallback that never errors, full envelope scannable. Effort M. Severity Medium. Deps F3.
- **U13 — Motion as a designed system + reduced-motion + non-jarring live inserts.** AC: motion tokens reserved for meaning (adoption, token dispatch, bar extension, scrub); `prefers-reduced-motion` fallback for all meaning-bearing motion; new-row highlight-fade + "N new above" pill. Effort M. Severity Medium. Deps C9/C10.
- **U14 — Responsive/full-bleed layout.** `max-w-7xl` starves swimlane/map/tables on large monitors. AC: per-view width strategy (reading constrained, map/swimlane/table full-bleed), ≥1920 tested; defined narrow-width behavior. Effort M. Severity Medium. Deps none.
- **U15 — App chrome: favicon, per-route `<title>`, OG metadata.** AC: favicon set; dynamic title ("wf-1234 · Failed · Aion"); shareable link preview (3am-handoff). Effort S. Severity Low. Deps none.
- **U16 — Migrate hardcoded component palettes to the semantic status registry (LaneBar, IncidentCard, ConnectionIndicator, ActivityGroup, FirehoseFeed, NamespaceSelector, swimlane) — precondition for light-theme safety + hand-plane compliance.** AC: each listed component names only §2.2 semantic intents (no Tailwind palette primitives, no opacity color-modifiers, no inline status palette); swimlane status→color map reads the single registry; `data-theme="light"` renders them correctly; DESIGN-TOKENS.md §8 guard passes on all of them. This is the §0.1 conformance-debt precondition that makes the "zero component edits to add light" property real rather than aspirational. Effort M. Severity High (binding precondition for U3/U4/U5). Deps none.

### Accessibility (WCAG 2.2 AA)

- **Y1 — Skip link + landmark/`<h1>` structure in AppShell.** AC: visible-on-focus skip-to-main first; exactly one `<h1>` per route; unique landmarks; axe bypass/landmark/heading rules pass. Effort S. Severity High. Deps none.
- **Y2 — a11y test infrastructure (axe + RTL a11y queries).** AC: axe runs against every top-level view + interactive component, zero violations; CI gate; assert by role/name. Effort M. Severity High. Deps Q1.
- **Y3 — Global `prefers-reduced-motion`.** AC: non-essential motion neutralized; failover transition degrades to instant + announced; skeleton pulse stills. Effort M. Severity High. Deps U13, C10.
- **Y4 — Visible focus indicator audit (2.4.11/2.4.13).** AC: every focusable has ≥2px ≥3:1 unclipped indicator (esp. `overflow-hidden` LaneBar); both themes. Effort M. Severity High. Deps none.
- **Y5 — Color contrast pass (1.4.3/1.4.11).** `--text-muted` ~4.0:1 borderline + `opacity-70` dim fail. AC: all text/UI meet AA both themes; muted raised or restricted; status fills ≥3:1. Effort M. Severity High. Deps U3 (coordinate).
- **Y6 — Firehose live region (throttled, opt-in).** Silent today (4.1.3 fail). AC: visually-hidden `role="log"` polite region announces summarised batches; operator toggle (default off); list itself not the live region. Effort L. Severity Critical. Deps none; richer cluster announcements need WS3.
- **Y7 — Swimlane live-append announcements.** AC: throttled polite region for the viewed workflow announces material transitions only; suppressed while scrubbing. Effort M. Severity High. Deps F5.
- **Y8 — Connection/failover/provenance change announcements.** Reconnecting banner has no role today. AC: status transitions polite/assertive; "viewing a survivor"/stale announced. Effort M. Severity High. Deps D3, D5.
- **Y9 — Triage incident arrival/clearance + calm announcements.** AC: new/top-incident, clearance, and "all clear" announced; cards are a labelled list. Effort M. Severity Critical. Deps F8.
- **Y10 — Command palette a11y from commit one.** AC: combobox/listbox pattern, `aria-activedescendant`, results-count live region, focus trap + restore, ⌘K and `/` (without breaking SR find). Effort L. Severity Critical. Deps D1.
- **Y11 — Swimlane keyboard roving nav, virtualization-safe.** AC: roving-tabindex 2-D model (←/→ along lane, ↑/↓ between lanes, Enter selects, Home/End); arrowing to off-screen bar scrolls+mounts before focus (no focus loss). Effort L. Severity Critical. Deps F5, D7.
- **Y12 — Scrubber keyboard parity + live-region discipline.** AC: native range keyboard incl. PageUp/Down large-step, Home/End; entering scrub mutes Y7 + announces historical; release announces resumed-live. Effort S. Severity Medium. Deps Y7.
- **Y13 — DetailPanel focus management.** AC: selection moves focus to panel; Esc closes + restores focus to originating bar; announced on open. Effort S. Severity High. Deps Y11.
- **Y14 — Triage/search/list keyboard nav.** AC: arrow nav between cards/results/rows, Enter opens, inline actions reachable; single tab stop per row. Effort M. Severity High. Deps F8, F9.
- **Y15 — Status by more than color (1.4.1).** AC: swimlane bar status in accessible name + glyph/shape; grayscale screenshot test distinguishes all states. Effort M. Severity High. Deps U3, F5.
- **Y16 — Reopen-diff a11y (1.4.1 + structure).** AC: each row disposition in text/`aria-label`; before/after semantic grouping; disabled commit uses `aria-disabled` + associated reason. Effort M. Severity High. Deps F7.
- **Y17 — NamespaceSelector + loading/empty/error/calm semantics.** AC: Radix select keyboard/typeahead verified + labelled + announced switch; loading busy, error assertive+retry, calm positive status — three distinct non-silent states. Effort S. Severity Medium. Deps none.
- **Y18 — Tooltip a11y (1.4.13) + no info only in `title`.** AC: hover info also keyboard/AT-available; native `title`-only meaning (NodeCard `metricsError`) surfaced to AT. Effort S. Severity Medium. Deps none.
- **Y19 — Cluster-map non-visual model.** AC: parallel keyboard-navigable structural representation / table-view toggle; every deep-link element reachable with descriptive name; token animation decorative+`aria-hidden` with facts as text. Effort L. Severity High (when built). Deps C9, Y3.
- **Y20 — Target size 2.5.8 + new-2.2 SC sweep.** 14px marker bars fail 24px. AC: sub-24px targets enlarged/equivalent; sticky header doesn't obscure focus (2.4.11); no drag-only interaction without alternative (2.5.7); consistent help (3.2.6). Effort M. Severity High. Deps Y4, F5.
- **Y21 — Manual NVDA + VoiceOver pass per view as release gate.** AC: documented SR script per view, executed, signed off. Effort M (recurring). Severity High. Deps views.

### Testing & QA

- **Q1 — Interactive DOM test harness.** Component tests are `renderToStaticMarkup` only — clicks/drags/keyboard/hover are untestable. AC: jsdom/happy-dom + `@testing-library/react` + `user-event`; ≥1 real interaction test per interactive component. Effort L. Severity Critical. **Do first among QA.** Deps none.
- **Q2 — Frontend CI gates.** No `typecheck`/`check`/`test` in CI today. AC: required PR job runs frozen install → typecheck → biome → test, fails merge on non-zero. Effort S. Severity Critical. Deps none.
- **Q3 — Coverage measurement + floor.** AC: coverage in CI with high floor on pure-logic modules (laneLayout/scrub/timeline/computeReopen/incidents/client-normalize/websocket-protocol); delta on PR. Effort M. Severity High. Deps Q1.
- **Q4 — Negative-compile exhaustiveness test.** AC: `@ts-expect-error`/typecheck-only spec proving an unhandled `Event['type']` breaks `tsc`. Effort S. Severity High. Deps F3.
- **Q5 — Payload decode fuzz.** AC: ≥1000 random/garbage inputs → raw fallback, never throws; truncated/binary/empty/oversized cases. Effort S. Severity Medium. Deps none.
- **Q6 — Pure-module correctness incl. engine-cross-check.** AC: table-driven tests for laneLayout/scrub/computeReopen; `computeReopen` cross-checked against a Rust-exported failed-run fixture. Effort M. Severity High. Deps F5, F7.
- **Q7 — Subscription-lifecycle regression tests.** AC: one subscribe per lifetime; unsubscribe on unmount; namespace/filter re-scope once. Effort S. Severity Medium. Deps R4, A10.
- **Q8 — Rust emit-site + cluster-stream + command + search tests.** AC: per-variant emit test (C1); cluster subscription snapshot/delta/lag/isolation test (C2); per-command authz-deny/happy/idempotency/rejection (incl. existing cancel); `/events/search` filter/pagination/isolation or 501 contract. Effort L. Severity Critical. Deps WS3, CMD, AW.
- **Q9 — Playwright e2e harness + single-node flows.** Dep installed, zero specs/config. AC: config + CI job booting a real server; list/swimlane/live-append (network-asserted no-refetch + one subscribe)/scrubber/deep-link round-trip/forced-drop indicator. Effort L. Severity Critical. Deps Q1 infra parity, server runnable in CI (it is).
- **Q10 — Multi-node failover-in-UI e2e.** Existing `lsub5` asserts engine only. AC: Playwright client on cluster stream; kill node → asserts rendered shard move + survivor provenance + token re-route + exactly-once result; polling not sleeps. Effort L. Severity High (product thesis). Deps C1/C2/C9, Q9.
- **Q11 — Resync/overnight-liveness e2e.** AC: forced drop → resync to live, no gap/dup; compaction-past-cursor → honest gap-recovery not silent truncation. Effort M. Severity High. Deps Q9.
- **Q12 — Visual-regression suite + hand-plane lint gate.** AC: CI grep gate = zero opacity/glow/shadow; screenshot baselines per view × {loading, empty, error, populated} block merge. Effort L. Severity High. Deps Q9, U3.
- **Q13 — Perf-budget benchmark in CI + nightly WS soak.** AC: laneLayout 10k under budget, rendered-frame budget, triage first-paint, virtualization assertions; nightly soak (high-rate firehose + many subscribers) asserts bounded memory, zero silent drops, contiguous resync, bounded backoff. Effort L (M for bench, L for soak). Severity High/Medium. Deps D6, C2, Q9.
- **Q14 — Discipline conformance suites (ADR-015/016/017).** AC: deep-link round-trip matrix; provenance footer + no-console-swallow guard; keyboard-only walkthrough reaching every surface + issuing an action. Effort M. Severity High. Deps Q9, D1/D2/D3.
- **Q15 — Test hygiene.** AC: remove `as unknown as never`/`as never` from app+test code (grep gate); single shared verified fake WS manager; behavior assertions over SSR-string. Effort S. Severity Medium. Deps Q1.
- **Q16 — Memory-leak / listener-cleanup audit.** AC: every subscribe/onError/onStatusChange/onConnect cleaned on unmount; soak shows flat heap + no detached-DOM growth. Effort M. Severity High. Deps R5, Q13.

### Security (pre-production gate)

- **S1 — Secure-by-default posture.** Auth off by default = self-asserted namespaces; dev headers (`x-aion-namespaces`/`x-aion-deploy`) trusted verbatim. AC: non-loopback/production bind REFUSES `auth.enabled=false` + dev-header path; loud dev warning; dev headers ignored entirely in prod posture. Effort M. Severity Critical. Deps config production signal.
- **S2 — Contain the compiled-in dev-token shared secret.** Plain `==` bearer compare vs `jwks_url` when `auth` feature off. AC: prod builds compile `auth`; startup asserts feature present when enabled; constant-time compare if retained. Effort S. Severity High. Deps none.
- **S3 — JWT hardening.** Only `exp` validated today. AC: pinned `iss`; `aud` validated or deliberately documented-off; algorithm allowlist (no `none`, no alg-confusion); bounded leeway; rejection tests per class. Effort M. Severity High. Deps `iss`/`aud` config.
- **S4 — RBAC / capability model.** Only `deploy: bool` exists; ADR-020 spec would make "can deploy" == "can drain/kill". AC: read vs per-workflow-command (namespace-scoped) vs cluster-control (separate high grant) capabilities; each `ClusterCommand` maps to a named capability; per-workflow commands reuse the namespace ownership guard (cross-tenant = NotFound); `ChaosKillNode` behind chaos flag + highest grant; negative tests. Effort L. Severity Critical. Deps WS3/CMD, claim fields.
- **S5 — Outbox/node/shard command authorization + server-computed blast radius.** AC: redrive verifies row→namespace→grant; drain/handoff require cluster-control; blast radius computed server-side, not client-trusted. Effort M. Severity High. Deps S4, outbox query surface, FEP.
- **S6 — Operator-action audit log (incl. audited denials).** Deploy has an audit line; commands don't. AC: every mutating command emits structured line (subject, grant_source, transport, namespace, target, capability, outcome, correlation id, enacting node); denials audited; durable/parseable sink with retention (append-only/hash-chained preferred). Effort M (L for durable sink). Severity High. Deps S4.
- **S7 — Long-lived WS token re-validation.** AC: periodic re-check or max-socket-lifetime tied to `exp`; typed terminal frame + close on expiry → dashboard re-auths. Effort M. Severity Medium. Deps WS3 lifecycle.
- **S8 — WS token-in-URL leak hygiene + short-lived WS tokens.** AC: server/proxy never log `/events/stream` URIs (keep no request-URI logging); prefer `Sec-WebSocket-Protocol` token passing; document proxy constraint; short-lived tokens. Effort M. Severity High. Deps S7.
- **S9 — Browser token storage policy.** AC: prefer in-memory + silent refresh; never logged; redacted from error surfaces. Effort M. Severity High. Deps dashboard auth flow.
- **S10 — Security response headers.** None today. AC: strict CSP (self + API/WS origin), `frame-ancestors 'none'`/`X-Frame-Options: DENY` (anti-clickjack on drain/kill), `nosniff`, `Referrer-Policy: no-referrer`, HSTS over TLS; tests; must not break WS handshake/query-param auth. Effort M. Severity High. Deps S8 (Referrer-Policy aids URL-leak).
- **S11 — Payload XSS audit.** AC: no `dangerouslySetInnerHTML`/`innerHTML` on payload/error/event-field paths; raw fallback inert text; bake into F3 acceptance. Effort S. Severity High. Deps F3.
- **S12 — TLS posture.** In-process TLS refused today (`reject_n_until_supported`). AC: implement rustls OR hard-require TLS-terminating proxy + refuse non-loopback plaintext in prod. Effort L/S. Severity High. Deps S1.
- **S13 — Rate/concurrency/timeout limits + WS pre-auth bounds + command idempotency.** None today. AC: per-IP/subject rate limit on auth + WS upgrade; concurrent-subscription cap with typed rejection; request timeouts; idle-handshake timeout + max subscription-request frame size; command idempotency key honored. Effort M-L. Severity High. Deps WS3, CMD.
- **S14 — JWKS fetch hardening.** AC: enforce `https` (except loopback); cap key staleness after repeated refresh failures, surfaced via readiness. Effort S. Severity Medium. Deps none.
- **S15 — CSRF posture documented.** AC: keep header/bearer auth (no ambient cookies); guard test asserts `allow_credentials` stays off unless cookies deliberately adopted. Effort S. Severity Medium. Deps S9.

### Deploy / ops

- **O1 — Cache-Control on assets.** None today → full re-download per load + stale-`index.html`-on-upgrade risk. AC: hashed assets `immutable max-age=31536000`; `index.html` `no-cache`; header test. Effort S. Severity High. Deps none.
- **O2 — `/version` build-info endpoint.** None today. AC: returns server version, git sha, build time, embedded dashboard version, features, wire-contract version; rendered in console footer (ADR-016). Effort S. Severity High. Deps none.
- **O3 — Wire-contract version stamp + skew detection.** AC: monotonic version (or types hash) in both bundle and server; handshake/first-query surfaces "client/server mismatch — reload" rather than rendering wrong data; reload prompt on reconnect if skewed. Effort M. Severity High. Deps O2, O1.
- **O4 — Dashboard in publish ordering + publish-time embed guard.** Dashboard absent from `publish-crates.sh`; could ship placeholder UI to crates.io. AC: runbook + script require `cargo xtask build-dashboard` + smoke pass before `aion-server` publish. Effort S. Severity High. Deps none.
- **O5 — Strip sourcemaps from embed + frozen-lockfile install + embed-dir cleanliness guard.** AC: no `.map` in embedded `dist`; `bun install --frozen-lockfile` in pipeline; CI asserts `dashboard-embed/` git contents = placeholder + `.gitignore` only. Effort S. Severity Medium. Deps none.
- **O6 — Precompressed assets (gzip/brotli) + ETag/304.** AC: `Content-Encoding` negotiation with measured reduction; strong ETag. Effort M. Severity Medium. Deps O1.
- **O7 — Security headers on dashboard HTML.** (= S10 from the serving side; coordinate.) Effort M. Severity High. Deps S10.
- **O8 — Base-path / sub-path deploy + reserved-path guard from router.** `base_path` half-wired; reserved list hardcoded (`workflows`/`events`) — breaks when `/events/search`, `/schedules`, `/version` added. AC: server base-path matches build base; reserved prefixes derived from mounted router (test). Effort M. Severity Medium. Deps AW.
- **O9 — Dashboard-serving metrics + readiness includes dashboard source.** AC: counters for asset/SPA hits, active console WS subscriptions by kind, lag/drop, reconnects; `/health/ready` reflects asset source resolved. Effort M. Severity Medium. Deps WS3 (cluster dimension).
- **O10 — Run/operate doc, container image, systemd example.** None today. AC: operator guide (build `--features release`, config blocks, auth, reverse-proxy, upgrade/reload-on-skew); multi-stage non-root Dockerfile with `/health/ready` healthcheck; reference systemd unit. Effort M. Severity High (doc) / Medium (container). Deps O7, O8, O2, G-series.
- **O11 — Config docs + broadcast-capacity/lag knob + no-magic-defaults audit.** AC: documented `[dashboard]`/`[websocket]` blocks with defaults; lag knob sizing guidance + UI surfacing; audit new serving knobs for hardcoded values. Effort S. Severity Medium. Deps O8.
- **O12 — Dashboard CHANGELOG + browser-support/limitations + upgrade/compat matrix.** AC: user-visible changelog tied to embed version; supported browsers + Phase-2-unavailable note + known edges; per-release compat statement (which server versions, reload-required, rollback story). Effort S. Severity Medium. Deps O3.

### Documentation

- **G1 — Console operator guide + per-view reference + swimlane reading guide + 3am runbook.** None today (lone README is build/embed). AC: clean operator can reach console, pick namespace, read a swimlane, diagnose a failure, know available vs gated actions; annotated swimlane anatomy; triage runbook honestly marking server-gated incident classes. Effort L. Severity P0. Deps views stable.
- **G2 — Provenance/keyboard/deep-link explainers + in-app help overlay.** AC: provenance-footer meaning; ⌘K + per-view binding cheat sheet mirrored by `?`-overlay; per-view URL query-param schema for hand-built links. Effort M. Severity High. Deps D1/D2/D3.
- **G3 — Rendered 29-event + cluster + command + status-projection reference.** 184 ts-rs doc-comment lines sit unused. AC: auto-generated event reference (grouped, envelope + key fields) wired into the `export_dashboard_wire_types` discipline so it can't drift; cluster/command references flagged "typed but not emitted"/"handler unimplemented"; status-as-projection explained. Effort M. Severity P0 (events) / P1 (rest). Deps types.
- **G4 — Capability matrix ("real vs gated vs aspirational").** AC: each VISION §4 concept × {shipped / server-gated (names missing piece) / Phase-1.5 / Phase-2-blocked}; the canonical honesty artifact. Effort S. Severity P0. Deps none.
- **G5 — Dashboard CONTRIBUTING + architecture + quality-gates checklist + design-language doc + type-gen workflow.** AC: feature-folder map, WS-manager seam, projection layer, bespoke-swimlane-vs-house convention; the 12 binding gates as a self-check; hand-plane palette + prohibitions; regenerate-types ripple incl. the exhaustiveness guard. Effort L. Severity P0. Deps none.
- **G6 — Two-execution-modes architecture doc + engine-invariants-in-UI + screenshots/clips.** AC: thin-client vs additive-WASM with §8 honesty (WASM doesn't compile, untested, Phase-1 independent); invariants → why reopen issues a command not a write; canonical screenshot set (post-hand-plane) + motion clips (swimlane append, scrub, token flow, failover) reproducible from seeded data. Effort M-L. Severity P1 / P0 (screenshots). Deps G1, U3, views.
- **G7 — Console-facing WS-protocol section + failover-demo console walkthrough + microcopy + gated-affordance copy.** AC: add `cluster` variant + console resync framing to API doc; console-driven failover walkthrough with captures; reviewed calm/empty/error copy; honest disabled-control reasons (from GATED_FEEDS). Effort M. Severity P1. Deps C9, F8.

---

## Critical path & sequencing (to M1)

```
Reconciliation (R1 R2 R3)                         [Critical gate — green tsc/biome/test]
   ↓
Live-path correctness (R4 → R6 R7 → R5)           [fix resubscribe storm FIRST; makes perf real]
   ↓
QA infra in parallel (Q1 → Q2; Q4)                [DOM harness + CI gates unblock everything testable]
   ↓
S0 shell + routes (F1)                            [serial; publishes routes; WS-survives-nav]
   ↓
Parallel cohort once F1 green:
   ├─ F2 list-live + virtualization (D8)
   ├─ F3 29-variant + panel  → F4 live-detail → F5 swimlane → D7 virtualization → U6/U7/U8 → F6 scrubber
   ├─ F7 reopen-diff (preview)
   ├─ F8 triage + calm (workflow-only first)
   └─ F9 search (client-floor first)
   ↓
WS3 integration (server then client):
   C1 emit sites → C2 socket routing → C3 client arm → C4 seq-gate → C5 lag → C6/C7/C8
   ↓ (enables)
   F8 topology incidents · C9 cluster map → C10 failover-motion → C11 time-scrub · C12 push-not-poll
   ↓
WS4 disciplines woven across views:
   D1 palette+keyboard · D2 deep-link · D3 provenance (needs server node+seq stamp) · D4 watchdog · D5 stale-veil · D6/D7/D8 perf · D9 calm states
   ↓
A11y + hand-plane + UX foundation in parallel:
   U1 U2 U3 (live bugs + binding gate, do early) · U4 U5 U11 · Y1–Y21
   ↓
S9 hardening (A1–A11 resync/gap/lifecycle) + provenance/freshness end-to-end
   ↓
Quality close-out: Q9 e2e → Q10 failover-in-UI · Q12 visual-reg · Q13 perf/soak · Q14 conformance · Q16 leak audit
   ↓
Security + deploy + docs gate (S1–S15 pre-prod set · O1–O12 · G1–G7)
   ↓
=== M1 SHIPPABLE ===
```

External blockers on the critical path: **AW** (REST/WS contract: resync floor seq A6, node+seq stamp D3, keepalive D4, search endpoint F9/Q8, base-path O8); **WS3** (cluster emit + routing — partly in flight); **FEP** (shard-event promotion for C7/C10). Do early/independent regardless: U1, U2, U3, R4, Q1, Q2, S10/O1, G4.

M2 starts after M1's command transport + RBAC foundations (S4) and each Tier-2 server command. M3 starts only after WASM substrate remediation passes real headless-browser tests.

---

## Decisions (resolved 2026-06-29)

1. **RBAC / capability model (S4).** Today authorization is `deploy: bool` + namespace grants. ADR-020's spec would conflate "can deploy a package" with "can drain/kill a node." Recommend a three-tier capability split (read / per-workflow-command / cluster-control) with per-workflow commands reusing the namespace ownership guard. Needs sign-off before any M2 command lands, because it shapes claim fields and the command-seam contract.
   - **Resolved (ADR-022):** Three-tier capability split — READ / PER-WORKFLOW-COMMAND (cancel/reopen, reuses the namespace-ownership guard) / CLUSTER-CONTROL (drain/handoff/kill, a separate high grant). A deploy token must NOT imply cluster-kill authority; this replaces the flat `deploy: bool` + namespace grants. Lands before any M2 command.
2. **Control-action safety model.** Idempotency-key + wait-for-effect-event reconciliation (never optimistic history write), server-computed blast-radius pinned to `cluster_seq` with optimistic-concurrency commit-block. Confirm this is the round-trip discipline before building the command client — it's hard to retrofit.
   - **Resolved (ADR-023):** Idempotency-key + WAIT-FOR-EFFECT-EVENT reconciliation (the UI never optimistically writes history — it waits for the server's effect event), plus a server-computed BLAST-RADIUS preview pinned to `cluster_seq` with optimistic-concurrency so a stale preview cannot execute. Mirrors the engine's fencing/CAS model.
3. **Swimlane axis: seq-rank vs temporal (U6).** Seq-rank preserves the ShiViz partial-order argument but makes duration meaningless; operators expect width to mean time. Recommend dual-mode (logical default, temporal toggle) — confirm priority, as it's L effort on the centerpiece.
   - **Resolved (ADR-024):** DUAL-MODE — logical seq-rank default (preserves the partial-order/ShiViz correctness argument) + a temporal-width toggle (operator-expected duration). L effort accepted on the centerpiece.
4. **Freshness/staleness mechanism (D4).** Server keepalive frames (preferred, needs server work) vs client last-frame-time heuristic. Decide before S9, and confirm whether AW can add keepalive + resync-floor-seq + node/seq response stamping (D3/A6 depend on it).
   - **Resolved (ADR-027):** Server KEEPALIVE FRAMES (+ server stamps node + last-applied seq on responses) over a client heuristic; feeds ADR-016 provenance — a connected-but-silent socket downgrades freshness.
5. **Virtualization library (D7/D8).** Recommend `@tanstack/react-virtual` (house-stack-aligned). Alternatively, accept strict server-paging as the list "virtualization story" — but the 10k-event swimlane needs real windowing regardless. Confirm the dependency add.
   - **Resolved (ADR-028):** `@tanstack/react-virtual` (house-stack aligned). The 10k-event swimlane needs real client-side windowing regardless of server-paging.
6. **Theme scope (U4).** Is light theme a Phase-1 obligation (Temporal ships both) or deferrable? Token set already exists; the work is the toggle + a light-theme audit of status colors.
   - **Resolved (ADR-025):** Light-theme DELIVERY deferred to Phase 1.5 (ship dark-first, polished); but the design-TOKEN ARCHITECTURE is built to best practice NOW (multi-tier semantic tokens, fully theme-swappable) so adding light mode later is a token-map addition, never a component refactor. Both theme maps live at the token layer (dark = shipped, light = defined-now/delivered-1.5). See DESIGN-TOKENS.md.
7. **Phase-1 boundary for the cluster map.** Tier-0/1 ride WS3; if WS3 slips, do we ship the failover view as the cluster surface and defer the general map, or block? Recommend: ship coarse cluster state derived from existing query data with honest "last-known" provenance (C7), upgrade when WS3 lands.
   - **Resolved (ADR-029):** If WS3 slips, ship coarse LAST-KNOWN cluster state from existing query data with honest provenance (C7), upgrade to the live map when WS3 lands; never block the console on it.
8. **TLS posture (S12).** Implement in-process rustls vs hard-require a TLS-terminating proxy. Recommend proxy-required + refuse non-loopback plaintext in prod for M1, with rustls as a later option.
   - **Resolved (ADR-026):** Proxy-required for M1 + refuse non-loopback plaintext in prod; in-process rustls kept as a later option.
9. **Audit durability (S6).** `tracing::info!` is not an audit store. Decide the durable sink (dedicated stream / `audit` namespace / hash-chained log) before M2 commands carry real authority.
   - **Resolved (ADR-030):** A real DURABLE sink (dedicated `audit` namespace or hash-chained log) before any M2 command carries authority; record DENIALS too. `tracing::info!` is not an audit store.
10. **Event-reference generation (G3).** Auto-generate from ts-rs doc-comments (no drift, recommended) vs hand-curate (richer prose, drifts). Recommend generation wired into the existing wire-types CI guard.
    - **Resolved (ADR-031):** AUTO-GENERATE from ts-rs doc-comments, wired into the existing wire-types CI guard (zero drift).

---

## Definition of done (highest standard)

Every shippable increment must pass:

- **Green gates:** `bun typecheck` + `bun check` (Biome) + `bun test` all pass; zero `any`, `@ts-ignore`, or lint-disable comments; these run as required CI checks (Q2).
- **Exhaustiveness:** all 29 event variants handled with a compile-time `assertAllVariantsHandled(never)` that *fails the build* on an unhandled variant (proven by a negative-compile test, Q4).
- **No silent failures:** every WS error, payload-decode failure, resync edge, and gap surfaces to the UI; no `console.warn` swallow (guard test).
- **Deep-linkable (ADR-015):** every shareable view state in the URL, router-driven; copy→reload→back/forward restores exact state.
- **Provenance (ADR-016):** source node + last-applied seq + freshness + "viewing a survivor" always visible; a silently stale view is a defect; a connected-but-silent socket downgrades freshness.
- **Keyboard-first (ADR-017):** every primary surface operable without a mouse; command palette spine; bindings surfaced inline; full SR/keyboard a11y (combobox pattern, roving nav, focus management).
- **Performance budgeted (ADR-018):** swimlane ≤16ms/frame at 10k events (virtualized), triage top-incident <1s/one-screen, list/search stream not block; budgets written down and measured in CI; regression is a defect, not a tuning opportunity.
- **Calm state (ADR-019):** healthy/empty/loading/error states designed and distinct per surface; calm = positive vitals, not an empty box.
- **Hand-plane (VISION §1):** zero opacity color modifiers / glow / shadow (CI grep gate); status reads by solid color + shape + position + accessible label; every pixel serves reading/diagnosis/action.
- **Read-only to live history (invariant 3):** dashboard never appends to a live workflow's history; all mutations are commands to the server's single writer (code audit).
- **House stack & generated types:** React 19 / Vite / Tailwind v4 / Bun / Biome / TanStack Query / shadcn-Radix; wire types regenerated via ts-rs, never hand-edited (`wire-types-no-diff` CI guard).
- **No magic defaults:** no hardcoded limits/timeouts; values from builder/server/config (review gate).
- **No zombie code:** all exports reachable; dead code removed.
- **A11y AA:** axe-clean in CI + manual NVDA/VoiceOver pass per view; reduced-motion honored; AA contrast both themes; 24px targets.
- **Tested behavior:** interactive DOM tests for every interactive component; e2e (incl. failover-in-UI) and resync/overnight-liveness pass; visual-regression baselines hold; coverage floor met.
- **Secure:** the pre-production security set (secure-by-default, RBAC, audit, security headers, TLS-required, rate limits, token hygiene, XSS audit) passes before operators touch production.
- **Operable & documented:** `/version` + skew detection; cached/compressed/hardened asset serving; container + run doc; operator guide, capability matrix, event reference, contributor guide — and every gated capability is documented as gated, never implied shipped.
