# Aion-Authoring — Checklist

## Declare-once codegen (WA-001)

- [ ] **C1** — An activity is declared once as a typed signature plus a tier annotation (InVm | RemotePython | RemoteRust) in the activities module, and that declaration is the only per-activity artifact an author writes.
- [ ] **C2** — `aion generate` derives, from the typed declaration, the activity.new wrapper, the value-type codec pairs, the worker handler stub, the worker registration entry, and the workflow.toml activities entry.
- [ ] **C3** — `aion generate` derives the wire-compat golden for each remote activity from the value-type codecs, so the worker/SDK wire shapes are pinned without a hand-derived literal.
- [ ] **C4** — `aion generate --check` regenerates every generated file in memory and exits non-zero if any on-disk generated file differs, so a hand-edit to generated output is a build failure.
- [ ] **C5** — Generated activity configuration carries no invented defaults: a retry policy, timeout, or backoff appears in generated code only when the author declared it, otherwise the required hole remains required (ADR-001).
- [ ] **C6** — Deleting every generated file for a worked example (e.g. order-saga) and running `aion generate` reproduces them byte-identical, proven by a round-trip test.

## The instant loop — aion dev (WA-002)

- [ ] **C7** — `aion dev` watches a package and on save rebuilds, repackages, and hot-reloads the new content-hash version without restarting the engine.
- [ ] **C8** — The dev server triggers a workflow run and streams that run's events live over the existing WebSocket event stream.
- [ ] **C9** — The dev server replays a failed run and lets the author mock a named activity's result on an opt-in basis for a given run.
- [ ] **C10** — The dev loop runs the real engine, store, and event stream — there is no mock-only execution path whose semantics diverge from production (CN4).
- [ ] **C11** — An end-to-end test edits a workflow under `aion dev` and observes the new version serve a fresh run with no engine restart.

## Server-as-compiler (WA-003)

- [ ] **C12** — The aion-toolchain crate compiles and type-checks Gleam workflow source by shelling out to the gleam binary and packages a .aion on success, embedding no compiler.
- [ ] **C13** — aion-server exposes authoring endpoints, gated on --gleam-path, that accept Gleam source and return the gleam type error inline on a compile failure.
- [ ] **C14** — On a successful compile the server packages via aion-toolchain and hot-loads the new version, making authoring a live loop against the running engine.
- [ ] **C15** — Without --gleam-path the server deploys pre-built .aion files only and carries no compiler dependency (CN7).

## The lens — time-travel debugger (WA-004)

- [ ] **C16** — A run's history is navigable event-by-event through the `aion inspect` surface, reading the existing event store with no second debug log (CN5).
- [ ] **C17** — At each event the debugger shows the workflow-visible state projection and the recorded now() and random() values for that step.
- [ ] **C18** — On a NonDeterminismError the debugger surfaces the exact divergent command (expected vs found at the sequence) the resolver already computes.
- [ ] **C19** — The `aion inspect` surface offers a what-if re-run from a chosen event with a mocked outcome via the existing aion/testing replay path.
- [ ] **C20** — The per-event state projection is exposed by the engine from history and replay, not maintained as a parallel mutable store.

## The canvas — bidirectional visual projection (WA-005)

- [ ] **C21** — A workflow's primitive structure (run / spawn / receive / all / race / sleep and control flow) is extracted from the package as a graph model automatically.
- [ ] **C22** — The graph model identifies each node by its correlation key (activity sequence, signal name, timer id, child ordinal) so a consumer can map a run's recorded events onto it.
- [ ] **C23** — Extracting an example's structure and diffing the node/edge set against the workflow's known structure matches, proving the graph model is derived from the source rather than hand-drawn.
- [ ] **C24** — A bounded structural delta regenerates Gleam that still type-checks; the graph model is never the authoritative artifact (CN6).

## Agentic authoring and the determinism gate (WA-006, WA-007)

- [ ] **C25** — `aion new agent` scaffolds a durable agent loop (scout -> act -> verify -> signal-gated review) parameterised by prompts + schemas + gate, generalising the stacked-dev shape.
- [ ] **C26** — The agent scaffold compiles and runs a trivial agent end-to-end, including a human-in-the-loop signal wait with a timeout.
- [ ] **C27** — The agent scaffold's human approval pause is a workflow.receive with a timeout, not a bespoke polling mechanism.
- [ ] **C28** — `aion check --deterministic` statically flags any wall-clock or entropy call reachable from workflow code and passes a clean workflow, proven by a positive and a negative fixture.
- [ ] **C29** — `aion generate` emits an aion/testing skeleton per workflow: each activity pre-mocked, a clock advance per timer, and a replay-determinism assertion.
- [ ] **C30** — `aion input <workflow_type>` emits a valid input skeleton derived from the workflow's input type, so an input document is never hand-written from scratch.
