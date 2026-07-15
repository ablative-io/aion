# Spec: time-based swimlane with true sub-lanes, axis toggle, continuous scrubber

Operator-approved design (2026-07-15). Rework the workflow-detail swimlane in
`apps/aion-ops-console/src/features/workflow-detail/swimlane/` so the chart
runs on a time axis, fits the viewport, nests child workflows as true
sub-lanes inside the chart, scrubs continuously, and can toggle back to a
stepped (ordered) axis. Pure front-end: no server, engine, or schema changes.

## Ground truth (verified in code — trust these)

- Every timeline event already carries `recorded_at` (UTC string) on its
  envelope: `event.data.envelope.recorded_at`, read helper at
  `features/workflow-detail/lib/timeline.ts:83`. It is the engine's
  replay-determinism timestamp — parent and children share one store clock.
- Today's x-axis is a dense rank over the parent's own event sequences
  (`laneLayout.ts` — `buildRankIndex`, `layoutSwimlane`), 80px per rank
  (`Swimlane.tsx` `COLUMN_WIDTH`), so track width grows with event count and
  scrolls horizontally. The operator wants that gone.
- The scrubber is stepped over distinct seqs (`scrub.ts` `scrubSequences`)
  and state reconstruction is a pure prefix slice (`prefixUpTo(entries,
  scrubSeq)`), already seq-cut based and reusable.
- Child lanes exist (`child:<workflowId>` in laneLayout) and the current
  expand toggle renders a full `EmbeddedRunView` BENEATH the parent chart —
  the operator explicitly rejected that placement ("birds in the nest"): the
  child's events must render INSIDE the chart as sub-lanes.
- Child timelines are fetched with the existing `useWorkflowHistory` hook
  keyed by (namespace, child workflow id); `EmbeddedRunView.tsx` shows the
  wiring including the ancestry cycle guard — keep the guard, retire the
  beneath-the-chart placement.

## The build

### 1. Time axis, fit-to-view

- New pure module (e.g. `timeLayout.ts`) mapping each bar to fractional
  positions: `x = (recorded_at − t0) / (tEnd − t0)`. `t0` = first event's
  recorded_at across the parent AND every expanded child; `tEnd` = last
  event's recorded_at, or "now" for a running workflow (the chart compresses
  as time passes — accepted).
- Track width = container width minus the 168px label column
  (`LANE_LABEL_WIDTH`). Use a ResizeObserver; never scroll horizontally.
- Bars whose time span is under a minimum render width get a floor (e.g.
  6px) and render as markers. Overlapping sub-threshold neighbors in one lane
  collapse into a cluster chip showing the count (e.g. "×4"); clicking a chip
  opens selection on its first event and the chip's members are reachable
  from the detail surface. This is the burst treatment the operator accepted.
- Linear time only. NO gap compression in this pass (explicitly deferred).

### 2. True sub-lanes (the point of the whole change)

- Expanding a child lane splices the CHILD's own lanes as depth-indented rows
  directly under the parent's child lane row, positioned on the SAME shared
  axis (time mode: by the child's own recorded_at values; stepped mode: see
  §4). Lane list becomes a flattened tree with a `depth` field; indent the
  label cell per depth, keep every track starting at the same x origin so
  bars align across depths.
- Recursive: an expanded child's own child lanes expand identically. Keep the
  ancestry cycle guard from EmbeddedRunView.
- Lazy: fetch a child's timeline only on expand (existing hook — history
  backfill is durable and complete); collapse drops the rows. Late expansion
  must show the child's whole history (this is already the data contract).
- Selection: clicking a child's bar selects within that child — the
  DetailSheet / AttemptNavigator / transcript / intervention surfaces render
  scoped to the CHILD workflow id (they are already workflow-id-driven
  components; feed them the right id + namespace). This replaces
  EmbeddedRunView entirely — delete the beneath-the-chart rendering path and
  its placement-specific tests, migrate the still-valid behaviors (cycle
  guard, late-expand full history, lazy subscription) into the new shape's
  tests.

### 3. Continuous scrubber

- The scrubber becomes a continuous time slider over [t0, tEnd]: drag
  position → timestamp t → per-workflow cut seq (max seq whose recorded_at ≤
  t, computed per expanded workflow from the same t) → existing `prefixUpTo`
  per workflow. Between events the reconstruction plateaus — honest.
- In stepped mode the scrubber snaps to the global order positions (§4).
- Bar-click selection stays seq-exact (unchanged semantics).

### 4. Axis toggle: time ↔ stepped (operator's addition)

- A visible toggle on the chart switches the x mapping:
  - **time**: §1.
  - **stepped**: dense rank over the UNION of all visible events (parent +
    every expanded child) ordered by `(recorded_at, seq)` — one global order
    across workflows, uniform spacing, still fit-to-view (rank width =
    trackWidth / rankCount, with a sane minimum that may reintroduce
    horizontal scroll ONLY in stepped mode when rankCount is huge).
- The toggle is pure view state (default: time). Both modes share the same
  lane tree, selection, and scrubber semantics (stepped scrub = snap to
  ranks).

## House rules (non-negotiable)

- Work ONLY in this worktree. NEVER write to /tmp; build outputs and logs go
  under the repo. Console commands run in `apps/aion-ops-console`.
- Rust untouched. Do NOT run the embed regen (`cargo xtask build-ops-console`)
  — the orchestrator regenerates the embed from the main checkout at fold
  time (shared-target path-baking hazard). Console gates only.
- Biome format (`bunx biome format --write .`), `bun run typecheck`,
  `bun test` — full outputs to files under `<worktree>/time-swimlanes-gates/`
  with an exit-code manifest.txt. All must exit 0.
- TS/React house style: no `any`, no suppressions, match the feature's
  existing component/test conventions (SSR string-render tests, pure-module
  unit tests).
- Commit with explicit paths (never `git add -A`), trailer:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
  Commit this spec file with the work.

## Tests (the bar: each test fails if its feature breaks)

1. timeLayout pure math: positions from recorded_at; min-width floor;
   cluster grouping; running-workflow tEnd handling.
2. Lane tree: expand splices indented child rows on the shared axis;
   recursion (grandchild); cycle guard; collapse removes rows; late expand
   shows full child history.
3. Toggle: time↔stepped produce the documented mappings; stepped union order
   is (recorded_at, seq) across parent+children.
4. Scrubber: continuous t → per-workflow cuts → prefix reconstruction;
   stepped snapping.
5. Selection at depth: child bar click yields child-scoped selection state
   (the detail surfaces receive the child workflow id).
6. At least one mutation-style guard: re-introduce the old behavior (e.g.
   rank math over parent-only seqs, or beneath-the-chart child rendering)
   and show exactly the guarding test fails — record it in the gates dir,
   then restore.

## Out of scope (do not touch)

Gap compression; server/engine anything; transcript feature internals;
EmbeddedRunView replacements beyond what §2 names; ops-console embed regen;
any other feature directory.

## Report back

Branch, head commit, gates manifest (verbatim exit codes), summary of files
touched, deviations from this spec with reasons, and any open questions —
honestly. Unrun gates are unproven claims.
