# yg-fix — the fix half of a diagnosis → fix loop

`yg-fix` is a durable Aion workflow that consumes the **confirmed-findings
report** from an adversarial diagnosis run and drives the other half: it fans
out one fix agent per finding, has each proposed fix adversarially reviewed,
and synthesises a fix report. It is the counterpart of the Claude Code
`yg-review-salvage` diagnosis workflow — same finding shape in, verified fixes
out.

## Where it came from (Claude Code → Aion mapping)

The diagnosis workflow was a Claude Code orchestration script:
`agent(prompt, {schema})` calls, `parallel(...)` fan-out, plain glue. The port
to Aion is mechanical:

| Claude Code script | Aion workflow |
|---|---|
| `agent(prompt, {schema})` | an **activity** driving a Norn agent in driven mode with that schema as its `--output-schema`, plus a Gleam type + codec |
| `parallel([...])` | `workflow.all([...])` |
| the pure glue between agent calls (dedup, accumulation, `.filter(Boolean)`) | deterministic **Gleam** in the workflow body |
| `BATCH_VERDICTS_SCHEMA`, `REPORT_SCHEMA` | `schemas/*.json` (aion codegen subset) |

The diagnosis script's output — a `findings` array of
`{id, title, file, line, severity, category, detail, recommendation}` — is the
*exact input* this workflow ingests. The two workflows compose end to end.

## The flow (every phase is a distinct, console-visible step)

```
ingest (code)              validate + cap findings, clamp reviewers 1..3
   │
   ├─ fix ×N (agent) ──────one fix agent per finding, IN PARALLEL
   │                        → structured proposed patch + rationale
   ├─ review ×(N·M) (agent) M independent adversarial reviewers per fix
   │                        → pass | reject + evidence-backed blockers
   ├─ tally ×N (code)       MAJORITY verdict per fix
   │                        rejected → BOUNDED rework round (fed the blockers)
   │
   ├─ synthesize (agent)   operator report + disposition table (high effort)
   └─ integrate (code)     final structured report + severity/category rollups
```

Caps are working-defaults, all overridable from the input, never bare
constants: `max_findings` 25, `max_reviewers` 1 (clamped 1..3),
`max_fix_rounds` 1. Slice the input to a severity band or the top-N to go
wider. Cap exhaustion always completes with a report — no silent death.

**Fixes are proposed, not landed.** Each fix agent returns a structured patch +
rationale; applying/landing to the repo is a deliberate downstream step (the
one part with a real write-conflict hazard for same-file fixes). This keeps the
first cut safe and reviewable and makes it a clean stress test + ops-console
visualisation.

## What's here vs what's next

**Here (this deliverable):**
- `src/yg_fix.gleam` — the complete workflow, modelled on the proven
  `plan_fanout` structure (RawJson pass-through, decode-only-what-you-branch-on,
  bounded rework loop, majority tally).
- `schemas/` — `findings_input.json` (the input contract, byte-compatible with
  the diagnosis output), `fix_output.json`, `review_output.json`,
  `synthesis_output.json`.

**Next (not built — the worker):** a Rust worker on the `yg-fix` task queue
that serves the six activities. Three are **agent** activities routed to the
composed Norn harness in driven mode (`fix`, `review`, `synthesize`), each
handed its schema via `--output-schema` and its prompt assembled from the
activity input (finding + cited file for `fix`; finding + fix_output for
`review`; the results array for `synthesize`). Three are **code** activities
with unit-tested logic (`ingest` = parse/validate/cap; `tally` = majority over
M reviews, `blocked` iff `blocker_count * 2 > total`; `integrate` = fold the
synthesis + compute rollups). The worker copies the live-verified pattern from
`examples/incident-triage/worker` (liminal server-push transport,
`serve_with_redial` with a composed harness, session identity derived from the
activity input). Then add `gleam.toml` + `workflow.toml`, `aion package
examples/yg-fix`, and deploy.

Not written yet (the packaging wiring): `gleam.toml`, `workflow.toml`, and the
worker crate. This deliverable is deliberately just the workflow logic + the
JSON types — the shape you asked to see before we commit to the worker build.
