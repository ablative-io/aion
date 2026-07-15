# AWL flow vocabulary — design brief (rev 0, for the operator's tear)

2026-07-15. Operator ruling that triggered this: the authoring canvas must
show fan-out / fan-in / loops as first-class flow structure, and the
language's control-flow vocabulary is wrong — "a fork is a split in the
road": in plain English **fork means decision**, yet AWL uses `fork` for
parallel fan-out (and for static parallel branches too — one keyword,
two concepts, neither of them a decision). The operator's benchmark: the
visual flow languages (Salesforce Flow, BPMN, n8n) whose element
vocabulary users already understand.

This brief fixes the vocabulary, adds the constructs a flow language
needs, and keeps one principle above all: **what you author is what the
canvas draws** — flow structure lives at step level, not buried inside
step bodies.

## 1. The concept inventory

Every visual flow language converges on the same small set. Where AWL
stands today:

| # | Concept | Plain name | BPMN / Salesforce | AWL today | Disposition |
|---|---------|-----------|-------------------|-----------|-------------|
| 1 | Do a unit of work | action | task / Action | `call(…) -> x` ✓ | keep |
| 2 | Then | sequence | arrow / connector | statements; step fall-through ✓ | keep |
| 3 | Decide (one path taken) | **decision** | XOR gateway / Decision | `outcome … when … route` ✓ | keep; draw as a diamond; a body-less step with only outcomes is a pure decision node |
| 4 | Do different things at once | **branch** | AND gateway | bare `fork` named branches (misnamed; direct-compile refused) | rename → `branch`; promote later |
| 5 | Do the same thing per item | **fan out** | multi-instance subprocess / Loop-over-collection | `fork item in list` (misnamed, intra-step, body = statement list only) | replace → fan-out **steps** over **flows** (§3) |
| 6 | Come back together | **join** | AND-join / Merge | `join -> x`, all-or-fail-fast only | keep word; add modes (§5) |
| 7 | Go around again | **loop** | loop marker / cycle | `loop … until … max` intra-step; backward `route` between steps ✓ (implemented) | prefer step cycles; add `max N visits` bound (§6) |
| 8 | A flow within a flow | **flow** (subflow) | subprocess / Subflow | only separately-deployed child workflows | ADD — the missing keystone (§2) |
| 9 | Wait for the world | wait | timer/message events | `wait signal`, `sleep` ✓ | keep |
| 10 | When it goes wrong | failure path | boundary error event / Fault path | `on failure` grammar ✓ (direct-compile pending) | unchanged here |

Three real gaps: **flows (8)**, **fan-out as a step over a flow (5)**,
**join modes (6)** — plus the vocabulary correction and the `visits`
bound. Everything else exists.

## 2. Flows — the keystone

The operator's shape: "send it out to seven agents in parallel, each one
follows a path of its own — dev step, then plan step, then run-checks
step, and they can loop back on themselves." A fanned-out branch is not
a statement list; it is a **flow**: steps, decisions, and bounded loops
of its own.

```awl
flow dev_item(item: WorkItem, notes_dir: String)
  outcome out: type ItemVerdict

  step develop
    run_agent(instructions: dev_instructions, prompt: "Item " + item.id + …) -> note

  step review
    run_agent(instructions: review_instructions, prompt: …) -> verdict
    outcome redo: when verdict.verdict == "reject", route develop
    outcome ok:   otherwise, route out(verdict)
    max 3 visits
```

- **One anatomy at every scale.** A `flow` is declared exactly like a
  `workflow`: typed inputs, typed outcomes, steps. A workflow is the
  deployable scale; a flow is the in-document scale; a child workflow is
  a flow that happens to be deployed separately. Nothing new to learn.
- Flows nest: a flow's step may itself fan out over another flow.
- Flows compile **inline** (no separate deploy, no engine object). The
  emitter already proves the needed techniques (continuation nesting,
  bounded recursion).
- On canvas: collapsed = one node; expanded = its own step graph inside
  the parent node (box-in-box, BPMN subprocess style).

## 3. Fan out — a step, not a statement

Fan-out is flow structure, so it is a **step kind**, visible at the top
level of the document and on the canvas:

```awl
step wave fans out item in state.items
  dev_item(item: item, notes_dir: notes_dir)
  join all -> results
```

- The body names ONE flow call (or one action call for the trivial
  case). Per item, one instance of that flow runs; instances run in
  parallel (`sequential` stays available as a modifier).
- **The join is the step's boundary.** `join … -> name` is the last line
  of a fan step; the collected results flow to whatever the step routes
  or falls through to. On canvas the join renders as an explicit
  fan-in bar — the operator's "when they join together, that should be
  a step" — and the next node receives it.
- `fork item in …` (intra-step) is superseded. Grammar keeps parsing it
  through a deprecation window; the canvas-first surface is fan steps.
  The keyword `fork` then leaves the language — in English a fork is a
  decision, and AWL's decisions already have the right surface
  (`when`/`otherwise` outcome routing).
- Bare `fork` (heterogeneous named branches) renames to `branch` when it
  is promoted to the direct path — same reasoning, right word.

## 4. Decisions — already right, now drawn right

A step whose body is empty (only `outcome` lines over existing bindings)
is a **pure decision node**. No new syntax; the projection tags it and
the canvas draws the diamond with one labeled edge per arm. Mixed steps
(work + outcomes) draw as a node with a trailing diamond.

## 5. Join modes

Today's join is wait-all + engine-owned fail-fast: one branch's terminal
failure fails the run. Waves of agents need a gentler mode:

- `join all -> results` — today's semantics. v1 default, unchanged.
- `join settled -> results` — wait for every branch; failures arrive as
  data. Result element type is the builtin parametric `Settled(T)`:
  `{ ok: Bool, value: T?, error: String }` (joins `List(T)` and `T?` as
  the third builtin parametric).
- `join first -> result` — race; named for completeness, not in v1.

Honesty note: `all` is pure compiler work. `settled` needs the branch
failure captured before engine fail-fast triggers — either an engine
option on the fan-out barrier or lowering through the completed
`on failure` path. It is the ONE item in this brief that may touch the
engine; it ships after `all`, not with it.

## 6. Loops — step cycles, honestly bounded

Backward `route` to an earlier step is already the implemented
"state machine" loop form. Two additions make it the primary form:

- `max N visits` — a step-level bound; the checker accepts a route
  cycle when at least one member step carries a visits bound (or an
  input-derived one). Closes a real soundness gap: today's rule ("some
  member contains a bounded `loop`") is satisfiable by a decoy
  `max 1` loop that bounds nothing.
- `visits` — a builtin `Int` readable in that step's outcome guards
  (`when verdict.overall == "reject" and visits < 3`).

Intra-step `loop` remains for tight value-threading iteration; flow
loops are step cycles.

## 7. Authoring ergonomics (rides alongside)

- **`const`** — top-level named literals (prompts, schemas, gate lists):
  `const dev_instructions = """…"""`. Also fixes the parser wart where a
  statement cannot START with a string literal.
- **Raw strings** — triple-quoted `"""…"""`: newlines literal, no escape
  backslashes; JSON pastes in as JSON.
- **`schema of Type`** — compile-time expression yielding the type's
  JSON Schema as a `String`. The toolchain already derives schemas from
  AWL types; this makes it reachable from inside a document. Hand-written
  escaped-JSON schemas disappear.

## 8. The worked example — dev_flow rewritten

```awl
workflow dev_flow
  input task: Task
  outcome done:   type Complete,   route success
  outcome failed: type Incomplete, route failure

const agent_schema = schema of AgentOut
const coordinator_instructions = """You are the coordinator…"""

flow dev_item(item: WorkItem, notes_dir: String)
  outcome out: type ItemVerdict
  step develop
    run_agent(…, prompt: "Item " + item.id + " — " + item.goal, …) -> note
  step review
    run_agent(…) -> verdict
    outcome redo: when verdict.verdict == "reject", route develop
    outcome ok:   otherwise, route out(verdict)
    max 3 visits

step plan
  run_agent(instructions: coordinator_instructions, …) -> state

step wave fans out item in state.items
  dev_item(item: item, notes_dir: notes_dir)
  join all -> results

step fold
  run_agent(…resume coordinator…) -> state
  outcome next:   when state.items is present, route wave
  outcome finish: otherwise, route done(value: state.summary)
  max 3 visits
```

## 9. What the canvas draws

Node vocabulary:

```
 ┌────────┐    ◇ when/otherwise     ═══╦═══  branch split
 │ step   │      decision diamond      ║     (later)
 └────────┘
 ╔═[⫴ fan out: item in xs]═╗   ──▶  sequence / route
 ║   (flow graph inside)   ║   ↩    cycle back-edge, ×N bound
 ╚═══════[join all]════════╝
```

dev_flow on the canvas:

```
            ┌───────────┐
            │   plan    │
            │ run_agent │
            └─────┬─────┘
                  ▼
 ╔═[⫴ fan out: item in state.items]══╗
 ║  ┌─────────┐      ┌──────────┐    ║
 ║  │ develop │ ───▶ │  review  │    ║
 ║  │run_agent│      │run_agent │    ║
 ║  └─────────┘      └───◇──────┘    ║
 ║       ▲       redo    │           ║
 ║       └───────────────┘  ×3       ║
 ╚═════════════[join all]════════════╝
                  ▼
            ┌───────────┐
      ┌───▶ │   fold    │
      │     │ run_agent │
      │     └────◇──────┘
      │ items    │      │ done
      │ left ×3  │      ▼
      └──────────┘   (success)
```

Projection note: the canvas is served from the parsed document, not the
compiled one — so the correct picture lands as soon as **parser +
checker + projection** understand these forms, before the emitter does.
Canvas value ships first.

## 10. Lowering and compatibility

- No engine change for everything except `join settled` (§5). Fan steps
  lower to the existing fan-out machinery (direct-compiles since the
  fork-generality work); flows lower to inline functions with bounded
  recursion; step cycles already lower.
- Existing documents keep compiling through the deprecation window
  (`fork`/intra-step forms parse with a deprecation diagnostic).
  staged_rounds / dev_brief / examples migrate as the proof corpus.
- Workers unchanged. `run_agent` and the general worker are untouched —
  this is all authoring surface.

## 11. Build sequence

1. **Ratify this brief** (operator tear → fold → ratify).
2. **Ergonomics batch**: `const`, raw strings, `schema of`, literal-
   statement parser fix. Small, independent, immediate relief.
3. **Grammar + checker + projection** for `flow`, fan steps,
   `join all`, `max N visits`, decision-node tagging → canvas draws the
   real shape (check-only; emitter untouched).
4. **Emitter lowering** for flows + fan steps → direct-compile parity.
5. **`join settled`** (engine-option or on-failure route), then
   `branch` promotion and `fork` retirement.
