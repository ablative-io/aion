# Workflow-Toolkit — Checklist

## Facts Projection

- [ ] **C1** — src/stacked_dev/facts.gleam defines DevFacts (blocked_requirement_ids: List(String), changed_files: List(String)), ReviewFacts (drifted: List(DriftedRequirement), has_fixes: Bool), and ScoutFacts (a success marker with no report fields), each with a hand-written round-tripping codec.
- [ ] **C2** — Every field of every facts type corresponds to a value brief_dev read off a full report before the reshape: DevFacts.blocked_requirement_ids from dev_report.enrichments[].status == blocked, DevFacts.changed_files from the deduplicated files_changed[].path, ReviewFacts.drifted from enrichments[].alignment == drifted with issues, ReviewFacts.has_fixes from any enrichment fixes non-empty — no field is invented and none of the old routing signals is dropped.
- [ ] **C3** — The facts are derived on the worker (in locals/handlers) from the full report, not in workflow code; the workflow receives the facts already projected.

## Sealed Payloads

- [ ] **C4** — scout, dev, dev_resume, and dev_review each return a result pairing the stage facts with the full stage report sealed as an opaque aion_flow pass-through payload (via the aion_kit seal primitive), and the workflow holds the sealed payload without ever calling raw/peek on it.
- [ ] **C5** — brief_dev.gleam contains no field access on ScoutReport, DevReport, or ReviewReport and no import of aion_stacked_dev_io's report decoders for control flow: grep for the report type field accessors in brief_dev.gleam returns nothing.
- [ ] **C6** — The reshaped BriefDevResult carries the three sealed stage payloads (for enrich_brief) plus the facts the outer arc reads (the changed-file paths and the dev summary the outer DevResult derivation needs), and crosses the parent boundary with its codec round-tripping losslessly.
- [ ] **C7** — The enrich_brief activity decodes the sealed scout/dev/review payloads on the worker (aion_kit payload.raw + json) to merge the full reports into the brief, and no full report is decoded in any workflow module.

## Worker-Side Rendering

- [ ] **C8** — Prompt rendering for scout, dev, dev_resume, and dev_review happens in the activity bodies on the worker (src/stacked_dev/render.gleam and worker/src/render.rs), built via the aion_kit template primitive over the brief document, resolved context, and the decoded prior-stage payloads.
- [ ] **C9** — src/stacked_dev/prompts.gleam is deleted along with its five projection test modules (prompts_render/scout/dev/review/resume_test.gleam); brief_dev.gleam no longer imports stacked_dev/prompts and there is no second in-workflow rendering path left alongside the worker-side one (ADR-002).
- [ ] **C10** — render.gleam preserves the projection discipline of the retired prompts.gleam: anchored ADRs as 'id: title — decision' lines, verbatim quotes with speaker attribution, requirements with inline resolved C#/S# texts, boundaries in every stage prompt, and the dev projection inlines scout findings per requirement while the review projection renders the dev attestation and measured checks but never the scout (ADR-010).
- [ ] **C11** — render_test.gleam pins each stage prompt's content and character budget against the seeded fixture (docs/design/brief-dev/briefs/BD-001.json) with the same budgets the deleted prompts tests asserted (scout 6000, dev 9000, review 12000).

## Reshaped brief_dev Workflow

- [ ] **C12** — brief_dev.gleam threads sealed payloads between stages and reads only the facts projection for routing: DevBlocked from DevFacts.blocked_requirement_ids, scoped_checks files from DevFacts.changed_files, ReviewDrifted from ReviewFacts.drifted, the harden re-verify trigger from ReviewFacts.has_fixes.
- [ ] **C13** — The stage flow, the seven status phases (scouting, developing, verifying, fixing, reviewing, hardening, converged), the bounded verify-fix loop with required verify_fix_cap and round_backoff_ms, and the six-variant BriefDevError taxonomy are preserved exactly as RM-017 landed them — the reshape changes the data flow, not the control flow.
- [ ] **C14** — brief_dev.gleam imports no rendering module (prompts is deleted, render is worker-side) and dispatches only scout, warm_build, dev, scoped_checks, dev_resume, and dev_review — the same six activity calls, now with reshaped result types.
- [ ] **C15** — schemas/brief_dev_output.json is updated to the reshaped result (sealed reports + facts) inside the codegen v1 subset (no $ref/$defs/oneOf/default), aion codegen --check passes clean, and src/aion_stacked_dev_io.gleam is regenerated, not hand-edited.

## Outer Arc Immutability

- [ ] **C16** — stacked_dev.gleam is touched only at the two sites that consume the brief_dev child result (the outer DevResult derivation and the enrich_brief calls); provision/gate/review/land argv, the StartupTask/StartupResult envelope, the review_verdict signal, and the brief_dev spawn_and_wait shape are byte-identical pre- and post-cluster (CN1).
- [ ] **C17** — The outer DevResult (session_id, files_touched, summary) and the enrich_brief inputs are produced from the reshaped BriefDevResult without decoding a full report in workflow code: files_touched comes from the carried facts and the sealed reports flow straight to enrich_brief.

## Extracted Worker-Harness

- [ ] **C18** — The worktree/build/check/land harness (provision-worktree, warm-build, scoped-checks, full-checks, land-via-yg) lives in a single reusable module (src/stacked_dev/harness.gleam and worker/src/harness.rs) that a new family imports, rather than the bespoke per-family implementations in locals.gleam/handlers.rs.
- [ ] **C19** — The lifted harness invocations are byte-identical in argv and order to the live-proven originals: yg branch add/provision, cargo build, yg graph affected --plain --direct-only then yg diagnostics check (scoped --package... and --workspace fallback), and land's git add -A then git commit then yg branch merge <branch> --yes run from repo_root (CN6).
- [ ] **C20** — harness_test.gleam pins each lifted CLI invocation's argv equal to its pre-extraction value and asserts the loud-fallback scoping (empty affected set falls back to a named workspace-wide scope, never zero checks).
- [ ] **C21** — The land harness preserves the live-proven hazards verbatim: it commits before merge (norn leaves work uncommitted) and runs the merge from repo_root, not the worktree it deletes (CN1/CN6).

## Extracted Pipeline Template

- [ ] **C22** — The scout→dev→verify→review→harden control skeleton lives in src/stacked_dev/pipeline.gleam parameterised by a PipelineConfig (src/stacked_dev/pipeline_config.gleam) carrying the stage prompt templates, the stage schemas, and the gate command set, so the loop body is shared rather than re-authored per family.
- [ ] **C23** — PipelineConfig bakes no default cap, backoff, deadline, or timeout: every bound is a config or input field, and the pipeline imposes none of its own (CN7, ADR-001 spirit).
- [ ] **C24** — brief_dev is expressed as a PipelineConfig over the shared skeleton (its prompts, its stage schemas, its gate commands) rather than a from-scratch workflow body, and pipeline_test.gleam drives the same stage flow from that config plus a second distinct config to prove reuse without re-authoring the body.
- [ ] **C25** — The agent step is parameterised by the config, not baked into the skeleton, and the norn agent driver stays worker-side (ADR-011, CN8) — pipeline.gleam imports no norn-specific runtime.
- [ ] **C26** — crates/aion-cli/templates/dev_pipeline/ is updated to scaffold the thin harness+pipeline shape (facts/sealed payloads, worker-side render, imported harness, config-driven skeleton), and its rendered worker gates pass: cargo fmt --check, cargo clippy --all-targets -D warnings, cargo test.

## Verification and Gates

- [ ] **C27** — The TK-002 dogfood runs end to end through real norn against the aion repo after WT-001: dispatched, scouted/developed/verified/reviewed/hardened, landed on main with the enriched brief in the merge, and a SIGKILL mid-run resumes with no re-executed activities and no workflow-process heap exhaustion (CN9).
- [ ] **C28** — wire_compat.rs pins every facts type and sealed-payload envelope byte-compatible both directions against the Gleam codecs; handlers_shims.rs covers the render-and-seal handler shape.
- [ ] **C29** — The hermetic gleam suite (facts_codecs_test, render_test, harness_test, pipeline_test, and the updated aion_stacked_dev_test) passes, and the drift gate (check-schema-drift.sh) still exits 0 with no docs/design-system/ file touched (CN5).
- [ ] **C30** — At the cluster's landing tip cargo fmt --check, cargo clippy --workspace --all-targets -- -D warnings, the workspace suite, the package gleam test suite, and aion codegen --check all pass clean, with no #[allow]/unwrap/expect/panic and no file over 500 code lines.
