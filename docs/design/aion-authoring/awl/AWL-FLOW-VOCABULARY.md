# AWL flow vocabulary — design brief (rev 2, folding the operator's second tear)

2026-07-15. The law this revision is built on, in the operator's words:

1. **Distribute is its own step** — and it contains nothing else. Not
   the target, not the join. One step, one split.
2. **The subflow is its own step.** When a subflow is used, its
   invocation is a step of the workflow like any other.
3. **The collect is its own step**, and the action is called
   `collect` — not `join`; we are collecting the answers.
4. **The code and the canvas are 1:1.** Every step in the document is a
   node on the canvas; every node on the canvas is a step in the
   document. (Rev 1 had four steps in the code and five nodes on the
   canvas. That inconsistency was the tell that its model was wrong.)
5. Distribution has two patterns — **parallel** and **sequential**.
   v1 ships parallel; the word `distribute` covers both.

## 1. The model: distribute opens a per-item region, collect closes it

`distribute` splits the track: everything after it runs once per item,
until a `collect` step merges the track back to one. The steps between
are ordinary steps — calls, decisions, loop-backs — they just run per
instance, with the distributed binding in scope:

```awl
step wave
  distribute item in state.items

step develop            // runs once per item
  run_agent(…item…) -> note

step review             // runs once per item
  run_agent(…) -> verdict
  outcome redo: when verdict.verdict == "reject", route develop
  outcome ok:   otherwise (fall through)
  max 3 visits

step gather
  collect verdict -> results
```

That is the operator's original description verbatim: "fanned out to
each of the dev steps, and each of those dev steps is followed by a
plan step, and each of those is followed by a run-checks step, and they
can loop back on themselves."

- `distribute <var> in <collection>` — the step's only line. Instances
  run in parallel (v1). `distribute sequential <var> in <collection>`
  is the ordered one-at-a-time pattern (named now, built when needed).
- `collect <binding> -> <name>` — the step's opening line; waits for
  every instance and gathers each instance's `<binding>` into a list
  (`[T]`). The step may route or continue like any step. All instances
  must succeed (today's engine semantics); letting a failed instance
  arrive as data instead is a later modifier on `collect`, one line,
  deferred until wanted.
- Checker: every `distribute` reaches exactly one `collect`
  downstream; loop-backs inside the region stay inside the region;
  no route may leave the region except through its `collect`.

## 2. Subflow — a named container, used as a step

A `subflow` is declared like a workflow — typed inputs, one typed
outcome, its own steps with decisions and bounded loop-backs — and
lives in the same document. Using it is a step:

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

- One canvas node per use; expandable to show the subflow's own step
  graph. Collapsed = one box.
- Compiles inline: no separate deploy, no engine object. Subflows
  nest, and may contain distribute/collect regions of their own.
- v1: exactly one success outcome type per subflow — that is the type
  the invocation binds.
- Inside a distribute region, a subflow step runs per instance like
  any other step — that is the common wave shape: distribute → subflow
  step → collect.

## 3. Everything already right stays

- **Decisions**: `outcome … when … route` — drawn as diamonds; a
  body-less step with only outcomes is a pure decision node.
- **Loops**: backward `route` to an earlier step (implemented). Add
  `max N visits` as the step-level cycle bound (also closes the
  checker soundness gap where a decoy `max 1` loop satisfies today's
  cycle-bound rule) and `visits` readable in outcome guards.
- **Waits**: `wait signal`, `sleep` — unchanged.
- **`fork` leaves the language.** In English a fork is a decision.
  AWL's decisions already have their surface; fan-out is `distribute`.
  Intra-step `fork`/`join` parse with a deprecation diagnostic through
  a migration window, then go. Bare named-branch `fork` renames to
  `branch` when promoted; out of scope here.

## 4. Authoring ergonomics (rides alongside)

- **`const`** — top-level named literals (prompts, schemas):
  `const dev_instructions = """…"""`. Also fixes the parser wart where
  a statement cannot start with a string literal.
- **Raw strings** — triple-quoted `"""…"""`: newlines literal, no
  backslash escaping; JSON pastes in as JSON.
- **`schema of Type`** — compile-time expression yielding the type's
  JSON Schema as a `String` (the toolchain already derives it; this
  makes it reachable inside a document).

## 5. The worked example — dev_flow

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
  distribute item in state.items

step build
  dev_item(item: item, notes_dir: notes_dir) -> verdict

step gather
  collect verdict -> results

step fold
  run_agent(…resume coordinator…) -> state
  outcome next:   when state.items is present, route wave
  outcome finish: otherwise, route done(value: state.summary)
  max 3 visits
```

Five steps in the code. Five nodes on the canvas.

## 6. What the canvas draws

```
   ┌────────┐
   │  plan  │
   └───┬────┘
       ▼
   ┌──────────┐
   │ wave   ⫴ │  distribute item in state.items
   └───┬──────┘
       ▼  ×N (one track per item)
   ┌──────────────┐
   │ build     ×N │  dev_item — expandable subflow
   └───┬──────────┘
       ▼
   ┌──────────┐
   │ gather ⫵ │  collect verdict -> results
   └───┬──────┘
       ▼
   ┌────────┐
┌─▶│  fold  │◇── done ──▶ (success)
│  └───┬────┘
└──────┘ more, ×3
```

The multi-step-region shape (no subflow) draws the same way — the
per-item steps simply appear in sequence between the ⫴ and ⫵ nodes,
each marked ×N, loop-backs included.

The projection is served from the **parsed** document, so this picture
lands as soon as parser + checker + projection understand the forms —
before emitter work. Canvas relief ships first.

## 7. Lowering and compatibility

- No engine change. Distribute/collect regions lower to the existing
  fan-out machinery (direct-compiles since the fork-generality work);
  subflows lower to inline functions with bounded recursion
  (continuation nesting — proven emitter technique); step cycles
  already lower. The deferred failed-instance-as-data collect modifier
  is the only future item that may touch the engine.
- Existing documents keep compiling through the deprecation window.
  staged_rounds / dev_brief migrate as the proof corpus.
- Workers untouched. `run_agent` and the general worker are unchanged.

## 8. Build sequence

1. **Ratify this brief** (operator tear → fold → ratify).
2. **Ergonomics batch**: `const`, raw strings, `schema of`,
   literal-statement parser fix.
3. **Grammar + checker + projection** for `subflow`, `distribute`,
   `collect`, `max N visits`, decision-node tagging → the canvas draws
   the real shape (check-only; emitter untouched).
4. **Emitter lowering** for subflows + distribute/collect →
   direct-compile parity.
5. Deprecation window closes: `fork` retired; `branch` promotion and
   the collect failure-tolerance modifier when wanted.
