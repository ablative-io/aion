# AWL-0 — aion workflow language: spec draft

Status: DRAFT for Tom's cold read. The syntax proposal itself is rendered as
[sketch H](../syntax-sketches/H-awl.md) against the shared `research_report`
fixture — read that first; this document is the contract behind it.

## Position

Tom ruled (2026-07-04): aion gets **its own tiny workflow language**. YAML-likes
and markdown are dead as authoring surfaces (markdown = generated docs only).
Execution lands in two stages: an interpreter tier (#216) first, a beamr
bytecode emitter as the north star. Gleam stays full-fat for workflows that
outgrow the language.

This supersedes the DSL clause of **ADR-014** ("the typed Gleam module is the
single source of truth; no separate DSL"). The reframing that survives from
REVIEW-AND-RECOMMENDATIONS Theme 4: the invariant was never "no DSL", it was
**no second source of truth**. AWL keeps that invariant the other way around:
the `.awl` document *is* the source of truth — the canonical workflow model is
its lossless parse, and anything else (Gleam, canvas, docs) is a generated
view. Nobody ever edits generated output.

## Inherited design constants

From the aion-authoring corpus (AUTHORING-MODEL-DISCUSSION, SURFACE-RETHINK),
unchanged:

1. **Determinism by construction.** The language has no vocabulary for clock,
   randomness, or I/O. The only world-touching verb is calling an action
   (`do`); time is engine-mediated (`sleep`, `timeout`, `wait`).
2. **Canonical model; lossless parse ↔ print.** Parsing a document yields the
   exact workflow graph; printing yields the document back, byte-identical
   after one format pass.
3. **Types on actions.** Every call is checked against a declared contract;
   error quality is a compiler's, not a validator's.
4. **Prose is load-bearing.** `about` flows to the canvas node label, the ops
   console narration, and generated docs. A running workflow narrates itself
   in its author's words.
5. **AI agents are the primary authors.** The grammar must be small enough to
   specify exhaustively in a system prompt and must not resemble an existing
   language (lookalike syntaxes give LLMs plausible-but-wrong priors).

## Lexical rules

- Encoding UTF-8; comments `//` to end of line; blank lines insignificant.
- Step bodies are indented two spaces; handler blocks (`on timeout`,
  `on failure`) nest one further level. Indentation is structure (the printer
  emits exactly one shape, so there is nothing to get wrong on re-emit).
- Identifiers `snake_case`; type names `TitleCase`; keywords reserved.
- Literals: `"strings"` (escapes `\" \\ \n \t`), integers, floats, `true`,
  `false`, durations (`Ns`, `Nm`, `Nh`, `Nd`), lists `[a, b]`, record
  construction `TypeName(field: expr, …)`.
- `about` is prose to end of line, unquoted (narration should not need
  escaping).

## Document grammar

A file is one workflow document, declarations in canonical order:

```
workflow <name>
about <prose>
input <name>: <Type>          // repeatable
output <Type>
error <Type>                  // optional; defaults to the engine taxonomy
signal <name>: <Type>         // repeatable
type <Name> { field: Type, … }    // repeatable
action <name>(param: Type, …) -> Type    // repeatable; routing fields may
  queue "<task_queue>"                   // follow indented, same as steps
  node "<node>"
  timeout <duration>
  retry <n> every <duration>
step <name>                   // repeatable, ordered
  <fields…>
finish <expr>
```

Types are nominal records and (in a later rev) enums; `List(T)`, `Option(T)`,
`Bool`, `Int`, `Float`, `String`, `Nil`, `Dir` (content-addressed snapshot
handle) are built in. Every type is JSON-schema derivable — the start form,
structured-output authoring, and the worker contract all fall out of the same
declaration (kills the "same shape described 3×" wound).

### Step fields

One per line, canonical order as listed; all optional except `do`/`wait` (a
step does exactly one of: call, child call, signal wait, sleep):

| field | meaning |
|---|---|
| `about <prose>` | narration; canvas label; console narration |
| `when <expr>` | guard — step is skipped when false; with `as`, the prior binding flows on (rebinding rule) |
| `each <id> in <expr>` | fan-out: the `do` runs once per item, results collected in order; `parallel` semantics with exactly-once per item |
| `do <call>` | call an action; `do child <workflow>(args)` spawns + awaits a child workflow |
| `wait <signal>` | durable gate on a declared signal |
| `sleep <duration>` | durable timer |
| `repeat up to <expr>` | bounded cycle: re-run the `do` with the rebound `as` value threaded through |
| `until <expr>` | early exit condition for `repeat` (checked after each round) |
| `retry <n> every <d>` / `retry <n> backoff <d>..<d>` | per-attempt retry policy |
| `timeout <duration>` | attempt timeout; with `wait`, the gate deadline |
| `on timeout` | handler block: `do` lines then `finish <expr>` or `fail` |
| `on failure` | compensation block: `do` lines then `finish <expr>` or `fail` |
| `as <name>` | bind the result; the same name later **rebinds** (kills the switch contortion — G's proven rule) |
| `queue "<q>"` / `node "<n>"` | routing overrides (usually on the action declaration instead) |

### Expressions (the keel)

Per VESPER-LYND-METHOD-NOTES: the keel is the expression grammar, not the
carrier. It is deliberately a micro-grammar: references, field access
(`approval.ok`), calls (only in `do`), record construction, list literals,
`not`/`and`/`or`, comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`), string
concatenation (`+`). No arithmetic beyond `+` on strings in rev 0 (open
decision 4), no closures, no user functions — real computation lives in
actions behind the contract boundary.

## Semantics

- **Evaluation order** is document order; steps run sequentially. Fan-out
  (`each`) is the parallel primitive (SDK `map`). Heterogeneous parallel
  groups and `race` are deferred (open decision 2).
- **One-call bodies.** `each` and `repeat` bodies are exactly one `do`. A
  multi-step unit is a child workflow — keeps documents flat, the canvas
  clean, every composite independently observable and versioned.
- **Bindings** are single-assignment except explicit rebinding via `as` on a
  guarded or repeated step. A `when`-guarded rebind that doesn't fire leaves
  the prior value flowing — this is the conditional-rebinding rule proven in
  sketch G.
- **Errors.** A failed `do` (after retries) fails the step; without an
  `on failure` block the workflow fails with the engine taxonomy mapped to
  the declared `error` type. `on failure` runs compensation `do` lines, then
  must end in `finish <expr>` or `fail` (re-raise). Exit-status-is-data
  workflows (dev_brief's gates) don't need any of this — record outcomes as
  values in action results, the same doctrine as today.
- **Determinism** is not linted, it is unexpressible: no clock, no entropy,
  no ambient I/O exist in the grammar. Engine-mediated forms (`sleep`,
  `wait`+`timeout`) are recorded events, replay-safe by the same mechanism as
  the Gleam SDK.

## Mapping to the SDK (coverage check)

| SDK primitive | AWL |
|---|---|
| `workflow.run(activity)` | `step` + `do` |
| `workflow.map` | `each … in … do …` |
| `workflow.all` / `race` | deferred (open decision 2) |
| `child.spawn`/`await` | `do child name(args)` |
| `signal.receive` | `wait <signal>` |
| `workflow.sleep` / timers | `sleep`; `timeout` on `wait` |
| `with_timeout` | `timeout` field |
| `activity.retry/timeout/task_queue/node` | `retry`/`timeout`/`queue`/`node` fields |
| `continue_as_new` | deferred — `repeat` covers the dev_brief-class loop; long-lived loops stay in Gleam for now |
| `workflow.now/random` | deliberately absent from rev 0 (open decision 3) |
| codecs / `run` ceremony / schemas | generated from `type`/`input`/`output` decls |

## What AWL-0 builds

1. **Parser** → the canonical workflow model (the graph
   `crates/aion-package/src/structure/` already defines: nodes/edges with
   correlation keys). Rust crate `aion-awl`.
2. **Typechecker** — action contracts, binding/rebinding flow, guard/rebind
   liveness, exhaustive step-field validation. Compiler-quality errors with
   spans.
3. **Printer** — the formatter; property test: `parse ∘ print = id` and
   `print ∘ parse ∘ print = print`.
4. **Fixture goldens** — `research_report.awl` (sketch H) and a real one:
   `dev_brief.awl` re-expressing examples/dev-brief's pipeline. Round-trip +
   typecheck goldens in CI.

Execution is explicitly **not** AWL-0: the interpreter tier is #216; the beamr
bytecode emitter is the north star after that. Until then `aion check` on
`.awl` files is real and useful on its own (authoring feedback loop).

## Open decisions (recommendations inline)

1. **File extension.** `.awl` (recommended — `.aion` is taken by package
   archives) vs `.flow`.
2. **Heterogeneous parallel + race.** Deferred out of rev 0 (recommended):
   `each` covers the dominant fan-out shape; a `parallel`/`race` step group
   syntax can land additively when a real workflow needs it.
3. **`now()`/`random()`.** Absent from rev 0 (recommended): workflows that
   need deterministic entropy/clock are past the language's complexity
   budget — Gleam is right there. Revisit on demand.
4. **Arithmetic.** Rev 0 has comparisons and string `+` only (recommended);
   integer arithmetic invites computation that belongs in actions.
5. **Enums/match.** Rev 0 models alternatives as `Bool`/`String` fields +
   `when` guards (recommended for now); a `type X = A | B` + `match` rev
   follows if fixtures demand it.

## Ratification path

Tom cold-reads sketch H against A–G (the fixture-based measurement from
VESPER-LYND-METHOD-NOTES §3; optionally the second leg: hand an assistant the
same spec and measure authoring accuracy). If H smells right, AWL-0 build
starts: parser → typechecker → printer → goldens, with the mechanical parts
dispatched through the dev_brief pipeline itself.
