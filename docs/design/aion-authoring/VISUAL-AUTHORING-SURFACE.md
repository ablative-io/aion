# Visual Workflow Authoring Surface — design of record (draft 2)

Status: DESIGN DRAFT, PAIR-REVIEWED, awaiting Tom's conversational review.
Authors: the certifying pair (Vesper Lynd + Waffles the Terrible), per Tom's
direction of 2026-07-11 (~12:28Z): a visual workflow authoring surface, smooth
not blocky, with live sharing, where defining an action in either view
scaffolds the worker, through check → emit → package → deploy → run. Just the
pair on this for now.

Draft 2 folds the second half's review rounds R1–R3 (run-mode revision
binding; the seam's collaboration contract; the seam's delta primitive) and
conditions C1–C5. The D1 recommendation survived review intact WITH those
contracts on paper.

Subordinate to: [awl/AWL-UX.md](awl/AWL-UX.md) (the north star — especially
anti-goal 2), [awl/AWL-2-SPEC.md](awl/AWL-2-SPEC.md) (the language),
DESIGN.md (authoring model). Grounded in three code surveys run 2026-07-11:
Iridium (`~/Developer/ablative/iridium`), the aion ops console
(`apps/aion-ops-console` + `crates/aion-server`), and Meridian's editor/canvas
stack (`~/Developer/ablative/meridian/apps/web`).

---

## 1. The one constraint that shapes everything

AWL-UX anti-goal 2: **no second source of truth.** The canvas was always in
the north star — but as a *generated view*. Visual AUTHORING therefore cannot
be a separate visual artifact with an exporter; it must be a **projectional
editor over the `.awl` file itself**:

- The `.awl` text document is the only artifact. Always.
- The canvas renders a projection of the parsed document (steps → nodes,
  `route`/fall-through/`after` → edges, doc comments → node labels).
- A canvas gesture (add step, wire an outcome, rename a binding, edit prose)
  is a **structured edit to the AST**, immediately re-printed through the
  canonical printer back into the text pane.
- A text edit re-parses and re-projects the canvas.

This is only honest because of assets that exist as of this week, all on aion
main: `parse ∘ print = id` with **lossless comments** (AWL-2), **byte-canonical
fmt** (one true rendering — so printer round-trips never churn diffs), the
**span-indexed semantic API** (`aion_awl::semantic`, e1ef9a93 — types,
declaration sites, docs, binding provenance for hover/selection-sync), and the
**tree-sitter grammar** (`tools/tree-sitter-awl` — the single highlighting
source). Without byte-canonical printing, projectional editing produces diff
noise and dies in review; with it, a canvas edit produces exactly the minimal
text change.

Corollary (AWL-UX §5.1): the step's doc prose is the canvas node label AND the
ops-console live narration. So the authoring canvas and the run-view canvas
are **one component in two modes** — edit mode (this document) and run mode
(live narration over the existing swimlane/event-stream substrate). We build
edit mode; run mode inherits.

## 2. What the surveys established

**aion ops console** (`apps/aion-ops-console`): React 19 + Vite 7 + Tailwind
v4 + Zustand + TanStack Query, embedded in the server binary via rust-embed
(`crates/aion-server/src/ops_console/assets.rs`), served by `aion server`
out of the box. Real-time via one WS endpoint (`/events/stream`) with typed
subscription filters and resume. Deploy (`POST /deploy/packages`), start,
transcripts, intervene, reopen are all wired. **There is no text editor and
no AWL access**: the AWL toolchain (`check`/`fmt`/`emit`/`lsp`) is CLI-only
today — `aion_awl` is a dependency of `aion-cli` alone; the server's only
authoring endpoint compiles raw Gleam (`api/http/authoring.rs`) and nothing
calls it.

**Meridian** (`apps/web`): the same stack family (React 19, Vite, Tailwind
v4, Zustand, TanStack). CodeMirror 6 with real in-house extension work (git
gutter, blame decorations, review gutter, merge/diff views). Its LSP UI is
**not** CM's native autocomplete — completion/hover are custom React overlays
positioned via the editor's pixel-metrics API, backed by REST. And the
collaboration problem is **already solved in production**: `features/canvas/`
is React Flow + **Yjs + y-websocket + Awareness** (shared doc, presence,
cursors — `useCanvasSync.ts`), plus three more React Flow surfaces including
a read-only workflow graph viewer with typed nodes.

**Iridium** (`~/Developer/ablative/iridium`): a genuinely well-built GPU
editor core — wgpu/glyphon rendering, rope buffer, **undo tree**, folding,
minimap, search, git decorations, **transparent glass compositing**, and a
**span-source-agnostic highlighting seam** (`setHighlightSpans`, written "for
Meridian server integration"). Workspace compiles clean today (native and
wasm targets both exit 0). Honest gaps on the *web* surface: single-cursor
(multi-cursor exists in core, unwired), no IME, no accessibility, no LSP
client or popup UI, no general widget/decoration API, **WebGPU-only** (no
fallback), no e2e tests, tree mid-refactor. Verdict: CM6 reaches
"daily-drivable with AWL highlighting + hover/diagnostics" in weeks; Iridium
needs ~2–4 months of integration-surface work — but its core is *nicer*
(rendering feel, undo tree, compositing) and its host-integration model is
the same one Meridian already uses for CM6.

## 3. Decisions (with reasoning)

### D1 — Editor: a seam, not a marriage. CM6 first, Iridium behind the same seam.

The convergent fact: Meridian's CM6 integration ALREADY treats the editor as
"text box + pixel metrics + decorations", with hover/completion as host-owned
overlays — which is exactly the contract Iridium offers (`positionToPixel`,
`getLayoutMetrics`, `applyCompletion`, `onMouseHover`, span-fed highlights).
So we define **the editor seam** as precisely that contract:

- content get/set + change events
- **apply-text-delta(ranges)** — the gesture-update primitive (review R3): a
  canvas gesture's canonical re-print reaches the editor as minimal range
  deltas (old and new text are both canonical, so the diff is well-defined
  and small), with selection/scroll position mapped across the edit and the
  whole gesture grouped as ONE undo unit. Full `setContent` per gesture is
  forbidden — it destroys cursor, scroll, selection, and undo granularity,
  and "smooth not blocky" dies at our own seam.
- **collaboration contract** (review R2, spec'd now, built in P4): remote
  deltas apply WITHOUT echoing through local change events (origin-tagged
  application), undo is scoped to LOCAL edits only, and remote cursors/
  selections render via decoration primitives. Both editors implement this
  one contract: CM6 via y-codemirror.next; Iridium's bill explicitly includes
  a Yjs-aware binding — its undo TREE must become origin-scoped or shared
  mode kills its own differentiator. Without this contract on paper, P4's
  two deliverables (sharing + the Iridium option) collide inside the seam
  and the no-rewrite claim is false.
- pixel metrics (`positionToPixel`, layout metrics, scroll events)
- highlight spans pushed in (byte-range + capture name), never computed
  inside the editor from its own grammar
- `applyCompletion`-class primitive edits
- decoration primitives: line background, gutter marks/text, underline
  ranges, remote-cursor/selection markers

Everything AWL-aware (hover cards, diagnostics, completion, selection⇄node
sync) lives in the host layer against the seam. **Ship on CM6** (mature,
IME/a11y/multi-cursor/browser-support for free, in-house expertise exists).
**Iridium takes the seat when its web surface catches up** — no host-layer
rewrite, because we never used CM6's Lezer or native autocomplete. Iridium's
differentiators (GPU smoothness, glass compositing OVER the canvas) are real
and worth wanting; this seam keeps the door open without betting the surface
on a 2–4 month editor gap. Highlighting single-source: **the tree-sitter-awl
grammar** produces spans (web-tree-sitter in a worker, Meridian already has
the pattern; or server-side), consumed as CM6 decorations today and
`setHighlightSpans` tomorrow. No Lezer grammar is ever written — one grammar,
three consumers (Zed, Neovim, web).

### D2 — Where it lives: the aion ops console, Meridian-hostable later.

Ops-console-out-of-box doctrine: the surface must work the moment you run
`aion server`. New `features/authoring/` module + route in the console,
following its conventions. The stacks are near-identical to Meridian's, so
hosting the same components inside Meridian later (Tom: "I actually do want
this inside of Meridian") is a packaging exercise, not a rewrite.

### D3 — The server grows an AWL facade; the browser never runs a checker.

New endpoints in `aion-server` (pattern already exists at
`api/http/authoring.rs`): `POST /awl/check` (span diagnostics as JSON),
`POST /awl/fmt` (canonical text), `POST /awl/emit` (staged behind the
same deploy capability gating as `/deploy/packages`).

**Check returns the semantic snapshot, not just diagnostics** (review C1): a
check response carries diagnostics PLUS the span-indexed semantic index
(types, declaration sites, docs, provenance — the `aion_awl::semantic`
analysis serialized). Hover, go-to-definition, and selection⇄node sync are
then **client-side lookups against the last-good index — zero round-trips on
mouse movement**. "The browser never runs a checker" stands: it runs a
lookup. A separate `POST /awl/semantic` endpoint is unnecessary; the stdio
LSP remains untouched as the editor-plugin surface.

**Emit-subset diagnostics are part of check, not a later surprise** (workbench
finding F26): three check-green shapes today refuse only at emit — the Gleam
stopgap emits a SUBSET of the checked language. The facade's check response
therefore carries TWO diagnostic classes: language errors (blocking, red) and
emit-subset warnings ("checks, but the current backend cannot deploy this
shape — <the stopgap's own message>", amber, span-anchored). On a canvas,
check-green-but-undeployable is betrayal-shaped; the boundary must be visible
while authoring. This class dissolves when #240 makes emission total for
checked programs — the warning channel then simply goes quiet. The response
also carries the roll-up (review C2): a per-document **deploys-green bit**
(check-clean AND emit-subset-clean), which is the bit the P3 deploy flow and
any document list render — never the check bit alone. In-process crate calls
— no stdio-LSP-over-WebSocket contortion; the stdio LSP remains the
editor-plugin surface (Zed/Neovim), the HTTP facade is the web surface, and
both are thin over the ONE checker. One wall of errors, now reachable by the
canvas.

### D4 — Canvas: React Flow, projectional, one component for edit + run modes.

React Flow (four production instances in Meridian, one read-only workflow
viewer already) + dagre auto-layout with manual-position override persisted
OUTSIDE the `.awl` (layout is view state, not source — a sidecar per-document
layout record server-side; the `.awl` never carries pixel coordinates).
Node = step (label = doc prose, per AWL-UX §5.1; badge = loop/fork/wait
markers). Edges = outcome routes (labeled `when`/`otherwise`), fall-through,
`after` dependencies (visually distinct). Selection syncs both ways through
spans (semantic API gives node↔span). Gestures in P2 (add step, draw route,
edit prose, rename binding) apply AST edits → canonical print → text pane
updates **as seam deltas** (D1); anything not yet expressible as a gesture
stays text-only — the canvas NEVER blocks the language.

Layout-sidecar keying (review C3): records key by step name, so the rename
gesture **migrates the layout record atomically with the rename edit**;
orphaned records (steps deleted or renamed outside the surface) are
garbage-collected on load. Layout is **per-user** view state (awareness-
style), decided now so P4 sharing doesn't reopen it — shared presence,
private arrangements.

Diagnostics on the canvas (review C4, workbench F18): one real defect can
manufacture phantom errors on legal bindings today. The canvas renders
**primary vs cascade distinctly** — the first error at a span paints its
node red; same-document downstream errors whose steps are already marked
unreachable render as muted/secondary, never four-alarm red. The checker
learning cascade suppression (F18's fix) improves this; the canvas must not
wait for it.

Child calls (review C5 — the exam pair exists today): a `child` call
projects as its own node type showing the DECLARED contract (name +
signature). If the child's document is present in the workspace, the node
links to open it; cross-document graph expansion (inlining the child's
steps) is explicitly out of scope for P1–P3. A child whose document is
absent renders the contract stub with no link — never an error.

### D5 — Scaffolding: an action signature is a worker contract; generate the worker.

Defining an action (either view) yields a typed contract. "Scaffold worker"
generates a ready-to-run worker project for the chosen runtime — **Rust /
Python / Zig templates** (extending `aion new`'s scaffold machinery) with the
action's types projected into the target language and a TODO body per action.
**Shell actions need no code at all**: a shell-command worker runs a declared
command with typed args/env mapping — config, not source. The shell manifest
maps action name → command + arg/env projection ONLY; the contract (types,
timeout, retry) lives in the `.awl` and nowhere else — the manifest is
wiring, never a second source of truth. The scaffold, like everything else,
is generated FROM the `.awl` declarations (schemas fall out of the same
declarations — AWL-UX §3.1).

### D6 — Sharing: Yjs + awareness, the Meridian pattern, designed-in now, wired in P4.

Text doc as the shared artifact (Yjs text binding on the editor seam),
awareness for presence/cursors on both text and canvas. Meridian's
`useCanvasSync` + y-websocket relay is the proven shape. P0–P3 are
single-author; the seam (document store abstracted from the editor) is
designed so Yjs slots in without rework.

### D7 — The run loop closes in the same surface — and run mode binds to the DEPLOYED revision, never the buffer.

Check-clean → emit → package → deploy → start → watch, as one guided flow
using endpoints that already exist (deploy/start/transcript/swimlane) plus
D3's facade. This is #215's `aion run --watch` promise wearing a UI. Run mode
of the canvas (live narration, prose labels, the swimlane substrate) is the
same projection driven by the event stream instead of the cursor.

**Revision binding (review R1 — correctness stakes).** If run mode projected
the editor buffer while the engine runs the deployed package, one edit after
deploy makes the narration a lie: nodes lighting up on a graph that no
longer matches the running workflow. Therefore: the document store carries
**content-hash identity** per revision; deploy records the deployed
revision's hash; **run mode projects the DEPLOYED revision's text**, fetched
by hash — never the live buffer. When the buffer has moved past the running
revision, the surface shows an explicit **"editor has drifted from the
running revision"** state (with a one-click diff). A stale artifact must
never silently narrate as fresh — this is the F14 lesson wearing a UI.

## 4. Phases

- **P0 — text authoring in the console.** D3 facade (check/fmt/semantic);
  `features/authoring/` route; CM6 behind the seam; tree-sitter spans via
  worker; diagnostics-on-idle (check debounce); hover/defs from semantic;
  fmt button; save/load documents (server-side document store, workspace
  dir). Exit bar: author `doc_certification.awl` from scratch in the browser
  with live diagnostics, never touching a terminal.
- **P1 — canvas projection (read).** Graph view of the open document, prose
  labels, selection sync text⇄node, layout sidecar, primary-vs-cascade
  diagnostic rendering, child-node contract stubs (D4). Exit bar: every
  corpus file (166+) projects without error **and re-prints byte-identically
  after projection** (proves the projection reads losslessly); clicking any
  node lands the cursor on its step.
- **P2 — canvas editing (projectional).** Gesture set v1: add step, add
  action (with type editor), draw outcome route / fall-through, edit prose,
  rename binding (semantic-API-backed), delete step. Every gesture =
  AST edit → canonical print. Exit bar: build a 5-step workflow entirely on
  the canvas; the produced text is byte-canonical.
- **P3 — scaffold + run.** D5 worker scaffolds (shell first — zero code —
  then Rust/Python/Zig); D7 guided deploy-and-run flow; run-mode canvas.
  Exit bar: Tom's demo — draw it, scaffold a shell worker, deploy, run,
  watch it narrate itself, without leaving the browser.
- **P4 — sharing + Iridium option.** Yjs/awareness live sharing wired
  against the seam's collaboration contract (spec'd since D1 — origin-tagged
  deltas, local-scoped undo, remote-cursor decorations); Iridium behind the
  seam if/when its web surface closes the gap, its bill now explicitly
  including the Yjs-aware binding (or as the glass-composited "pro" mode
  where the editor floats over the canvas).

## 5. Open questions for Tom (conversational)

1. **Editor call** — the seam + CM6-first + Iridium-later is our
  recommendation; the alternative is reviving Iridium NOW and accepting the
  editor gap on the critical path. (Our read: the surface shouldn't wait on
  the editor; the seam preserves the Iridium future.)
2. **Layout persistence** — sidecar view-state server-side (recommended) vs
  pure auto-layout always (simpler, but "smooth not blocky" suffers when the
  graph re-arranges under you).
3. **Where documents live** — a server-side workspace directory of `.awl`
  files (recommended; git-friendly, CLI-parity) vs database-only documents.
4. **Scaffold languages order** — shell (P3, zero-code) then Rust first?
  Python/Zig next? (Gleam full-fat remains, as always.)

## 6. Anti-goals (inherited + new)

All of AWL-UX §6 applies unchanged. Additionally: **no pixel coordinates in
the `.awl`** (layout is view state); **no canvas-only semantics** (anything
the canvas can express, the text expresses — the canvas is a view, never a
capability gate); **no second highlighting grammar** (tree-sitter-awl is the
single source; no Lezer port); **no editor lock-in** (nothing AWL-aware may
import a CM6 type — everything goes through the seam).
