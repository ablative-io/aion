# AWL workbench — fluent-author authoring seat, session 1 (2026-07-11)

Instrument: continuation of the deep-surface playtest under Tom's authoring-seat
commission — real workflows, real domains, findings F22+ (playtest numbering),
friction logged as requirements on the visual authoring surface. Toolchain:
aion 0.8.0, ~/.cargo/bin/aion rebuilt at HEAD e1ef9a93-era (post-F14 swap).

## Deliverables (all fmt-canonical, all check-green)

| file | steps | emit | domain |
|---|---|---|---|
| `cargo_gates.awl` | 1 | GREEN | Tom's daily wish: check+clippy+tests+fmt as one named-branch fork, exit-status-is-data, guard verdict |
| `release_land.awl` | 4 | GREEN | my landing doctrine: gates re-run as CHILD on final state, operator's word (wait, no timeout), land with backoff, compensation |
| `mouthpiece_mission.awl` | 3 | GREEN | outcome-contract mission: politeness-tier routing, sleep, wait+timeout tri-state, never-infer (silence → unconfirmed) |
| `awl_sitting.awl` | 5 | GREEN | one exam sitting: provision → candidate → grade → turn-2 → row; crashes become Never rows (exit-status-is-data) |
| `awl_exam.awl` | 4 | **REFUSED — the F23 exhibit** | the exam as standing regression suite: fork over sittings calling `sit_one` child |

Plus 4 probes (child-in-plain-body, enum-guard, call-site override, fork-chain)
and the playtest's earlier 8. Check runs this session: 11. Emit runs: 9.

## Findings F22–F26

### F22 — Combinator predicate poverty (precisely shaped by 3 runs)
`filter` accepts EXACTLY a bare, single-level, Bool-typed `.field` accessor.
Refused: comparisons (`filter(.check.class == FirstTry)` → "`filter` takes a
`.field` accessor argument") and nested accessors (`filter(.check.first_try)` →
same). Meanwhile enum comparisons work fine in `when` guards, AND the checker
proves enum-arm totality without `otherwise` (probe_enum_guard: three `when
x.temp == Variant` arms, no otherwise, accepted). So predicate power lives in
guards, not filters. Consequence in a real workflow: the exam row carries THE
SAME FACT THREE TIMES (CheckClass enum → check.first_try Bool → row.first_try
Bool) because each hoist is the only way to move a selection bit somewhere
`filter` can reach. Spec wording feeds it: "plus the predicates … and
comparisons" reads as filter-arg vocabulary. Part docs, part language gap;
visual-surface requirement: the canvas will render these duplicated fields —
a projectional editor could HIDE the hoist plumbing, or the language could just
allow one accessor level + comparison in filter.

### F23 — Child call inside a fork branch: checks, will not emit, message lies
`fork spec in sittings / sit_one(spec: …)` passes check; emit refuses with
"`sit_one` names no declared action" — but sit_one IS declared, as a child; the
message misnames the defect. Child calls in PLAIN step bodies emit fine
(probe_child_call, release_land). This blocks the natural N×M×P architecture
(one child run per sitting: isolation + per-sitting observability) at the last
pipeline stage. Error-message sub-finding: should say "child calls are not yet
lowerable inside fork branches".

### F24 — Named-fork results cannot be aggregated; list literals are argument-only
`[a, b, c, d] -> gates` → "expected a statement, found `[`"; same as a pipe
head. List literals exist ONLY in argument/constructor position. So four
named-branch gate results can never become a filterable list; the failed-only
sublist is inexpressible and cargo_gates' failure outcome carries all four
results with flags instead (defensible, but forced). F16's fixed-arity twin.

### F25 — Call-site node/timeout override: real, verified, unspelled in the spec
The spec promises "a step may override `node`/`timeout` at the call site" and
shows no syntax. Natural inline guess (`fetch(…) timeout 30s -> x`) dies with
generic "expected end of line, found keyword `timeout`". The actual spelling is
the action-declaration config line transplanted under the call (indented
`timeout 30s` line) — VERIFIED effective in emitted Gleam
(`activity.timeout(duration.milliseconds(30000))`), not parse-and-ignored.
One spec example fixes this.

### F26 — The emit subset is systematic, and nothing warns the author (umbrella)
Three check-green shapes now refuse only at emit: `on failure` + body-terminal
route (playtest F19a), child-in-fork (F23), and multi-statement collection-fork
branches ("a collection fork lowers one unbound action call per item in the
Gleam stopgap" — honest, well-worded). Pattern: the checker validates the
LANGUAGE; the stopgap emits a SUBSET; the author discovers the boundary only by
running emit. Direct input to the visual surface's D3: whatever serves
diagnostics must serve EMIT-SUBSET diagnostics too, or canvas users will build
check-green graphs that cannot deploy. (Each stopgap message individually is
good; the seam is the missing early warning.)

## The fan-out pattern matrix (Tom's N×M×P question, answered by construction)

| pattern | check | emit | exhibit |
|---|---|---|---|
| collection fork, one action per item | ✓ | ✓ | dev_brief, playtest flagship |
| collection fork, chained statements per item | ✓ | ✗ (F26) | probe_fork_chain |
| collection fork over CHILD calls | ✓ | ✗ (F23) | awl_exam |
| named-branch fork, heterogeneous | ✓ | ✓ | cargo_gates |
| child call, plain body (composition) | ✓ | ✓ | release_land, probe_child_call |
| matrix cross-product (N×M×P → [spec]) in VM | ✗ (no list construction/iteration) | — | expand-matrix worker action is the honest shape |

Bottom line for the exam suite: expansion of harness×model×effort into
[SittingSpec] is worker-side data prep (fine); per-sitting fan-out WANTS
child-per-item and that is exactly the one blocked cell. Until the emitter
learns it, the only emittable exam is one-action-per-sitting where the action
wraps the entire sitting pipeline worker-side — losing engine-visible
provision/grade/feedback stages. The right fix is the emitter, not the design.

## Positives ledger (what just worked, first try)

Actions returning `[T]` (F7 partially dissolved — wrapper types not needed for
lists); enum variants as construction values; empty list literals typed in
context; nested record construction in route payloads; Float literals;
body-less guard-only steps; `sleep` + statement-route; wait WITHOUT timeout
binds plain T with readable fields (the F15 contrast case); 4-way `and` in
guards; enum totality proving; `retry N backoff D..D` form; two-step
compensation in `on failure` (call then route); child composition end-to-end;
fmt idempotent on all five files after one canonicalization.

## Legal-but-ugly (self-caught)

- `gates_run: 4` — arity hardcoded because count can't run over a literal list.
- The triple-copy first_try fact (F22's exhibit).
- `Verdict.corrected_outcome: String` non-optional with worker-contract "empty
  string when not corrected" — because an optional would be unreadable at the
  guard (F15), the honest `String?` loses to the readable `String`.
