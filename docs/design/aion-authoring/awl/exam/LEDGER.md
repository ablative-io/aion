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

### Turn-2 synthesis (all six candidates, mined 2026-07-11)

Confidence before check: opus 0.55, sonnet 0.60, haiku 0.40, sol-medium 0.82,
sol-high 0.86, sol-xhigh 0.84. Recurring items across BOTH gene pools carry the
strongest signal. Full synthesis + per-candidate envelopes retained in the
sitting scratch; findings below numbered F3+ with INVIGILATOR ANNOTATIONS where
the candidates' classification is corrected by knowledge of the real language.

### F3 — Pack has only one worked example, and it is a single linear step
Docs gap (pack). Signal: 6/6, unanimous, both pools. All six asked for a second
example exercising branching + a routed-to step + a cross-step binding — the
exact shape the task requires. Action: add a ~12-line second example (two+
steps, conditional outcome, route-to-step, binding used in routed step). This
one fix dissolves F4/F5/F6/F8/F9.

### F4 — `after` vs `route <step>` relationship undocumented
Docs gap, tied to F2. Signal: 5/6, both pools. Nobody could tell if `after` is
required, redundant, or forbidden on a routed-to step — and per F2 the checker
accepts both, so wrong guesses are never corrected. Action: decide the intended
semantics (F2 ruling), then one doc sentence + checker enforcement together.

### F5 — "visible in all later steps" ambiguous once steps form a routing graph
Docs gap. Signal: 6/6. Candidates guessed whether "later" means text order or
reachability. Action: restate binding scope in control-flow terms.

### F6 — `outcome` keyword overloaded (terminal vs step-local branch labels)
Language-surface gap. Signal: 4/6, both pools. Candidates paused over whether
step-local `outcome x: when ...` labels are terminal outcomes or local labels.
Action: one distinguishing doc sentence now; consider a distinct keyword
(`branch`/`case`) as an AWL advisory-backlog item.

### F7 — No documented way to discard an action result
Language + docs gap. Signal: 3/6 (all norns); xhigh INVENTED wrapper record
types solely to give side-effect actions a return. Action: document that unused
bindings are legal; consider a discard/void form in the advisory backlog.

### F8 — No compact grammar/syntax reference in the pack
Docs gap. Signal: 5/6. Action: ~10-line syntax reference block within the
one-page budget.

### F9 — Pack shows no example checker diagnostic
Docs gap. Signal: 4/6. Action: one failing snippet + its diagnostic + the fix,
in the pack. (Sharper given F1.)

### F10 — Intro promises "loop iteration" durability; no loop is shown
INVIGILATOR CORRECTION: candidates classified this as a false promise / missing
construct — AWL HAS loops (`loop ... until ... max`, see dev_brief.awl); the
pack references a construct it never teaches. Reclassified: pack wording bug.
Action: either drop "loop iteration" from the intro or add "(loops exist,
out of scope for this task)".

### F11 — What happens when retries are exhausted?
Genuine language-semantics question (opus). Ties DIRECTLY to the pre-existing
retry-semantics advisory ruling (retry N = N total attempts? backoff?) already
in the AWL backlog. Action: fold into that ruling; document exhaustion
behavior ("route failure? which outcome?") as part of it.

### F12 — Conditions look limited to `== <literal>`
INVIGILATOR CORRECTION: richer conditions exist in the corpus (`is empty`,
`==` on expressions); the pack shows only one comparison. Part docs gap, part
real question (no `and`/`or` shown anywhere — confirm what the grammar
actually admits and document the boundary). Action: condition grammar line in
the F8 reference block; boolean-composition question to the advisory backlog.

### F13 — `node shell` unexplained; candidates cargo-culted it
Docs gap. Signal: 2/6 reported it, 6/6 DID it (every candidate copied `node
shell` onto actions that needed only timeout/retry). Action: one sentence on
what `node` selects + that each config field is optional — or remove `node`
from the pack's example config line entirely (minimal-canonical principle).

**Priority: F3 first** (collapses five findings), then F2+F4 as one
semantics-ruling-plus-enforcement unit, then F10/F13 pack edits, then the
advisory-backlog items (F6 keyword, F7 discard, F11 retry-exhaustion, F12
boolean composition).


---

## Fluent-author deep-surface playtest (Waffles the Terrible, 2026-07-11)

NOT a blind sitting. The seventh candidate's blind sitting was voided by his
own disclosure (he had read this ledger, the pack, and AWL-2-SPEC before the
exam rules reached him — messages crossed; no fault). Replaced by agreement
with a deep-surface playtest: a fluent author building a real certifying-pair
doc-flow workflow across the surface the exam never touched (signals,
wait/timeout flow-typing, forks, backward routing, enums, compensation,
combinators), with honest instruments (check-run count, errors verbatim,
confidence-before-verdict). Findings number F14+. Spec-vs-checker
disagreements are flagged the moment they are confirmed, not reconciled by
the author; classification is the invigilator's.

### F14 — Installed CLI lagged main across the 2026-07-11 loop rulings (two spec-vs-checker flags, one root cause)
Flagged mid-playtest per the disagreement boundary, before the DONE pack.
Two probes passed the shipped checker where AWL-2-SPEC §loop requires
refusal: (a) a loop-carrying step with zero outcome clauses checked "ok"
(R1 — silent-exhaustion shape), (b) a bare `route` statement inside a loop
body checked "ok" (R3). INVIGILATOR CLASSIFICATION: **toolchain-distribution
gap** — a NEW finding class. Neither the spec nor the checker is wrong at
source: enforcement (7d1fcc8b) and spec amendment (814f279d) landed together
on main at 06:06–06:07 +1000; the installed ~/.cargo/bin/aion had been built
at 05:48, eighteen minutes earlier, and nothing reinstalls the CLI when main
moves. Verified: both probes REFUSED at HEAD with the ruled spans/messages
(loop 15:3, route 19:5); fresh binary installed 2026-07-11 ~11:2xZ, both
refusals reproduce against ~/.cargo/bin/aion. Residual action: the spec of
record and the installed toolchain need a freshness guard (same class as the
ops-console embed-by-default CI guard) — an author following the documented
path must not be able to certify against a checker that silently disagrees
with the spec. Both probe files preserved in the playtest pack.

### Playtest marked (invigilator, 2026-07-11 ~11:45Z) — pack archived at exam/playtest/
Flagship `doc_certification.awl` verified by the invigilator's own hands on the
fresh binary: check GREEN (6 steps), emit GREEN, byte-canonical under fmt.
Instrument honesty verified: 13 check-runs logged with verbatim errors and a
hard stale/fresh line at the F14 swap; every probe verdict re-verified
post-swap; self-caught confessions; can't-say list; confidence (0.9) stated
before the verdict. The exam's headline SURVIVES a fluent author: the spine
held everywhere it was spec'd (backward routing, forks, loops, arm-local
narrowing); the friction concentrated in the same seams the blind candidates
found, plus the deeper ones below. Two exam findings independently confirmed
by a fluent author: F7 (wrapper-type invention) and F13 (node cargo-culting).

### F15 — Language gap (HEADLINE): wait-with-deadline decisions cost a worker round-trip
Flow-typing is arm-local AND single-predicate: `when x is present and
x.approved` does not narrow, and no later step reads the field — so after
`wait s timeout D -> x` the payload's fields are unreadable everywhere in the
VM. The operator-approval-with-deadline pattern is expressible only via a `T?`
action param round-trip to read one Bool — the exact absurdity the 2026-07-10
combinator ruling was written to kill, reborn one construct over. F15b:
diagnostic suggests adding the `is present` guard the arm already has (F1
class). Remedy candidates (advisory backlog): narrowing distributes over `and`
within the arm; or `when x is present as y` re-binding. INVIGILATOR: ruling
required, same weight as the combinator ruling.

### F16 — Language gap: fork results cannot be merged in the VM
Two `[T]` fork bindings; no concat/flatten (`map(.findings)` would strand
`[[T]]`). Merging reviewers' findings needs a worker action for a pure fold.
The `+`-on-lists diagnostic itself is good.

### F17 — Language gap: no zero-iteration loop
Body runs at least once; skip-topology alternative correctly refused by the
every-path binding rule, so authors pay a wasted dispatch.

### F18 — Diagnostics: dead-end cascade manufactures phantom errors
One real graph defect → 10 errors, 4 of them PHANTOM already-bound complaints
on legal bindings (reproduced byte-for-byte at HEAD, `cascade_repro.awl`);
the already-bound message appends its loop-rebinding hint even with no loop
in sight. Action: suppress binding-flow analysis on steps already marked
unreachable.

### F19 — check-green ≠ emit-green; fmt alignment rule unstated
(a) `on failure` + body-terminal route passes check, refused at emit (honest
stopgap message) — a check-satisfied author ships an undeployable workflow.
Either check carries stopgap-lowerability diagnostics or the split gets one
doc sentence. (b) The printer's column-alignment rule lives in examples only,
never in the printer-contract prose.

### F20 — Docs gap: statement-position `route` is legal, load-bearing, unshown
The only unconditional successor for a step whose fall-through target is
route-targeted; spec shows route only as pipe terminator and in outcome
clauses. One grammar line + one example.

### F21 — Advisory: bounded-cycle rule accepts bounds that don't bound
Route re-entry resets the loop, so a legal backward cycle is unbounded in the
rule's spirit while satisfying its letter; single-assignment-under-re-entry
semantics unstated. F2 + F20 + F21 are one family: the spec's control-flow
story is written step-forward; route-graph corners live in checker behavior
only. To the advisory backlog beside F2 and retry-exhaustion (F11).

**Updated priority after the playtest: F15 ruling first among language items**
(it re-opens the combinator-ruling wound), then F3 (pack), F2+F4, F18/F19
diagnostics, F20 doc line, advisory backlog (F6/F7/F11/F12/F16/F17/F21).
