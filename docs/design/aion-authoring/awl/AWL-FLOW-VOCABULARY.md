# AWL flow vocabulary — design brief (rev 1, folding the operator's tear)

2026-07-15. Rev 0 made the mistake this brief exists to kill: it folded
the join into the fan-out step as a closing line and drew the fan as a
container box around the branch. The operator's ruling, twice given, is
the law of this design: **fan out is a step. Join is a step.** Distinct
nodes, in sequence, on the canvas and in the text:

```
plan → fan out → subflow ×N → join (collect) → fold → (back to fan out)
```

And the naming ruling: the repeatable container is a **subflow** — never
"flow"; there is a workflow, and inside it there are subflows.

## 1. The elements

The element vocabulary, benchmarked against Salesforce Flow / BPMN / n8n.
Where AWL stands today:

| # | Element | BPMN / Salesforce | AWL today | Disposition |
|---|---------|-------------------|-----------|-------------|
| 1 | step (do work) | task / Action | `step` + calls ✓ | keep |
| 2 | sequence | connector | fall-through / `route` ✓ | keep |
| 3 | decision (one path) | XOR gateway / Decision | `outcome … when … route` ✓ | keep; draw as diamond; body-less step with only outcomes = pure decision node |
| 4 | **fan out** (split to N parallel) | AND/multi-instance split | intra-step `fork item in xs` (misnamed, buried) | → its **own step kind** (§3) |
| 5 | **subflow** (repeatable container) | subprocess / Subflow | none inline (only separately-deployed children) | → ADD (§2) |
| 6 | **join / collect** (converge) | AND-join / Merge | `join ->` line buried inside fork | → its **own step kind** (§4) |
| 7 | loop | cycle / loop marker | backward `route` ✓ (implemented) + intra-step `loop` | step cycles primary; add `max N visits` (§5) |
| 8 | wait | timer/message event | `wait signal`, `sleep` ✓ | keep |
| 9 | failure path | boundary error / Fault | `on failure` grammar ✓ | unchanged here |
| 10 | static named branches | AND gateway (heterogeneous) | bare `fork` named branches | rename → `branch` when promoted; out of scope here |

The `fork` keyword leaves the language after a deprecation window: in
English a fork is a decision, and AWL's decisions already have their
surface (`when`/`otherwise` routing). Nothing else gets to squat on a
decision word.

## 2. Subflow — the repeatable container

A `subflow` is declared like a workflow — typed inputs, typed outcome,
its own steps with decisions and bounded loop-backs — and lives in the
same document. It is the thing a fan out step instantiates once per
item: each instance follows its own path through the subflow's steps
("dev step, then plan step, then run-checks step, and they can loop
back on themselves").

```awl
subflow dev_item(item: WorkItem, notes_dir: String)
  outcome out: type ItemVerdict

  step develop
    run_agent(…, prompt: "Item " + item.id + " — " + item.goal, …) -> note

  step review
    run_agent(…) -> verdict
    outcome redo: when verdict.verdict == "reject", route develop
    outcome ok:   otherwise, route out(verdict)
    max 3 visits
```

- Compiles inline: no separate deploy, no engine object. Subflows nest
  (a subflow's steps may fan out over another subflow).
- v1: exactly one success outcome type per subflow — that type is what
  the join collects. (Multiple outcome types → union — deferred.)
- On canvas: its own node, marked ×N, collapsed to one box or expanded
  to show its internal step graph.

## 3. Fan out — a step

A fan out step does exactly one thing: split the line. Its body is the
one statement:

```awl
step wave
  fan out item in state.items into dev_item(item: item, notes_dir: notes_dir)
```

- `fan out <var> in <collection> into <target(args…)>` — the target is
  a subflow or, for the trivial case, a single action call (no subflow
  ceremony needed to fan seven `run_agent`s).
- One instance of the target runs per item, in parallel (`sequential`
  modifier available).
- A fan out step contains nothing else — no prep work, no trailing
  join. That is what keeps the canvas node honest: one node, one split.
- `fork item in …` (intra-step) parses through a deprecation window,
  then goes.

## 4. Join — a step

The instances converge at a join step (a collect step):

```awl
step collect
  join all -> results
```

- `join <mode> -> <name>` opens the step; the step may route or carry
  further statements after it, or just fall through.
- The parent-level shape is enforced by the checker: a fan out step's
  successor is its subflow instances, and their completion flows to
  exactly one join step — written adjacent (fall-through) or named by
  route. Every fan out has its join; every join has its fan out. The
  bare form `join <mode>` is unambiguous under adjacency; the explicit
  form `join <mode> from wave` exists for when graphs grow.
- Modes:
  - `join all -> results` — wait for every instance; `results` is
    `[T]` where `T` is the subflow's outcome type. Today's fail-fast
    semantics: an instance's terminal failure fails the run. v1.
  - `join settled -> results` — wait for every instance; failures
    arrive as data. Element type is the builtin parametric
    `Settled(T)`: `{ ok: Bool, value: T?, error: String }` (alongside
    `List(T)` and `T?`). Honesty note: settled needs instance failure
    captured before engine fail-fast triggers — an engine option on the
    barrier or lowering through the completed `on failure` path. The
    one item here that may touch the engine; ships after `all`.
  - `join first -> result` — race. Named for completeness; not v1.

## 5. Loops — step cycles, honestly bounded

Backward `route` to an earlier step is already implemented as the
state-machine loop form. Two additions make it the primary loop:

- `max N visits` on a step — the checker accepts a route cycle when a
  member step carries a visits bound (or an input-derived one). Closes
  a real soundness gap: today's rule ("some member contains a bounded
  `loop`") is satisfiable by a decoy `max 1` loop that bounds nothing.
- `visits` — builtin `Int` readable in that step's outcome guards.

Intra-step `loop` remains for tight value-threading; flow-level loops
are routes back to earlier steps.

## 6. Authoring ergonomics (rides alongside)

- **`const`** — top-level named literals (prompts, schemas, gate
  lists): `const dev_instructions = """…"""`. Also fixes the parser
  wart where a statement cannot start with a string literal.
- **Raw strings** — triple-quoted `"""…"""`: newlines literal, no
  backslash escaping; JSON pastes in as JSON.
- **`schema of Type`** — compile-time expression yielding the type's
  JSON Schema as a `String`. The toolchain already derives schemas from
  AWL types; this makes it reachable inside a document.

## 7. The worked example — dev_flow rewritten

```awl
workflow dev_flow
  input task: Task
  outcome done:   type Complete,   route success
  outcome failed: type Incomplete, route failure

const agent_schema = schema of AgentOut
const coordinator_instructions = """You are the coordinator…"""

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
  run_agent(instructions: coordinator_instructions, …) -> state

step wave
  fan out item in state.items into dev_item(item: item, notes_dir: notes_dir)

step collect
  join all -> results

step fold
  run_agent(…resume coordinator…) -> state
  outcome next:   when state.items is present, route wave
  outcome finish: otherwise, route done(value: state.summary)
  max 3 visits
```

## 8. What the canvas draws

Distinct nodes in sequence — no container boxes:

```
        ┌───────────┐
        │   plan    │
        └─────┬─────┘
              ▼
        ┌───────────┐
        │ wave   ⫴  │  fan out: item in state.items
        └─────┬─────┘
              ▼ ×N
   ┌──────────────────────┐
   │ dev_item          ×N │  (subflow — expandable)
   │  ┌─────────┐ ┌─────┐ │
   │  │ develop │▶│revw │ │
   │  └─────────┘ └──◇──┘ │
   │       ▲   redo  │    │
   │       └─────────┘ ×3 │
   └──────────┬───────────┘
              ▼
        ┌───────────┐
        │ collect ⫵ │  join all -> results
        └─────┬─────┘
              ▼
        ┌───────────┐
   ┌──▶ │   fold    │
   │    └────◇──────┘
   │ more    │    │ done
   │ ×3      │    ▼
   └─────────┘ (success)
```

Collapsed, `dev_item` is one ×N box; expanded it shows its own steps,
decisions, and loop-backs. The projection is served from the **parsed**
document, so this picture lands as soon as parser + checker +
projection understand the forms — before emitter work. Canvas relief
ships first.

## 9. Lowering and compatibility

- No engine change for anything except `join settled` (§4). Fan out /
  join steps lower to the existing fan-out machinery (direct-compiles
  since the fork-generality work); subflows lower to inline functions
  with bounded recursion (continuation nesting — proven emitter
  technique); step cycles already lower.
- Existing documents keep compiling through the deprecation window
  (intra-step `fork`/`join` parse with a deprecation diagnostic).
  staged_rounds / dev_brief migrate as the proof corpus.
- Workers untouched. `run_agent` and the general worker are unchanged —
  this is all authoring surface.

## 10. Build sequence

1. **Ratify this brief** (operator tear → fold → ratify).
2. **Ergonomics batch**: `const`, raw strings, `schema of`,
   literal-statement parser fix. Small, independent, immediate relief.
3. **Grammar + checker + projection** for `subflow`, fan out steps,
   join steps (`all`), `max N visits`, decision-node tagging → the
   canvas draws the real shape (check-only; emitter untouched).
4. **Emitter lowering** for subflows + fan out/join → direct-compile
   parity.
5. **`join settled`**, then `branch` promotion and `fork` retirement.
