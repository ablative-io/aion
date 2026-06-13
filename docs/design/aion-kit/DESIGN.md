---
type: design
cluster: aion-kit
title: aion_kit: the runtime-agnostic worker standard library
---

# aion_kit: the runtime-agnostic worker standard library

> **Cluster:** aion-kit

## Intention

Authoring a new aion workflow family should be writing prompts, schemas, and a gate — not re-deriving the data plumbing underneath every one. When this lands, the plumbing that brief_dev hand-rolled inside one example (text composition for prompts, JSON wrangling for reports and enrichment, and a way to carry an activity result a workflow never wants to open) is a small, well-tested package that ships with the worker, so the next family inherits all of it for free. aion_kit is a toolbox of cross-cutting primitives, deliberately lean: it carries only what genuinely applies across the board, and it stays out of the determinism boundary's way by being pure. The norn agent driver and every other runtime harness stay where they belong — in the worker layer that consumers own (ADR-011) — so the standard library couples to no particular agent runtime and ships no consumer-specific concern.

## Problem

brief_dev (the live-dogfooded v2 pipeline) hand-rolled three pieces of plumbing inside one example. First, prompt rendering: prompts.gleam grew composition primitives (a section/block joiner, a line joiner, a bulleted list, key-value lines) and per-stage projection functions over the brief — bespoke, untested as a reusable layer, and re-implemented from scratch by every future family. Second, report decoding and the enrichment merge: each stage report was decoded and merged into the brief document with hand-written merge_scout/merge_dev/merge_review helpers, again bespoke per family. Third, and most damaging: there was no way for a deterministic workflow to carry an activity result it does NOT want to inspect. So the full structured stage reports (a ~12KB scout report) were decoded inside the workflow process to render the next prompt. That decode re-runs on every replay, bloats the workflow heap, and — via a latent beamr heap-reservation bug fixed in 0.6.1 — crashed a real dogfood run with a silent heap-full right after scout (ADR-012). Every future agentic family would re-pay all three costs: re-write the templating, re-write the JSON wrangling, and re-hit the heavy-decode-in-workflow trap.

## Solution

A new Gleam package `aion_kit` (lives at gleam/aion_kit, sibling to gleam/aion_flow and gleam/aion_client) with three pure modules, plus one small opaque type added to the authoring SDK `aion_flow`. The package root aion_kit.gleam is declaration-only (matching the aion_flow/aion_client convention); the public surface is the three namespaced modules. `aion_kit/template` generalises the prompts.gleam primitives: join_blocks (blank-line-separated section joiner that drops empty sections), join_lines (newline joiner that drops empty lines), bulleted (one '- item' per line), kv (a 'key: value' line), and a named-template render that substitutes '{name}' placeholders from a binding list into a template string. These are exactly the composition shapes prompts.gleam already proved; lifting them lets a family compose prompt text without re-deriving the joiners, and render lets a static template carry its holes explicitly. `aion_kit/json` carries the data-wrangling brief_dev open-coded: project decodes a thin typed view from a JSON string and ignores every field the view does not name (the ADR-012 facts projection — peek's sibling for plain JSON); merge deep-merges two JSON values append-only and order-stable (the enrichment pattern — objects merge key-by-key with the right side winning leaf conflicts, arrays concatenate left-then-right, insertion order preserved); pluck reads a value at a path (a list of object keys) and returns it or a typed miss. The keystone is the opaque payload, split across the two packages by who needs which half. The TYPE a workflow holds and threads between activities WITHOUT decoding — `Sealed` — lives in `aion_flow` (at src/aion/payload.gleam), because workflow code imports it: a workflow receives a Sealed from one activity and hands it straight to the next as that activity's input, never opening it. Sealed is a thin wrapper over a content-type tag and the raw payload bytes (held as a String — one binary on the workflow heap, not a decoded nested structure), mirroring the type-erased Payload the engine already moves (invariant 1). The HELPERS live in `aion_kit/payload`, because they are worker-side primitives the activity bodies call: seal turns a typed value (via a json encoder) into a Sealed inside the activity that produced it; raw returns the Sealed's bytes so the next activity can decode them with its own typed decoder; peek decodes ONLY a thin facts view from a Sealed (the same project shape, specialised to Sealed) so a workflow CAN read the few fields it needs to route control flow (pass/fail, blocked, changed files, drift) without materialising the whole report. The module boundary is the design's central call: the type sits with the code that holds it (aion_flow, the workflow's vocabulary), the operations sit with the code that runs them (aion_kit, worker-side), and the line between 'hold and forward' (workflow) and 'open and decode' (activity) is exactly the determinism-boundary line ADR-012 draws. aion_kit depends on aion_flow for the Sealed type (KIT-001 establishes that dependency and the package skeleton; KIT-002 and KIT-003 are disjoint files atop it). Every module is pure and deterministic — no wall clock, no entropy, no IO — so the same functions are safe to call from workflow code (peek on a Sealed to route) or activity code (seal/raw/template/json to build prompts and wrangle reports), which is the whole reason the toolkit can straddle the determinism boundary (invariant 2). No new wire schemas are introduced by this cluster; any schema a future family adds for its own reports stays the family's, written in the codegen subset (ADR-007). brief_dev itself is NOT reshaped here — that is RM-023's thin-workflow reshape, which consumes this toolkit; this cluster ships the toolkit and proves it with its own tests.

## Principles

- **P1** — Across-the-board or out: a primitive earns its place in aion_kit only if it applies to families generally (templating, JSON wrangling, the opaque payload). Convenience for one family is not the test; runtime harnesses fail it (ADR-011).
- **P2** — Pure and deterministic, always: every aion_kit function and the Sealed accessors are total pure functions of their arguments — no wall clock, no entropy, no IO — so they are safe on either side of the determinism boundary (invariant 2).
- **P3** — Hold versus open is the boundary: a workflow holds a Sealed and forwards it without decoding; an activity opens it. peek is the only workflow-side read, and it materialises a thin facts view, never the whole structure (ADR-012).
- **P4** — No invented shapes or limits: functions impose no caps, no size limits, no default content-types or merge policies beyond what the caller passes; absent input is a typed result, never a guessed default (ADR-001).
- **P5** — Replace, don't shim: brief_dev's bespoke helpers are superseded by aion_kit when RM-023 reshapes it; aion_kit ships no compatibility layer for the old open-coded plumbing (ADR-002).

## Decisions

- ADR-011 — The standard library carries cross-cutting primitives only; runtime harnesses stay out — aion_kit (the worker standard library) carries only primitives that apply across the board — data transformation/wrangling, rendering/templating, and the opaque payload — plus further cross-cutting primitives as they prove general. Runtime harnesses, the norn agent driver chief among them, stay as worker-side code that consumers (Meridian) own and integrate; they may live in a worker for convenience but never ship as standard-library surface. Rejected: bundling the norn/agent harness into the standard library — it couples a general toolkit to one runtime and ships a Meridian concern to every consumer.
- ADR-012 — Workflow code stays thin: large activity results ride as opaque payloads — Workflow code stays thin. Large activity results ride between stages as opaque sealed payloads the workflow never opens; the workflow decodes only a small facts projection — the few fields it needs to route control flow (pass/fail, changed files, blocked, drift). Decoding and prompt rendering happen in the consuming activity on the worker, which has a full heap and runs the work once rather than on every replay. Rejected: decoding and rendering full reports in workflow code — it puts the determinism boundary's heavy lifting in the wrong place and was the regression that exposed the beamr crash.
- ADR-001 — No arbitrary limits, no assumed defaults — Configurable values come from the builder/author or are deferred to the layer that owns them (e.g. beamr's own defaults). No caps, rate limits, or hardcoded defaults invented at the aion layer. Values are discussed before implementation. Rejected: convenience defaults — they are decisions made for the user without telling them.
- ADR-002 — No backwards compatibility during the build — Replace, don't add alongside. No compat shims, no zombie code, no #[deprecated] markers. Breaking changes are made cleanly and consumers move forward. Rejected: incremental deprecation cycles — they double the surface under test for an audience of zero.
- ADR-007 — Design system v2: JSON ledgers above clusters, enrichment in place — Two project ledgers (roadmap.json, decisions.json) above the clusters; stage contracts as first-class schemas inside the aion codegen subset; the brief is one living document — the pipeline appends scout/dev/review per requirement and an execution block per brief, in place, never touching authored fields. Rejected: a sibling runs/ ledger — the brief as a single spec-plus-record document was the original intent, and aion's event history already provides the append-only audit trail.

## Goals

- The aion_kit package compiles and its full gleam test suite passes against the published aion_flow it depends on, with gleam format --check clean.
- A workflow can receive a Sealed from one activity and pass it as the input to the next activity without the Sealed type carrying or exposing a decoded view of its contents — proven by a test that seals a large value, forwards the raw bytes, and decodes them only on the far side.
- peek decodes a thin facts view from a Sealed and ignores every field the view does not name, proven against a fixture sealing a report far larger than the facts view.
- template's join_blocks, join_lines, bulleted, kv, and render reproduce the exact composition behaviour prompts.gleam relies on (empty-section dropping, empty-line dropping, placeholder substitution), proven by unit tests pinning representative outputs.
- json's project, merge, and pluck each have unit tests covering the documented behaviour including the negative cases (a field absent from a projection, a leaf conflict in merge, a missing path in pluck) returning typed results, never panics or guessed defaults.
- No module in aion_kit imports aion/workflow, aion/activity, aion/query, aion/signal, or performs any IO; the package carries no norn/agent-runtime code (ADR-011).

## Non-Goals

- Reshaping brief_dev to thread sealed payloads and move prompt rendering into the activities. — That is RM-023 (workflow-toolkit cluster, WT briefs); this cluster delivers the toolkit brief_dev will consume, and proves it with its own tests rather than by rewiring the example.
- Shipping the norn agent driver or any runtime harness as standard-library surface. — ADR-011 draws the line: harnesses stay in the worker layer consumers own; aion_kit carries cross-cutting primitives only.
- A schema-validation engine or codegen for arbitrary JSON schemas inside aion_kit. — Stage-contract codecs are generated by aion codegen from a family's own schemas (ADR-007); aion_kit's json module is value-level wrangling (project/merge/pluck), not a validator.
- A general expression or logic language inside template render. — render substitutes named placeholders only; conditionals and loops are ordinary Gleam in the calling projection (the prompts.gleam pattern), which keeps the primitive small and total.
- Streaming, chunking, or size-limiting large payloads inside Sealed. — Sealed holds the bytes as one opaque binary by design (the cheap-to-hold property); any size policy is the engine's, not a toolkit cap (ADR-001).

## Structure

| Path | Note | Brief |
|------|------|-------|
| `gleam/aion_kit/gleam.toml` | aion_kit package manifest: name, version, erlang target, deps (gleam_stdlib, gleam_json, aion_flow), dev-dep gleeunit | KIT-001 |
| `gleam/aion_kit/src/aion_kit.gleam` | Declaration-only package-root module (PackageRoot marker), matching the aion_flow/aion_client convention | KIT-001 |
| `gleam/aion_kit/src/aion_kit/payload.gleam` | Opaque payload helpers: seal (typed value -> Sealed), raw (Sealed -> bytes), peek (Sealed -> thin facts view); operates on aion_flow's Sealed type | KIT-001 |
| `gleam/aion_kit/src/aion_kit/template.gleam` | Text composition primitives (join_blocks, join_lines, bulleted, kv) and named-template render; pure, generalised from prompts.gleam | KIT-002 |
| `gleam/aion_kit/src/aion_kit/json.gleam` | JSON wrangling: project (thin typed view, ignoring the rest), merge (append-only deep, order-stable), pluck (path get); pure | KIT-003 |
| `gleam/aion_kit/test/payload_test.gleam` | seal/raw/peek round-trip and forward-without-decode tests | KIT-001 |
| `gleam/aion_kit/test/template_test.gleam` | join/bulleted/kv/render composition pins | KIT-002 |
| `gleam/aion_kit/test/json_test.gleam` | project/merge/pluck behaviour and negative-case tests | KIT-003 |
| `gleam/aion_flow/src/aion/payload.gleam` | The Sealed pass-through TYPE a workflow holds and threads without decoding (content-type tag + opaque bytes), plus its construction/accessor surface | KIT-001 |
| `gleam/aion_flow/test/payload_test.gleam` | Sealed construction/accessor and hold-without-decode tests on the aion_flow side | KIT-001 |

## Inventory

- `gleam/aion_flow/` — The typed authoring SDK (v0.4.0). Modules live under src/aion/ with the aion/ namespace; root aion_flow.gleam is declaration-only. No payload module exists yet; src/aion/codec.gleam holds the Codec(a) encode/decode pair and DecodeError used at the boundary.
- `gleam/aion_client/src/aion_client/payload.gleam` — An existing CALLER-side opaque Payload (content_type + bytes) for the client SDK. Distinct package, distinct concern: aion_flow's Sealed is the WORKFLOW-side pass-through type. Same shape family, deliberately not shared (ADR-002: no cross-package shim).
- `examples/stacked-dev/src/stacked_dev/prompts.gleam` — Live prompt projections with the join_blocks/join_lines/bulleted primitives and files_line/reference_line key-value lines that aion_kit/template generalises. Stays as-is until RM-023 reshapes the family to consume the toolkit.
- `examples/stacked-dev/src/stacked_dev/enrich.gleam` — Live hand-written merge_scout/merge_dev/merge_review/merge_execution helpers — the enrichment merge aion_kit/json.merge generalises. Stays until RM-023.
- `gleam/aion_kit/` — Does not exist; greenfield package created by this cluster.

## Constraints

- **CN1** — Runtime-agnostic: no module in aion_kit or the aion_flow payload module imports or references the norn agent driver, any agent/process-spawning runtime, or engine-runtime internals; grep for 'norn' and for process/port spawning in the package finds nothing (ADR-011).
- **CN2** — Pure and deterministic: every public function in aion_kit/template, aion_kit/json, aion_kit/payload, and every aion_flow Sealed accessor reads no wall clock, draws no entropy, and performs no IO — they are total functions of their arguments and safe to call from workflow code or activity code (invariant 2).
- **CN3** — Hold-without-decode: the Sealed type exposes no API that returns a decoded structured view of its contents; the only reads are raw (the opaque bytes) and peek (a caller-supplied thin decoder). Holding a Sealed on the workflow heap costs one binary, never a decoded nested structure (ADR-012).
- **CN4** — No imported wire schemas and no $ref/$defs/oneOf/anyOf/default in anything this cluster introduces; aion_kit ships no JSON schema files, and the json module is value-level wrangling, not schema validation (ADR-007).
- **CN5** — No arbitrary defaults or limits: no module supplies a default content-type, a default merge policy, a size or depth cap, or any hardcoded bound; absent input (a missing projection field, a missing pluck path, a decode failure) is returned as a typed result the caller handles (ADR-001).
- **CN6** — No backwards-compatibility shims: aion_kit does not re-export, wrap, or alias brief_dev's bespoke prompts.gleam/enrich.gleam helpers or aion_client's Payload; it is the replacement those call sites move to under RM-023, not a parallel path (ADR-002).
- **CN7** — Each source file stays at or under 500 code lines (excluding tests, comments, whitespace); no #[allow]-equivalent suppression, and no Gleam panic/assert/let-assert in library code — decode and lookup failures are returned as Result/Option values.
