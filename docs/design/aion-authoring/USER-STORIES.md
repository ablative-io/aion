# Aion-Authoring — User Stories

## Human Developer — Authoring a new workflow family

**S1.** As a developer, I want to declare an activity once as a typed signature and a tier, so that I do not hand-mirror its name, codecs, worker handler, registration, and manifest entry across five to seven files that must agree byte-for-byte.

**S2.** As a developer, I want generated plumbing to be regenerable and drift-checked, so that a stale hand-edit is caught by the build instead of corrupting a wire shape silently.

**S3.** As a developer, I want generated activity configuration to contain only the policies I chose, so that I am never silently bound by a retry or timeout default I did not set.

## Human Developer — Iterating in the inner loop

**S4.** As a developer, I want my workflow to rebuild and hot-reload on save, so that I iterate in seconds instead of running a manual build-package-deploy cycle each time.

**S5.** As a developer, I want to trigger a run and watch its events stream live in a local UI, so that I can see exactly what happened without reading raw logs.

**S6.** As a developer, I want the local experience to use the real engine and event stream, so that what works locally works in production with no fidelity gap.

## AI Agent — Authoring a workflow through the running engine

**S7.** As an authoring agent, I want to submit Gleam source to the server and get type errors back inline, so that I can correct a workflow against the real type-checker without a local toolchain.

**S8.** As an authoring agent, I want a successful submission to package and hot-load automatically, so that authoring is one live loop against the running engine.

## Operator — Diagnosing a run

**S9.** As an operator, I want to scrub a run event-by-event and see its state and the recorded clock and random values at each step, so that I can understand exactly what the workflow did and why.

**S10.** As an operator, I want a non-determinism failure to point at the exact divergent command, so that the scariest class of durable-execution bug becomes a one-glance explanation instead of a guessing game.

## Stakeholder — Reading and shaping a workflow without writing Gleam

**S11.** As a non-coding stakeholder, I want to see a workflow as a diagram that stays in sync with the code and lights up as a run executes, so that I can follow and discuss the process without reading the source.

## Human Developer — Authoring a durable agent and proving determinism

**S12.** As a developer, I want to scaffold a durable agent loop with a human approval pause from prompts, schemas, and a gate, so that a new agentic family is configuration rather than bespoke code.

**S13.** As a developer, I want a static check that flags any non-deterministic call reachable from workflow code, so that I can prove a workflow is replay-safe in CI rather than discovering a desync in production.

**S14.** As a developer, I want a generated test skeleton and an input skeleton for each workflow, so that testing and triggering a workflow start from a valid scaffold instead of a blank file.
