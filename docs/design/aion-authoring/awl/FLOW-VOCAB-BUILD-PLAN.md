# Flow-vocabulary build plan

Executes `AWL-FLOW-VOCABULARY.md` rev 3 (4b526b63). Four briefs, run as
dev_brief workflows through Aion itself — the operator follows each run
in the ops console; norn does the work underneath the dev-brief worker.
No corpus migration; the proof workflow is dev_flow rewritten in the new
surface.

## Briefs and order

| # | Brief | What lands | Depends on |
|---|-------|-----------|------------|
| B1 | `briefs/flow-vocab-B1-ergonomics.md` | `const`, raw strings, `json {}`, `schema of`, literal-statement fix | — |
| B2 | `briefs/flow-vocab-B2-shape.md` | grammar + checker: `subflow`, `distribute`/`sequence`, `collect`/`collect ?`, `max N visits`, decision tagging | B1 (same files) |
| B3 | `briefs/flow-vocab-B3-canvas.md` | server projection + authoring-canvas rendering of the new shape | B2 |
| B4 | `briefs/flow-vocab-B4-lowering.md` | MIR + emitter lowering; failure-as-value surface; dev_flow end-to-end proof | B2 (+ investigation) |

Strictly serial B1 → B2; B3 and B4 both follow B2 and may run in
parallel (disjoint areas: console TS vs compiler Rust).

**Investigation before B4** (orchestrator-owned, not a brief): read the
generated-Gleam SDK surface and engine fan-out semantics to determine
whether a branch activity failure can already be captured as a value.
Outcome is a one-page memo that fixes B4's scope: pure emitter work vs
a small SDK addition. This same capability retires the two `on failure`
direct-compile refusals.

## Checkpoints

- After B3: the operator reviews the canvas against rev 3 §6 **before**
  B4 merges — the picture is the point; layout judgment belongs to the
  operator, early.
- After B4: dev_flow (new surface) runs live through the general worker;
  check → canvas → emit → gleam build → deploy → run, all green, or the
  landing is not done.

## The trickiest parts (ranked)

1. **B2 checker region rules** — region formation and nesting,
   definite assignment of the collected binding, no-escape routing,
   `visits` bounds interacting with the existing cycle analysis. Wrong
   rules are either unsound or maddening. Adversarial review mandatory.
2. **B4 `collect ?`** — the failure-as-value surface is the one genuine
   unknown; hence the investigation gate before dispatch.
3. **B1 printer round-trip** — `json {}` and raw strings must survive
   the lossless canonical printer with source-correct spans; this is
   where AWL-1/2 hid their blockers.
4. **B3 canvas layout** — region banding, ×N marking, subflow
   expansion. Not deep, but judgment-heavy; hence the operator
   checkpoint.

## House rules (all briefs)

Workspace laws (no unwrap/expect/panic incl. tests, no
`#[allow]`/`#[ignore]`, files ≤500 code lines, `mod.rs` re-exports
only); run the gates and leave exit-code evidence; explicit-path
commits with the `Co-Authored-By: Claude Fable 5
<noreply@anthropic.com>` trailer; worktree isolation; no /tmp; truthful
report-back — unrun gates are unproven claims.
