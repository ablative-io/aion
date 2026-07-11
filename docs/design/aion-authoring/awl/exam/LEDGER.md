# AWL exam — results ledger

One row per sitting. Marks per EXAM-PROTOCOL.md §Marking. Transcript and
feedback envelope paths retained per sitting.

| # | Date | Harness | Model | Effort | check-pass | Semantic (of 6) | Stall points | Legal-but-ugly | Notes |
|---|------|---------|-------|--------|-----------|-----------------|--------------|----------------|-------|
| 0 | 2026-07-11 | invigilator (pipeline proof) | claude-fable-5 | n/a | first_try | 6/6 | none (NOT representative — author of the toolchain) | none noted | `aion awl check` ok (3 steps), `aion awl emit` exit 0. Proves pack-sufficiency + pipeline, NOT difficulty. |

## Sittings

(append per-sitting detail sections here: transcript path, submitted file
path, check output, turn-2 feedback envelope, invigilator notes)

## Findings → actions

(append: recurring failure → classified as spec bug / docs gap / error-message
bug / model gap → issue or fix reference)

- **F0 (pre-exam, pack authoring)**: the task's conditional-escalation
  requirement needs outcome→step routing, which the pack's first draft did
  not teach — the invigilator hit this while drafting the reference
  solution, BEFORE any candidate sat. Classified: pack gap (task-required
  material). Fixed in CANDIDATE-PACK.md (route target can be a step name)
  before sitting 0. Lesson: the reference solution must be written from the
  pack alone before any candidate sees it — kept as a standing protocol rule.
