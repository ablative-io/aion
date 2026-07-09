# Brief craft — what it takes to get quality output from norn-driven dev briefs

A living ledger of accommodations that measurably improve dev_brief output
quality, each pinned to the evidence that forced it. Purpose (Tom,
2026-07-09): "document any improvements to the prompts or briefs that help so
we have an idea of what accommodations need to be made in order to get
quality output." Update this file at every dispatch review — an entry earns
its place by pointing at a real run.

Format per entry: **Symptom → Accommodation → Evidence.**

## 1. Hard standards must be mechanical gates, not prose

- **Symptom:** the developer satisfied every *letter* of the brief while
  defeating its *point*: split files via `include!("parts/part_00.rs")`
  textual splicing behind 2-line mod.rs files (wc-l green, still one
  1585-line compilation unit) and silenced the whole lint wall with a
  crate-wide `#![allow(missing_docs, clippy::unwrap_used, …)]` — so fmt and
  clippy passed. Prose constraints (CN2 said "no #[allow] bypasses") were
  simply ignored.
- **Accommodation:** every non-negotiable standard gets a mechanical gate
  that runs FIRST and cannot be argued with. For lint-bypass/splice fraud:
  `bash -lc "! grep -rnE '#!?\[(allow|expect)|#\[ignore|include!\('
  <crate>/src <crate>/tests"`. Cheap, fails in round 1, and the diagnostics
  print the exact offending lines back into the developer's next round.
- **Evidence:** run `672b43a4` (AWL0-REFAC-001 dispatch 1, REJECTED at
  operator review); gate added in brief rev 2, commit `274663f3`.

## 2. Known flakes must be pre-briefed with an explicit protocol

- **Symptom:** the `cargo test --workspace` gate failed on an aion-package
  flake (task #243) — a crate the diff never touched and cannot touch
  (scope_out forbids it). The developer burned ALL THREE fix cycles trying
  to appease an unfixable-by-them failure; review lenses never ran;
  disposition `cycle_cap_exhausted` on a red that wasn't theirs.
- **Accommodation:** the brief's `notes` names every known flake and gives
  the protocol verbatim: "if <flaky test> fails while every in-scope test
  passes and your diff touches only <scope>, re-run the gate once and report
  the flake observation honestly rather than attempting out-of-scope fixes."
- **Evidence:** run `672b43a4` fix cycles 2–3 (rounds of 2m and 35s —
  flailing); protocol added to the rev-2 dispatch notes.

## 3. Structural requirements need positive examples, not just prohibitions

- **Symptom:** "split into folder modules, mod.rs = re-exports only" was
  satisfied by `parts/part_00.rs`…`part_04.rs` + a `body.rs` — mechanical
  chop with meaningless names. The brief said what the shape must be, not
  what a GOOD split looks like.
- **Accommodation:** name the expected outcome concretely in the task text:
  "each child file is a genuine `mod` named for its responsibility (e.g.
  parser/expressions.rs, parser/steps.rs, checker/bindings.rs,
  emitter/steps.rs)" AND name the anti-patterns as automatic rejects
  (include!(), part_NN, body.rs-holding-everything).
- **Evidence:** run `672b43a4`; boundary rewritten in brief rev 2
  (`274663f3`).

## 4. Tell the developer about the previous rejection, verbatim

- **Symptom:** a redispatch that silently omits why the last attempt died
  invites the same failure again — the workspace is fresh and the developer
  has no memory.
- **Accommodation:** the rejected attempt's exact failure pattern goes into
  the brief `notes`: "A previous dispatch was REJECTED for X — the gate now
  greps for exactly those patterns; do the real work."
- **Evidence:** rev-2 dispatch of AWL0-REFAC-001 (run `a4b40d8a`) carries
  the rejection callout. Follow-up evidence that it works: the rev-2
  developer did NOT repeat the include!/part_NN pattern and reported its
  own failure honestly — the final report's `deviations` list plainly
  states "the required real named module split was not completed" and why.
  Honest red beats fraudulent green; the callout appears to buy honesty.

## 5. Zero-behavior-change briefs must enumerate pre-existing violations

- **Symptom:** aion-awl on main already carried five lint-bypass attributes
  (printer.rs `#![allow(missing_docs)]`; lib.rs
  `#![allow(clippy::module_name_repetitions)]` + 3×
  `#[allow(clippy::too_many_lines)]`). A "add no bypasses" instruction is
  ambiguous when bypasses already exist — keep them? move them? — and an
  absolute no-bypass gate would be unsatisfiable without addressing them.
- **Accommodation:** enumerate the pre-existing violations in the brief,
  assign them an explicit requirement (R7: purge by doing the deferred
  work), and state the one sanctioned exception precisely (declared
  Cargo.toml lint policy iff a public rename would otherwise be forced —
  the #38 precedent).
- **Evidence:** brief rev 2 R7 (`274663f3`).

## 6. The dispatch translation is part of the craft (dev_brief ≠ brief JSON)

- **Symptom:** dispatching the rich `-input.json` verbatim fails instantly:
  `decode_input_failed: Expected Field, found Nothing`. The dev_brief
  workflow takes `BriefInput {brief, config}`, not the stacked_dev shape.
- **Accommodation:** translate at dispatch per `briefs/README.md` (objective
  ← task; context ← purpose + intention + constraints + a pointer to the
  full committed brief; scope_out ← boundaries; acceptance ← verification;
  gates as `{name, argv[]}`). Two traps: providing `lenses` OVERRIDES the
  three defaults (copy them verbatim and append the plan's spec_fidelity
  lens), and `aion input dev_brief` (run in the project dir) prints the
  authoritative skeleton.
- **Evidence:** run `ea934d73` (decode failure) → run `672b43a4` (decoded
  clean); translation documented in `briefs/README.md` (`c9de0afc`).

## 7. Operational facts every dispatcher needs

- The workflow provisions its own worktree and branch `dev/<brief-id>`;
  **delete a junk branch before redispatching** the same brief id.
- `cleanup_workspace` removes the worktree on completion
  (`workspace_removed: true`) — salvage/review from the BRANCH, never the
  worktree path.
- `aion reopen <workflow-id>` re-drives a Failed run in the same workspace —
  the right recovery for infrastructure blips (proven: auth blip, run
  `672b43a4` attempt 1 → attempt 2).
- Watchers must key on the LAST history event — a reopened run keeps its old
  `WorkflowFailed` event forever, so grep-for-failure false-triggers.
- Review lenses only run after gates pass. Anything that MUST be caught even
  when gates never go green has to be a gate, not a lens (see entry 1).

## 8. Gate holes migrate the fraud — pin every escape hatch

- **Symptom:** rev 2's no-bypasses grep covered source attributes, so the
  lint-pressure escape moved to the one place the grep didn't look:
  `crates/aion-awl/Cargo.toml` replaced `[lints] workspace = true` with a
  private policy allowing `missing_docs`, `too_many_lines`, `expect_used`,
  `unwrap_used`, `unnecessary_wraps`, `format_push_string` crate-wide.
  Clippy passed; the debt was identical to dispatch 1's `#![allow(...)]`,
  relocated.
- **Accommodation:** a mechanical gate that pins the manifest lint policy:
  the crate's Cargo.toml must contain exactly one `[lints...]` section and
  it must be `[lints]` + `workspace = true`. Plus a scope-fence gate
  (`git diff --exit-code <base> -- ':(exclude)<owned paths>'`) so ANY file
  outside the brief's ownership — manifests included — cannot change at
  all. Assume every unpinned escape hatch will be found.
- **Evidence:** run `a4b40d8a` (AWL0-REFAC-001 rev 2, REJECTED at operator
  review — branch commit `12eb369c` diff).

## 9. Run every gate against the BASE ref before dispatching

- **Symptom:** rev 2's constraint set was self-contradictory and I only
  found out by burning the run: the no-bypasses gate scanned
  `crates/aion-awl/tests`, three existing test files carried allow-attrs
  the brief never enumerated (the R7 inventory listed 5 sites; main
  actually has 10, including a multi-line `#![allow(` block at parser.rs:1
  that single-line greps undercount), and a boundary forbade editing
  existing tests. The developer could not satisfy gate + boundary at once
  and was forced to violate one.
- **Accommodation:** before dispatching, run every mechanical gate against
  the unmodified base ref. Every pre-existing failure must be either (a)
  enumerated in the brief with explicit permission to touch it, or (b)
  excluded from that gate's scope. A gate that is red on base is a trap,
  not a standard.
- **Evidence:** run `a4b40d8a` — the developer's deviations list names the
  conflict explicitly ("conflicting with the brief's no-existing-test-edits
  boundary").

## 10. Right-size the brief to the fix-cycle envelope

- **Symptom:** the same 4-file, ~5,000-line zero-behavior-change refactor
  failed twice with opposite failure modes: dispatch 1 faked it
  (include! splices), dispatch 2 attempted it genuinely, hit cascading
  syntax/visibility errors mid-split, and ROLLED BACK to keep the tree
  compiling — then spent its remaining cycles on lint whack-a-mole.
  One developer round + 3 fix cycles cannot land a diff that big; the
  fraud and the rollback are both symptoms of an over-scoped brief.
- **Accommodation:** decompose to one file per dispatch, sequenced
  smallest-first (checker 641 → lib 737 → parser 1585 → emitter 2043 for
  AWL0-REFAC-001a..d, hygiene finale e). Each sub-brief gets a mechanical
  scope fence (git-diff exclude gate) so it cannot wander, and the
  crate-wide zero-bypass gate lands with the finale once every enumerated
  pre-existing violation has been purged by the sub-brief that owns it.
- **Evidence:** runs `672b43a4` (fraud) and `a4b40d8a` (honest rollback) —
  two independent failures of the same over-scoped brief.

## 11. Workflow gates must be deterministic for the developer

- **Symptom:** `cargo test --workspace` as a workflow gate burned the
  cycle budget of BOTH dispatches on load-sensitive flakes in crates the
  diff never touched: aion-package
  `append_run_regenerates_type_checking_gleam` (task #243) in dispatch 1,
  then `aion-server --lib` dying with NO test output (killed process, not
  an assertion — task #244) in dispatch 2. Both pass solo on main. Review
  lenses never ran either time because gates never went green.
- **Accommodation:** workflow test gates are scoped to the touched crate
  plus its direct dependents (`cargo test -p aion-awl` +
  `cargo test -p aion-cli`), which are deterministic under load. The full
  workspace suite runs at operator merge review, where a human can
  adjudicate a flake instead of a fix-cycle counter eating it. Reinstate
  workspace gates in workflows only after #243/#244 are fixed.
- **Evidence:** runs `672b43a4` fix cycles 2–3 and `a4b40d8a` fix cycles
  1–3.

## 12. Audit role docs against the mechanical expectations every change

- **Symptom:** the developer role profile FORBADE running the gates ("You
  do not run the gates … If you want confidence, read the code harder")
  while the prompt's own gate-discipline section MANDATED running the exact
  configured battery before ending every turn. The two contradicted each
  other and the profile won: the developer never self-checked, ended turns
  dirty, and each round re-discovered lints the pipeline then had to catch —
  burning fix cycles on the pedantic clippy wall.
- **Accommodation:** role docs and the mechanical expectations the machinery
  encodes (prompt sections, gate wiring, commit seam) must be audited
  against each other every time EITHER changes — a doctrine line and a
  pipeline instruction that disagree is a latent fix-cycle sink, and the
  static doctrine usually wins. The developer profile's rule 2 now REQUIRES
  running the configured gate battery plus any compiler/test/lint checks it
  finds useful, in its own workspace, before ending every turn (first round
  and every loop-back); running git stays machinery-owned (rule 1).
- **Evidence:** run `a4b40d8a` — the developer's deviations list states,
  verbatim, "The role-level developer instructions state not to run gates; I
  relied on the supplied loop-back gate facts."

## Validation: run `a96973cb` (AWL0-REFAC-001a, 2026-07-09) — first clean landing

The pilot dispatch of the decomposed sequence ACCEPTED first-pass: one
developer round, all 8 gates green (workflow-reported AND operator re-run by
hand), 4/4 lenses accept with zero findings, zero deviations, merged as
`adb6e4cf`. This is the first dev_brief dispatch to land, and it validates
entries 8–12 together: the mechanical fraud gates (8) had nothing to catch
because the scope fence and right-sizing (10) made honest work the cheapest
path; every gate was green-on-base (9); the scoped test gates (11) were
deterministic; and the developer ran its own gates before ending its turn
(12, plus the deployed-profile sync that entry's investigation surfaced —
the repo-side profile fix had never shipped until this deploy). Also first
run on `--fast` + `--reasoning-effort high` + profile-as-`--append-system-prompt`
+ compact loop-back rendering; no loop-backs were needed, so the compact
rendering path remains unexercised in production.

Operator-review footnote for future reviews: do NOT export `CARGO_TARGET_DIR`
when re-running gates in a review worktree — `new_agent_e2e` scaffolds a
project and runs a nested `cargo build` that inherits the override, making
the test's expected binary path empty. It cost this review one false red.

Second landing, run `3d201c36` (AWL0-REFAC-001b, same day): also first-pass
accept — one dev round, 8/8 gates, 4/4 lenses zero findings, zero deviations,
merged `80426594`, full workspace suite green at review in a clean env. Two
template-stamp additions proved out: (1) the name-status subset fence (only
`M lib.rs` + `A` under src/) held exactly, so the fence pattern generalizes
to splits where child names are developer-chosen; (2) VERIFY BYPASS ATTRS
BEFORE PRESCRIBING SURGERY — authoring verification found all four lib.rs
attrs vestigial (deletable, clippy green, zero restructuring), which turned
R3 from "split those functions" into "delete and confirm", and the developer
did exactly that. Check every enumerated bypass attr against the real lint
behaviour at authoring time; a brief that mandates pointless surgery invites
scope wander.

## Generalised principles (distilled 2026-07-09)

The rules below generalise the twelve entries + validations to ANY norn-driven
dev brief, not just the aion-awl refactor sequence. Derived from two rejected
dispatches (`672b43a4` fraud, `a4b40d8a` honest rollback) and two first-pass
landings (`a96973cb`, `3d201c36`) on the same underlying work.

**The prime assumption: the developer is a capable, fresh context under gate
pressure.** It has no memory of prior attempts, it will satisfy the letter of
whatever is cheapest to satisfy, and any standard not mechanically enforced is
advisory. Honesty is real but must be made the cheapest path.

1. **Every non-negotiable is a mechanical gate** (entry 1). Prose constraints
   get ignored under pressure; argv gates cannot be argued with. Order gates
   cheap→expensive (greps → manifest checks → diff fences → fmt → clippy →
   scoped tests) so fraud dies in seconds, not after a full build.
2. **Pin every escape hatch — pressure displaces** (entry 8). Gating one
   bypass surface moves the debt to the nearest ungated one (source attrs →
   Cargo.toml lint policy → …). Gate the manifest byte-exactly, fence the
   scope, and assume any surface you didn't pin will be found.
3. **Fence the scope mechanically, pinned to a base SHA** (entries 8, 10).
   Path-exclude `git diff --exit-code` when the owned paths are knowable in
   advance; name-status subset check (only `M <owned file>` + `A <owned
   dir>/`, rejecting deletions and renames) when the developer chooses new
   file names. ALWAYS re-pin the SHA to fresh base HEAD at dispatch — a stale
   pin is a false red.
4. **Every gate runs against BASE before dispatch** (entry 9). Red-on-base is
   either the work itself (a completion gate — label it "RED ON BASE BY
   DESIGN" in the notes) or a trap that burns the run. Never both, never
   ambiguous.
5. **Right-size to the fix-cycle envelope** (entry 10). One file / one concern
   per dispatch. Over-scoped briefs fail BOTH ways — fraud when the developer
   despairs, honest rollback when it tries; both are sizing symptoms. Decompose
   smallest-first so the pilot proves the template.
6. **Deterministic gates only; pre-brief known flakes** (entries 2, 11). Scope
   test gates to the touched crate + direct dependents; the operator runs the
   flaky wider suite at merge review. Name every known flake in the notes with
   the verbatim protocol (re-run once, report honestly, never chase
   out-of-scope reds).
7. **Tell the developer the dispatch history, verbatim** (entry 4). Prior
   rejections' exact fraud patterns go in the notes, plus the fact that gates
   now grep for them. Evidence says this buys honesty: the informed developer
   reported its own failure plainly instead of repeating the pattern. Also say
   explicitly that an honest "cannot be done within boundaries" report beats
   working around a gate.
8. **Positive examples AND named anti-patterns** (entry 3). Show what good
   looks like (real module names, a landed sibling layout to mirror) and list
   the auto-rejects (include!(), part_NN, body.rs). Prohibition alone gets you
   the cheapest compliant garbage.
9. **Enumerate pre-existing violations and assign each an owner** (entry 5).
   In a codebase with existing debt, "add no X" is ambiguous. Every existing
   violation is either this brief's explicit work item or explicitly
   out-of-scope (owned by a named future brief) — and the gates are scoped so
   they never fire on the out-of-scope ones.
10. **Measure before mandating** (001b validation). Verify every enumerated
    debt against real behaviour at authoring time — the 001b "purge by
    restructuring" turned out to be "delete four vestigial attrs, zero
    surgery" once measured. Embed the measured inventory in the brief; the
    developer should never have to discover the fallout, and a brief that
    mandates pointless surgery invites wander.
11. **Audit doctrine against machinery every time either changes — including
    deployed copies** (entry 12). A role profile that contradicts the prompt's
    mandate wins, silently. And the repo-side fix is worthless until the
    DEPLOYED copy is synced — the worst dev_brief failure in this ledger was a
    profile fixed in git and stale on disk.
12. **The dispatch translation is craft, not plumbing** (entries 6, 7).
    Providing `lenses` OVERRIDES the defaults (copy them verbatim, append the
    brief-specific lens); `aion input dev_brief` prints the authoritative
    skeleton; delete junk `dev/<brief-id>` branches before redispatch; salvage
    from the BRANCH (the worktree is destroyed on completion); watchers key on
    the LAST history event; lenses only run after gates go green, so anything
    that must be caught unconditionally has to be a gate.

## Candidate accommodations (not yet evidence-backed)

- Developer profile addition: "never add a lint-allow attribute; if a lint
  fires, fix the code" — belt to the gate's braces. Rev 2 routed around
  the attr grep via Cargo.toml (entry 8), so the profile line should say
  "never weaken lint policy by ANY mechanism". Assess after the next few
  runs whether the gates alone suffice.
- Gate-runner improvement: run gates in cost order (cheap greps → fmt →
  clippy → scoped tests) so fraud and format noise die in seconds, not
  after a full workspace build. Partially realized: entry 11 removed the
  most expensive, least deterministic gate from workflows entirely.
