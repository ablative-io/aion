# Ops Console Design Language

Companion to [OPS-CONSOLE-UX.md](./OPS-CONSOLE-UX.md). That doc says what the
console must do; this one says what it is made of — tokens, type, motion,
and the component kit — so every view inherits the craft instead of
re-inventing it. Written 2026-07-03 after a four-way study: the current
console, meridian's web app, the ablative.com.au site, and the two component
libraries Tom rates highest (motion-primitives, cult-ui).

## The thesis

An ops console whose *function* is the point, built like a restored Stanley
hand plane: you're here to work, and the tool being beautiful is part of why
the work is good. Not brutalist ("it's a tool, I'm not here to look at it")
and not decorated — crafted. Concretely:

1. **Summon and dismiss.** Best use of space ≠ cramming controls in. Controls
   and consoles arrive when needed (morphing out of the thing you're looking
   at) and leave when not. Focus on the work by default; spread out on demand.
2. **Motion is continuity, not flourish.** Springs and morphs exist to
   preserve the operator's sense of where things went. Never hard, never
   snapping, never gratuitous. Keyboard actions especially need motion to
   show what happened.
3. **One material.** Tokens, type, radius, springs decided once, applied
   everywhere. A new view ships in the language or it doesn't ship.

## Provenance: inherit meridian, don't invent

Meridian's web app is Tom's ops-console taste already built and battle-tested
on this exact domain (agent transcripts, runs, pipelines), on the identical
stack (React 19, Tailwind v4 CSS-first, shadcn new-york, Vite, Bun, Biome,
`motion` v12). The console **inherits meridian's design language** — token
structure, motion vocabulary, signature components — tightened where the
console's smaller scope allows. The marketing site contributes *restraint*:
feathered separation (gradients/blur over hard lines), generous focus, warmth.

## Tokens

Adopt meridian's token architecture wholesale (single vocabulary — this
replaces the console's current two competing sets, and defines the missing
`--surface-default`):

- **Canvas & surfaces (dark-first):** background `#121218` (warm near-black),
  surface ladder `--surface-base #0f0f14` → `-elevated #16161d` → `-card
  #1a1a22` → `-hover #252530`, code `#0d0d12`.
- **Borders:** alpha-white hairlines — `--border-subtle rgba(255,255,255,.04)`,
  `--border-default …,.08)`, focus ring in the accent at 50%.
- **Semantic status set, each with a 12%-alpha glow companion:** green
  `#4ade80` (healthy/complete), amber `#fbbf24` (running/working), red
  `#f87171` (failed/destructive), purple `#a78bfa` (sub-agent/special),
  cyan `#22d3ee` (live/streaming). Status is always communicated the same
  way: small colored dot + glow-tinted chip. Tokenized — never hardcoded
  per-component again.
- **Glass (summoned surfaces only):** `oklch(0.16 0.01 280 / 0.85)` bg,
  12px blur, oklch border — for palettes, floating panels, islands; not for
  in-flow cards.
- **Radius/spacing/shadows:** meridian's scales (`--radius 0.625rem` default,
  sm→2xl ladder; `--space-1..16`; layered shadows + glow shadows).

### The accent decision

Meridian is cyan `#22d3ee`; the Ablative brand is warm terracotta `#d4845a`.
**Decision: terracotta is the console's primary accent** — interactive
elements, selection, focus, the wordmark warmth; **cyan is demoted to a
semantic status color meaning "live/streaming"** (pulsing connection dot,
live-attempt indicator, growing Gantt bar edge). This makes the console
unmistakably Ablative, keeps warm-on-dark distinctiveness (everyone else's
console is blue), and gives "live" a dedicated color instead of overloading
the brand accent. It's one token to revert if it doesn't sing.

## Typography

- Body **DM Sans** (variable), mono **JetBrains Mono** — meridian's pairing.
  **Self-hosted woff2 in the embed** — the console ships in an air-gappable
  single binary; no external font requests, ever. (Finding: the current
  console *declares* these fonts but never loads them — everyone has been
  seeing system fallback.)
- Headings 600, `letter-spacing -0.02em`, lh 1.2; body lh 1.6; metrics in
  tabular-nums mono. Section headers uppercase, `tracking 0.15em`, 10-11px.
- Dense-but-legible: meridian's small-type register (10-12px chrome, 13-14px
  content) — an operator surface, not a marketing page.

## Motion vocabulary

Library: **`motion` v12** (`motion/react`). House rules:

- **The signature spring** (Tom's, from meridian — "the spring animation IS
  the aesthetic"): `{ type: 'spring', stiffness: 550, damping: 45, mass: 0.7 }`
  for surface morphs (island expand, panel summon). Secondary elements
  `{ stiffness: 350, damping: 35 }`; success/confirm `{ stiffness: 500,
  damping: 22 }` (a touch bouncy). Micro-transitions (hover, chevrons) stay
  CSS at 150-200ms ease-out.
- **Trigger→surface = shared-`layoutId` FLIP morph** (the flying-dot trick,
  the expandable card). **Expansion = measured height** (`react-use-measure`)
  under `AnimatePresence`. **Exits** fade + slight scale + `blur(10px)`.
- **Continuous values lerp**: Gantt bar growth, scrubber position, fit-to-width
  rescaling, and live metrics (per-digit odometer for counters) all animate
  through springs — nothing jumps.
- **Reduced-motion**: respect `prefers-reduced-motion` (springs → opacity
  fades). Craft includes accessibility.

## The component kit

Everything below is copy-in source (MIT, motion-primitives / cult-ui /
meridian's own components) adapted to our tokens — no new runtime deps beyond
`motion`, `cmdk`, `react-use-measure`, and the Radix primitives we already
carry. Vendored into `src/components/kit/`, restyled once, reused everywhere.

1. **MorphSurface / chat input** — port meridian's `AgentMorphSurface`
   pattern: the intervention chat is a 36-44px docked pill beneath the
   transcript (status dot, capability badges) that spring-morphs into the
   full input on focus/⌘I. The send button carries the **escalation state
   machine** (interrupt → shutdown → kill with 3s auto-decay) — the UI face
   of #200 cancel-escalation. Draft text persists across collapse.
2. **MorphingPopover / FloatingPanel** (motion-primitives + cult-ui) —
   non-blocking expand-in-place for: attempt actions, payload inspectors,
   priority toggle, deploy forms. Nothing modal unless it must be.
3. **Expandable rows** (cult-ui `Expandable` + meridian's per-tool renderer
   registry) — transcript tool calls/results collapsed to one-line summaries
   with status dots and pips, spring-expanding to rich per-kind detail
   (diffs, code with highlighting, stdout/stderr).
4. **Status island** — a Dynamic-Island-descendant surface in the workflow
   header: morphs between compact (workflow status dot + name) and expanded
   states as the run moves idle → running → intervening → terminal; borrow
   cult-ui's queued size-state reducer for choreographed transitions.
5. **Omni-palette** (`cmdk`, meridian's 4-mode pattern) — ⌘K: navigate
   workflows/namespaces, run actions (start, reopen, cancel, deploy), search
   events. The keyboard-first front door.
6. **Keybinding registry** — port meridian's scoped, remappable action
   registry (`localStorage`-persisted). Every operator action registered;
   list traversal (j/k), palette (⌘K), chat (⌘I), views (g then w/e/n).
7. **AnimatedBackground** (motion-primitives) — the shared-layout sliding
   highlight for nav tabs, attempt navigator selection, segmented controls.
8. **SlidingNumber / AnimatedNumber** — live counters (event counts,
   durations, token usage) tick like instruments, not re-renders.
9. **Disclosure / TransitionPanel** — animated collapsibles and directional
   view switches for detail panels.

**Drag-and-drop:** only where it earns its keep — the start-form JSON file
drop zone (native DnD, no library). No dnd dependency until a real
reordering need appears.

**Delivery:** the kit lives in the console first; once stable, publish to the
`@ablative` shadcn-style registry so meridian, the console, and future beamr
frontends share one material.

## What this changes in the build order

The UX doc's sequencing gains a **Phase 0: the material** — tokens (one
vocabulary, terracotta+status set), self-hosted fonts, `motion` + kit
primitives, palette + keybinding registry, URL-backed selection state. It's
the foundation the four functional builds (navigator, chat transcript,
start-form, Gantt) are built *from*, so craft is inherited, not retrofitted.
Phase 0 also pays down: the missing fonts, the token split, hardcoded status
colors, the undefined `--surface-default`.

Then per OPS-CONSOLE-UX.md: translator dedupe → navigator + chat reframe
(on the new kit) → start-form → Gantt. Keyboard + deep-link disciplines apply
to each piece as built. Every view is verified against the live console —
in-browser, eyes-on — before it merges.
