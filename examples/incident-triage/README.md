# Incident triage — the prospekt → Aion bridge

Proof that a **prospekt**-validated structured document drives an **Aion**
workflow as typed input, and the workflow returns typed structured output.

- **prospekt** mints and ready-checks an `incident` document (the `debug-loop`
  model): a payload of `title`, `severity`, `observed`, `expected`,
  `environment`, plus injected `id`, `model`, `model_version`, `state`, and a
  `forensics` slot.
- **Aion** runs the `incident_triage` workflow. It decodes the incident into a
  typed `Incident` record, schedules ONE `triage` activity, and completes
  returning a typed `TriageSummary` (`incident_id`, `severity`, `headline`,
  `next_action`).
- The **Rust worker** serves `triage` with plain severity → next-action logic —
  no AI, no network beyond the engine.

Deterministic: no timers, no fan-out, no entropy. The whole point is
**typed-structured-input in, typed-structured-output out** across the boundary.

## Files

| Path | What it is |
|---|---|
| `src/incident_triage.gleam` | The Gleam workflow: `Incident`/`TriageSummary` types, codecs, one `triage` dispatch. |
| `workflow.toml`, `schemas/` | Packaging descriptor + input/output JSON Schemas. |
| `worker/` | Standalone Rust worker (`Worker::builder(...).run()`) serving `triage`. |
| `RUNBOOK.md` | **Operator crib sheet** — copy-pasteable path from a fresh server to a completed run, with verified output. |

Start at [`RUNBOOK.md`](RUNBOOK.md).

## Notes for the prospekt resolve design

The effective incident document maps cleanly to an Aion typed input, with two
shape frictions worth recording:

1. **Injected fields are not in the kind's `schema.json`.** The document the
   workflow receives carries `id`, `model`, `model_version`, `state`, and the
   `forensics` slot on top of the incident payload. The Aion input schema
   therefore sets `additionalProperties: true` and the decoder ignores the
   extras. A prospekt "resolve to workflow input" step would ideally emit the
   effective schema (payload + injected fields) so the consumer can validate the
   whole document rather than only the payload.
2. **The `model` name collides across nesting levels.** The document root has
   `model` (the model *name*, `"debug-loop"`), and the incident payload has
   `environment.model` (the LLM in play). They live at different depths so
   typed decoding is unambiguous here, but a flatten-to-input transform would
   collide them. The non-overlap law that reserves `model` at the root only is
   exactly what keeps this safe — this example is a live witness of that rule.
