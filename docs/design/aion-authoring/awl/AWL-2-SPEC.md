# AWL-2 — aion workflow language, rev-2 surface

Status: DRAFT for market test (Tom's cold-read survey of external models).
Designed with Tom 2026-07-10 over five design rounds; the surface replaces the
AWL-0/AWL-1 grammar ([AWL-0-SPEC-DRAFT](AWL-0-SPEC-DRAFT.md)) entirely. The
doctrine underneath — determinism by construction, lossless parse ↔ print,
one declaration feeding every schema consumer, AI agents as primary authors —
is unchanged and restated here so this document stands alone.

An earlier cut of the flagship example passed a four-model cold-read survey
(2026-07-10): every model reconstructed the full semantics — the dependency
graph, the loop bounds, the fork/join, the deny-override review policy, the
operator-merges contract — with zero construct misreads. Three fixes from
that survey are folded in below (language-owned loop counter, `?`
optionality, evidence-carrying outcomes).

## Position

- The `.awl` document is the source of truth. The canonical workflow model is
  its lossless parse; everything else (generated Gleam during the stopgap,
  canvas, docs, schemas) is a generated view. Nobody edits generated output.
- Execution target is **direct beamr bytecode emission**
  ([AWL-BC-DESIGN-DRAFT](AWL-BC-DESIGN-DRAFT.md), #240). The Gleam emitter is
  a live, proven stopgap and dies when AWL-BC lands. Gleam remains a
  first-class authoring language for workflows that outgrow AWL.
- Tom's design test, now a constant: **the file must be so straightforward it
  requires no explanation.** Concretely: every construct must be something a
  model already knows cold from training data (assignment-free dataflow,
  if-like guards, CI-style dependencies, fork/join, repeat-until). Invented
  vocabulary is a defect unless it names something no mainstream idiom covers
  (`step`, `outcome`, `route` — the durable-workflow nouns themselves).

## Design constants (inherited, unchanged)

1. **Determinism by construction.** No vocabulary for clock, randomness, or
   ambient I/O. Calling an action is the only world-touching verb; time is
   engine-mediated (`sleep`, `wait` + `timeout`) and recorded.
2. **Canonical model; lossless parse ↔ print.** `parse ∘ print = id`;
   `print ∘ parse ∘ print = print`.
3. **Types on everything.** Every call and every route is checked against a
   declared contract; error quality is a compiler's.
4. **Prose is load-bearing.** Doc comments flow to schemas, canvas labels,
   and console narration. A running workflow narrates itself in its author's
   words.
5. **One declaration, every consumer.** A type declaration IS its JSON
   Schema; the start form, the model's `--output-schema`, and the worker
   contract are the same bytes.

## The mental model

Three concerns, in file order:

1. **Types** — the schemas: workflow input contract, outcome contracts,
   every value that moves between steps.
2. **Operations** — the things that happen:
   - *actions*: non-deterministic, worker-executed, world-touching;
   - *transformations/selections*: deterministic combinators over data,
     executed in the workflow VM (`filter`, `map`, `sort`, `count`);
   - *decisions*: conditional outcomes that route the flow;
   - *delegation*: child workflows.
3. **Flow** — steps and how they connect: sequence (fall-through), DAG
   dependencies (`after`), parallelism (`fork`/`join` and independent
   steps), iteration (`loop`), and routing (`route`).

**Unified anatomy:** everything that runs — workflow, step, substep — has
*inputs* and *outcomes*; an outcome has a *type* and a *route*.

## The five rules

The whole execution surface:

1. `->` means **becomes / goes to** — a call's result is bound
   (`provision(…) -> workspace`), a value is routed.
2. `|>` means **then, with the value** — a genuine pipe: the left value is
   the input of the right stage (`verdicts |> filter(.blocking) -> blocking`).
   Only used where a value actually threads through; never as decoration.
3. `route <target>` means **control goes there** — to another step, or to a
   workflow outcome (which ends the run with that payload).
4. `fork … join -> <name>` is the **only intra-step parallelism**;
   independent steps sharing an `after` dependency are the inter-step form.
5. `loop … until … max …` is the **only iteration**.

Everything not inside a `fork` or `loop` runs in written order. Order,
parallelism, and repetition are each visibly marked — never inferred.

## Lexical rules

- UTF-8. `//` comments are ignorable trivia to end of line.
- `//!` doc lines before the `workflow` header are the workflow's narration.
  `///` doc lines attach to the next declaration (type, field, action, step)
  and are **data, not trivia**: they flow into derived JSON Schema
  `description`s and console narration. There is no `about` keyword —
  doc comments are the one prose mechanism.
- Indentation is structure, two spaces per level; the printer emits exactly
  one canonical shape.
- Identifiers `snake_case`; type names and outcome-payload constructors
  `TitleCase`; keywords reserved everywhere.
- Literals: `"strings"` (escapes `\" \\ \n \t`), integers, floats, `true`,
  `false`, durations (`30s`, `5m`, `3h`, `2d`), lists `[a, b]`, record
  construction `TypeName(field: expr, …)`.
- `.field` after a value is field access; a bare `.field` inside a
  combinator call is the accessor shorthand (`filter(.blocking)` ≡ keep
  items whose `blocking` is true; `map(.reject_reason)` ≡ project the field).

## Document grammar

One file, one workflow, declarations in canonical order:

```
//! <workflow narration — one or more lines>
workflow <name>
  input <name>: <Type>                        // repeatable
  signal <name>: <Type>                       // repeatable, optional
  outcome <name>: type <Type>, route success|failure   // repeatable, ≥1

type …                                        // repeatable
worker <name>                                 // repeatable
  action …                                    //   one or more per worker
child <name>(param: Type, …) -> <Type>        // repeatable, optional

step <name> [after <step>, …]                 // repeatable, ordered
  <body>
  <outcomes>
```

### Workflow header

- `input name: Type` — the start contract. All inputs validate against
  their (derived or imported) JSON Schema at start; **an explicit `null` in
  the input document is a start-time error** — absence is expressed by
  omitting an optional (`?`) field, never by null.
- `signal name: Type` — a durable external signal the workflow can `wait` on.
- `outcome name: type T, route success|failure` — the terminal taxonomy.
  Each names one way the run can end, the payload type it carries, and which
  engine terminal status it maps to (`success` → Completed, `failure` →
  Failed). Console, retries, and alerting key off the route without
  inspecting payloads. **Outcome types carry the evidence they promise** —
  if the workflow's narration says the handoff includes review verdicts, the
  outcome type includes the verdicts (survey fix 3; a brief-craft rule the
  checker cannot enforce but reviews must).

### Types — three doors, one type system

All three forms declare the same kind of nominal type; every declared type
converts losslessly to JSON Schema (draft 2020-12) and back:

**1. Shorthand** — typed JSON, defined as a 1:1 abbreviation of the schema
it derives:

```awl
/// One adversarial reviewer's verdict.
type LensVerdict {
  lens: String,
  blocking: Bool,
  /// Present only when the verdict is blocking.
  reject_reason: String?,
}
```

**2. Inline raw schema** — paste an existing JSON Schema verbatim:

```awl
type Round = schema {
  "type": "object",
  "required": ["summary", "gates_green"],
  "properties": {
    "summary":     { "type": "string" },
    "gates_green": { "type": "boolean" }
  }
}
```

**3. File import** — shared contracts stay in their files:

```awl
type Brief = schema("schemas/brief.schema.json")
```

Rules (carried over from the 2026-07-09 decisions, unchanged in substance):

- **Optionality is `?`.** `field: Type?` means the field may be absent; it
  maps to "not in `required`" in schema terms, in both directions. Explicit
  `null` is invalid everywhere — in input documents, in imported schemas'
  instances, in record construction. Absent and `?` go together; null does
  not exist in the language. (Survey fix 2; also resolves the long-open
  null-vs-Option ruling. `Option(T)` is gone; `T?` replaces it.)
- Builtins: `Bool`, `Int`, `Float`, `String`, `Nil`, `[T]` (list), `T?`
  (optional), `Dir` (content-addressed snapshot handle). `List(T)` is gone;
  `[T]` is the one list spelling, matching the JSON it describes.
- Payload-less enums: `type Category = Urgent | Routine | Spam` — derives a
  string enum. Payload-carrying variants remain deferred; the outcome
  mechanism covers the tagged-union need at the workflow boundary.
- `///` doc lines on types and fields flow into `description` at each level.
- Canonical layout: single-line iff the rendered line fits 100 columns,
  else one field per line with trailing commas; the parser tolerates
  missing/trailing commas both ways.
- Imported schemas: typing uses the record-shaped projection
  (`object`/`properties`/`required`, nested objects, arrays, string enums,
  `$defs`-local `$ref`); a property absent from `required` types as `T?`.
  Structural keywords the model can't honor (`oneOf`, `anyOf`,
  `patternProperties`, conditionals…) are check ERRORS naming the keyword
  and JSON path. Constraint keywords (`minLength`, `pattern`, bounds,
  `format`) are ignored for typing but preserved on re-emit. The file is
  source: it travels content-addressed into the package; missing/unparseable
  is a check error. `aion awl fmt` never rewrites it.
- Constraint vocabulary is deliberately NOT authorable inline: types +
  descriptions + enums are the contract surface; value validation is an
  action's job.
- Schema derivation stays a pure public function in `aion-awl`
  (`schema_for_type`); `aion package` embeds schemas for contract-reachable
  types; the server exposes them (start forms, #209); an action result used
  as a model output contract rides to the worker on dispatch (the #186 seam).

### Worker blocks — actions are typed imports

Workers implement actions; the workflow file declares its **requirement**:
"an action of this name and shape must exist on this worker's queue." The
block reads as an import of a worker's surface:

```awl
worker dev_brief
  action provision(repo_root: String, base_branch: String, brief: Brief) -> Workspace
    node shell, timeout 5m, retry 2 every 30s
  action fix_round(brief: Brief, scout: ScoutReport, workspace: Workspace, prior: Round) -> Round
    node developer, timeout 30m
```

- The worker name is the task queue. `node`, `timeout`, `retry N every D`
  (or `retry N backoff D..D`) are per-action config on the indented line;
  a step may override `node`/`timeout` at the call site when it must pin.
- Calls use **required named arguments**, checked against the declaration —
  no positional form (the positional-swap bug is unwritable).
- Deploy-time contract verification (rides #186): at deploy and at worker
  registration, every declared requirement is checked schema-against-schema
  with what registered workers provide; mismatches refuse loudly.
- One declaration feeds the typechecker, the worker handler stubs, the
  start-form schema, and the model's structured-output contract.

### Child workflows — delegation

```awl
child fix_subtask(brief: Brief, parent_round: Round) -> Round
```

Declared like actions (outside worker blocks — the engine routes children,
not a queue). Called like actions in a step body (`fix_subtask(…) ->
result`, awaited). `spawn <child>(args)` is fire-and-forget: durably
started, no binding (`->` after `spawn` is a check error), observable in the
console like any run. Deploy binds the declared name to a deployed workflow
whose input/output schemas must match.

## Steps

```awl
step <name> [after <step>[, <step>…]]
  <statements and blocks>
  [<outcomes>]
```

### Dependencies and scheduling: `after`

- `after a, b` — the step starts when ALL named steps have completed. This
  is the CI `needs:` idiom: dependencies declared on the consumer.
- **No `after` and no incoming route** → fall-through: the step depends on
  the step written immediately above it. Linear workflows write no `after`
  and no `route`.
- Two steps that share a dependency and not each other run **in parallel**
  — inter-step parallelism is emergent from the graph, never announced.
- A step targeted by a `route` runs when that route fires (its `after`
  dependencies, if any, must also be complete).
- The checker proves: every `after`/`route` target exists; no dependency
  cycles (loops use `loop`, or a route to an earlier step — see routing);
  no unreachable steps; every non-terminal step has a successor.

### Step bodies — statements in written order

- **Action / child call with binding:** `provision(repo_root: config.repo_root, …) -> workspace`.
  A call whose result is unused needs no `->` (side-effect steps are legal;
  reviewers will read the absence of a binding as the tell that only the
  effect matters).
- **Pipe chains:** `name |> greet |> .greeting |> shout -> shouted` — each
  stage receives the previous value: an action of one argument, a `.field`
  access, or a combinator. Terminate with `-> name` (bind) or
  `route <outcome>` (the piped value is the payload).
- **Combinators** (deterministic, VM-executed — the transformations and
  selections vocabulary): `filter(pred)`, `map(proj)`, `sort(key)`,
  `count`, plus the predicates `is empty`, `is present` / `is absent` (for
  `T?` values) and comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`), boolean
  `not/and/or`, string `+`. Arguments are `.field` accessors or literals.
  This is a **fixed vocabulary, not an expression language**: no closures,
  no user functions, no arithmetic beyond comparison and string `+`.
  Computation heavier than plumbing lives in actions. (This deliberately
  reverses AWL-1's "combinators out" exclusion — Tom ruled 2026-07-10 that
  paying a worker round-trip for a filter is absurd; the determinism
  constant is untouched because every combinator is pure.)
- **Durable waits:** `wait <signal> [timeout <duration>] -> name` — a
  durable gate on a declared signal. With `timeout`, the binding is `T?`
  (absent on expiry) and the step's outcomes branch on `is present`.
  `sleep <duration>` is a durable timer statement.
- **Indexing:** literal-only `items[0]`; out-of-range is a step failure
  carrying the expression's span. Computed indices are computation — out.

### `fork` / `join` — intra-step parallelism

```awl
fork lens in config.lenses
  review_lens(lens: lens, workspace: workspace, round: round)
join -> verdicts
```

- Collection form: one branch per item, exactly-once per item, results
  joined **in input order** regardless of completion order (determinism).
- Named-branch form for heterogeneous work: each branch is its own chain
  with its own binding; `join` (no `->`) waits for all:

```awl
fork
  fetch_profile(id: input.id) -> profile
  fetch_history(id: input.id) -> history
join
```

- A branch's failure (after that action's own retries) fails the step.
- `sequential` on the fork line (`fork lens in config.lenses sequential`)
  runs branches one at a time in order; first failure stops the remainder.
  Bare `fork` is parallel. The author always chooses; neither is silent.

### `loop` — bounded iteration

```awl
loop round = Round(summary: "", gates_green: false) counting cycles
  fix_round(brief: brief, scout: scout_report, workspace: workspace, prior: round) -> round
  until round.gates_green
  max config.max_fix_cycles
```

- `loop <name> = <seed>` declares the ONE value threaded between
  iterations; the body must rebind it (`-> round`). The body runs at least
  once; `until` is evaluated after each pass against the fresh binding;
  `max` is the mandatory ceiling (an expression over inputs/bindings).
- `counting <name>` binds a language-maintained `Int` — the number of
  completed iterations — usable in the step's outcomes and every later
  step. **The engine counts; workers never carry a counter.** (Survey
  fix 1: a worker-maintained counter can silently rot; a language-owned
  one cannot. The whole bug class is unexpressible.)
- After the loop, the threaded binding and the counter flow to the step's
  outcomes. Exhaustion (ceiling hit with `until` still false) is not
  implicit: the step's outcomes must cover it (`when`/`otherwise`), and the
  checker verifies an exhausted loop cannot fall off the end of a step with
  conditional outcomes uncovered.
- Unbounded `loop … until` (no `max`) stays illegal until the engine's
  implicit continue-as-new lands (unchanged ruling; the threshold value
  still requires explicit discussion — no silent default ships).

### Outcomes — evaluation and direction in one clause

```awl
outcome green: when round.gates_green, route review
outcome spent: otherwise,
  route exhausted(reason: "gates never went green", cycles_spent: cycles)
```

- Outcomes are evaluated **in written order after the body completes**;
  the first `when` that holds fires. `otherwise` is the complement and, if
  present, must be last.
- `route <step>` transfers control to a step. `route <workflow-outcome>(…)`
  constructs the payload and **ends the run** — there is no `finish`
  keyword; finishing IS routing to a workflow outcome. When the payload is
  a single already-bound value of the right type, `route <outcome>` alone
  picks it up by name.
- A step with no `outcome` lines has one implicit outcome: fall through to
  the next step (or, for the final step, the checker requires an explicit
  route — a workflow may not end by running out of file).
- Routing **backward** to an earlier step is legal and is the second loop
  form (state machine style); the checker requires any cycle formed by
  routes to carry a `max`-bounded `loop` or a workflow-input-derived bound
  on some step in the cycle — unbounded cycles are unwritable.
- If a step's outcomes are conditional, they must be exhaustive: an
  `otherwise` arm, or `when` arms the checker can prove total (enum
  subjects: all variants covered).
- Guard-dependent optionality: an outcome arm guarded by
  `when x is present` may use `x` as `T` within that arm (the one flow-typing
  rule; mirrors the proven conditional-rebinding checker logic).

### Failure and compensation

An action failure (after its declared retries) fails the step. Without
handling, the workflow fails with the engine taxonomy — that is usually
right. For explicit compensation a step may declare:

```awl
on failure
  delete_assets(assets: assets)
  route failed(reason: "publish failed after compensation")
```

`on failure` runs its calls then must end in a `route` (to a workflow
outcome or a step) — silent swallowing is unwritable. Exit-status-is-data
workflows (dev_brief's gates) need none of this: outcomes-as-values remains
the doctrine.

### Substeps

A step body may contain `step` declarations. Inner steps see the parent's
bindings; inner fall-through and routes resolve within the parent first;
the parent's outcomes are the boundary (an inner route may target a parent
outcome or a sibling substep, not a step outside the parent). A substep
group is the manually-declared sub-workflow; promotion to a real `child`
workflow is the refactor when it needs independent deployment or
observation. **Implementation note: substeps ship in the front end only
after a committed fixture exercises them** — no untested grammar.

## Reserved keywords — complete inventory

| group | keywords |
|---|---|
| document | `workflow` `input` `signal` `outcome` `type` `schema` `worker` `action` `child` `step` |
| step surface | `after` `fork` `join` `loop` `counting` `until` `max` `sequential` `spawn` `wait` `sleep` `timeout` `retry` `every` `backoff` `node` `on` `failure` |
| outcomes/routing | `when` `otherwise` `route` `success` `failure` |
| combinators/predicates | `filter` `map` `sort` `count` `is` `empty` `present` `absent` |
| operators | `->` `\|>` `not` `and` `or` `==` `!=` `<` `<=` `>` `>=` `+` `?` |
| literals | `true` `false` durations |

Builtin type names reserved: `Bool` `Int` `Float` `String` `Nil` `Dir`.

Gone from AWL-0/1 (the parser of the new front end never accepts them):
`about` `do` `as` `each` `in order` `repeat` `up to` `finish` `fail`
`match` `case` `parallel` `race` `output` `error` `Option` `List` `=`
(as a statement binder; `=` survives only in `loop x = seed` and
`type X = …`).

## Semantics summary

- **Scheduling**: a step is eligible when its `after` set is complete and
  (if route-targeted) a route has fired; eligible steps with no mutual
  dependency run concurrently. Written order breaks no ties — it only
  defines fall-through edges.
- **Bindings** are single-assignment per scope; the `loop` threaded value
  is the one sanctioned rebinding. Bindings flow forward along the graph;
  the checker rejects reads of bindings not guaranteed on every path into
  a step.
- **Every dispatch, timer, signal, fork branch, and loop iteration is a
  recorded event**; replay is deterministic because the language can
  express nothing the log doesn't capture. Combinators replay as pure
  recomputation.
- **Exactly-once fan-out** (fork over collections) keeps the SDK `map`
  semantics: per-item dedup keys, results ordered by input.

## Mapping to the engine/SDK

| engine/SDK primitive | AWL-2 |
|---|---|
| `workflow.run(activity)` | call statement in a step body |
| `workflow.map` | `fork x in xs … join` |
| `workflow.all` | named-branch `fork … join` |
| sequential fold | `fork … sequential` or `loop` |
| `child.spawn`/`await` | `child` declaration + call |
| `child.spawn` (detached) | `spawn` |
| `signal.receive` | `wait <signal>` |
| timers / `with_timeout` | `sleep`; `timeout` on `wait`/actions |
| retry/queue/node config | `retry`/`worker`/`node` on the action declaration |
| terminal status | workflow `outcome … route success\|failure` |
| `continue_as_new` | deferred (unbounded loop gate, unchanged) |
| `workflow.now/random` | deliberately absent (unchanged) |
| codecs / schemas / start form | derived from `type`/`input`/`outcome` decls |

## Canonical formatting (printer contract)

- Two-space indentation; declaration order as the grammar lists.
- Type bodies: the 100-column single-line rule (unchanged).
- One statement per line; a pipe chain longer than 100 columns breaks
  before each `|>` with one extra indent.
- Outcome clauses one per line; payload construction breaks after `route`
  with one extra indent when over 100 columns.
- Round-trip properties unchanged: `parse ∘ print = id`,
  `print ∘ parse ∘ print = print`, comments and doc lines lossless.

## Worked example 1 — awl_hello

```awl
//! Greet a name, then shout it — the first workflow written in AWL and run for real.
workflow awl_hello
  input name: String
  outcome shouted: type Shouted, route success

type Greeting { greeting: String }
type Shouted  { text: String }

worker awl_hello
  action greet(name: String) -> Greeting
  action shout(text: String) -> Shouted

step greet_and_shout
  name |> greet |> .greeting |> shout |> route shouted
```

## Worked example 2 — dev_brief (the flagship)

Survey artifact with all three survey fixes applied: `counting cycles`
replaces the worker-carried counter (Round no longer has a `cycles` field),
`reject_reason` is honestly optional, and the outcomes carry the evidence
the narration promises (Landed carries the round summary; Rejected carries
the blocking verdicts themselves).

```awl
//! dev_brief: a development brief goes in; an adversarially reviewed branch comes out.
//! Nothing is pushed — the handoff is the branch plus this run's evidence; the operator merges.
workflow dev_brief
  input brief: Brief
  input config: RunConfig

  outcome landed:    type Landed,    route success
  outcome rejected:  type Rejected,  route failure
  outcome exhausted: type Exhausted, route failure

type Brief     = schema("schemas/brief.schema.json")
type RunConfig = schema("schemas/run_config.schema.json")

type Workspace   { path: String, branch: String, base_commit: String }
type ScoutReport { summary: String, pointers: [String] }
type BuildWarmth { warmed: Bool, detail: String }
type Round       { summary: String, gates_green: Bool }
type Lens        { name: String, charter: String }

/// One adversarial reviewer's verdict.
type LensVerdict {
  lens: String,
  blocking: Bool,
  /// Present only when the verdict is blocking.
  reject_reason: String?,
}

type Landed    { branch: String, fix_cycles: Int, first_pass: Bool, summary: String }
type Rejected  { branch: String, verdicts: [LensVerdict] }
type Exhausted { reason: String, cycles_spent: Int }

worker dev_brief
  action provision(repo_root: String, base_branch: String, brief: Brief) -> Workspace
    node shell, timeout 5m, retry 2 every 30s
  action scout(brief: Brief, workspace: Workspace) -> ScoutReport
    node scout, timeout 15m
  action warm_build(workspace: Workspace, gates: [String]) -> BuildWarmth
    node shell, timeout 20m
  action fix_round(brief: Brief, scout: ScoutReport, workspace: Workspace, prior: Round) -> Round
    node developer, timeout 30m
  action review_lens(lens: Lens, workspace: Workspace, round: Round) -> LensVerdict
    node reviewer, timeout 20m

step provision
  provision(repo_root: config.repo_root, base_branch: config.base_branch, brief: brief) -> workspace

step scout after provision
  scout(brief: brief, workspace: workspace) -> scout_report

step warm_build after provision
  warm_build(workspace: workspace, gates: config.gate_commands)

step fix_cycle after scout, warm_build
  loop round = Round(summary: "", gates_green: false) counting cycles
    fix_round(brief: brief, scout: scout_report, workspace: workspace, prior: round) -> round
    until round.gates_green
    max config.max_fix_cycles

  outcome green: when round.gates_green, route review
  outcome spent: otherwise,
    route exhausted(reason: "gates never went green", cycles_spent: cycles)

step review
  fork lens in config.lenses
    review_lens(lens: lens, workspace: workspace, round: round)
  join -> verdicts

  verdicts |> filter(.blocking) -> blocking

  outcome accepted: when blocking is empty,
    route landed(branch: workspace.branch, fix_cycles: cycles, first_pass: cycles == 1,
      summary: round.summary)
  outcome blocked: otherwise,
    route rejected(branch: workspace.branch, verdicts: blocking)
```

Reading the graph: provision runs first; scout and warm_build both declare
`after provision` and nothing else, so they run concurrently; fix_cycle
declares `after scout, warm_build`, so it is the join — the developer
starts only when the recon is in AND the build is warm. warm_build binds
nothing: the effect on disk is the point. review has no `after` and is
route-targeted by fix_cycle's `green` outcome.

## Deliberate exclusions (unchanged unless noted)

- No clock, no randomness, no ambient I/O; no closures or user functions;
  no arithmetic beyond comparisons and string `+`; no computed indices;
  no inline constraint vocabulary; no `null`.
- `race` (first-wins heterogeneous arms) is deferred out of rev-2 — the
  signal-or-deadline case is covered by `wait … timeout`; revisit when a
  fixture demands first-wins. (AWL-1 had sanctioned it; rev-2 narrows to
  what the examples exercise.)
- A policy vocabulary for review-style aggregation (e.g. quorum vs
  deny-override) stays out: `filter` + `is empty` expresses it and the
  survey showed it reads correctly.

## Migration from AWL-0/1

The front end (lexer, parser, printer, typechecker) and the emitter are
rebuilt against this grammar; the AWL-0 surface is not maintained in
parallel and no compatibility parse is offered (one committed example
workflow exists; it migrates by hand). The canonical workflow model keeps
its shape (nodes/edges with correlation keys) with additions: DAG edges
(`after`), conditional outcome edges, loop nodes with language-owned
counters, fork groups. Fixtures to migrate: `awl_hello.awl`,
`research_report`, `bounded_cycle`, `typed_contract` (all re-expressed in
rev-2; `bounded_cycle` becomes a `loop`+`counting` fixture and finally
typechecks by design). The AWL-EXECUTION-PLAN wave ladder is re-cut against
this spec; the AWL-BC bytecode design is unaffected in its back half
(the canonical model is its input) and gains the combinator ops.

## Ratification path

1. Tom takes this document (or just its two examples) to market — cold
   reads by external models, same protocol as the 2026-07-10 survey.
2. Verdicts fold in; Tom ratifies.
3. The wave ladder is re-cut: front-end rebuild briefs (lexer → parser →
   printer → checker → emitter adjustments), each dispatched through
   dev_brief with fixtures-first discipline.
