# AWL exam — results ledger

One row per sitting. Marks per EXAM-PROTOCOL.md §Marking. Transcript and
feedback envelope paths retained per sitting.

| # | Date | Harness | Model | Effort | check-pass | Semantic (of 6) | Stall points | Legal-but-ugly | Notes |
|---|------|---------|-------|--------|-----------|-----------------|--------------|----------------|-------|
| 0 | 2026-07-11 | invigilator (pipeline proof) | claude-fable-5 | n/a | first_try | 6/6 | none (NOT representative — author of the toolchain) | none noted | `aion awl check` ok (3 steps), `aion awl emit` exit 0. Proves pack-sufficiency + pipeline, NOT difficulty. |
| 1 | 2026-07-11 | claude -p | opus | default | first_try | 6/6 | none visible | added `node shell` to every action unprompted (copied from pack's config example) | 3 steps, textbook route-to-step escalation. |
| 2 | 2026-07-11 | claude -p | sonnet | default | first_try | 6/6 | none visible | **route-target step ALSO declares `after fetch_and_confirm`** — checker accepts a step that is both dependency successor and route target (→ F2) | 2 steps; everything else clean. |
| 3 | 2026-07-11 | claude -p | haiku | default | never | n/a (parse fail; shape would have scored 6/6) | mixed pipe input with positional call args (`order_id \|> escalate(order_id, refreshed_order.status)`) | pipes used where named calls are simpler throughout | Checker error misleading (→ F1): "unterminated pipe chain: end with `-> <name>`" on a line that already ends with `-> result`. |
| 4 | 2026-07-11 | norn | gpt-5.6-sol | medium | first_try | 6/6 | none visible | none noted | 2 steps, clean. |
| 5 | 2026-07-11 | norn | gpt-5.6-sol | high | first_try | 6/6 | none visible | none noted | 3 steps — closest of all candidates to the invigilator's reference. |
| 6 | 2026-07-11 | norn | gpt-5.6-sol | xhigh | first_try | 6/6 | none visible | none noted | 2 steps, clean. |

First-sitting headline: **5/6 first-try check pass, all five passers 6/6 semantic**, across two gene pools and one page of docs. The single failure produced an error-message finding, and one passer produced a checker-gap finding — the exam is measuring the language, as designed. All candidates copied `node shell` from the pack's example config line (observation: example fragments get cargo-culted wholesale; keep pack examples minimal-canonical).

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
- **F1 (sitting 3, haiku)**: error-message bug. Mixing pipe input with
  positional call args (`x |> f(a, b)`) reports "unterminated pipe chain:
  end with `-> <name>` or `route <target>`" — on a line that already ends
  with `-> result`. The real defect (positional args / pipe-call arity) is
  never named, and the suggested fix is already present, so the message
  actively misleads. Action: aion-awl diagnostics issue — the parser should
  name the actual construct error; candidate transcript retained as repro.
- **F2 (sitting 2, sonnet)**: checker gap / semantics ruling needed. A step
  may simultaneously declare `after <step>` AND be the target of a
  conditional outcome's `route` — the checker accepts it silently. What
  does it mean? If `after` fires on the predecessor's completion regardless
  of which outcome routed, the escalation step would run even on the
  delivered path (double-trigger); if route wins, the `after` is dead text.
  Either reading makes one of the two declarations a lie. Action: language
  ruling (reject, or define precedence + warn) → AWL advisory backlog
  alongside the retry-semantics ruling.
