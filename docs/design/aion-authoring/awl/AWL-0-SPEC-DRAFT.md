# AWL-0 — aion workflow language: spec draft

Status: DRAFT for Tom's cold read. The syntax proposal itself is rendered as
[sketch H](../syntax-sketches/H-awl.md) against the shared `research_report`
fixture — read that first; this document is the contract behind it.

## Position

Tom ruled (2026-07-04): aion gets **its own tiny workflow language**. YAML-likes
and markdown are dead as authoring surfaces (markdown = generated docs only).
Execution (updated per Tom's ruling 2026-07-09): the Gleam emitter is a live
stopgap; the target is **direct beamr bytecode emission** (AWL-BC, see
[AWL-BC-DESIGN-DRAFT](AWL-BC-DESIGN-DRAFT.md)), which supersedes the
interpreter tier (#216). Gleam stays full-fat as a first-class *authoring*
language for workflows that outgrow AWL or need full control.

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
- `///` doc lines (AWL-1) are NOT comments: on a `type` declaration or a
  field they are load-bearing descriptions that flow into the derived JSON
  Schema (`description`). `//` remains ignorable trivia.
- Step bodies are indented two spaces; handler blocks (`on timeout`,
  `on failure`) nest one further level. Indentation is structure (the printer
  emits exactly one shape, so there is nothing to get wrong on re-emit).
- Identifiers `snake_case`; type names `TitleCase`; keywords reserved.
- Literals: `"strings"` (escapes `\" \\ \n \t`), integers, floats, `true`,
  `false`, durations (`Ns`, `Nm`, `Nh`, `Nd`), lists `[a, b]`, record
  construction `TypeName(field: expr, …)`.
- `about` is prose to end of line, unquoted (narration should not need
  escaping).

## Reserved keywords — the complete inventory

Every word the language claims, in one place. All are reserved everywhere
(an identifier may not shadow any of them). Words marked **rev-1** are
sanctioned additions specified in the AWL-1 section below; everything else is
implemented in `aion-awl` today.

| group | keywords |
|---|---|
| document declarations | `workflow` `about` `input` `output` `error` `signal` `type` `action` `step` `finish` |
| step fields | `when` `each` `in` `do` `child` `wait` `sleep` `repeat` `up` `to` `until` `retry` `every` `backoff` `timeout` `on` `as` `queue` `node` |
| handler blocks | `on timeout` `on failure` `finish` `fail` |
| expression operators | `not` `and` `or` |
| literals | `true` `false` |
| rev-1 additions | `otherwise` `match` `case` `parallel` `race` `spawn` `order`; `child` gains a declaration position (`child name(params) -> Type`); `///` doc lines; named arguments required in calls |

Builtin type names (reserved as type identifiers): `Bool`, `Int`, `Float`,
`String`, `Nil`, `List(T)`, `Option(T)`. Note: `Dir` (content-addressed
snapshot handle) is spec'd below but **not yet implemented** in the checker —
tracked as an implementation gap, not silently dropped.

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

Execution is explicitly **not** AWL-0. Updated 2026-07-09: the Gleam emitter
(`aion awl emit`) is the live stopgap, proven e2e; **AWL-BC direct bytecode
emission** ([AWL-BC-DESIGN-DRAFT](AWL-BC-DESIGN-DRAFT.md)) is the target and
supersedes the interpreter tier (#216). `aion awl check` on `.awl` files
stands alone as the authoring feedback loop either way.

## AWL-1 — sanctioned rev: control flow, concurrency, data plumbing

Sanctioned by Tom (2026-07-09, reconfirmed with scope): control flow belongs
in the workflow file. Tracker: task #241. Everything here is spec text for the
rev, marked with its own open questions; nothing below is implemented yet.
The determinism constant is untouched: conditions, bounds, and match subjects
derive only from workflow inputs and step results; `do` remains the only
world-touching verb.

### Design line: plumbing yes, computation no

Ruled with Tom: AWL must be sufficient to author a complete workflow alone —
schemas, routing data between steps, selecting fields and elements — without
reaching for Gleam. What stays out is *computation*: folds, string
manipulation, arithmetic-heavy transforms live in actions behind the contract
boundary. Concretely: `type` declarations (JSON-schema derivable), field
access, record construction, list literals, `each` over any list expression,
and (rev-1) enums, `match`, and indexing are all plumbing — in. A `sum` /
`filter` / `regex` vocabulary is computation — out.

The canonical fan-out shape needs no new syntax; it works in rev 0 today:

```
step size
  about break the job into batches the workers can chew
  do size_work(job: input) as batches       // returns List(Batch)

step process
  about fan every batch out in parallel
  each batch in batches
  do process_batch(batch: batch) as results  // List collected in input order
```

### Branching: `otherwise` and `match`

`otherwise` is a step field marking the complement of the nearest preceding
`when`-guarded step that binds the same `as` name (the conditional-rebinding
rule already proven in sketch G, completed into a true either/or):

```
step fast_path
  when triage.simple
  do quick_answer(q: input.q) as answer

step slow_path
  otherwise
  do deep_research(q: input.q) as answer
```

The checker enforces: `otherwise` requires such a predecessor; both arms bind
the same name with the same type; no step may carry both `when` and
`otherwise`.

`match` is a step construct for enums, `Option`, and declared alternatives.
Arms are `case` lines; each arm body follows the one-call rule (`do` /
`do child` / `spawn`); arms that bind must all bind the same `as` name and
type. The checker enforces exhaustiveness — no default arm exists, on
purpose: adding a variant must break every workflow that fails to route it.

```
step route
  match triage.category
  case Urgent
    do page_oncall(ticket: input.ticket) as handled
  case Routine
    do enqueue(ticket: input.ticket) as handled
  case Spam
    do discard(ticket: input.ticket) as handled
```

### Type declarations: JSON-shaped, described, schema-emitting (DECIDED 2026-07-09)

Types are the output contracts we hand to models (`--output-schema`), the
start-form schemas, and the worker contracts — so the declaration syntax
looks like the JSON object it describes, and every declaration converts
losslessly to JSON Schema. Three rules:

**1. JSON-shaped layout.** Braces, `field: Type` pairs, comma-separated —
exactly the shape everyone already reads all day. Canonical rendering is
deterministic (round-trip holds): single-line iff the whole declaration fits
within 100 columns, otherwise one field per line with a trailing comma on
every line (prettier/rustfmt convention). The 100-column measure is the total
character count of the rendered single-line form — the entire physical line
from column 1, including the `type Name {` prefix and the closing `}` (type
declarations are top-level, so no indentation enters the count); a line of
exactly 100 characters fits. The parser accepts both forms and
tolerates missing/trailing commas either way — AI authors emit both, and a
comma should never be the reason a check fails.

```
type Brief {
  id: String,
  title: String,
  pointers: List(String),
  acceptance: List(String),
}
```

**2. Descriptions are `///` doc lines** — on the type and on any field. They
are data, not trivia (`//` remains ignorable comment trivia): they flow into
the JSON Schema `description` at each level, which is what the model actually
reads when the type is used as an output contract. Same convention as
Rust/C#, instantly recognizable to every AI author:

```
/// A development brief: the unit of work a dev_brief run executes.
type Brief {
  /// Stable identifier, e.g. "AWL1-001".
  id: String,
  /// One-line human title shown in the console.
  title: String,
  pointers: List(String),
}
```

**3. Every type is a JSON Schema, on demand — and automatically.** `aion awl
schema <file> [--type Name]` emits JSON Schema (draft 2020-12) for any
declared type, or for the workflow's `input`/`output` contracts: records →
`object` with `properties`/`required` (non-`Option` fields are required;
`Option(T)` fields are optional), enums → string enums, `///` text →
`description` at every level. One declaration, every consumer — the model's
output contract, the start form, and the worker contract are the same bytes.

**Seamlessness is a requirement, not a convenience (Tom, 2026-07-09):**
schema derivation is a pure public function in `aion-awl`
(`schema_for_type(checked_doc, name) -> serde_json::Value`), and every
consumer reads that one derivation automatically — `aion package` embeds the
schemas for all contract-reachable types in the package manifest at packaging
time, the server exposes them (start forms, #209), and an action whose result
type is a model output contract has its schema carried to the worker on
dispatch so norn's `--output-schema` receives it with NO manual step (rides
the #186 deploy-time contract verification seam). The CLI subcommand is an
on-demand inspection of the same derivation, never a required pipeline step.
Plumbing lands as its own brief (AWL1-015) so AWL1-001 stays narrow.

Deliberately NOT authorable inline: the constraint vocabulary (`minLength`,
`pattern`, numeric bounds…). Types + descriptions + enums are the contract
surface; value validation is an action's job (plumbing yes, computation no).

### Referencing existing JSON Schema files (DECIDED 2026-07-09)

Shops already have JSON Schemas; AWL consumes them without transcription:

```
type Brief from "schemas/brief.schema.json"
```

declares a nominal type whose shape is loaded from a JSON Schema file at
check time (path relative to the `.awl` file). Rules:

- **Typing uses the record-shaped projection.** The checker maps
  `object`/`properties`/`required`/`description`, nested objects, `array` →
  `List(T)`, `string`/`integer`/`number`/`boolean`, `$defs`-local `$ref`,
  and string `enum` onto the same nominal record/enum model as inline
  declarations; a property absent from `required` types as `Option(T)`.
  Structural keywords the type model cannot honor (`oneOf`, `anyOf`,
  `patternProperties`, `additionalProperties: <schema>`, conditional
  schemas…) are check ERRORS with a diagnostic naming the unsupported
  keyword and its JSON path — never silently ignored.
- **Constraint keywords pass through.** `minLength`, `pattern`, numeric
  bounds, `format` are ignored for typing but PRESERVED: `aion awl schema`
  for an imported type re-emits the source schema canonically (sorted keys,
  stable whitespace), constraints intact. Existing contracts keep their
  validation vocabulary; AWL just doesn't let you author it inline.
- **The file is source.** It travels with the document into the package
  (content-addressed, so the deployed contract is pinned); a missing or
  unparseable file is a check error with the declaration's span. `aion awl
  fmt` never rewrites the imported file; the declaration's canonical form is
  the single line above.

### Enums

`type` grows payload-less variants, JSON-schema derivable as string enums:

```
type Category = Urgent | Routine | Spam
```

Variants with payloads (tagged unions) are deliberately deferred — they pull
destructuring-pattern grammar in with them; revisit when a fixture demands it.
`match` over `Option(T)` uses `case Some as <name>` / `case None` (the single
built-in payload-carrying match, because optionals are unavoidable plumbing).

### Loops

- `repeat up to <expr>` + `until <expr>` (rev 0) remains the bounded cycle —
  the while-loop with a mandatory ceiling. The `until`-vs-fresh-`as` binding
  question rides #238 for Tom's ruling.
- **Unbounded `repeat until <expr>`** (no ceiling) becomes legal ONLY when
  the engine's implicit continue-as-new lands: the runtime rolls history at a
  threshold, invisibly to the author (AWL-1 open question 3 — needs Tom's
  explicit ruling on the mechanism; no silent default ships).
- **Sequential iteration**: `each <id> in <expr> in order` — same collection
  semantics as `each`, but items run one at a time in list order and the
  first failed item fails the step (remaining items never start). Bare
  `each` stays parallel. The author always chooses; neither is a silent
  default (spelling = AWL-1 open question 1).

### Heterogeneous concurrency: `parallel` and `race`

`parallel` runs 2+ differently-shaped calls concurrently and joins all:

```
step gather
  parallel
    do fetch_profile(id: input.id) as profile
    do fetch_history(id: input.id) as history
```

All arms must bind (distinct names); the step completes when every arm
completes; any arm's failure fails the step (after that arm's own retries),
and the step's `on failure` sees it.

`race` runs 2+ arms and completes with the first to finish; losing arms are
cancelled through the engine's normal cancellation path:

```
step first_quote
  race
    do quote_vendor_a(job: spec) as quote
    do quote_vendor_b(job: spec) as quote
```

All arms bind the SAME name and type (whichever wins, downstream code sees
one binding). Note: *signal-or-deadline* does not need `race` — rev 0 already
covers it with `wait <signal>` + `timeout <duration>` + `on timeout`.

### Named arguments in calls (DECIDED 2026-07-09)

Action and child calls use **required named arguments** —
`do provision_workspace(repo_root: config.repo_root, brief: brief)` — and the
checker enforces that names match the declaration exactly (no positional
form, no partial naming). Record construction already worked this way; the
positional call syntax was the inconsistency. Rationale: AI agents are the
primary authors, and named arguments make the positional-swap bug — the
classic silent plausible-but-wrong failure — unwritable. Migration: the
parser rejects positional calls with a fix-it diagnostic naming the declared
parameters in order.

### Typed child-workflow contracts (DECIDED 2026-07-09)

Child workflows are declared exactly like actions, with `child`:

```
child fix_round(brief: Brief, prior: Round) -> Round
```

`do child fix_round(…)` and `spawn fix_round(…)` must reference a
declaration; results are first-class typed values (field-accessible,
passable, returnable). This closes the rev-0 gap where child results were
opaque — the spec's own bounded-cycle pattern didn't typecheck, and the two
committed `bounded_cycle` fixtures fail `aion awl check` today (invisible in
CI because fixtures only run through parser/printer goldens — fixed as part
of the same brief: the checker runs over every committed fixture). Deploy
binds the declared name to a deployed workflow whose input/output schemas
must match — the same deploy-time contract verification actions get.

### Detached children: `spawn`

`do child name(args)` spawns and awaits. `spawn name(args)` is fire-and-
forget: the child is durably started, the parent does not wait and cannot
bind a result (`as` on a `spawn` step is a check error). The child's fate is
its own; it is observable in the console like any run.

### Action declarations stay in the document (DECIDED 2026-07-09)

Workers implement actions, but the declaration in the workflow file is not
the implementation — it is the workflow's **requirement**: "an action of
this name and shape must exist on queue X." It stays in-file because the
checker types against it and the AI authoring loop needs contracts
in-context; a workflow document remains completely self-contained. The
missing half is **deploy-time contract verification**: at deploy and at
worker registration, every declared requirement is checked schema-against-
schema (both sides derive from the same `type` declarations) against what
registered workers provide, and mismatches are refused loudly. Rides the
#186 worker-in-package design. If contract duplication across many
workflows ever hurts, a shared-contracts include is the escape valve — not
before the pain is real.

### Indexing (plumbing, with a runtime edge)

`items[<int-literal>]` selects one element (`batches[0]`). Out-of-range is a
step failure at runtime with the expression's span in the error — the checker
cannot prove lengths and does not pretend to. Non-literal indices are
rejected in AWL-1 (computed indexing is computation). (AWL-1 open question 2.)

### SDK mapping additions

| SDK primitive | AWL-1 |
|---|---|
| `workflow.all` | `parallel` block |
| `workflow.race` | `race` block |
| sequential fold over `workflow.run` | `each … in order` |
| `child.spawn` (detached) | `spawn name(args)` |
| `continue_as_new` | implicit at history threshold under unbounded `repeat` (pending ruling) |

### AWL-1 rulings (ratified by Tom, 2026-07-09)

All four recommendations accepted as spec:

1. **Sequential-each spelling: `each x in xs in order`.** One iteration
   construct, explicit modifier, no lookalike-language prior. Bare `each`
   stays parallel; the author always chooses.
2. **Indexing: literal-only `xs[0]`.** No `first`/`last` pseudo-fields, no
   computed indices (computation belongs in actions). Out-of-range = step
   failure with the expression's span.
3. **Continue-as-new: implicit, engine-side.** The runtime rolls history at
   a threshold under unbounded `repeat until`, invisible to the author; no
   language construct. The threshold VALUE is an engine-config decision that
   still requires explicit discussion with Tom before implementation — no
   assumed default ships.
4. **`match` arm bodies: one-call rule.** Consistent with `each`/`repeat`;
   multi-step units are child workflows.

Additionally, resolving the #238(2) spec question (ratified same date):

5. **`until` sees the step's own fresh `as` binding.** The canonical poll
   loop — `repeat up to 10` / `do check_status() as status` /
   `until status.done` — is the construct's reason to exist; `until` is
   evaluated after each round, so referencing that round's result is
   deterministic and must typecheck. The checker's current rejection is an
   implementation bug against this ruling, fixed as part of the #241 rev.

## Open decisions (recommendations inline)

1. **File extension.** `.awl` (recommended — `.aion` is taken by package
   archives) vs `.flow`.
2. **Heterogeneous parallel + race.** ~~Deferred out of rev 0~~ — RESOLVED
   2026-07-09: sanctioned for AWL-1, spec'd above (`parallel` / `race`
   blocks).
3. **`now()`/`random()`.** Absent from rev 0 (recommended): workflows that
   need deterministic entropy/clock are past the language's complexity
   budget — Gleam is right there. Revisit on demand.
4. **Arithmetic.** Rev 0 has comparisons and string `+` only (recommended);
   integer arithmetic invites computation that belongs in actions.
5. **Enums/match.** ~~Rev 0 models alternatives as `Bool`/`String` fields +
   `when` guards~~ — RESOLVED 2026-07-09: sanctioned for AWL-1, spec'd above
   (payload-less enums + exhaustive `match`; tagged payloads still deferred).

## Ratification path

Tom cold-reads sketch H against A–G (the fixture-based measurement from
VESPER-LYND-METHOD-NOTES §3; optionally the second leg: hand an assistant the
same spec and measure authoring accuracy). If H smells right, AWL-0 build
starts: parser → typechecker → printer → goldens, with the mechanical parts
dispatched through the dev_brief pipeline itself.
