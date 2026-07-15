# AWL flow vocabulary — design brief (rev 3, for ratification)

2026-07-15. Rev 2 ratified in shape; this revision folds the operator's
refinements and completes the set. Nothing in this document is
deferred: everything named here ships in this build.

The laws, in the operator's words:

1. **The code and the canvas are 1:1.** Every step is a node; every
   node is a step.
2. **Distribute is its own step** and contains nothing else.
3. **The subflow is its own step** when used.
4. **The collect is its own step**, and the action is `collect` — we
   are collecting the answers.
5. The two delivery patterns are two verbs: **`distribute`**
   (parallel) and **`sequence`** (one at a time, in order).
6. JSON schemas are written **as JSON** — never as escaped strings —
   or derived from types. Half the point of this build.

## 1. Distribute / sequence — open a per-item region

```awl
step wave
  distribute item in state.items
```

- `distribute <var> in <collection>` — instances run in parallel.
- `sequence <var> in <collection>` — same region semantics, one
  instance at a time in collection order.
- Either is its step's only line. Everything downstream runs once per
  item, with `<var>` bound, until a `collect` step merges the track.
- The steps between are ordinary steps — calls, decisions, loop-backs
  (`max N visits`) — they simply run per instance:

```awl
step wave
  distribute item in state.items

step develop           // per item
  run_agent(…item…) -> note

step review            // per item; loops back on itself
  run_agent(…) -> verdict
  outcome redo: when verdict.verdict == "reject", route develop
  outcome ok:   otherwise (fall through)
  max 3 visits

step gather
  collect verdict -> results
```

- Empty collection: zero instances; the collect yields `[]` and flow
  continues.

## 2. Collect — close the region

```awl
step gather
  collect verdict -> results
```

- `collect <binding> -> <name>` waits for every instance and gathers
  each instance's `<binding>` into `results: [T]`. Any instance
  failing terminally fails the run (strict form).
- **`collect verdict? -> results`** — the tolerant form, using the
  language's existing `?`: a failed instance's slot is absent instead
  of failing the run. `results: [ItemVerdict?]`, one slot per item, in
  item order — count and alignment preserved; filter with the existing
  combinators when only successes matter. The run history holds each
  failure's detail; the workflow sees presence/absence.
- **No `from` clause.** Regions nest like brackets: a `distribute`
  opened inside a region must `collect` inside it, and a bare
  `collect` always closes the nearest open region. Interleaved or
  overlapping regions are unwritable, so naming the source would name
  the only possibility. If graphs ever grow a genuine ambiguity, the
  checker will demand a disambiguator then — today there is nothing to
  disambiguate.
- Checker: every `distribute`/`sequence` reaches exactly one
  `collect`; the collected binding is assigned on every success path
  through the region; loop-backs stay inside the region; no route
  leaves the region except through its collect.

## 3. Subflow — a named container, used as a step

```awl
subflow dev_item(item: WorkItem, notes_dir: String)
  outcome out: type ItemVerdict
  step develop
    run_agent(…) -> note
  step review
    run_agent(…) -> verdict
    outcome redo: when verdict.verdict == "reject", route develop
    outcome ok:   otherwise, route out(verdict)
    max 3 visits

step build
  dev_item(item: item, notes_dir: notes_dir) -> verdict
```

- Declared like a workflow: typed inputs, one typed outcome, own steps
  with decisions and bounded loop-backs. Same anatomy at every scale.
- Used, it is one step — one canvas node, ×N inside a region,
  expandable to its internal graph.
- Compiles inline (no separate deploy, no engine object). Subflows
  nest and may contain regions of their own.

## 4. Constants, raw strings, JSON, schemas

Four pieces; together they end escaped-string authoring.

- **`const`** — document-level named values:

  ```awl
  const max_waves = 3
  const dev_instructions = """
    You are a wave worker. Do NOT modify repository code.
    Write your approach note to the path the prompt names.
    """
  const verdict_schema = schema of ItemVerdict
  ```

  Values: any literal (including raw strings and `json` blocks),
  `schema of Type`, list literals, and `+` concatenations of these —
  all folded at compile time. Usable wherever an expression is. No
  cycles (checker). This also fixes the parser wart where a statement
  could not start with a string literal.

- **Raw strings** — triple-quoted `"""…"""`: newlines literal, no
  escape processing. Prompts and prose paste in verbatim.

- **`json { … }` literals** — actual JSON, written as JSON:

  ```awl
  const item_schema = json {
    "type": "object",
    "properties": {
      "id":    { "type": "string", "description": "git-ref-safe slug" },
      "title": { "type": "string" },
      "goal":  { "type": "string", "description": "one-sentence outcome" }
    },
    "required": ["id", "title", "goal"],
    "additionalProperties": false
  }
  ```

  The lexer consumes the balanced-brace body verbatim; the checker
  parses it and rejects invalid JSON with a span-accurate error. The
  value is a `String` (the verbatim text), so it drops straight into
  `output_schema:` arguments. Descriptions and every other schema
  keyword are just… written, because it is just JSON.

- **`schema of Type`** — compile-time `String` holding the type's JSON
  Schema. The deriver exists today and already carries `///` doc
  comments through as `description` fields and maps `?` fields to
  not-required — so the type route gives descriptions too:

  ```awl
  /// One planned work item, minted by the coordinator.
  type WorkItem {
    /// git-ref-safe slug; names the branch.
    id: String,
    title: String,
    goal: String,
  }
  ```

  `schema of WorkItem` yields the schema with those descriptions in
  place. Types when you have a type; `json {}` when you want to write
  schema by hand. Both first-class.

## 5. Everything already right stays

- **Decisions**: `outcome … when … route`, drawn as diamonds; a
  body-less step with only outcomes is a pure decision node.
- **Loops**: backward `route` to an earlier step (implemented), with
  `max N visits` as the step-level bound (closes the checker soundness
  gap where a decoy `max 1` loop satisfies the cycle rule) and
  `visits` readable in outcome guards.
- **Waits**: `wait signal`, `sleep` — unchanged.
- **`fork` leaves the language.** A fork is a decision; decisions
  already have their surface. Intra-step `fork`/`join`/`loop` parse
  with a deprecation diagnostic through the migration window while the
  example corpus moves, then `fork` is removed. (Intra-step `loop`
  remains — it is value-threading iteration, not flow structure.)

## 6. The worked example — dev_flow, final surface

```awl
workflow dev_flow
  input task: Task
  outcome done:   type Complete,   route success
  outcome failed: type Incomplete, route failure

const coordinator_instructions = """You are the coordinator…"""
const agent_schema = schema of AgentOut

subflow dev_item(item: WorkItem, notes_dir: String)
  outcome out: type ItemVerdict
  step develop
    run_agent(…) -> note
  step review
    run_agent(…) -> verdict
    outcome redo: when verdict.verdict == "reject", route develop
    outcome ok:   otherwise, route out(verdict)
    max 3 visits

step plan
  run_agent(instructions: coordinator_instructions, output_schema: agent_schema, …) -> state

step wave
  distribute item in state.items

step build
  dev_item(item: item, notes_dir: notes_dir) -> verdict

step gather
  collect verdict? -> results

step fold
  run_agent(…resume coordinator…) -> state
  outcome next:   when state.items is present, route wave
  outcome finish: otherwise, route done(value: state.summary)
  max 3 visits
```

Five steps in the code, five nodes on the canvas:

```
   ┌────────┐
   │  plan  │
   └───┬────┘
       ▼
   ┌──────────┐
   │ wave   ⫴ │  distribute item in state.items
   └───┬──────┘
       ▼ ×N
   ┌──────────────┐
   │ build     ×N │  dev_item (expandable)
   └───┬──────────┘
       ▼
   ┌──────────┐
   │ gather ⫵ │  collect verdict? -> results
   └───┬──────┘
       ▼
   ┌────────┐
┌─▶│  fold  │◇── done ──▶ (success)
│  └───┬────┘
└──────┘ more, ×3
```

The multi-step-region shape draws the same way: the per-item steps
appear in sequence between ⫴ and ⫵, each marked ×N, loop-backs shown.

## 7. Lowering — all in scope

- Distribute/sequence regions and subflows lower to the existing
  fan-out machinery and inline bounded recursion (both proven on the
  direct path). Step cycles already lower. `sequence` lowers on the
  already-implemented sequential branch delivery.
- **`collect ?` requires activity-failure-as-value at the workflow
  layer** — the same capability the `on failure` construct needs. That
  surface is IN SCOPE of this build, not assumed: it is one work item,
  and completing it also unblocks the two on-failure direct-compile
  refusals. One capability, two constructs paid off.
- Projection/canvas: `SemanticIndex` gains subflows, region
  membership, step kinds (distribute / collect / decision / subflow
  call); the canvas draws the node vocabulary of §6. Served from the
  parsed document, so the canvas is correct as soon as parser +
  checker + projection land — before the emitter.
- Workers untouched; `run_agent` and the general worker unchanged.

## 8. Build sequence (one build, four landings)

1. **Ergonomics**: `const`, raw strings, `json {}` literals,
   `schema of`, literal-statement parser fix.
2. **Language shape** (parser + checker + projection): `subflow`,
   `distribute`/`sequence`, `collect`/`collect ?`, `max N visits` +
   `visits`, decision tagging → canvas draws the truth, `aion awl
   check` enforces it.
3. **Lowering** (emitter + the failure-as-value surface): full
   direct-compile parity, on-failure refusals fall with it.
4. **Migration**: example corpus rewritten (dev_flow first, as the
   proof), deprecation window closes, `fork` retired.
