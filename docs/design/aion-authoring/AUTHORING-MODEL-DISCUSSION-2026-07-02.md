---
type: discussion
cluster: aion-authoring
title: Authoring model — architecture discussion & open questions (2026-07-02)
status: OPEN — converging; see Addendum (types-first verified feasible, top-down scaffolding + file inputs settled, syntax flavour = the main remaining call)
related:
  - ./DESIGN.md                              # the aion-authoring cluster design (ADR-014)
  - ./COMPETITIVE-RESEARCH-2026-07-02.md     # 24-engine landscape + Aion proposal
  - ./competitive-research-raw-2026-07-02.json
---

# Authoring model — architecture discussion & open questions

> A working capture of a long design conversation about **the process of authoring a
> workflow + its actions**. Nothing here is decided; it records where the thinking got
> to and the forks still open, so we can resume without re-deriving. Scope was
> deliberately narrowed (Tom): *only* the authoring of a workflow and its actions —
> not versioning, not the visual editor internals, not the NOI proof.

## The pain we're attacking

Authoring a workflow on Aion today is heavier than it should be:
- **The "run ceremony":** every workflow hand-writes an identical ~27-line
  `run(raw_input: Dynamic)` that decodes the engine's raw payload → runs the input
  codec → calls the typed body → re-encodes output/error. Pure glue, copied everywhere.
- **The codec tax:** Gleam has no derive/macros, so every boundary type needs a
  hand-written encoder + decoder. A real workflow carries thousands of lines of codecs.
- **The same shape described 3×:** a Gleam type + a `schemas/*.json` + a Rust/Python
  worker struct.
- **A separate worker:** the activity's real body lives in a separate binary/language
  from the workflow that calls it.

Tom's north star for the fix: *"a workflow should basically be a single file that
controls the order of operations, and the worker/actions are generated from — or
referenced by — that."* And a strong secondary concern: **Gleam is young/unfamiliar**,
so the way you *order* a workflow should not require fluency in it.

## What the architecture actually allows (verified against source, 2026-07-02)

Two read-only probes over the engine + SDK established what is ESSENTIAL vs INCIDENTAL:

- **"Why isn't a workflow just a `main` function?"** — Because the *engine* calls it
  (like a Lambda handler / HTTP route — you never write `main` for those), and it
  **re-runs it from the top on every crash-recovery** (replay) to fast-forward to where
  it was. That "the engine invokes and replays it" is ESSENTIAL. But the author seeing
  `run(raw_input: Dynamic)` and hand-writing JSON decode/encode is **INCIDENTAL** — the
  code itself (`crates/aion-package/src/structure/facts.rs:6-9`) calls `run` "the
  engine-facing adapter, while the harness drives the typed `execute`." A toolchain that
  *generates* `run` as a hidden wrapper around a typed `execute(input)` fully satisfies
  the engine. **The author should never see `run` or `Dynamic`.**

- **Actions are NOT boilerplate — they're the side-effect boundaries.** The workflow is
  replayed, so it must not itself touch the world (or the side effect fires again on
  every replay). Anything that touches reality — an API, an LLM, the clock, randomness —
  is pushed into an **action** that runs **once** and whose result is **recorded**;
  replay hands back the recorded result. So: **pure logic/sequencing lives in the
  workflow and is durable-by-replay; anything with a side effect must be an action.**
  That split is the one load-bearing idea in durable execution; it is not removable.

- **The one-file / worker-generated model is ~80% already built.** Aion has an activity
  **tier** concept (`InVm | RemotePython | RemoteRust`, `gleam/aion_flow/src/aion/activity.gleam:48-57`).
  An **in-VM execution primitive exists** (`crates/aion/src/activity/dispatch.rs` — runs
  an activity as a linked child BEAM process on the dirty scheduler, typed result/error
  propagation). The `Activity` value **already carries the author's Gleam body** (the
  `runner` field), and codegen **already treats InVm as worker-less**
  (`activity_project.rs` test `in_vm_tier_emits_neither_worker_nor_golden`). The **single
  missing wire:** the dispatch seam (`crates/aion/src/runtime/nif_activity_dispatch.rs`)
  UNCONDITIONALLY routes every activity to a remote worker; `tier` never crosses it. Make
  that seam tier-aware — "if InVm, spawn the author's Gleam body as a BEAM child; else
  ship to a worker" — and the common case has **no separate worker at all**. Remote
  Rust/Python then becomes the opt-in escape hatch (native speed / a Python library).

- **`aion generate` already exists** and even *runs the author's Gleam* (`gleam run` on a
  probe) to extract the activity manifest. On the generated path (order-saga) an activity
  already collapses to ~2 hand-authored things (a `declare` line + the body). BUT: it is
  still **schema-first** (`schemas/*.json` is the authored source; the Gleam types are
  generated FROM it) — which contradicts ADR-014's stated "types are the source of truth."
  And the `aion new agent` scaffold is a **static template not wired to the generator at
  all**, so a NEW user gets the fully-hand-written path — the worst inconsistency.

## Where the direction landed

Tom is **happy to keep Gleam as the (full-fat) authoring language for now**, but wants
the architecture to support more approachable surfaces later. The shape we converged on:

### The "surface vs model" decision (the crux)
Multiple authoring surfaces (Gleam / a simple DSL / a visual editor) only work if the
source of truth is an **underlying canonical model**, not one of the surfaces. If a
*surface* (e.g. Gleam) is canonical, every other surface must be reverse-engineered from
it — lossy the moment it contains something the others can't express (the BPMN/n8n
"git-diff hell" trap). If a **canonical workflow model** (the typed graph of steps +
typed edges + control flow) is the truth, then Gleam, the DSL, and the canvas are all
**lossless views** of it. You never sync surface↔surface; each surface is a two-way
translator to the model: `visual ↔ model ↔ DSL ↔ model ↔ Gleam`.

### How that reconciles with "types as the single source of truth"
- **Types live on the ACTIONS.** Each action (any language) declares its input/output
  type, once, where it's implemented. That registry of typed action contracts is the
  types-source-of-truth. **No `schemas/*.json`, no re-declaration in the workflow.**
- **Orchestration lives in the MODEL.** It references actions by name and wires them; it
  is type-checked *against the action contracts*, so whichever surface authored the order
  (Gleam / DSL / canvas), the wires are checked. Type-safety without the author needing
  Gleam fluency, because the safety comes from the action contracts, not the syntax.

### On "our own workflow language on BEAM"
Tom clarified he did NOT mean a general-purpose language — just "the things we need, with
more approachable/familiar syntax (YAML/JSON/Nix-flavoured)." Verdict:
- A **full general-purpose language: no** — multi-year build, and a bespoke language is
  *more* unfamiliar than Gleam (works against the adoption goal).
- A **small, declarative, determinism-safe orchestration DSL: yes, promising** — because
  it only expresses the tiny orchestration vocabulary (sequence / branch / parallel /
  fan-out / wait / timer / child), it's tractable, AND it can make a **non-deterministic
  workflow literally unrepresentable** (no clock/random/IO in the language; the only verb
  that touches the world is "call an action"). That kills the #1 industry pain point (the
  determinism footgun) — a real differentiator. Owning beamr gives the substrate to run
  it natively.

### The DSL sketch (fan-out / fan-in agent example)
Illustrative syntax only — the syntax flavour is still open:
```
workflow: research_report
input:   Brief
output:  Report

do:
  questions = plan(input)                    # split the brief → a list of questions

  findings =
    fan out over questions as q:             # one agent per question — parallel, across
      investigate(q)                         #   workers, collected exactly-once on failover

  draft = synthesize(findings)               # combine findings into a report

  approval = wait for review                 # durable human gate — free while idle, survives restarts

  unless approval.ok:
    draft = synthesize(findings, approval.notes)

  give draft
```
Properties: data flows top-to-bottom (name a step only when a later line reaches back);
**no type declarations except the boundary**; **no schema files**; fully type-checked
because `investigate : Question -> Finding` etc. are known from the action contracts;
non-determinism unrepresentable. Types live once beside the actions:
```gleam
pub fn investigate(q: Question) -> Finding      // the norn agent action, typed once
pub type Finding { Finding(question: Question, summary: String, sources: List(String)) }
// wire codec is GENERATED from the type — never hand-written, never a schemas/*.json
```

## Open questions (resume here)

1. **Tom is "still not totally convinced."** The DSL/surfaces story hasn't fully landed.
   Likely needs: a fuller, more convincing worked example (more patterns — fan-out +
   branch + await + error/compensation), and a crisper answer on the type story feeling
   truly zero-boilerplate.
2. **The big architectural fork:** ship the **fast cut** (Gleam-canonical: kill the run
   ceremony + types-first codecs + in-VM actions — weeks) vs commit to the
   **canonical-model architecture** (model-in-the-middle enabling DSL + visual + true
   two-way sync — a couple of quarters). They're compatible if the model starts as a
   *view derived from Gleam* (statically extract the graph — already planned as WA-005)
   and only later becomes the source.
3. **DSL syntax flavour:** YAML-ish (as sketched) vs more JSON/Nix-like. Cosmetic over the
   same machinery, but affects approachability.
4. **Implicit vs explicit data flow** in the DSL (implicit-with-explicit-when-needed, as
   sketched, vs always-show-the-wiring for newcomer clarity).
5. **Boundary type declaration:** keep `input:/output:` on the workflow as its public
   contract (as sketched) vs infer from first/last step.
6. **Resolve the ADR-014 contradiction:** retarget codegen to read Gleam types (the real
   "declare once", needs a proper Gleam type reader — `facts.rs` is only a tokeniser,
   likely multi-quarter) vs amend ADR-014 to bless schema-first-as-single-source (the
   pragmatic 90%).

## Concrete near-term changes this implies (independent of the big fork)
These are safe, additive, and unblock everything else:
- **Generate `run` invisibly** — add an SDK entrypoint (`workflow.entrypoint(definition)`)
  so the author writes only the typed body; the `run(Dynamic)` shim becomes one generated
  line. No engine change.
- **Wire the in-VM tier into the dispatch seam** — the key unlock so the common-case
  workflow needs no separate worker; the primitive + stored body already exist.
- **Put the `aion new agent` scaffold on the generated path** — today it's a static
  template, so new users get the worst experience.

## Addendum — later session, 2026-07-02 (post-compaction continuation)

The conversation resumed and moved several open items. Newly established:

1. **The "multi-quarter Gleam type reader" objection is dead.** `gleam export
   package-interface` (verified against `examples/order-saga`) emits machine-readable
   JSON with every public function's parameter/return types AND every type's
   constructors with field names/types. The compiler does the type-reading; types-first
   codegen needs no hand-written parser. This resolves the ADR-014 contradiction in
   favour of **types-first**; `schemas/*.json` becomes a *generated* artifact (emitted
   for external reference — agents, HTTP clients, docs), never authored.
2. **"The `.aion` file IS the canonical model."** Because the DSL is declarative and
   closed, parsing it yields the exact graph and printing the graph yields the file
   back. The visual editor is another *editor of the same file* (Windmill-style clean
   diffs) — no surface↔surface sync protocol. Gleam-authored workflows remain the
   full-power tier, view-only graph rendering later. This dissolves the "fast cut vs
   canonical-model (quarters)" fork from Open Question 2.
3. **Top-down scaffold-first authoring is a peer to code-first** (Tom's flow): sketch
   the workflow + shapes first, `aion check` reports not-implemented actions,
   `aion scaffold <action> [--rust --worker <name>]` generates typed stubs (in-VM Gleam
   default; Rust/Python worker structs + handler skeletons generated FROM the contract).
   Generation always flows contract → worker. Options later for add-to-existing-worker
   vs new worker, and saved scaffold templates.
4. **File/directory inputs** (kills the "cat JSON into the workflow" inelegance):
   structured inputs (`brief: Brief` ← `--input brief=./brief.json`) are parsed +
   validated by the CLI at start and embedded in history; bulk inputs (`File`/`Dir` ←
   `--input corpus=./sources/`) are snapshotted into the **haematite content-addressed
   store** and the workflow receives a durable hash handle — replay-safe (immutable),
   cross-node safe (fetch by hash), size-safe (history stores a hash, not megabytes).
   The workflow never does I/O; the boundary and the actions do.
5. **Syntax flavour exploration** → `syntax-sketches/` (new folder): the same workflow
   (fan-out + retry + durable gate + timeout + diamond revision + compensation) rendered
   in JS-lookalike, YAML, canonical JSON, Nix-flavour, Gleam/Elixir pipes — and, after
   Tom's decisive constraint, **F: a real-TypeScript subset**.
6. **The AI-authorship constraint (Tom) reset the syntax verdict.** The primary workflow
   authors will be AI agents; a bespoke lookalike syntax gives LLMs plausible-but-wrong
   priors ("we want the syntax, not the language"). TOML was evaluated and rejected
   (YAML's wounds + nesting hostility). **New lean — sketch F:** the workflow file IS
   TypeScript — parsed by a real TS parser, typechecked by `tsc` against a `.d.ts`
   generated from the action contracts, subset-enforced, **statically extracted, never
   executed** (no Node runtime; the engine still runs Gleam; determinism stays
   by-construction). Correct priors for AI authors, tsc diagnostics as the
   self-correction loop, free editor tooling. Precedents: AssemblyScript, Encore.ts.
   Second AI lane: the canonical JSON + schema means agents with structured output can
   author the graph directly (syntactically-invalid output impossible), rendered to the
   `.ts` surface for human review. Starlark noted as Python-shaped runner-up.

7. **Tom's aesthetic steer reset the verdict again → sketch G (step document).** He
   doesn't love TS as the surface ("we want the syntax, not the language" — meaning a
   *document*, not a program): define the types/schemas, then lay out the control flow
   as a described step sequence, "borderline almost like markdown." Three independent
   pulls toward the document shape (the TOML question, "the YAML thing comes across so
   cleanly", the markdown instinct). **Sketch G keeps flavour B's document shape and
   fixes its three wounds by designing the format:** (1) one tiny familiar expression
   grammar in designated fields (`do:`/`when:`/`finish:`), typechecked by `aion check`
   against the contracts; (2) `when:` + `as:` conditional REBINDING kills the switch
   contortion; (3) calls look like calls (bare names = references, quotes = strings).
   Real YAML carrier (standard parsers/editors/JSON Schema). For a *data* format the
   AI-drift risk mostly evaporates (schema validation / constrained decoding) — G is the
   safest AI-authoring surface of all flavours. `about:` prose per step is load-bearing:
   canvas labels, `aion doc` markdown render, and **live ops-console narration of a
   running workflow** (NOI fit). Fan-out bodies are one call (multi-step per item = a
   child workflow — keeps documents flat). F stays compatible as a later code-first
   VIEW via the canonical model ("multiple languages to work from"). Nobody ever edits
   the generated Gleam — compiler-output status.

Still open after the addendum: Tom's read of sketch G; the expression micro-grammar's
exact boundary; `on failure:` shape for multi-step compensation; `types:` inline vs
sibling file in the scaffold; step-name canonical form (spaces vs snake_case).

## Also captured (not this conversation, but adjacent)
`COMPETITIVE-RESEARCH-2026-07-02.md` — an 11-agent sweep over 24 engines
(Temporal/Restate/DBOS/Inngest/Trigger/Golem/Step-Functions/Camunda/Prefect/Dagster/
Airflow/n8n/Windmill/LangGraph/Mastra/Protobuf/tRPC/Encore/Effect-Schema/serde/…). Key
takeaways that back the above: the whole industry makes **types the source of truth and
derives codecs** (Gleam's hand-codec tax is a Gleam-specific outlier); **Encore.ts** is
the model for zero-codec in a no-macro language (build-time analyzer emits codecs +
schema); **Windmill** is the one system that made a visual workflow git-reviewable (plain
files, canonical form); **LangGraph Studio** is the agent-observability bar to beat; the
**determinism footgun** and **versioning of in-flight work** are the two universal wounds
nobody has solved.
