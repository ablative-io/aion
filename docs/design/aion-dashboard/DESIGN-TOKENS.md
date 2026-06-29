# Aion Ops Console — Design-Token Architecture

**Status:** Binding contract for WS4 / implementation.
**Scope:** `apps/aion-dashboard` (React 19 / Vite 7 / Tailwind CSS v4 / shadcn-Radix / Bun / Biome).
**Authority:** This document is the single normative spec for the dashboard's color, surface, type, spacing, radius, motion, focus, and status token system. Where it conflicts with ad-hoc values in components, **this document wins** and the component is wrong.
**Related decisions:** ADR-015..021 (`docs/design/decisions.json`); resolved ops-console decisions D1–D10; VISION.md §1 (the hand-plane principle). This spec is the concrete discharge of **D4** — _build the token architecture to best practice NOW so adding light mode later is a token-map addition, never a component refactor._

> Tom, D4 (verbatim intent): "I really don't want it to become a nightmare later." The whole point of this document is that the nightmare is structurally impossible: components never name a color, surface, or status hue — they name an _intent_, and intents resolve through a theme map. A new theme is a new map. Nothing in `components/` changes.

---

## 0. The four load-bearing rules (read these first)

1. **Three tiers, one direction of reference.** Primitives → Semantic → (optional) Component. Reference only ever flows _downward_: semantic tokens reference primitives; component tokens reference semantic; **components reference semantic only**. Upward or sideways references (a primitive referencing a semantic, a component referencing a primitive) are defects.
2. **Components never name a raw value.** No `#hex`, no `rgb()/hsl()`, no Tailwind palette primitives (`bg-red-500`, `text-sky-300`), no opacity color-modifiers (`/30`, `/15`, `bg-black/40`). Components consume **semantic tokens only**, via the Tailwind `@theme`-bound utility classes (e.g. `bg-surface-default`, `text-status-failed`) or `var(--sem-*)`. Enforced by the lint/CI guard in §8.
3. **Theming happens at exactly one layer — the semantic layer.** Primitives are theme-agnostic raw scales. Semantic tokens are defined _twice_: once under the dark map, once under the light map. Switching themes re-points every `--sem-*` variable; not one component re-renders differently because of a code change.
4. **Hand-plane: status reads by SOLID color + shape + position + accessible label — never by opacity, glow, or shadow.** (VISION.md §1: "If a pixel does not aid reading, diagnosis, or action, it does not earn its place.") There is no shadow primitive and no glow primitive in this system. Their absence is intentional and load-bearing; do not add them.

### 0.1 Current conformance debt (the zero-edit property is an END STATE, not a present fact)

This document describes the **target** architecture. The shipped app does **not** yet conform: several components hard-code Tailwind palette primitives and opacity color-modifiers directly (the audit's `bg-red-500/15`, `text-sky-300`, `opacity-70` patterns), and define their own status palettes inline rather than reading the §2.2 semantic status registry.

Because of that, the "adding light mode touches no component" guarantee (§5, §9) holds **only after** the following components are migrated to consume the §2.2 semantic status registry and semantic surface/text/border tokens — never raw palette primitives or alpha:

- `LaneBar.tsx`
- `IncidentCard.tsx`
- `ConnectionIndicator.tsx`
- `ActivityGroup.tsx`
- `FirehoseFeed.tsx`
- `NamespaceSelector.tsx`
- the swimlane status→color map

Until that migration lands, switching `data-theme="light"` **will** render those components incorrectly (wrong/illegible colors), and the hand-plane CI guard (§8) will flag their hard-coded values. **This migration is a hard PRECONDITION** for both light-theme safety and §8 conformance — it is the first checklist item in §10 and is tracked as a work item in COMPLETE-SCOPE.md (M1 foundational backlog). The zero-component-edit property in §5/§9 is the property of the **post-migration** codebase: once a component names only intents, a new theme is a new map and nothing in `components/` changes again.

---

## 1. Tier 1 — Primitives (raw, theme-agnostic scales)

Primitives are the substrate: raw values with **no meaning attached**. A primitive never knows whether it is "a background" or "an error." It is just `cyan-500`. Primitives are identical across themes — a theme is a different _selection_ of primitives at the semantic layer, never a different primitive table.

Naming: `--prim-<category>-<step>`. Color steps follow a 50→950 ramp (50 = lightest, 950 = darkest), so the _same step number means the same lightness_ in any ramp — which is what makes light/dark inversion mechanical.

### 1.1 Color ramps

Neutral (the surface/text spine):

| Token | Value | Token | Value |
|---|---|---|---|
| `--prim-neutral-50`  | `#f7f7f8` | `--prim-neutral-500` | `#71717a` |
| `--prim-neutral-100` | `#ececef` | `--prim-neutral-600` | `#52525b` |
| `--prim-neutral-200` | `#d4d4d8` | `--prim-neutral-700` | `#3f3f46` |
| `--prim-neutral-300` | `#a1a1aa` | `--prim-neutral-800` | `#27272e` |
| `--prim-neutral-400` | `#8a8a93` | `--prim-neutral-850` | `#1f1f26` |
| | | `--prim-neutral-900` | `#1a1a22` |
| | | `--prim-neutral-950` | `#121218` |
| | | `--prim-neutral-1000`| `#0f0f14` |

Accent — cyan (brand / running / focus):

| Token | Value | Token | Value |
|---|---|---|---|
| `--prim-cyan-100` | `#cffafe` | `--prim-cyan-600` | `#0891b2` |
| `--prim-cyan-200` | `#a5f3fc` | `--prim-cyan-700` | `#0e7490` |
| `--prim-cyan-300` | `#67e8f9` | `--prim-cyan-800` | `#155e75` |
| `--prim-cyan-400` | `#22d3ee` | `--prim-cyan-900` | `#164e63` |
| `--prim-cyan-500` | `#06b6d4` | `--prim-cyan-950` | `#083344` |

Status hues — each a full ramp so both themes can pick a contrast-correct step:

| Green (healthy) | | Amber (degraded/pending) | | Red (failed) | | Violet (continued/special) | |
|---|---|---|---|---|---|---|---|
| `--prim-green-100` | `#d1fae5` | `--prim-amber-100` | `#fef3c7` | `--prim-red-100` | `#fee2e2` | `--prim-violet-100` | `#ede9fe` |
| `--prim-green-300` | `#6ee7b7` | `--prim-amber-300` | `#fcd34d` | `--prim-red-300` | `#fca5a5` | `--prim-violet-300` | `#c4b5fd` |
| `--prim-green-400` | `#34d399` | `--prim-amber-400` | `#fbbf24` | `--prim-red-400` | `#f87171` | `--prim-violet-400` | `#a78bfa` |
| `--prim-green-500` | `#10b981` | `--prim-amber-500` | `#f59e0b` | `--prim-red-500` | `#ef4444` | `--prim-violet-500` | `#8b5cf6` |
| `--prim-green-600` | `#059669` | `--prim-amber-600` | `#d97706` | `--prim-red-600` | `#dc2626` | `--prim-violet-600` | `#7c3aed` |
| `--prim-green-700` | `#047857` | `--prim-amber-700` | `#b45309` | `--prim-red-700` | `#b91c1c` | `--prim-violet-700` | `#6d28d9` |
| `--prim-green-900` | `#064e3b` | `--prim-amber-900` | `#78350f` | `--prim-red-900` | `#7f1d1d` | `--prim-violet-900` | `#4c1d95` |
| `--prim-green-950` | `#022c22` | `--prim-amber-950` | `#451a03` | `--prim-red-950` | `#450a0a` | `--prim-violet-950` | `#2e1065` |

White/black anchors for foreground-on-status legibility:

| Token | Value |
|---|---|
| `--prim-white` | `#ffffff` |
| `--prim-black` | `#0a0a0a` |

### 1.2 Spacing scale (4px base)

`--prim-space-0:0` · `-1:0.25rem` · `-2:0.5rem` · `-3:0.75rem` · `-4:1rem` · `-5:1.25rem` · `-6:1.5rem` · `-8:2rem` · `-10:2.5rem` · `-12:3rem` · `-16:4rem`.

### 1.3 Radius scale

`--prim-radius-sm:0.375rem (6px)` · `-md:0.5rem (8px)` · `-lg:0.625rem (10px)` · `-xl:1rem (16px)` · `-2xl:1.5rem (24px)` · `-full:9999px`.

### 1.4 Typography scale

Font families: `--prim-font-sans` (system UI stack) and `--prim-font-mono` (`ui-monospace, "SF Mono", "JetBrains Mono", monospace` — for ids, seqs, hashes, payloads).

Sizes (size / line-height): `--prim-text-xs:0.75rem/1rem` · `-sm:0.875rem/1.25rem` · `-base:1rem/1.5rem` · `-lg:1.125rem/1.75rem` · `-xl:1.25rem/1.75rem` · `-2xl:1.5rem/2rem`.

Weights: `--prim-font-normal:400` · `-medium:500` · `-semibold:600`. Tracking: `--prim-tracking-tight:-0.01em` · `-normal:0` · `-wide:0.02em`.

### 1.5 Z-index scale

`--prim-z-base:0` · `-sticky:10` · `-overlay:20` · `-dropdown:30` · `-modal:40` · `-popover:50` · `-tooltip:60` · `-toast:70`. A finite, named ladder — no ad-hoc `z-[999]`.

### 1.6 Motion / duration scale

Durations: `--prim-duration-instant:0ms` · `-fast:100ms` · `-normal:160ms` · `-slow:240ms`.
Easings: `--prim-ease-standard:cubic-bezier(0.2,0,0,1)` · `-entrance:cubic-bezier(0,0,0.2,1)` · `-exit:cubic-bezier(0.4,0,1,1)`.

### 1.7 Deliberately absent primitives

There is **no** `--prim-shadow-*`, **no** `--prim-glow-*`, and **no** opacity/alpha scale for conveying state. Elevation is expressed by **surface-step contrast + a 1px solid border**, not by a drop shadow. Emphasis is expressed by **solid color + weight + position**, not by glow. This absence is a hand-plane invariant (VISION.md §1). The legacy `--accent-cyan-glow` token is **removed** by this spec; do not reintroduce it.

> The **only** sanctioned alpha use is **non-semantic dividers / hairlines** (`--prim-border-hairline-*` below) — a 1px rule that must sit on an unknown surface. It never encodes status, is never applied to text or to a status fill, and is **explicitly forbidden on focus rings and on status borders** (those are always solid). There is no "de-emphasize a glyph with alpha" carve-out: a faded glyph conveys state through opacity, which is exactly what the hand-plane rule (§0 rule 4) bans. De-emphasis is expressed by a solid muted token (`--sem-text-muted`, `--sem-status-*-fg` at the dimmer step), never by alpha. Even the hairline is optional — a solid neutral border is always acceptable instead.
>
> `--prim-border-hairline-dark: rgba(255,255,255,0.08)` · `--prim-border-hairline-light: rgba(0,0,0,0.08)`.

---

## 2. Tier 2 — Semantic / alias tokens (intent-based; defined per theme)

Semantic tokens carry **meaning**. A component asks for "the default surface" or "the failed-status color," not for `neutral-900` or `red-500`. Every semantic token is defined **twice** — once in the dark map (shipped now), once in the light map (defined now, delivered Phase 1.5 per D4). The maps below _are_ the proof that light is purely additive: same key set, different right-hand primitives.

Naming: `--sem-<group>-<role>`. Groups: `surface`, `text`, `border`, `interactive`, `focus`, `status`, `intent`, `motion`.

### 2.1 Surface, text, border, interactive, focus

| Semantic token | Dark → primitive | Light → primitive | Role |
|---|---|---|---|
| `--sem-surface-base`      | `prim-neutral-1000` | `prim-neutral-50`  | App background (lowest plane) |
| `--sem-surface-default`   | `prim-neutral-950`  | `prim-white`       | Card / panel background |
| `--sem-surface-raised`    | `prim-neutral-900`  | `prim-neutral-50`  | Nested panel / table header |
| `--sem-surface-overlay`   | `prim-neutral-850`  | `prim-white`       | Popover / dropdown / modal |
| `--sem-surface-hover`     | `prim-neutral-800`  | `prim-neutral-100` | Row/control hover |
| `--sem-surface-active`    | `prim-neutral-700`  | `prim-neutral-200` | Pressed / selected |
| `--sem-text-primary`      | `prim-neutral-50`   | `prim-neutral-900` | Body & headings |
| `--sem-text-secondary`    | `prim-neutral-300`  | `prim-neutral-600` | Supporting text |
| `--sem-text-muted`        | `prim-neutral-400`  | `prim-neutral-500` | Hints, timestamps, ids |
| `--sem-text-on-accent`    | `prim-black`        | `prim-white`       | Text on a filled accent/status |
| `--sem-border-default`    | `prim-neutral-700`  | `prim-neutral-200` | Standard 1px border / divider |
| `--sem-border-strong`     | `prim-neutral-600`  | `prim-neutral-300` | Emphasized border |
| `--sem-border-hairline`   | `prim-border-hairline-dark` | `prim-border-hairline-light` | Divider on unknown surface |
| `--sem-interactive-default` | `prim-cyan-400`   | `prim-cyan-700`    | Primary action fill / link (light = cyan-700 so white `text-on-accent` clears AA — see §5.2) |
| `--sem-interactive-hover`   | `prim-cyan-300`   | `prim-cyan-800`    | Primary action hover |
| `--sem-interactive-active`  | `prim-cyan-500`   | `prim-cyan-900`    | Primary action pressed |
| `--sem-interactive-disabled`| `prim-neutral-700`| `prim-neutral-300` | Disabled control |
| `--sem-focus-ring`        | `prim-cyan-400`     | `prim-cyan-700`    | Focus outline color (see §6) |

### 2.2 The STATUS system (the centerpiece)

The console's job is triage. There are **five canonical status intents** — every workflow status, node liveness state, outbox-row state, and worker-connection state maps onto exactly one of these. This is the unified table that replaces the per-component palettes (`StatusBadge`, `EventIcon`, `NodeCard`, `IncidentCard`) called out in the audit.

| Canonical status | Meaning | Maps from (examples) | Glyph (shape channel) | ARIA label root |
|---|---|---|---|---|
| `healthy`  | Running well / live / completed-ok | `Running`, `Completed`, node `live`, worker `connected`, outbox `acked` | filled circle ● / check | "healthy" |
| `degraded` | Working but impaired / timed-out / retrying / adopting | `TimedOut`, retrying, shard `adopting`, worker `reconnecting` | triangle ▲ | "degraded" |
| `failed`   | Errored / dead / dropped | `Failed`, node `dark`, worker `dropped`, outbox `dead` | filled square ■ / cross | "failed" |
| `pending`  | Not started / waiting / queued | queued, `WorkflowStarted`-not-yet, outbox `pending` | hollow ring ○ | "pending" |
| `unknown`  | Stale / unreachable / not-yet-observed | node `unknown`, post-failover-unconfirmed, lost keepalive | dashed ring ◌ | "unknown" |

> `Cancelled` and `ContinuedAsNew` are **lifecycle facts, not health states.** They render with neutral status styling (`status-pending` family for cancelled = inert; an `intent-info` accent for continued-as-new) plus their own glyph + label. They do not borrow `failed` red — a cancelled workflow is not an incident.

Each status carries a **triplet** of semantic tokens so it can paint a solid foreground mark, a solid fill, and a readable border without ever touching opacity:

| Status token | Dark → primitive | Light → primitive |
|---|---|---|
| `--sem-status-healthy-fg`   | `prim-green-400` | `prim-green-700` |
| `--sem-status-healthy-bg`   | `prim-green-950` | `prim-green-100` |
| `--sem-status-healthy-border` | `prim-green-500` | `prim-green-600` |
| `--sem-status-degraded-fg`  | `prim-amber-400` | `prim-amber-700` |
| `--sem-status-degraded-bg`  | `prim-amber-950` | `prim-amber-100` |
| `--sem-status-degraded-border` | `prim-amber-500` | `prim-amber-600` |
| `--sem-status-failed-fg`    | `prim-red-400`   | `prim-red-700` |
| `--sem-status-failed-bg`    | `prim-red-950`   | `prim-red-100` |
| `--sem-status-failed-border`| `prim-red-500`   | `prim-red-600` |
| `--sem-status-pending-fg`   | `prim-neutral-300` | `prim-neutral-600` |
| `--sem-status-pending-bg`   | `prim-neutral-900` | `prim-neutral-100` |
| `--sem-status-pending-border` | `prim-neutral-600` | `prim-neutral-400` |
| `--sem-status-unknown-fg`   | `prim-neutral-400` | `prim-neutral-500` |
| `--sem-status-unknown-bg`   | `prim-neutral-950` | `prim-neutral-50` |
| `--sem-status-unknown-border` | `prim-neutral-700` | `prim-neutral-300` |

**How the triplet encodes the hand-plane rule.** A status chip is a `bg` fill + a 1px solid `border` + an `fg` mark/text. It is _opaque at every layer_. The audit's `bg-red-500/15 text-red-300` pattern is replaced by `bg-[var(--sem-status-failed-bg)] border-[var(--sem-status-failed-border)] text-[var(--sem-status-failed-fg)]` — same visual weight, zero opacity. The status is _still_ legible in a grayscale screenshot because the **glyph (shape)** and the **ARIA label** carry it independently of hue. That is the four-channel guarantee: **solid color + shape + position + label.**

### 2.3 Intent backgrounds (banners, callouts, blast-radius previews)

For context surfaces (error banners, the D2 blast-radius preview, provenance/staleness notices per D6/ADR-016) that need a solid tinted fill behind text:

| Intent token | Dark → primitive | Light → primitive |
|---|---|---|
| `--sem-intent-info-bg` / `-fg` / `-border`    | `prim-cyan-950` / `prim-cyan-200` / `prim-cyan-700` | `prim-cyan-100` / `prim-cyan-900` / `prim-cyan-600` |
| `--sem-intent-success-bg` / `-fg` / `-border` | `prim-green-950` / `prim-green-300` / `prim-green-700` | `prim-green-100` / `prim-green-900` / `prim-green-600` |
| `--sem-intent-warning-bg` / `-fg` / `-border` | `prim-amber-950` / `prim-amber-300` / `prim-amber-700` | `prim-amber-100` / `prim-amber-900` / `prim-amber-600` |
| `--sem-intent-error-bg` / `-fg` / `-border`   | `prim-red-950` / `prim-red-300` / `prim-red-700` | `prim-red-100` / `prim-red-900` / `prim-red-600` |

### 2.4 Motion (semantic)

`--sem-motion-fast: var(--prim-duration-fast)` · `--sem-motion-normal: var(--prim-duration-normal)` · `--sem-motion-slow: var(--prim-duration-slow)` · `--sem-motion-ease: var(--prim-ease-standard)`. Reduced-motion override in §6.

---

## 3. Tier 3 — Component tokens (optional, rare)

Use a component token **only** when several decisions for one component must move together and re-mapping the underlying semantic token would over-reach. They reference **semantic** tokens, never primitives. At this scale almost nothing needs them; the shadcn button is the one justified case.

```css
--comp-button-primary-bg:        var(--sem-interactive-default);
--comp-button-primary-bg-hover:  var(--sem-interactive-hover);
--comp-button-primary-fg:        var(--sem-text-on-accent);
--comp-button-danger-bg:         var(--sem-status-failed-bg);
--comp-button-danger-border:     var(--sem-status-failed-border);
--comp-button-danger-fg:         var(--sem-status-failed-fg);
```

Default posture: **prefer semantic tokens directly in components.** Reach for a component token only after a second component would otherwise duplicate the same multi-token recipe.

---

## 4. Single source of truth + wiring

### 4.1 The chain

```
design-tokens.json            ← THE source. Hand-authored, version-controlled. Primitives + dark map + light map.
        │  (build: scripts/generate-tokens.ts, run in prebuild)
        ├─► src/styles/tokens.generated.css   :root/[data-theme] CSS custom properties (do not hand-edit)
        └─► src/lib/tokens.ts                  TS const + SemanticToken union type for typed access
src/index.css                 imports tokens.generated.css, then @theme inline binds --sem-* → Tailwind utilities
tailwind v4 @theme            generates bg-/text-/border- utilities FROM the bound vars (no JS color config)
shadcn-Radix components       consume the generated utilities / var(--sem-*) — never a raw value
```

Everything downstream of `design-tokens.json` is **generated**. There is exactly one place a hex digit may be typed: the `primitives` block of `design-tokens.json`. Anywhere else, a hex digit is a bug (§8).

> Implementation note for WS3/WS4: today `index.css` hand-declares both maps inline. That is acceptable as the _interim_ source, but the **normative end state** is `design-tokens.json` → generator. Until the generator lands, `index.css` is treated as the source and the same tier rules apply to it. The generator is a mechanical lift, not a redesign — the key set is already fixed by this document.

### 4.2 File / dir layout

```
apps/aion-dashboard/
├── design-tokens.json              # SOURCE: { primitives, themes: { dark, light } }
├── scripts/
│   ├── generate-tokens.ts          # source → CSS + TS
│   └── validate-tokens.ts          # parity + contrast gate (§5.3, §8)
├── src/
│   ├── index.css                   # @import generated css; @theme inline binding
│   ├── styles/tokens.generated.css # GENERATED — gitignored or committed-with-check
│   └── lib/tokens.ts               # GENERATED — SemanticToken union + value map
└── components.json                 # shadcn config (cssVariables: true)
```

### 4.3 `@theme` binding (Tailwind v4)

`index.css` binds **semantic** tokens into Tailwind's color space so utilities exist for them. The "primitives are never utility-reachable" rule applies to **COLOR primitives only** — color is the only scale that is theme-dependent, so a component reaching a color primitive directly would bypass the theme map. The **non-color scales (spacing, radius, type, z-index, motion) are theme-invariant and bind their PRIMITIVES directly to Tailwind theme keys** (`--radius-md: var(--prim-radius-md)`, `--ease-standard: var(--prim-ease-standard)`, etc.) — there is no semantic indirection layer for them because there is nothing to re-point per theme. This matches the live `index.css`, which binds `--radius-*` straight to utilities. So: color primitives are NOT bound (must never be reachable as a utility — that is half of what keeps components honest); non-color primitives ARE bound directly:

```css
@import "tailwindcss";
@import "./styles/tokens.generated.css";

@theme inline {
  --color-surface-base:    var(--sem-surface-base);
  --color-surface-default: var(--sem-surface-default);
  --color-surface-raised:  var(--sem-surface-raised);
  --color-surface-overlay: var(--sem-surface-overlay);
  --color-text-primary:    var(--sem-text-primary);
  --color-text-secondary:  var(--sem-text-secondary);
  --color-text-muted:      var(--sem-text-muted);
  --color-border-default:  var(--sem-border-default);

  /* status families — yields bg-status-failed-bg, text-status-failed-fg, etc. */
  --color-status-healthy-fg:    var(--sem-status-healthy-fg);
  --color-status-healthy-bg:    var(--sem-status-healthy-bg);
  --color-status-failed-fg:     var(--sem-status-failed-fg);
  /* …all status & intent families… */

  --radius-md: var(--prim-radius-md);
  --ease-standard: var(--prim-ease-standard);
}
```

shadcn-Radix's existing `--background`/`--primary`/`--destructive`/`--ring` names are kept as a **compatibility shim** that aliases onto semantic tokens (`--primary: var(--sem-interactive-default)`, `--destructive: var(--sem-status-failed-fg)`, `--ring: var(--sem-focus-ring)`), so shadcn components inherit the system for free. New code uses `--sem-*`; the shim exists only so the Radix primitives don't fork the palette.

> **The shadcn `new-york` style is NOT hand-plane compliant out of the box, and the `--ring` alias does not fix it.** Aliasing `--ring` only changes the ring's *color*; it does not stop the primitives from emitting a ring or a `box-shadow` at all. Verified in the generated primitives: `components/ui/select.tsx` ships `focus-visible:ring-[3px]`, `shadow-xs`, and `shadow-md`; the dropdown, popover, dialog, and tooltip primitives carry the same shadow-based elevation and ring-based focus. These are direct §7 violations (box-shadow elevation + ring focus). **Required remediation:** every shadow- or ring-bearing primitive (`select`, `dropdown-menu`, `popover`, `dialog`, `tooltip`, and any other `new-york` primitive emitting `shadow-*` or `ring-*`) must be patched on vendor-in to (a) replace `focus-visible:ring-*` with the §6 `outline` focus treatment, and (b) replace `shadow-*` elevation with surface-step contrast + a 1px solid `--sem-border-default` — or the shadcn style must be switched to one without shadow/ring defaults. This patch is part of the §10 conformance gate; the §8 guard's shadow rule will fail the build until it is done.

### 4.4 Theme-switch mechanism (data-attribute + CSS vars)

The theme is selected by a **`data-theme` attribute on `<html>`**. CSS variables cascade; switching the attribute re-points every `--sem-*` with zero component involvement.

```css
:root, [data-theme="dark"]  { color-scheme: dark;  /* dark map of --sem-* */ }
[data-theme="light"]        { color-scheme: light; /* light map of --sem-* */ }
```

```ts
// src/lib/theme.ts (illustrative — built by WS4, not by this doc)
export function setTheme(t: "dark" | "light") {
  document.documentElement.setAttribute("data-theme", t);
  localStorage.setItem("aion.theme", t);
}
export function initTheme() {
  setTheme((localStorage.getItem("aion.theme") as "dark" | "light") ?? "dark");
}
```

A blocking inline script in `index.html` applies `data-theme` before first paint to prevent a flash. The `.dark`/`.light` _class_ variants are retained as a redundant selector for the same maps so the switch is robust mid-transition, but `data-theme` is the source of truth.

---

## 5. Both theme maps (proof light is additive)

The dark and light maps in §2 are **the deliverable for D4**. They share an identical key set; only the right-hand primitive selection differs. Therefore:

- **Dark = shipped now.** It is the only map a user sees in Phase 1.
- **Light = defined now, delivered Phase 1.5.** Every key already has a light value above. Delivery, **once the §0.1 component migration is complete**, is purely: (a) implement the `data-theme` toggle UI, (b) run the §5.3 contrast gate, (c) ship — with **no component file touched** to add light. That zero-edit step is the end state the architecture buys, and it holds only after migration: in the current codebase, components still hard-code palette primitives (§0.1), so flipping `data-theme` today would mis-render them. The token maps prove light is *structurally* additive; §0.1 is the work that makes the proof apply to the shipped app.

### 5.1 Computed contrast pairings — dark (WCAG 2.2 AA; threshold per text size)

Ratios are computed (not eyeballed) by `validate-tokens.ts` and reproduced here. Threshold column states which AA bar applies: **4.5:1** for normal text, **3:1** for large text (≥18.66px bold / ≥24px) and non-text UI (borders, focus rings).

| Foreground | Background | Computed ratio | Threshold | Pass |
|---|---|---|---|---|
| `text-primary` (`neutral-50`) | `surface-base` (`neutral-1000`) | 17.85:1 | 4.5 | ✅ |
| `text-secondary` (`neutral-300`) | `surface-default` (`neutral-950`) | 7.28:1 | 4.5 | ✅ |
| `text-muted` (`neutral-400`) | `surface-default` (`neutral-950`) | 5.45:1 | 4.5 | ✅ |
| `status-failed-fg` (`red-400`) | `surface-base` (`neutral-1000`) | 6.91:1 | 4.5 | ✅ |
| `status-healthy-fg` (`green-400`) | `surface-base` (`neutral-1000`) | 9.94:1 | 4.5 | ✅ |
| `status-degraded-fg` (`amber-400`) | `surface-base` (`neutral-1000`) | 11.45:1 | 4.5 | ✅ |
| `text-on-accent` (`black`) | `interactive-default` (`cyan-400`) | 10.96:1 | 4.5 | ✅ |
| `status-failed-fg` on `status-failed-bg` (`red-400`/`red-950`) | — | 5.84:1 | 4.5 | ✅ |

### 5.2 Computed contrast pairings — light (WCAG 2.2 AA; threshold per text size)

| Foreground | Background | Computed ratio | Threshold | Pass |
|---|---|---|---|---|
| `text-primary` (`neutral-900`) | `surface-base` (`neutral-50`) | 16.15:1 | 4.5 | ✅ |
| `text-secondary` (`neutral-600`) | `surface-default` (`white`) | 7.73:1 | 4.5 | ✅ |
| `text-muted` (`neutral-500`) | `surface-default` (`white`) | 4.83:1 | 4.5 | ✅ |
| `status-failed-fg` (`red-700`) | `surface-base` (`neutral-50`) | 6.04:1 | 4.5 | ✅ |
| `status-healthy-fg` (`green-700`) | `surface-base` (`neutral-50`) | 5.12:1 | 4.5 | ✅ |
| `status-degraded-fg` (`amber-700`) | `surface-base` (`neutral-50`) | 4.69:1 | 4.5 | ✅ |
| `text-on-accent` (`white`) | `interactive-default` (`cyan-700`) | 5.36:1 | 4.5 | ✅ |
| `status-failed-fg` on `status-failed-bg` (`red-700`/`red-100`) | — | 5.30:1 | 4.5 | ✅ |

> **Two defects fixed here (adversarial review).** (1) `text-on-accent` was white on `cyan-600`, which is **3.68:1 — FAILS** AA normal text (the old "~4.7:1 ✅" was wrong). The light accent fill is darkened to `cyan-700` (white-on-fill = 5.36:1); see the §2.1 `interactive-*` shift. (2) The light status badge `bg` was `*-300`, giving fg-on-bg ratios of only ~3.4–3.6:1 (**FAILS** normal text — the old "~4.8:1 ✅" was wrong). Light badge `bg` is lightened to `*-100` (red 5.30, green 4.84, amber 4.51 — all clear 4.5:1); see §2.2.
>
> Status `fg` darkens (4xx→7xx) and `bg` lightens (9xx→1xx) on the light side — the mechanical ramp-step inversion the 50→950 scheme makes trivial. Amber is the contrast canary: `amber-700` on `amber-100` is the tightest badge pairing at 4.51:1. Every ratio above is **asserted** by the §5.3 gate, not eyeballed.

### 5.3 The contrast gate (build-time)

`scripts/validate-tokens.ts` resolves every semantic token to its primitive hex per theme and computes WCAG contrast. The tables in §5.1/§5.2 are a readable summary; the **gate asserts the full cross-product**, not a curated sample. For each theme it computes and asserts:

1. **Every text/status foreground × every surface it can render on.** The foreground set is `{text-primary, text-secondary, text-muted, status-{healthy,degraded,failed,pending,unknown}-fg}`; the surface set is `{surface-base, surface-default, surface-raised, surface-overlay, surface-hover, surface-active, intent-{info,success,warning,error}-bg}`. Every foreground that can legitimately paint on a given surface must clear its threshold for **that** pairing — the gate iterates the product rather than trusting one representative surface.
2. **Every status/intent `fg`-on-`bg` badge pair** (`status-*-fg` on `status-*-bg`; `intent-*-fg` on `intent-*-bg`).
3. **Focus ring and status/strong borders** against each adjacent surface — held to the **3:1** non-text bar.

**Threshold is size-aware, asserted per pairing:** normal text **≥4.5:1**; large text (≥18.66px bold / ≥24px regular) and non-text UI (borders, focus ring) **≥3:1**. A pairing declared as badge/body text is checked at 4.5:1; a pairing declared large or non-text is checked at 3:1. The gate **fails the build** on any pairing below its declared threshold, and refuses a pairing that is only declared "large" if the component actually renders it at body size (the declaration is reviewed, not self-asserted).

It also enforces **key parity**: the dark and light maps must contain the identical key set, so a token can never exist in one theme and silently vanish in the other.

> The dark-side full cross-product is comfortably above threshold (status `fg` on every dark surface lands 5.0:1–11.5:1). On the light side the binding constraints are the badge pairs (amber-700/amber-100 = 4.51:1) and `text-muted`/`unknown-fg` (neutral-500) on raised surfaces (4.51:1) — all clear 4.5:1 but with little headroom, which is why §5.1/§5.2 carry the exact computed numbers rather than approximations.

---

## 6. Accessibility tokens

- **Focus ring (visible, never a shadow).** Focus is a **solid 2px outline** via `outline`, not `box-shadow`. Tokens: `--sem-focus-ring` (color, §2.1), `--prim-focus-width: 2px`, `--prim-focus-offset: 2px`. Rule: `:focus-visible { outline: var(--prim-focus-width) solid var(--sem-focus-ring); outline-offset: var(--prim-focus-offset); }`. The ring color meets ≥3:1 against both adjacent surfaces in both themes.
- **Reduced motion.** `@media (prefers-reduced-motion: reduce)` zeroes the semantic motion tokens: `--sem-motion-fast/normal/slow: 0ms`. Components reference `--sem-motion-*`, so honoring the preference is automatic and global.
- **Status never relies on color alone (WCAG 1.4.1).** Every status surface carries a **glyph** (shape channel, §2.2) **and** an `aria-label`/visually-hidden text (label channel). Grayscale-legible by construction.
- **Color-scheme.** `color-scheme: dark|light` is set per theme so native form controls and scrollbars match.

---

## 7. Hand-plane compliance summary (what is banned, what replaces it)

| Banned (audit found these) | Why | Replacement |
|---|---|---|
| Opacity color-modifiers (`/30`, `/15`, `/70`, `bg-black/40`) | State by transparency is illegible + colorblind-hostile; violates VISION §1 | Solid `--sem-status-*-{fg,bg,border}` triplet |
| Glow tokens (`--accent-cyan-glow`) | Decorative; "does not earn its place" | **Removed.** Emphasis via solid color + weight + position |
| `shadow-*` / `box-shadow` for elevation | Chrome, not function | Surface-step contrast + 1px solid `--sem-border-default` |
| Tailwind palette primitives in components (`text-sky-300`) | Bypasses the semantic layer; un-themeable | Semantic utility (`text-status-healthy-fg`) |
| Raw hex / `rgb()` in `.tsx`/`.ts` | Source-of-truth fork | `var(--sem-*)` / bound utility |
| Box-shadow / ring-based focus indicator | A focus indicator is an `outline`, not a shadow; box-shadow focus is invisible in forced-colors/high-contrast mode and is clipped by `overflow:hidden` ancestors | Solid `outline` + `outline-offset` (§6) |

The **only** sanctioned alpha is the §1.7 hairline / divider on an unknown surface. There is **no** glyph-de-emphasis alpha exemption (a faded glyph encodes state by opacity — banned by §0 rule 4); de-emphasis uses a solid muted token. Alpha is never applied to a focus ring or a status border. Note this is a true exemption-free statement: there is **no box-shadow exemption** anywhere in this system — focus is an `outline`, so the focus indicator was never a `box-shadow` to exempt.

---

## 8. Migration-safety rule + the enforcement guard

**Rule (binding):** Components reference **semantic tokens only**. Never a primitive, never raw hex/rgb/hsl, never a Tailwind palette primitive, never an opacity color-modifier. This is what makes a new theme a token-map addition (D4) and what makes the no-opacity hand-plane rule auditable rather than aspirational.

**What enforces what (be honest about the tools):** Biome lints JS/TS *syntax and structure*; it **cannot** regex-match the *contents* of Tailwind class strings (they are opaque string literals to the linter). So the color/opacity/shadow rules below are NOT a Biome config — they are a dedicated, purpose-built **`scripts/check-tokens.ts`** that scans `src/**/*.{ts,tsx,css}` as text (excluding `tokens.generated.css` and `design-tokens.json`). Biome stays in the pipeline for ordinary lint; `check-tokens.ts` owns color/opacity/shadow/parity. Both are required CI checks. The patterns below are the authored regexes in `check-tokens.ts`.

**The guard (CI + local, blocking), run by `scripts/check-tokens.ts`:**

1. **No raw color values.** Regex `#[0-9a-fA-F]{3,8}\b` and `\b(rgb|rgba|hsl|hsla|oklch|oklab|color)\(` → **error**. The _only_ exemptions are `design-tokens.json` (the source) and the generated CSS.

2. **Color utilities must use a SEMANTIC suffix (allowlist-positive, not palette-denylist).** A denylist of palette names is unsound — it silently misses any palette it forgot (teal, orange, lime, rose, fuchsia, indigo, stone, purple, pink, slate, …), so a bypass is one un-listed color away. Instead, flag **any** color-bearing utility whose suffix is not in the semantic set. Color-bearing prefixes: `bg`, `text`, `border` (incl. directional `border-t|r|b|l|x|y`), `ring`, `fill`, `stroke`, `from`, `via`, `to`, `divide`, `outline`, `decoration`, `accent`, `caret`. Semantic suffixes (the allowlist): `surface`, `text`, `status`, `intent`, `border`, `interactive`, `focus`, `on-accent`, plus CSS keywords `transparent`/`current`/`inherit`. Pattern:

   ```
   (?<![\w-])(?:bg|text|border(?:-[trblxy])?|ring|fill|stroke|from|via|to|divide|outline|decoration|accent|caret)-(?!(?:surface|text|status|intent|border|interactive|focus|on-accent|transparent|current|inherit)\b)(?!\[)[a-z]+-\d{2,3}\b
   → error
   ```

   The `(?!\[)` guard skips arbitrary values like `grid-cols-[…]`/`bg-[url(…)]` (those are caught — or not — by rule 1, not here); the negative-lookahead on the semantic set is what makes this allowlist-positive: **only** `-surface-*`/`-text-*`/`-status-*`/`-intent-*`/`-border-*`/`-interactive-*`/`-focus*`/`-on-accent` survive, every other color suffix errors regardless of which palette it names.

3. **No opacity color-modifiers (directional-aware, arbitrary-value-safe).** The old pattern missed directional utilities (`border-l-red-400/70`) and false-positived on arbitrary values. Corrected:

   ```
   (?<![\w-])(?:bg|text|border(?:-[trblxy])?|ring|fill|stroke|from|via|to|divide|outline|decoration|accent|caret)-[a-z]+-\d{2,3}/\d{1,3}\b
   |(?<![\w-])(?:bg|text|border(?:-[trblxy])?|ring|fill|stroke|from|via|to|divide|outline|decoration|accent|caret)-[a-z]+/\d{1,3}\b
   → error
   ```

   The first alternative catches stepped palettes incl. directional borders (`border-l-red-400/70`); the second catches keyword fills (`bg-black/40`). Neither matches `grid-cols-[1fr_2fr]`, `bg-[url(/x.png)]`, or fractional widths like `w-1/2` (those have no color-prefix + step shape). Also flag `\bopacity-\d` when applied to a status/text element (the hand-plane gate — opacity must never encode state). The lone sanctioned alpha (§1.7 hairline) lives in `tokens.generated.css`, which is excluded, so it never trips the gate.

4. **No shadow/glow.** Regex `\b(shadow|drop-shadow)-` and any literal `box-shadow:` in `.css`, and any reference to a `*-glow` token → **error**. There is **no focus-ring exemption** (focus is an `outline`, §6/§7), so a `box-shadow:` is always a violation. Note this rule is what fails the build on un-patched shadcn `new-york` primitives (`shadow-xs`/`shadow-md`/`ring-[3px]`) until they are remediated per §4.3.

5. **Token parity + contrast** (`scripts/validate-tokens.ts`, §5.3) → **error** on missing key or any pairing below its declared threshold.

Wire as `bun run check:tokens` (which runs both `check-tokens.ts` and `validate-tokens.ts`) in CI alongside the existing wire-types guard (cf. D10) and the ordinary Biome lint job. Local: a `lint-staged`/pre-commit hook runs the same scripts so violations never reach CI. A violation is a **failed build**, not a warning. The guard is what converts every rule in this document from prose into a contract.

---

## 9. Worked example — adding a theme and adding a status, with zero component edits

> These examples describe the **post-§0.1 end state** — a codebase where every component already names only intents and reads the §2.2 registry. They are what the architecture guarantees *once conformance debt is paid*, not a property of today's app (see §0.1). Until the listed components are migrated, adding a theme or status still requires fixing their hard-coded values first.

### 9.1 Add a new theme ("high-contrast")

1. In `design-tokens.json`, add `themes["high-contrast"]` with the **same key set** as `dark`/`light`, choosing primitives that maximize contrast (e.g. `text-primary → prim-white`, `surface-base → prim-black`, status `fg` at the most-saturated step).
2. Run `generate-tokens` → a new `[data-theme="high-contrast"]` block appears in `tokens.generated.css`; `validate-tokens` confirms parity + contrast.
3. Add `"high-contrast"` to the theme toggle's option list.

**Components changed: zero.** Every component already reads `--sem-*`; the new attribute re-points them.

### 9.2 Add a new status ("quarantined" — e.g. a fenced-off shard)

1. Decide its canonical family. If it is a distinct sixth health state, add `--sem-status-quarantined-{fg,bg,border}` to **both** theme maps in `design-tokens.json` (e.g. dark: `violet-400`/`violet-950`/`violet-500`; light: `violet-700`/`violet-300`/`violet-600`), assign a glyph (e.g. hollow diamond ◇) and an ARIA root ("quarantined").
2. Add the status→token+glyph+label row to the single status registry the components already iterate (the §2.2 table, mirrored in `src/lib/tokens.ts`).
3. Run generate + validate (contrast asserted for both themes).

**Components changed: zero** — the badge/lane/node/icon components render from the status registry; a new row flows everywhere (badge, swimlane bar, node card, event icon, triage rail) at once. That single-registry fan-out is the dividend of unifying the four bespoke palettes the audit found.

---

## 10. Conformance checklist (for WS4 / reviewers)

- [ ] **PRECONDITION (§0.1): the hard-coded-palette components are migrated to the §2.2 semantic registry** — `LaneBar.tsx`, `IncidentCard.tsx`, `ConnectionIndicator.tsx`, `ActivityGroup.tsx`, `FirehoseFeed.tsx`, `NamespaceSelector.tsx`, and the swimlane status map name only intents (no palette primitives, no alpha). Until this is checked, the light-theme zero-edit property and the §8 guard cannot both be green. **This is the first gate.**
- [ ] shadcn `new-york` shadow/ring-bearing primitives (select, dropdown, popover, dialog, tooltip) are patched to `outline` focus + surface/border elevation, or the style is switched (§4.3); no primitive emits `shadow-*` or `ring-*`.
- [ ] Three tiers exist; reference flows only downward (prim ← sem ← comp ← component-usage).
- [ ] `design-tokens.json` is the sole place a hex digit is typed; CSS + TS are generated.
- [ ] Both `dark` and `light` semantic maps present with **identical key sets**.
- [ ] Theme switch is `data-theme` on `<html>` + pre-paint inline script; no FOUC.
- [ ] `@theme inline` binds **semantic color** tokens only; **color** primitives are not utility-reachable. Non-color primitives (spacing/radius/type/z/motion) bind directly — that is expected, not a violation (§4.3).
- [ ] Every status renders solid-color + glyph + position + ARIA label; no opacity/glow/shadow anywhere; no alpha on focus rings or status borders.
- [ ] Focus is a solid 2px `outline` (never a `box-shadow`); reduced-motion zeroes `--sem-motion-*`.
- [ ] Contrast gate green for the **full §5.3 cross-product** (every text/status fg × every surface it renders on, plus every fg-on-bg badge pair) in both themes, size-aware (4.5:1 normal / 3:1 large + non-text).
- [ ] The §8 guard (`check-tokens.ts` for color/opacity/shadow + `validate-tokens.ts` for parity/contrast; Biome for ordinary lint) runs in CI and pre-commit and **blocks** on any violation.
- [ ] `--accent-cyan-glow` and all `shadow-*` in app UI are removed.
