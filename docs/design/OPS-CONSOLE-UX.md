# Ops Console UX — Design Brief

Captured from Tom's live drive of the Phase-2 NOI demo (2026-07-03, workflow
`faaa1b04`), consolidating every UX direction given during the session. This
supersedes the framing of the old "dashboard professional-console disciplines"
backlog item (#130) and absorbs tasks #198/#201.

## Design values (the bar)

- **Linear-quality craft.** The console is a professional operating surface.
  When a choice arises, ask what Linear would do: information density without
  clutter, purposeful motion, dark-first polish.
- **Keyboard-first.** Every operator action reachable without the mouse:
  palette-style navigation, list traversal, focus that goes where the operator
  is looking. Retrofit-proof: new views ship with their keybindings.
- **Deep-linkable.** Every meaningful state has a URL — a workflow, an
  attempt, a transcript position, a timeline selection. "I can send you an
  exact link to the thing" is a hard requirement, not a nicety. (The SPA
  fallback fix for `/workflows/{id}` was the first brick; the rule
  generalizes.)
- **Operate, not observe.** Same doctrine as ops-console-out-of-box: every
  surface the operator can see, they can act on.
- **Real data only, socket-first** (standing; unchanged).
- **Nothing requires manual refresh. Ever.** If any part of a page updates
  live, all of it does. A "Refresh" button is an admission that a view was
  wired to a request/response endpoint when the data it needs is already
  flowing — or should be — on a stream the page holds. The live-attempts
  list is the canonical offender: the swimlane above it updates live from
  the workflow event stream while the attempts list (a wrap of the
  intervention router's REST enumeration) sits stale behind a button.
  Rule: views DERIVE from streams; REST is for actions and cold loads only.

## The step/attempt navigator (kills the Refresh button)

Replace the "Agent attempts" list with a step navigator derived from the
workflow event stream the page already subscribes to (plus history for cold
load): every attempt — running or finished — appears the moment its
ActivityScheduled/Started event flows, flips state on its terminal event, and
is selectable to load its durable transcript. Live attempts carry the chat/
intervene controls; finished ones are read-only. Selection auto-follows the
newest live attempt unless the operator has pinned one. No Refresh button.
This is the same substrate the Gantt click-through needs — one model, two
views.

**Post-mortem is a first-class requirement, not a live-only nicety.** Proven
live 2026-07-03: a run failed (norn hit a context limit) and the console
instantly showed NOTHING — the attempts list is the intervention router's
live enumeration, so a failed/finished workflow has zero inspectable
transcripts, exactly when the operator most needs them. The transcript data
is durable (O-keyspace tail); only the enumeration is live-only. The
navigator must enumerate attempts from workflow history/event stream — never
from liveness — so a dead run's every attempt remains selectable and
readable. Inspecting a failed run IS the ops-console job.

## The transcript surface

1. **Rolling windowed feed.** A fixed-height region the events roll through —
   the page itself never grows. Auto-follow at the tail; scrolling up detaches
   with a "jump to latest" affordance; virtualized so a multi-hour run stays
   light. (First cut shipped in `182c43c5` — verify it holds under a long live
   run; the page must not grow.)
2. **Intervention is chat.** Kill the "Inject message" side-box framing. A
   proper, well-proportioned chat input anchored beneath the feed; typing and
   sending IS the injection (priority toggle for interrupt-vs-queued); the
   operator's message renders inline in the feed as a turn in the
   conversation. Cancel stays as a distinct, honest control.
3. **Collapsible by kind.** Tool calls/results START COLLAPSED (one-line
   summary, expand on click); assistant messages and reasoning start expanded.
   Per-kind renderers over generic rows: tool args, diffs, code blocks with
   highlighting.
4. **Reasoning de-duplication.** The norn translator currently emits the
   reasoning summary twice — as the `reasoning_item` raw AND as an assistant
   Message with identical text — so the feed shows the same paragraph in both
   a "Reasoning" and a "Message · Assistant" entry. Fix at the source
   (`crates/aion-integration-norn/src/translate.rs`): a reasoning summary is
   reasoning, not an assistant message.
5. **Immediate intervention feedback.** Click → pending state within
   milliseconds; delivered/ack/timeout as distinct rendered outcomes (never a
   30-second silent wait ending in an opaque error). Pairs with the #200
   cancel-escalation work server-side.

## The timeline (swimlane → real Gantt)

The current lane view plots by SEQUENCE NUMBER, so a sub-second `provision`
renders longer than a ten-minute `scout`, bars never grow, and the scrubber
snaps between a handful of coarse steps. Rework:

1. **Time-proportional bars.** The x-axis is wall-clock time. A sub-second
   activity is a sliver; a ten-minute agent step dominates — as it should.
2. **Live-growing bars.** A running activity's bar extends in real time until
   its terminal event arrives.
3. **Continuous scrubber.** Smooth, pixel-continuous scrubbing over the run's
   time range — not sequence-number detents.
4. **Click-through.** Clicking an activity bar selects it and loads its
   detail — for agent steps, the transcript (deep-linkable per the URL rule).
5. Keep a compact sequence/list mode as the secondary view; time is the
   primary axis.
6. **Lane identity = activity, not occurrence.** Live-proven 2026-07-03: a
   dev↔review loop minted a NEW lane for every iteration, so the chart grew
   a row per cycle. Repeated executions of the same step must return to the
   same lane — one row per activity identity, with each attempt/iteration a
   separate bar along that row. The loop then reads as a visible ping-pong
   between two lanes, which is exactly the story the operator wants.
7. **Fit-to-width by default; zoom as the escape hatch.** Bars must not run
   off the edge into an unbounded horizontal scroll. Default behavior: the
   time axis rescales continuously so the whole run stays within the frame,
   proportions compacting as the run grows. Explicit zoom in/out (and then
   pan) is operator-summoned; on zoom-out-to-fit it snaps back to the
   self-scaling mode.

## Start-workflow form

1. **Nomenclature: "Workflow"** (the name), not "Workflow type" — align the
   field label, API docs surface, and console copy. One sweep, one vocabulary.
2. **Combobox, not free text.** The deployed packages/versions are known to
   the server — offer them (type-ahead filter). Typing a workflow name from
   scratch is a paper-cut with no upside.
3. **Input: file OR paste.** Accept a JSON file (file picker + drag-drop)
   as well as pasted JSON. Validate before submit; on error, point at the
   offending path.

## Sequencing note

None of this blocks the Phase-2 demo (the current surface is functional).
Natural build order: transcript items 4→2→3 (dedupe is a small server fix;
chat reframe and collapsing are one console pass), then the start-form (small,
self-contained), then the Gantt rework (largest), with keyboard/deep-link
disciplines applied to each piece as it's touched rather than as a big-bang
retrofit.
