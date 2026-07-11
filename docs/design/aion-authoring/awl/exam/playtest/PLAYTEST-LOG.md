# AWL fluent-author deep-surface playtest — final log

Instrument: NOT a blind sitting (invigilator's ruling, 2026-07-11 ~11:07Z). Author
read AWL-2-SPEC.md (reference of record), exam ledger F1–F13, both rev2 golden
examples, before writing. Measures the exam-untested surface: signal +
wait/timeout flow-typing, backward routing vs the bounded-cycle rule, enums,
on-failure compensation, spawn/child, combinators, and the check→emit→fmt
pipeline for a fluent author.

**Toolchain note (F14, invigilator-ruled):** runs 1–11 and the first 4 emit runs
used a STALE installed binary (built 05:48, predating the 06:06–07 ruling
enforcement commits — toolchain-distribution gap, ledger F14 on aion main
8b19c4de). Binary swapped ~11:25Z; runs 12–13 and the final emit are against
fresh HEAD 77ddad39. Every probe verdict was re-verified post-swap: only the two
F14 symptoms changed (both now refuse with well-worded, correctly-anchored
errors); all other verdicts identical, so all substantive findings below stand on
current code. Post-swap emit of the unchanged flagship shrank 1016→876 lines —
the emitter moved with main; noted, not investigated (invigilator's pipeline, not
this instrument's).

Deliverables in this directory:
- `doc_certification.awl` — the flagship: certifying-pair doc flow, 6 steps,
  check GREEN and emit GREEN on the FRESH binary, byte-canonical under fmt.
- 8 probe files (each a one-question boundary experiment; `cascade_repro.awl` is
  draft-1 reconstructed verbatim), `emitted.gleam`, `fmt_diff.txt`.

## Check-run ledger (13 runs, honest count)

| # | file | binary | result | what it told me |
|---|------|--------|--------|-----------------|
| 1 | doc_certification (draft 1) | stale | 10 errors | 1 real graph defect + 2 real optional-field errors + **7 cascade phantoms** — F18 |
| 2 | probe_backroute | stale | ok | backward/self routing + loop-value rebinding legal, as spec'd |
| 3 | probe_flowand | stale | error | `is present and x.field` does NOT narrow — F15 |
| 4 | probe_waitfield | stale | error | narrowing doesn't cross steps either — F15 |
| 5 | probe_narrow_use | stale | ok | narrowed optional usable as whole value in the arm |
| 6 | probe_optparam | stale | ok | `T?` action params legal → the worker-hop escape hatch — F15 |
| 7 | doc_certification (restructured) | stale | ok (5 steps) | statement-position `route <step>` legal — F20 |
| 8 | probe_loop_noout | stale | ok | flagged as spec-vs-checker → ruled F14 (stale binary) |
| 9 | probe_loop_route | stale | ok | flagged with run 8 → same F14 root cause |
| 10 | probe_listconcat | stale | 2 errors | GOOD primary message; names construct + types — F16 |
| 11 | doc_certification (final shape) | stale | ok (6 steps) | emit-driven restructure verified |
| — | — | **binary swap ~11:25Z** | — | invigilator rebuilt + reinstalled at HEAD |
| 12 | doc_certification (final) + all 8 probes re-run | fresh | flagship ok (6 steps); loop probes now refuse; all else identical | F14 symptoms closed; every other finding survives the swap |
| 13 | cascade_repro (draft-1 verbatim) | fresh | same 10 errors byte-for-byte | F18 stands on current code |

Emit runs: stale — flagship REFUSED (`on failure` + body-terminal route, F19),
probe_loop_route REFUSED (stopgap net), probe_loop_noout EMITTED (the since-closed
no-net-anywhere symptom), flagship-final OK (1016 lines); fresh — flagship-final
OK (876 lines). Fmt: copy-diff (alignment-only changes, F19b), then final
in-place; check still green after fmt.

## Findings (numbering continues the exam ledger; F14 is the invigilator's)

### F14 — (invigilator-ruled) toolchain-distribution gap
My two spec-vs-checker flags (loop-exhaustion fall-through accepted; route-in-loop
accepted at check) collapsed to one root cause: the installed binary predated the
same-morning enforcement commits; spec and checker were never apart on main.
Flagged immediately per protocol, repro files preserved, ruled and closed by the
invigilator (aion main 8b19c4de); residual freshness-guard action on Tom's
backlog. Post-swap both refusals reproduce with correctly-anchored spans.

### F15 — Language gap: wait-with-deadline decisions cost a worker round-trip
The one flow-typing rule is strictly arm-local and single-predicate:
`when x is present and x.approved` does not narrow (run 3), and no later step can
read the field (run 4). Consequence: after `wait s timeout D -> x`, the payload's
FIELDS are unreadable everywhere in the VM. The bread-and-butter pattern —
"operator approval with a deadline; inspect the answer; branch" — is expressible
ONLY by passing the `T?` to a worker action (`T?` params are legal, run 6) that
returns a readable record: a worker round-trip to read one Bool, the exact
absurdity the 2026-07-10 combinator ruling was written to kill. The flagship
carries this shape honestly (`assess_ruling`). Sub-finding F15b: the diagnostic
says "guard with `is present` before reading `.approved`" on an arm whose guard
ALREADY contains `is present` — F1's misleading-suggestion class.
Remedy directions (advisory backlog, not mine to rule): let narrowing distribute
over `and` within the arm; or a `when x is present as y` re-binding form.

### F16 — Language gap: fork results cannot be merged in the VM
Named-branch fork gives two `[T]` bindings; `+` is string-only, no concat, no
flatten (`map(.findings)` over reports would make `[[T]]` with no way down). The
natural authoring move — merge two reviewers' findings — needs a worker action
(`merge_reports`) for a pure structural fold. Hit while drafting, before any
check run. The `+`-on-lists diagnostic itself is GOOD ("`+` joins strings only —
arithmetic is not in the language (found [String] and [String])", run 10).

### F17 — Language gap: no zero-iteration loop
`loop` body runs at least once; "run zero or more times" is inexpressible. A doc
with no blocking findings still pays one full `revise_round` dispatch, because the
skip-topology alternative leaves the loop's bindings (`round`, `rounds`) unbound
on one path into later steps (checker's every-path rule, correctly enforced).
Chose the wasted dispatch; a fluent author shouldn't have to.

### F18 — Diagnostics: dead-end cascade manufactures phantom errors
ONE real graph defect (step `merge` dead-ends because its natural successor is
route-targeted, which kills fall-through) produces 10 errors — 3 unreachable-step
cascades and 4 PHANTOM "already bound — bindings are single-assignment per scope"
errors on bindings that are legal in the fixed file (runs 7/11/12 prove it). The
already-bound message also always appends "(the loop threaded value is the one
sanctioned rebinding)" even where no loop is involved. A first-time author would
chase the phantoms, not the cause. Reproduced byte-for-byte on the FRESH binary
(run 13, `cascade_repro.awl`) — stands on current code. Suggest: suppress
binding-flow analysis on steps already marked unreachable.

### F19 — check-green ≠ emit-green (and the author isn't warned)
(a) The checker accepts `on failure` + body-terminal route in one step; emit
refuses it ("the Gleam stopgap cannot tell a routed failure outcome from a step
failure there" — honest, well-worded). A check-satisfied author ships a workflow
that cannot deploy; either check should carry stopgap-lowerability diagnostics or
the split needs one doc sentence. Restructure cost: an artificial `complete` step
whose only job is holding the terminal route (see legal-but-ugly).
(b) fmt: the printer column-aligns type bodies and outcome clauses; the spec's
printer-contract prose never states the alignment rule (the examples embody it).
Hand-written fluent output is near-canonical but not canonical.

### F20 — Docs gap: statement-position `route` is legal, load-bearing, and unshown
A bare `route <step>` / `route <outcome>(…)` as a body statement parses, checks,
and emits — and it is the ONLY way to give an unconditional successor to a step
whose natural fall-through target is route-targeted (fall-through dies the moment
any route targets the next step). The spec shows route only as a pipe terminator
and in outcome clauses. One grammar line + one example fixes it. Positive twin:
backward routing and multi-step cycles work exactly as spec'd (runs 2, 7, 11, 12).

### F21 — Advisory: the bounded-cycle rule accepts bounds that don't bound
The flagship's `ruling_gate → revision_rounds` backward route forms a cycle whose
only bound is `max config.max_revision_rounds` — but every route re-entry gets a
FRESH loop, so the route cycle itself is unbounded while satisfying the rule's
letter. Checker accepts (arguably correctly per spec text). Sibling question: what
"single-assignment per scope" means under route re-entry is unstated — the checker
permits rebinding across re-execution (run 2). Both belong in the advisory backlog
next to F2 (after-vs-route) and the retry-semantics ruling; F2 + F20 + F21 are
really one family: the spec's control-flow story is written step-forward, and the
route-graph corner cases live in checker behavior only.

## Legal-but-ugly (self-caught, verbatim confessions)

- Invented `type Ack { ok: Bool }` solely because action declarations require a
  return type — F7's wrapper-type invention, reproduced by a fluent author.
- The `complete` step exists only to satisfy F19a — a workflow noun with no
  domain meaning.
- `Finding.must_fix: Bool` duplicates `Severity` because I flinched from
  `filter(.severity == Blocking)` without even probing it — F12's shadow. Honest
  note: unprobed; the flinch itself is the datapoint (spec's "arguments are
  `.field` accessors or literals" reads as forbidding it, so I didn't try).
- Wrote `node <x>` on every action out of example-gravity (F13 confirmed for
  fluent authors), though I did vary the node names meaningfully.

## Can't-say list (reached for it; the language refused)

1. List concat / flatten (F16) — worker action instead.
2. Zero-iteration loop (F17) — wasted dispatch instead.
3. Read a field of a timed-out-signal binding in the VM (F15) — worker hop instead.
4. Unconditional outcome clause (no `when true` / lone `otherwise` shown anywhere;
   statement route fills the hole, but only because F20 turns out to be legal).
5. Configurable durations: `timeout`/`sleep`/`wait timeout` take literal durations
   only per grammar; `max` takes expressions. A 48h ruling deadline that should be
   config is hardcoded. (Unprobed at the boundary — noted from grammar reading.)

## Confidence (stated before any verdict, per protocol)

Semantic correctness of doc_certification.awl against its own design intent: 0.9.
Static pipeline is green end-to-end on the fresh binary and the domain is mine;
residual risk sits in runtime semantics no static gate here can see
(fall-through-after-on-failure interplay, backward-route re-entry state,
fork-branch failure interaction with `on failure`) — exactly the class a live run
would settle.
