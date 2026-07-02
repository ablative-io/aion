# The Morphing-Entity Workspace

Tom's UI concept, captured verbatim-in-spirit 2026-07-03 during the ops
console design pass. This is a **standing vision doc** — bigger than the
console, feeding it. The console builds the seeds (see
[OPS-CONSOLE-DESIGN-LANGUAGE.md](./OPS-CONSOLE-DESIGN-LANGUAGE.md)); the full
workspace is a future build, likely spanning the console, meridian, and
future frontends.

## The core idea: entities with form factors

Every live thing in the system — a workflow, an agent step/attempt, a worker,
a session — is an **entity that can exist at four form factors**, morphing
between them dynamic-island-style (spring morphs, one continuous surface):

1. **Pill** — the minimum. Status dot, name, and *life*: the entity's
   activity gently streams through it (log headlines, tool-use one-liners —
   norn's per-tool `description` field is perfect fuel). Mouse over and the
   stream is legible; look away and it's ambient.
2. **Card** — click to expand. More context (current step, recent events,
   key metrics) plus a *small* input field for quick interactions — a
   one-line nudge to an agent without opening anything.
3. **Widget/window** — expand again: the full-fat experience. Full
   transcript, full controls; this is where a heavy control surface
   (meridian's smart-comms-style input) would legitimately spring out.
4. (Implied zero-state: a dot/badge when docked somewhere crowded.)

**The key principle — form, not scale.** At each smaller size the entity
takes a *different form that is more useful at that size*, not a shrunken
window. This is the explicit critique of tiling window managers (hyprland,
meridian's own windowing): shrinking a window makes it less useful;
collapsing an entity to a pill makes it *differently* useful.

## Docks: the other half

Entities are **draggable between dockable spots**. The workspace is managed
by a **dock manager, not a window manager** — it manages where entities are
docked and at what form factor, with hyprland-style layout flexibility.

- **The sliding sheet** — a large sheet that slides from screen edge: the
  browser's "alternate screen" (tmux sense). Summon it, arrange what you're
  following, dismiss it.
- **Named panes/workspaces** — "what I'm following now", per-project spaces;
  macOS-style workspace swapping between them. For an always-running ops
  surface you're monitoring across days, jumping between followed sets must
  be one gesture — the palette is good, but spatial memory is better.
- **Drag semantics** — drag a workflow from the Gantt into the sheet: it
  docks as a pill and keeps streaming. Drag an agent step into your
  following pane, expand it one notch to card. Everything collapses to a
  pill; everything expands to a window; where it sits is yours.

## The Gantt is the first home of this

The console's Gantt bars ARE entities in embryo:

- **Bar = inline pill.** A running bar streams its agent's activity through
  itself (or reveals it on hover): tool descriptions, step headlines.
- **Click → card** anchored from the bar (expand-in-place, non-blocking):
  recent transcript tail + quick-input field.
- **Expand → the full transcript view** (the workflow detail surface).
- Same interaction grammar everywhere else an entity appears: navigator
  chips, workflow list rows, the header island.

## The long game: pills as nodes

Once entities are first-class draggable objects, they can become **nodes on
a flow canvas**: drag workflow A's pill next to workflow B, wire "when A
completes → trigger B". Same for agents. The organizational surface
*becomes* an authoring surface — the pill grammar gains genuine
functionality (composition, triggers) instead of being just a cool way of
organizing things. (Converges with the workflow-authoring redesign and
graph appetites — a workflow-definition graph rendered from the same
entity/node substrate.)

## The assistant panel (norn-powered, specialist hats)

A summonable AI assistant surface (natural tenant of the sliding sheet),
powered by norn, with **switchable specialist hats**:

- **Workflow author** — loaded with the authoring docs, SDK types, deployed
  package inventory; writes/edits workflows conversationally. (Converges
  with the workflow-authoring-agent idea — the loop that authors *verified*
  workflows.)
- **System operator** — loaded with cluster state, namespaces, diagnostics;
  "why did this run fail?", "drain that node".
- More hats as roles emerge (incident triage, deploy shepherd).

It renders through the same transcript machinery the console already has,
and it is itself an entity — pill (idle, ambient) ⇄ card (quick question) ⇄
window (real session). Dogfood note: the assistant's runs should themselves
be aion workflows on a local worker — the console operating itself.

## What the console builds NOW (the seeds)

No dock manager, no sheet, no flow canvas yet. But the Phase-0 kit is built
so the vision drops in later without rework:

1. **The entity component API is `<Entity form={pill|card|window}>`** with
   spring morphs between forms — the header island and Gantt-bar
   interactions are its first two consumers. Not one-off widgets.
2. **The pill knows how to stream** — the ambient log-through-the-pill
   renderer (fed by the live event/transcript streams) is built once.
3. **Anchored expand-in-place** (MorphingPopover/FloatingPanel machinery)
   is the same primitive the card form uses.
4. **URL-addressable entities** — every entity's every form deep-links; a
   docked pill later is just a stored reference.
5. Keybindings/palette treat entities uniformly (select, expand, collapse,
   follow) so dock actions slot into the same registry later.
