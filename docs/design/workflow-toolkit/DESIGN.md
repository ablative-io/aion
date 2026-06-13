---
type: design
cluster: workflow-toolkit
title: Workflow Toolkit: the thin-workflow reshape and the reusable dev-pipeline plumbing
---

# Workflow Toolkit: the thin-workflow reshape and the reusable dev-pipeline plumbing

> **Cluster:** workflow-toolkit

## Intention

Authoring the next agentic family should cost a day, not a brief-set. When this is done, brief_dev is the proof of a pattern rather than a pile of bespoke code: the deterministic workflow process holds only the few thin facts it routes on and threads everything else as sealed payloads it never opens, the worktree/build/check/land harness is a module a new family imports instead of re-deriving, and the scout→dev→verify→review→harden skeleton is parameterised by (prompts, schemas, gate commands) so a new family is configuration plus its own prompts and schemas. The dogfood stays the dev process: aion still develops aion through this pipeline, but the pipeline is lighter, replay is cheap, and the class of crash that killed a real run is designed out.

## Problem

brief_dev decodes full structured stage reports (a 12KB scout report, full dev and review reports) and renders every stage prompt inside the deterministic workflow process. Workflow code is re-executed from the start on every replay (engine invariant 2), so that heavy decode-and-render runs again on every recovery and bloats the workflow heap — and via a latent beamr put_list/put_tuple2 heap-reservation bug (fixed in beamr 0.6.1) it crashed a real dogfood run with a silent heap-full right after scout (ADR-012's context). The decode/render belongs on the worker, which has a full heap and runs the work once. Separately, the scout→dev→verify→review→harden control skeleton and the provision-worktree / warm-build / scoped-checks / full-checks / land-via-yg harness are bespoke inside examples/stacked-dev: a second agentic family would re-author all of it from scratch. RM-023 names this — the next family needs the plumbing packaged, not re-derived; without the reshape every family also re-imports the replay-cost regression.

## Solution

Three serial reshapes of examples/stacked-dev, all building on aion_kit (RM-022). WT-001 makes the workflow thin (ADR-012): each agentic activity (scout, dev, dev_resume, dev_review) returns a small FACTS projection — exactly the fields the workflow routes control flow on — PLUS the full stage report sealed as an opaque aion_flow pass-through payload the workflow holds but never decodes. The facts are the negative space of the current in-workflow decode: scout produces no routing fact beyond success; dev produces the blocked R# ids and the deduplicated changed-file paths; dev_resume the same; dev_review the drifted R# ids (with issues) and a has-fixes flag. Prompt rendering moves OUT of brief_dev into the activity bodies on the worker, built from aion_kit's template primitive over the brief document, the resolved context, and the (now sealed) prior-stage payloads the activity decodes itself with aion_kit payload.raw + json. brief_dev stops importing prompts and stops decoding reports: it threads sealed payloads between stages and reads only facts. The enrich_brief activity, which legitimately needs the full reports, decodes the sealed payloads on the worker (it already runs worker-side). The outer stacked_dev arc is byte-immutable (CN1): provision/gate/review/land, the StartupTask/StartupResult envelope, the single review_verdict signal, and the child workflow type/spawn shape do not change; only the inner brief_dev data flow is reshaped, and the BriefDevResult that crosses the parent boundary keeps carrying what the outer arc needs (the sealed reports for enrich_brief, the facts the outer arc's DevResult derivation reads). After WT-001 the TK-002 dogfood is re-run through real norn end to end — the lighter pipeline must still land a brief (a verification requirement, not just a goal). WT-002 lifts the worker-harness — provision-worktree, warm-build, scoped-checks, full-checks, land-via-yg — out of examples/stacked-dev/worker into a reusable module so a new family imports the harness rather than re-implementing the live-proven yg/cargo/git invocations; behaviour stays byte-identical (the live-proven argv, the loud-fallback scoping, the commit-then-merge-from-repo-root land). WT-003 lifts the control skeleton: the scout→dev→verify→review→harden loop, parameterised by a config record carrying the stage prompts (templates), the stage schemas, and the gate commands, so the pipeline body is shared and a new family supplies a config plus its prompts/schemas. The dev_pipeline CLI template (already shipped by BD-007) is updated in the same wave to scaffold the thin, harness-and-template-based shape so `aion new` produces the cheap pattern by default. The norn agent driver stays worker-side and is NOT lifted into aion_kit (ADR-011); the template parameterises the agent step rather than baking a runtime into the toolkit.

## Principles

- **P1** — The workflow holds facts, not reports: deterministic workflow code decodes only the small projection it routes on and carries everything else as a sealed payload it never opens (ADR-012).
- **P2** — Heavy work runs once on the worker, never per replay: decoding full reports and rendering prompts happen in activity bodies, which run with a full heap and whose results are recorded, not re-executed (engine invariant 2).
- **P3** — The facts projection is the negative space of the old in-workflow decode: every field the reshaped workflow reads must have been a field the pre-reshape workflow read off a full report — no new routing signal is invented, none is dropped.
- **P4** — Reshape the inner data flow only: the outer stacked-dev contracts are live-proven and immutable; a change that alters provision/gate/review/land, the startup envelope, the signal, or the child spawn shape is out of scope by construction (ADR-008, CN1).
- **P5** — Extraction preserves behaviour byte-for-byte: a lifted harness or skeleton runs the same CLIs with the same argv in the same order as the live-proven original; the move is provable by the dogfood, not just the type-checker.
- **P6** — Replace, don't keep both: the in-workflow rendering and report-decoding paths are deleted, not left alongside the worker-side ones (ADR-002).

## Decisions

- ADR-012 — Workflow code stays thin: large activity results ride as opaque payloads — Workflow code stays thin. Large activity results ride between stages as opaque sealed payloads the workflow never opens; the workflow decodes only a small facts projection — the few fields it needs to route control flow (pass/fail, changed files, blocked, drift). Decoding and prompt rendering happen in the consuming activity on the worker, which has a full heap and runs the work once rather than on every replay. Rejected: decoding and rendering full reports in workflow code — it puts the determinism boundary's heavy lifting in the wrong place and was the regression that exposed the beamr crash.
- ADR-008 — brief_dev replaces onatopp_dev inside the stacked-dev family — Evolve in place: onatopp_dev.gleam is deleted and brief_dev.gleam takes its slot as stacked_dev's inner child; the outer arc keeps its live-proven contracts. Rejected: a parallel brief-dev family — two families serving one purpose is the zombie-code pattern ADR-002 prohibits, and the outer arc's provision/gate/review/land contracts took a full dogfood night to prove against real CLIs; duplicating them duplicates that risk.
- ADR-002 — No backwards compatibility during the build — Replace, don't add alongside. No compat shims, no zombie code, no #[deprecated] markers. Breaking changes are made cleanly and consumers move forward. Rejected: incremental deprecation cycles — they double the surface under test for an audience of zero.
- ADR-011 — The standard library carries cross-cutting primitives only; runtime harnesses stay out — aion_kit (the worker standard library) carries only primitives that apply across the board — data transformation/wrangling, rendering/templating, and the opaque payload — plus further cross-cutting primitives as they prove general. Runtime harnesses, the norn agent driver chief among them, stay as worker-side code that consumers (Meridian) own and integrate; they may live in a worker for convenience but never ship as standard-library surface. Rejected: bundling the norn/agent harness into the standard library — it couples a general toolkit to one runtime and ships a Meridian concern to every consumer.

## Goals

- brief_dev.gleam no longer imports stacked_dev/prompts and never decodes a full ScoutReport, DevReport, or ReviewReport in workflow code — the only stage data it reads is the typed facts projection, verified by grep and by the type signatures of the activities it dispatches.
- Each agentic activity (scout, dev, dev_resume, dev_review) returns a (facts, sealed-payload) result; prompt rendering for every stage happens in the activity body on the worker, built via aion_kit's template primitive, and the enrich_brief activity decodes the sealed reports worker-side.
- The TK-002 dogfood runs end to end through real norn against the aion repo after WT-001: dispatched, scouted, developed, verified, reviewed, hardened, and landed on main with the enriched brief riding the merge, with no workflow-process heap exhaustion.
- The worker-harness (provision-worktree, warm-build, scoped-checks, full-checks, land-via-yg) lives in one reusable module a new family imports, and the lifted invocations are byte-identical in argv and order to the live-proven originals (wire-compat and the dogfood prove it).
- The dev-pipeline control skeleton (scout→dev→verify→review→harden) is a shared, config-parameterised body so a new agentic family is its config record plus its prompts and schemas, demonstrated by the dev_pipeline template scaffolding the thin shape and its rendered-worker gates passing.
- The outer stacked_dev arc is byte-unchanged: provision/gate/review/land, the StartupTask/StartupResult envelope, the single review_verdict signal, and the brief_dev child spawn shape are identical pre- and post-cluster (diff is empty for those sites).

## Non-Goals

- Lifting the norn agent driver into aion_kit. — ADR-011 keeps runtime harnesses out of the standard library; the norn driver stays worker-side and the template parameterises the agent step instead of baking a runtime in.
- Changing any outer stacked_dev contract — provision/gate/review/land argv, the startup envelope, the review_verdict signal, or the child spawn shape. — CN1, ADR-008: the outer arc is live-proven; this cluster reshapes the inner data flow only.
- Parallel fan-out of WT briefs within the cluster. — All three WT briefs edit examples/stacked-dev (the workflow, the worker, the template), so they serialise; parallelism is across clusters, not within this one (RM-023 notes).
- Authoring the aion_kit package itself (the payload seal/raw/peek primitive, the template primitive, json/transform). — That is RM-022's cluster (KIT-001..003); this cluster consumes aion_kit as a dependency and is blocked on it landing.
- Reshaping the dispatch wave workflow or the assemble_wave dispatcher. — dispatch and assemble_wave read no full stage reports and render no prompts — they are not part of the thin-workflow regression; they stay as BD-006 landed them.
- Altering the stage-contract schemas or the design-system canon they mirror. — CN5: the scout/dev/review report shapes the agents emit are unchanged (ADR-012 quote — 'we're not changing the output of the agents'); only where they are decoded moves.

## Structure

| Path | Note | Brief |
|------|------|-------|
| `examples/stacked-dev/src/brief_dev.gleam` | Reshaped to ADR-012: threads sealed payloads, reads only the facts projection, no prompts import, no full-report decode | WT-001 |
| `examples/stacked-dev/src/stacked_dev/facts.gleam` | The thin facts projections (DevFacts, ReviewFacts, ScoutFacts) the workflow routes on, plus their codecs | WT-001 |
| `examples/stacked-dev/src/stacked_dev/types.gleam` | Activity result types reshaped to (facts, sealed payload); BriefDevResult reshaped to carry sealed reports + facts | WT-001 |
| `examples/stacked-dev/src/stacked_dev/activities.gleam` | scout/dev/dev_resume/dev_review reshaped to return (facts, sealed payload); their codecs change | WT-001 |
| `examples/stacked-dev/src/stacked_dev/locals.gleam` | Agentic locals render their own prompt (aion_kit template) and seal the full report; enrich_brief decodes sealed payloads | WT-001 |
| `examples/stacked-dev/src/stacked_dev/codecs_core.gleam` | Codecs for the facts types and the sealed-payload activity envelopes; report-envelope decode stays worker-side | WT-001 |
| `examples/stacked-dev/src/stacked_dev/codecs_workflows.gleam` | brief_dev IO codecs updated for the reshaped BriefDevResult (sealed reports + facts) | WT-001 |
| `examples/stacked-dev/src/stacked_dev/render.gleam` | Worker-side prompt rendering moved out of brief_dev: scout/dev/review/resume rendered from document+context+decoded prior payloads via aion_kit template | WT-001 |
| `examples/stacked-dev/src/stacked_dev.gleam` | Outer arc — touched only to consume the reshaped BriefDevResult facts/sealed-reports; provision/gate/review/land/signal/spawn byte-unchanged (CN1) | WT-001 |
| `examples/stacked-dev/schemas/brief_dev_output.json` | brief_dev output schema updated for the reshaped result (sealed reports + facts), codegen-subset clean | WT-001 |
| `examples/stacked-dev/src/aion_stacked_dev_io.gleam` | GENERATED by aion codegen — regenerated after the brief_dev_output.json reshape; not hand-edited | WT-001 |
| `examples/stacked-dev/worker/src/handlers.rs` | scout/dev/dev_resume/dev_review handlers render their prompt and return (facts, sealed payload), mirroring locals; enrich_brief decodes sealed reports | WT-001 |
| `examples/stacked-dev/worker/src/types.rs` | Serde facts types and sealed-payload envelopes join, mirroring the Gleam reshape | WT-001 |
| `examples/stacked-dev/worker/src/render.rs` | Worker-side prompt rendering for the agentic activities (the Rust mirror of render.gleam), built on the aion_kit template primitive | WT-001 |
| `examples/stacked-dev/worker/tests/wire_compat.rs` | Byte-compat pins for the facts types and sealed-payload envelopes, both directions against the Gleam codecs | WT-001 |
| `examples/stacked-dev/worker/tests/handlers_shims.rs` | Shim-based handler coverage updated for the render-and-seal handler shape | WT-001 |
| `examples/stacked-dev/gleam.toml` | Package manifest — gains the aion_kit dependency (payload + template) once RM-022 lands |  |
| `examples/stacked-dev/worker/Cargo.toml` | Worker manifest — gains the aion-kit Rust crate dependency |  |
| `examples/stacked-dev/test/aion_stacked_dev_test.gleam` | Hermetic pipeline suite updated for the facts/sealed-payload flow (stage outcomes unchanged in meaning) | WT-001 |
| `examples/stacked-dev/test/support/shims.gleam` | CLI shim builders updated to emit the report the worker seals; facts assertions added | WT-001 |
| `examples/stacked-dev/test/facts_codecs_test.gleam` | Round-trip and wire-pin tests for the facts codecs and sealed-payload envelopes | WT-001 |
| `examples/stacked-dev/test/render_test.gleam` | Worker-side render unit tests: budgets and content pinned against the seeded fixture, moved from the old prompts tests | WT-001 |
| `examples/stacked-dev/src/stacked_dev/prompts.gleam` | DELETED — in-workflow projection retired; rendering moved worker-side to render.gleam (ADR-002, ADR-012) | WT-001 |
| `examples/stacked-dev/test/prompts_render_test.gleam` | DELETED with prompts.gleam — assertions move to render_test.gleam | WT-001 |
| `examples/stacked-dev/test/prompts_scout_test.gleam` | DELETED with prompts.gleam | WT-001 |
| `examples/stacked-dev/test/prompts_dev_test.gleam` | DELETED with prompts.gleam | WT-001 |
| `examples/stacked-dev/test/prompts_review_test.gleam` | DELETED with prompts.gleam | WT-001 |
| `examples/stacked-dev/test/prompts_resume_test.gleam` | DELETED with prompts.gleam | WT-001 |
| `examples/stacked-dev/src/stacked_dev/harness.gleam` | The extracted worker-harness: provision-worktree, warm-build, scoped-checks, full-checks, land-via-yg as a reusable module a new family imports | WT-002 |
| `examples/stacked-dev/worker/src/harness.rs` | The Rust worker-side harness: the lifted yg/cargo/git invocations, byte-identical argv to the live-proven originals | WT-002 |
| `examples/stacked-dev/test/harness_test.gleam` | Harness unit/shim tests: each lifted CLI invocation's argv pinned identical to the pre-extraction value | WT-002 |
| `examples/stacked-dev/src/stacked_dev/pipeline.gleam` | The extracted scout→dev→verify→review→harden control skeleton, parameterised by a PipelineConfig (prompts, schemas, gate commands) | WT-003 |
| `examples/stacked-dev/src/stacked_dev/pipeline_config.gleam` | PipelineConfig type: stage prompt templates, stage schemas, gate command set, caps/backoff — the family's configuration surface | WT-003 |
| `examples/stacked-dev/test/pipeline_test.gleam` | Pipeline-skeleton tests: a config drives the same stage flow brief_dev had; a second config proves reuse without re-authoring the body | WT-003 |
| `crates/aion-cli/templates/dev_pipeline/` | CLI scaffold template (exists, BD-007) — updated in this cluster to scaffold the thin harness+pipeline shape so aion new produces the cheap pattern | WT-003 |

## Inventory

- `examples/stacked-dev/src/brief_dev.gleam` — 430 lines, live as of RM-017. Imports stacked_dev/prompts and calls scout_prompt/dev_prompt/review_prompt/resume_feedback in workflow code; decodes full ScoutReport/DevReport/ReviewReport and reads facts off them (blocked status, files_changed paths, drifted alignment, has_fixes). This in-workflow decode+render is the ADR-012 regression WT-001 reshapes.
- `examples/stacked-dev/src/stacked_dev/prompts.gleam` — 400 lines, pure projection functions (scout_prompt/dev_prompt/review_prompt/resume_feedback) over the brief document, resolved context, and explicit stage-report arguments. DELETED by WT-001; the rendering moves worker-side (render.gleam) and is generalised through aion_kit's template primitive.
- `examples/stacked-dev/src/stacked_dev/activities.gleam` — 240 lines. scout/dev_review/dev/dev_resume currently bind json_codec over the generated full stage-report pairs as their output codec. WT-001 reshapes these four to return the facts+sealed-payload result; the eight non-agentic activities are untouched.
- `examples/stacked-dev/src/stacked_dev/locals.gleam` — 704 lines. The agentic locals (scout/dev/dev_review/dev_resume) receive a rendered prompt string today and decode the full report from norn stdout (require_report). enrich_brief reads/decodes/writes the brief file. WT-001 moves rendering into these locals and has them return facts+sealed report; enrich_brief decodes the sealed payloads. The yg/cargo/git harness locals (provision/warm/scoped/full/land) are WT-002's to lift.
- `examples/stacked-dev/src/stacked_dev/codecs_core.gleam` — 394 lines. Holds report_envelope_decoder (the bare-or-{output} norn envelope seam), the startup envelope codecs, and the dev/scout/scoped/resume codecs. WT-001 adds the facts and sealed-payload codecs; the report-envelope decode stays worker-side.
- `examples/stacked-dev/src/stacked_dev.gleam` — 653 lines, the live-proven outer arc. Calls dev_result_of reading brief_dev_result.dev (a full DevReport) to derive the outer DevResult, and enrich_then_land passing the sealed reports to enrich_brief. WT-001 adjusts only those two consumption sites to the reshaped result; provision/gate/review/land/signal/spawn are byte-immutable (CN1).
- `examples/stacked-dev/worker/src/handlers.rs` — 698 lines. Handlers receive rendered prompt strings and decode reports from CLI stdout; enrich_brief merges reports. WT-001 moves prompt rendering into the agentic handlers and returns facts+sealed payload; WT-002 lifts the yg/cargo/git harness handlers into harness.rs.
- `examples/stacked-dev/worker/src/types.rs` — 956 lines of serde wire types, byte-compatible with the Gleam codecs, no aion_kit. WT-001 adds the facts/sealed-payload serde types.
- `examples/stacked-dev/gleam.toml` — Depends on aion_flow (local path) and stdlib/json only; NO aion_kit yet. The WT briefs add the aion_kit dependency once RM-022 lands (blocked_by).
- `examples/stacked-dev/worker/Cargo.toml` — Depends on aion-worker 0.6.0; no aion-kit. The Rust side of aion_kit (payload/template) is added once RM-022 lands.
- `crates/aion-cli/templates/dev_pipeline/` — 33-file scaffold shipped by BD-007 (C37), a minimal dev+gate pipeline mirroring the pre-reshape stacked-dev shape. WT-003 updates it to the thin harness+pipeline shape.
- `examples/stacked-dev/test/prompts_render_test.gleam` — Projection unit tests (render/scout/dev/review/resume) over prompts.gleam, deleted with that module in WT-001; their budget and content pins move to render_test.gleam.
- `examples/stacked-dev/src/dispatch.gleam` — 279 lines, the wave dispatcher. Reads no full stage reports and renders no prompts — out of scope; unchanged by this cluster.
- `examples/stacked-dev/src/gate.gleam` — 89 lines, the gate child. Out of scope; unchanged.

## Constraints

- **CN1** — The OUTER stacked-dev contracts are byte-immutable in this cluster: provision_workspace/full_checks(gate)/request_review/land argv and sequencing, the StartupTask/StartupResult startup envelope, the single review_verdict signal, and the brief_dev child workflow type plus its spawn_and_wait shape do not change. They are live-proven (ADR-008); only the INNER brief_dev data flow is reshaped. A diff touching any of those sites is a cluster defect (mirrors brief-dev CN8).
- **CN2** — Deterministic workflow code (brief_dev.gleam) decodes ONLY the facts projection and never a full ScoutReport, DevReport, or ReviewReport; full reports cross stage boundaries as opaque sealed aion_flow payloads the workflow holds without opening (ADR-012, engine invariant 2). grep for ScoutReport/DevReport/ReviewReport field access in brief_dev.gleam returns nothing after the reshape.
- **CN3** — Prompt rendering and full-report decoding happen on the worker (activity bodies / locals), never in workflow code; brief_dev.gleam does not import stacked_dev/prompts (which is deleted) or stacked_dev/render. The render path is built on aion_kit's template primitive (RM-022), not re-implemented.
- **CN4** — The facts projection is the negative space of the pre-reshape in-workflow decode: every field a facts type carries (dev: blocked R# ids, deduplicated changed-file paths; review: drifted R# ids with issues, has-fixes; scout: success only) was a field brief_dev read off a full report before. No new routing signal is invented and none is dropped (P3).
- **CN5** — The stage-contract schemas (scout/dev/review report) and the design-system canon they mirror are unchanged — the agents' output is identical (ADR-012 quote); only the consumption point moves. The drift gate (brief-dev CN7) still passes; no file under docs/design-system/ is touched.
- **CN6** — The lifted worker-harness (WT-002) runs the same CLIs with byte-identical argv in the same order as the live-proven originals: yg branch add/provision, cargo build, yg graph affected --plain --direct-only then yg diagnostics check (scoped and --workspace), meridian review request <branch> --reviewer... --as Meridian, git add -A then git commit then yg branch merge <branch> --yes from repo_root. Extraction is a move, not a rewrite (P5, CN1).
- **CN7** — The dev-pipeline skeleton (WT-003) is parameterised by an explicit PipelineConfig — stage prompts, stage schemas, gate commands, and the required caps/backoff — and bakes no default cap, backoff, deadline, or timeout (ADR-001 spirit, CN2 of brief-dev): every bound is a config or input field.
- **CN8** — The norn agent driver is not part of aion_kit and is not lifted into the toolkit (ADR-011): it stays worker-side; the pipeline skeleton and the template parameterise the agent step. aion_kit surface used by this cluster is the opaque payload (seal/raw/peek) and the template/json primitives only.
- **CN9** — After WT-001 the TK-002 dogfood completes through real norn end to end with no workflow-process heap exhaustion: dispatched from the roadmap, scouted/developed/verified/reviewed/hardened, and landed on main with the enriched brief riding the merge. The reshape is proven by the run, not only the type-checker (WT-001 verification, goal 3).
- **CN10** — The WT briefs are SERIAL within the cluster (WT-001 → WT-002 → WT-003): all three edit examples/stacked-dev (the workflow, the worker, the template) and would collide; parallelism is across clusters, not within this one (RM-023). The whole cluster is blocked on aion_kit (RM-022) landing first.
