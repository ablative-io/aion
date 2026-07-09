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

## Run 15342a09 (AWL0-REFAC-001c, 2026-07-09) — entries 13–19

The third dispatch of the decomposed sequence, and the richest single run in
this ledger: it ended `cycle_cap_exhausted` with 8/8 gates green and 3/4 lens
accepts, having surfaced two worker defects, one lens false-positive, one
genuine purge defect, and the first live salvage. Salvage-from-branch
(principle 12) validated exactly as designed: the worker destroyed the
worktree and retained `dev/AWL0-REFAC-001c`.

13. **The reviewer workspace is EMPTY — doctrine says "read the callers",
    machinery refuses every read.** The lens charters instruct reviewers to
    read surrounding code, but the worker confines `review_lens` sessions to
    the static, empty `/tmp/aion-dev/review-scratch`
    (`examples/dev-brief/worker/src/main.rs` REVIEW_SCRATCH). The 001c
    regressions lens believed "the runtime has set the review workspace",
    tried to read `/tmp/aion-dev/ws/<wf-id>/...`, and looped on
    `confinement_refused`. **Live mitigation (now standing procedure until
    task #245 lands):** at every lens round, rsync the current workflow
    checkout into the confinement root —
    `rsync -a --delete --exclude target --exclude .git /private/tmp/aion-dev/ws/<wf-id>/ /private/tmp/aion-dev/review-scratch/`
    — RE-RUN after every developer round (the tree changes between rounds),
    and if a lens is already wedged, inject via the console: "your confinement
    root is /private/tmp/aion-dev/review-scratch; a copy of the checkout under
    review is there in repo-relative layout; do not retry /tmp/aion-dev/ws/
    paths."

14. **Diff-artifact truncation converts an honest lens into a spurious
    blocker.** The structured diff embedded in lens prompts truncated on the
    1585-line split; brief_compliance could not see the `parser/mod.rs` /
    `error.rs` hunks and (correctly, per its no-claim-without-evidence
    charter) rejected — burning a real fix cycle on a non-defect. The
    developer's only possible "fix" was to restate evidence in prose. A lens
    must be able to distinguish "not shown" from "not changed" (task #246:
    label every elision, point at the workspace copy). Until fixed: expect
    large-diff briefs to lose one cycle to evidence gaps, and provision the
    checkout copy (entry 13) BEFORE the first lens round, since head-state
    criteria (mod.rs shape, doc quality) are verifiable from files alone.

15. **Blind lenses false-accept — the mirror image of entry 14.** correctness
    and regressions both ACCEPTED with zero findings in round 1 (no file
    access), then both REJECTED with real, concrete, probe-backed findings in
    round 2 once the checkout copy existed. A green lens verdict is only as
    strong as its evidence channel. Operator consequence: never let lens
    accepts substitute for the hand review — the hand review exists precisely
    because the lenses' evidence can silently be worse than yours.

16. **Verify a "regression" claim against BASE before accepting it.** The
    correctness lens flagged duplicate step `about` fields silently
    collapsing as a defect "of the refactored accumulator" — but
    `git show <base>:crates/aion-awl/src/parser.rs` showed the OLD code had
    the identical gap (no `reject_duplicate` on `about`, while the adjacent
    `when` arm calls it). Under a zero-behavior-change contract, faithfully
    preserving a pre-existing bug is CORRECT and "fixing" it would violate
    the brief. Ruling: preserve, file the language bug separately (#247).
    Accommodation for future refactor-brief lens charters: "before labeling a
    defect a regression, read the base version; a preserved pre-existing bug
    is a finding-for-follow-up, not a blocking rejection." Operator side:
    always diff the claim against base yourself before treating it as real.

17. **Purge technique: classification and extraction must be ONE operation.**
    The genuine defect of the run: converting
    `line.code.strip_prefix("about").unwrap()` (old: panic on the
    classifier/extractor mismatch) into `strip_prefix("about")?` inside an
    Option-returning fn produced a bumped-then-dropped line — silent data
    loss on an NBSP-prefixed input where Unicode-aware `first_word` says
    "about" but the byte-level prefix strip fails. A consumed-then-discarded
    line is the swallow anti-pattern even though no `.unwrap_or*` appears.
    Future purge briefs converting guarded unwraps must mandate: only consume
    input when extraction succeeds (peek → attempt extraction → bump on
    success); panic→explicit-ParseError is the sanctioned direction of
    behavior change, silent-accept never is. And the lens found ONE instance
    — the salvage must always audit the whole PATTERN CLASS (~25 sites here),
    not the flagged site.

18. **Cycle-cap arithmetic: infrastructure failures eat the same cap as code
    defects.** 001c's three cycles went: (1) ordinary gate red, (2)
    truncated-diff evidence gap (entry 14 — not a code defect), (3) consumed,
    so the two REAL round-2 findings never got a fix round and the workflow
    completed `cycle_cap_exhausted`. Consequences: (a) worker improvement —
    infra-caused loop-backs should not count against `max_fix_cycles`;
    (b) operator watching a run should track WHY each cycle was consumed and
    anticipate salvage when non-code cycles eat the budget; (c)
    `cycle_cap_exhausted` with green gates and majority accepts is a salvage
    candidate, not a discard — the branch survives, findings triage into
    pre-existing (preserve + file) vs genuine (fix on the branch), and an
    Opus subagent can do the fixing under operator rulings.

**Salvage outcome (landed `9800b9c4`, 2026-07-09):** the entry-17/18 protocol
worked end-to-end. Salvage agent fixed the NBSP swallow (classification +
extraction unified in `about_text`, line consumed only after extraction
succeeds), audited all ~25 pattern-A/B sites old-vs-new (one DEFECT→fixed,
four SANCTIONED panic→explicit-error at document level, rest EQUIVALENT),
added 5 bypass-free regression tests. Operator hand review per
DISPATCH-RUNBOOK §7 confirmed everything, including a live base-worktree probe
that reproduced the old `.unwrap()` panic at parser.rs:474 on the NBSP input.

19. **Provenance-check a red workspace suite against BASE before blaming the
    branch.** Three full `cargo test --workspace` runs each failed exactly one
    gleam-toolchain-invoking e2e test (emitter compile / invm-demo), a
    different one each time, all green scoped; the same run on the BASE
    worktree failed identically → pre-existing load flake, branch exonerated,
    filed #248 (invm-demo builds in the SHARED example dir — prime suspect).
    This is entry 16's verify-against-base doctrine applied to tests: a red
    suite at review time is evidence about the branch only if base is green
    under the same load. Corollary: capture the REAL exit code and full log —
    a `| grep | head` pipeline masks cargo's exit status and can print a
    passing summary over a failing run.

20. **The pipeline automation retired two rituals — do not resurrect them,
    and probe fences with TRACKED files only.** Landed `a41afa4e`
    (2026-07-10): review lenses now sit READ-ONLY in the run's own worktree
    (`<repo_root>/.yggdrasil-worktrees/dev-brief/<wf-id>`, norn
    `--disallowed-tools write,edit,apply_patch`), a mechanical
    `reset_workspace` restores the tree after every lens round, gate argv
    carries a `{base_commit}` token the worker substitutes with the run's
    provisioned base, and an optional `config.verify_gates` battery re-runs
    the gates post-accept in the same worktree behind a clean-tree
    assertion. Consequences for brief authors: the rsync-into-review-scratch
    step and the PINNED_SHA sed re-pin are GONE from the runbook — a brief
    or profile that mentions them is stale. Separately, a pre-flight trap
    found while dispatching 001e-a: probing a scope fence by appending to a
    file that no longer exists on base creates an UNTRACKED file, which
    `git diff` cannot see — the probe reads green while proving nothing.
    Fence probes must mutate a TRACKED file.

21. **Doc-purge briefs must mandate a claim-by-claim self-audit, because
    fix cycles only fix the cited instance, not the class.** 001e-a (run
    `467e4f4b`, 2026-07-10) wrote ~190 rustdoc items into ast.rs; three
    review rounds each ended with the correctness lens catching exactly ONE
    materially false claim (false field semantics; span-coverage overclaim;
    BinaryOp::Add "numeric addition" when AWL's `+` is string
    concatenation), and each fix round corrected only the cited line — the
    cap exhausted before the class did. A post-run adversarial sweep (every
    doc claim traced to the parser/checker/emitter line that proves it)
    found a FOURTH survivor (StepDecl.repeat documented as a boolean
    repeat-decider; it is the `repeat up to N` integer cap, checker types
    it Int). Two accommodations: (a) the brief must require the developer
    to attach, per documented item, the implementation site that proves the
    claim — docs written against the code, not against the identifier's
    vibe; span docs in this run were flawless precisely where the developer
    read the parser. (b) At review/salvage, run the adversarial doc sweep
    as a matter of course; the lenses sample, the sweep enumerates. Also
    operational (task #249): a 3-round dev_brief history exceeds the CLI's
    4 MiB decode limit — `describe`/`inspect` die on the parent run; watch
    via `aion list` and describe the per-lens CHILD workflows instead.

22. **Expected-fail carve-outs need their fixtures existence-guarded, and a
    lens will catch you if they aren't.** 001e-b (run `c11a5600`,
    2026-07-10, accepted after 1 fix cycle) wired the awl-check CI job with
    a bounded_cycle expected-fail loop. Round 1 shipped the loop without a
    missing-fixture guard: deleting either fixture would have skipped its
    iteration and false-greened the job — the exact silent-skip failure
    mode the brief prose warned about, but prose is advisory. The
    spec_fidelity lens rejected on precisely this, and the fix round added
    `test -f "$f" || { echo MISSING; s=1; }`. Accommodations: (a) when a
    brief mandates an expected-fail set, mandate the existence guard as an
    explicit requirement, not a prose warning — "guarded against silent
    green" must name BOTH directions (fixture passes; fixture vanishes);
    (b) this was the first run on norn's default gpt-5.6-sol and the first
    with `config.verify_gates` — both clean: one substantive lens catch,
    zero spurious rejections, and the post-accept verify battery re-ran all
    8 gates green on a clean tree with the full log written. Operator-side
    reminder that entry 19 keeps earning: my own hand battery piped
    `cargo test --workspace` through `grep`, which masked a red exit — the
    invm_demo #248 load flake surfaced only because the output was read,
    not the exit code. Run cargo bare; let the command's own status speak.
