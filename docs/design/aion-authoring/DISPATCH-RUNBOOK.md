# dev_brief Operator Runbook

The complete operator procedure for dispatching development briefs through the
Aion `dev_brief` pipeline: authoring, verification, dispatch, live operations,
outcome handling, hand review, merge, and bookkeeping. Written so a human
operator (or any agent) can run the entire process end-to-end without prior
context. Companion doc: `awl/BRIEF-CRAFT.md` (the evidence ledger for WHY each
step is shaped this way — read its 12 generalised principles first).

**Prime rule: NEVER trust the workflow's own green.** Every gate is re-run by
the operator's hands before merge. Merges and pushes are operator actions
only; no agent or workflow merges to main. The post-accept **verify stage**
(below) re-runs a configured battery mechanically and records the result — it
saves the operator retyping, it does NOT replace the operator hand-review in
§7, which remains the real bar.

---

## 0. Prerequisites (check before any dispatch)

- Aion server running; ops console at its usual address. Worker for
  `dev_brief` + `review_lens` running (check the plist / process list).
- The deployed profiles directory is a COPY, not the repo:
  `/Users/tom/Developer/ablative/deploy/aion/dev-brief-profiles`. **Any
  profile change in the repo is inert until synced to that directory and the
  worker restarted.** A stale deployed profile was the worst pipeline failure
  on record — check sync whenever behaviour looks profile-shaped.
- **Worktree location.** Each run's worktree now lives INSIDE the target repo
  at `<repo_root>/.yggdrasil-worktrees/dev-brief/<workflow-id>` (git-ignored),
  not under `/tmp/aion-dev/ws`. It is still destroyed at completion (salvage
  from the BRANCH). The review lenses are rooted at this SAME worktree,
  read-only (their write/edit/apply_patch tools are denied), so the old manual
  rsync-into-review-scratch ritual is gone (tasks #245/#246 — landed).
- Norn runs in **driven mode only**. Developer sessions run with `--fast` and
  `--reasoning-effort high`. Profiles ride as `--append-system-prompt`
  (never `--system-prompt`).
- Known workspace-suite flakes (tasks #243 aion-package, #244 aion-server
  --lib): while open, workflow test gates stay SCOPED (`-p aion-awl`,
  `-p aion-cli`); the operator runs the full workspace suite at merge review.

## 1. Brief authoring (delegate to an Opus subagent)

One brief = one file / one concern, sized to the fix-cycle envelope
(1 developer round + `max_fix_cycles` fix rounds; we use 3). The authoring
agent must:

1. **Read first**: the plan entry for the brief (the CONTRACT — plan wins over
   the tasking prompt), `awl/BRIEF-CRAFT.md` in full, and the two most recent
   landed brief pairs as templates (stamp, don't improvise).
2. **Measure first** in a clean detached worktree on current origin/main
   (`git worktree add --detach <scratch> origin/main`):
   - For a purge brief: delete ONLY the bypass block, run
     `cargo clippy -p <crate> --all-targets -- -D warnings`, enumerate EVERY
     error verbatim (lint, item, line; note line-offset from the deleted
     block), and collapse unwrap/expect sites into repeating patterns with the
     sanctioned in-file fix helper named.
   - For a split brief: exact line count, seams (read the file properly),
     proposed responsibility-named child modules each ≤500 lines.
   - **Re-export traps**: grep the whole tree for `use crate::<module>::` and
     check `lib.rs`'s `pub use` — every symbol consumed elsewhere must be
     re-exported by the new `mod.rs` byte-compatibly (consumers are out of
     scope and uneditable).
   - **STOP-conditions** (flag, never silently author around): any public-fn
     signature change required; fallout too large for one dispatch after
     pattern-collapse (author anyway, RECOMMEND a split — operator decides).
3. **Author exactly two files** in `docs/design/aion-authoring/awl/briefs/`:
   `<ID>-input.json` (the durable R1–RN contract) and `<ID>-dispatch.json`
   (the BriefInput translation — see §2 for the shape). Embed the measured
   inventory VERBATIM in the notes; the developer must never discover fallout
   mid-run.
4. **Verify gates on base** (§3 checklist) and record exit codes.
5. **Commit ONLY the two files** (explicit paths, never `git add -A`), push.
   If push rejected because main moved: `git pull --rebase`, re-push.

## 2. The BriefInput dispatch contract

```
{ "brief": { "id", "title", "objective", "context", "pointers": [],
             "scope_in": [], "scope_out": [], "acceptance": [], "notes" },
  "config": { "repo_root": "/Users/tom/Developer/ablative/aion",
              "base_branch": "main",
              "max_fix_cycles": 3,
              "gates": [ { "name", "argv": [] }, ... ],
              "lenses": [ { "name", "charter" }, ... ] } }
```

- `lenses` **OVERRIDES** the workflow defaults. Always supply the standard
  four: `correctness`, `regressions`, `brief_compliance`, plus
  `spec_fidelity` re-chartered to THIS brief's plan entry and R-contract
  (copy from the last landed dispatch file, adapt the spec_fidelity text).
  Include the docs-quality clause: placeholder docs are a blocking finding.
  For refactor briefs add to correctness/regressions charters: "before
  labeling a defect a regression, read the base version of the code — a
  faithfully preserved pre-existing bug is a finding-for-follow-up, not a
  blocking rejection" (BRIEF-CRAFT entry 16).
- `notes` must carry: DISPATCH HISTORY verbatim (prior rejections and
  landings — it demonstrably buys honesty), TECHNIQUE, the MEASURED
  INVENTORY, SIZING judgment, and the GATES paragraph (green-on-base
  evidence, red-by-design labels, and the #243/#244 flake-honesty protocol).
  The manual PINNED_SHA re-pin instruction is GONE — use the `{base_commit}`
  token (below); the worker substitutes it with the provisioned base commit at
  gate-run time and records the resolved argv in the gate run.
- `config.verify_gates` (OPTIONAL, same shape as `gates`): the post-accept
  verification battery. On an `accepted` run the worker re-runs it in the
  clean worktree BEFORE cleanup, asserting a clean tree first (`git status
  --porcelain` empty ⇒ the directory is the branch head ⇒ the gates test
  exactly what merges), and attaches the outcome to the result's
  `verification`. A red verify gate is EVIDENCE ONLY — it never loops back and
  never changes the disposition. Full untruncated logs land at
  `<repo_root>/.yggdrasil-worktrees/dev-brief/logs/<workflow-id>-verify.log`.
  Stamp `verify_gates` with the expensive battery (fmt/clippy/tests) so the
  accepted result carries a mechanical merge-readiness signal; omit it (or
  leave it empty) to skip the stage entirely.
- The workflow provisions its own worktree and branch `dev/<brief-id>`.

### Gate argv library (stamp these; adapt paths per brief)

All gates are `{"argv": ["bash", "-lc", "<script>"]}` except cargo ones.

**no-bypasses-scoped** (GREEN on base; guards landed-clean surfaces):
```
ps="crates/aion-awl/src/checker crates/aion-awl/src/lib.rs crates/aion-awl/src/lexer"; \
[ -e crates/aion-awl/src/parser ] && ps="$ps crates/aion-awl/src/parser"; \
[ -e crates/aion-awl/src/emitter ] && ps="$ps crates/aion-awl/src/emitter"; \
! grep -rnE '#!?\[(allow|expect)|#\[ignore|include!\(' $ps
```
**CRITICAL: only name paths that EXIST (the `[ -e ]` guards).** On this
machine grep is ugrep 7.5.0: a nonexistent path exits 2, and the leading `!`
flips that into a FALSE GREEN. Proven live — a planted bypass passed a gate
that named a deleted file. Never name a deleted or not-yet-created path.

**lints-pinned** (GREEN on base; blocks the Cargo.toml escape hatch):
```
test "$(grep -c '^\[lints' crates/aion-awl/Cargo.toml)" -eq 1 && \
grep -A1 '^\[lints\]$' crates/aion-awl/Cargo.toml | grep -q '^workspace = true'
```

**scope-fence** (GREEN on base; the wander-killer):
```
git diff --exit-code --no-ext-diff {base_commit} -- . \
  ':(exclude)crates/aion-awl/src/parser.rs' ':(exclude)crates/aion-awl/src/parser'
```
Keep the literal `{base_commit}` token in the committed file: the worker
replaces it with the run's provisioned base commit at gate-run time (in every
argv element, for both `gates` and `verify_gates`) and records the resolved
argv in the gate run — no operator sed, no re-pin. Excludes = exactly the
owned paths. (A brief with a literal SHA still works; there is just no token
to substitute.)

**split-complete** (RED ON BASE BY DESIGN — it IS the work; label it so):
```
test ! -f crates/aion-awl/src/parser.rs && test -f crates/aion-awl/src/parser/mod.rs && \
wc -l crates/aion-awl/src/parser/*.rs | awk '$2!="total" && $1>500{bad=1; print "OVER 500:", $0} END{exit bad}' && \
! grep -rnE '#!?\[(allow|expect)|#\[ignore' crates/aion-awl/src/parser
```

**Expensive gates, verbatim, in this order after the mechanical ones**:
`["cargo","fmt"]` · `["cargo","clippy","--workspace","--all-targets","--","-D","warnings"]`
· `["cargo","test","-p","aion-awl"]` · `["cargo","test","-p","aion-cli"]`

## 3. Operator verification of an authored brief (before dispatch)

Never dispatch on the authoring agent's word. Checklist:

1. `git fetch` — the commit is on origin/main and touches EXACTLY the two
   brief files (`git show --stat <sha>`). No junk `dev/<brief-id>` branch
   exists (`git branch -a | grep <id>`; delete if found).
2. **Spot-verify every load-bearing claim**: line citations (`sed -n 'Np'`),
   counts (grep -c), re-export claims (grep the consumers), and ANY novel
   mechanism the agent introduced (reproduce its evidence — when an agent
   says "the old gate form false-greens", plant a synthetic bypass and watch
   both forms with your own eyes). Resolve every discrepancy before
   proceeding — an apparent mismatch usually hides an offset or a
   same-named-method false match; find out which.
3. **Run the gate battery on fresh HEAD** in a clean detached worktree:
   ```
   git worktree add --detach <scratch> <HEAD-sha>
   ```
   Expect: no-bypasses GREEN, lints-pinned GREEN, scope-fence GREEN (with the
   pin substituted to that same sha), split-complete RED (if red-by-design).
   Then plant a synthetic bypass (`printf '#![allow(missing_docs)]\n' >`
   into an owned path) and confirm no-bypasses goes RED. Remove the worktree
   (`git worktree remove --force <scratch> && git worktree prune`).
4. Rule on the sizing recommendation (split vs one dispatch). Over-scoped
   multi-concern dispatches are what killed the parent AWL brief — when in
   doubt, split, but never strand a red-by-design completion gate across two
   dispatches.

## 4. Dispatch

The manual re-pin sed is GONE: the `{base_commit}` token in a scope-fence (or
any gate/verify_gates argv) is substituted by the worker at gate-run time with
the run's OWN provisioned base commit, so the committed dispatch file is
dispatched as-is.

```
python3 -c "import json; json.load(open('docs/design/aion-authoring/awl/briefs/<ID>-dispatch.json'))"   # valid JSON
aion start dev_brief --input-file docs/design/aion-authoring/awl/briefs/<ID>-dispatch.json --pretty
```
Record the returned `run_id` / `workflow_id`. Because the fence pins to
`{base_commit}` (the run's provisioned base), the scope fence is correct no
matter how far main has moved since authoring — no re-verification of a
substituted SHA is needed. Still confirm on the fresh HEAD that gates 1–3 are
green and gate 4 red-by-design (§3) if main moved since you last checked.

**Arm the watcher** (poll every 30s; NOTE: `status` is a read-only zsh
variable — use `st`):
```
wf=<workflow-id>; prev=""
while true; do
  d=$(aion describe "$wf" 2>/dev/null) || { sleep 30; continue; }
  st=$(echo "$d" | jq -r '.summary.status // "unknown"')
  n=$(echo "$d" | jq -r '.history | length')
  last=$(echo "$d" | jq -r '.history[-1].type // "none"')
  cur="$st $n $last"
  [ "$cur" != "$prev" ] && { echo "status=$st events=$n last=$last"; prev="$cur"; }
  case "$st" in Completed|Failed|Cancelled|TimedOut) exit 0;; esac
  sleep 30
done
```

**KNOWN LIMIT (task #249): `aion describe` dies on histories over 4 MiB**
("decoded message length too large") — a 3-round dev_brief run exceeds this,
so the watcher above reads `parse-err` forever on such runs and `inspect`
fails the same way. Fall back to terminal-state polling via
`aion list | jq '.[] | select(.workflow_id=="'$wf'") | .status'`, and read
verdicts from the per-lens CHILD workflows (small histories; find them with
`aion list | jq '.[] | select(.workflow_type=="review_lens")'` filtered by
start time, then `aion describe <child> --pretty | jq
'.history[-1].data.result'`). The worker log
(`deploy/aion/logs/worker-dev-brief.log`) gives the parent's activity
timeline by ordinal.

## 5. Live operations during a run

**Reviewer workspace — no mitigation needed (tasks #245/#246 landed).** Review
lenses are now rooted read-only at the run's ACTUAL worktree
(`<repo_root>/.yggdrasil-worktrees/dev-brief/<workflow-id>`), at the exact
reviewed state — dev work and gate normalization committed, tree clean. A lens
can grep the code and run `git diff <base_commit>` for the full change itself;
its write/edit/apply_patch tools are denied, and the mechanical
`reset_workspace` activity restores the tree after every lens round (recording
any droppings a misbehaving lens left — normally none). The old manual
rsync-into-review-scratch step and the `confinement_refused` console note are
retired; do NOT rsync anything. The diff in the lens prompt is still clipped —
when it shows a `bytes TRUNCATED` marker, the lens is told (in its profile and
prompt) to run `git diff <base_commit>` for the whole change.

**Cycle-cap tracking.** `max_fix_cycles` is shared between gate-red
loop-backs, infra-caused rejections (evidence gaps), and real findings. Track
what consumed each cycle (`aion describe | jq` over `ChildWorkflowCompleted`
results: `.data.result | {lens, overall, findings}`). When non-code cycles eat
the budget, expect `cycle_cap_exhausted` and prepare for salvage rather than
being surprised.

**Weight lens verdicts by their evidence.** An accept from a lens that could
not read files is weak (BRIEF-CRAFT entry 15). The hand review (§7) is the
real bar regardless.

## 6. Outcome handling

`aion describe <wf-id>` → final `WorkflowCompleted` result carries
`disposition`, `branch`, `fix_cycles`, gate diagnostics, the full diff, and —
on an accepted run that configured `verify_gates` — `verification` (the
post-accept battery's `pass`/`pre_clean`/`post_clean`, per-command runs, and
the log-file path). A green `verification` is a mechanical merge-readiness
signal; it does NOT substitute for §7.

- **Accepted (first_pass or after fix cycles)** → read `verification` if
  present (the untruncated log is at
  `<repo_root>/.yggdrasil-worktrees/dev-brief/logs/<workflow-id>-verify.log`),
  then go to §7 hand review regardless.
- **`cycle_cap_exhausted` or rejected** → SALVAGE, not discard, when gates
  are green and the findings are bounded. The branch `dev/<brief-id>`
  survives (the worktree does NOT — salvage from the BRANCH). Procedure:
  1. Pull every rejecting lens's findings verbatim
     (`jq '... select(.overall=="reject")'`).
  2. **Triage each finding against BASE before believing it**:
     `git show <base-sha>:<old-file>` — if the old code has the same
     behaviour, it is PRE-EXISTING: preserve it (zero-behavior-change
     contract), file a follow-up task, do NOT fix on this branch. Only
     genuine defects of the change get fixed.
  3. Dispatch an Opus subagent to fix ON THE BRANCH in its own worktree
     (`git worktree add <dir> dev/<brief-id>`), with the operator rulings
     baked in as binding, an instruction to audit the WHOLE pattern class
     (not just the flagged instance), a new clean test file for the fixed
     defects (no bypass attrs; note the scope-fence will flag it — sanctioned
     salvage extension, report raw + adjusted fence), the full gate battery,
     explicit-path commits, NO push, NO merge.
  4. Then §7 as usual.
  - If the approach itself was wrong: discard the branch and redispatch a
    tightened/decomposed brief (that is how AWL0-REFAC-001a..e was born).

## 7. Operator hand review (before ANY merge — never trust green)

In a clean worktree of the branch (or the branch checkout the subagent used,
after inspecting `git status` is clean):

1. **Fence**: `git diff --exit-code --no-ext-diff <pinned-sha> -- . <excludes>`
   — empty outside owned paths (plus any operator-sanctioned salvage files,
   which you enumerate explicitly, not by pattern).
2. **Structure**: `mod.rs` is declarations + re-exports ONLY (read it);
   every child ≤500 lines (`wc -l`); child names honest (read a sample);
   required re-exports present (grep the consumers still compile-resolve).
3. **Parity** (for splits): function-count parity old vs new —
   `git show <base>:<old-file> | grep -cE '(^|\s)fn '` vs the same grep
   summed over the new files, counted PER-FILE in a loop (multi-arg
   `git show rev:a rev:b` fails silently).
4. **Bypass grep** over the whole owned surface:
   `grep -rnE '#!?\[(allow|expect)|#\[ignore|include!\(' <paths>` → nothing.
5. **Cargo.toml byte-identity**: `git diff <base> -- crates/<crate>/Cargo.toml`
   → empty.
6. **Format**: run the FORMATTER (`cargo fmt`), then `git diff --quiet` —
   never a `--check` variant (formatters may be run; format-check commands
   are never run).
7. **Clippy**: `cargo clippy --workspace --all-targets -- -D warnings`.
8. **Scoped tests**: `cargo test -p aion-awl && cargo test -p aion-cli`.
9. **FULL workspace suite**: `env -u CARGO_TARGET_DIR cargo test --workspace`.
   **NEVER export CARGO_TARGET_DIR here** — `new_agent_e2e` scaffolds a
   nested cargo project that inherits it and false-reds ("worker binary
   missing"). Known flakes #243/#244: if one fires, confirm the diff never
   touches that crate, re-run once, and record the flake honestly.
10. **Behavioral spot-probes** for anything a lens or the brief called out
    (construct the concrete inputs; for parser work check a round-trip case
    and one error-message/span for byte-identity vs base).
11. Read the developer/salvage reports for claimed deviations; verify each.

## 8. Merge + bookkeeping (operator hands only)

```
git checkout main && git pull --ff-only
git merge --no-ff dev/<brief-id> -m "<type>(<area>): <operator-verified message>"
git push
git branch -d dev/<brief-id>
git worktree prune
```
Then the bookkeeping commit (explicit paths):
- Plan entry marked LANDED with run id, disposition, merge sha.
- **BRIEF-CRAFT.md updated at EVERY dispatch review** — new entries earn
  their place by pointing at a real run; include what the run taught even
  when it landed clean.
- The pinned dispatch JSON used (if not already committed) and any new
  follow-up tasks filed.

## 9. Known traps (all bitten live; do not rediscover)

- **ugrep exit-2 false green**: never `! grep` over a path that might not
  exist (§2). Use `[ -e ]` guards.
- **CARGO_TARGET_DIR** in review worktrees → nested-cargo e2e false red (§7.9).
- **zsh `status`** is read-only → name watcher variables `st`.
- **zsh bare `=word`/parens** in echo → command aborts before your real
  command runs; quote or avoid.
- **Multi-arg `git show rev:a rev:b`** silently truncates → loop per file.
- **`| head -N` on test output** can truncate before failures → filter, don't
  truncate, or read the full log.
- **`git worktree add <dir> main`** fails if main is checked out → use
  `--detach`, and never chain a `cd` whose failure leaves later commands
  running in the wrong directory.
- **Deployed profile copies go stale** (§0) — sync + restart worker on every
  profile change.
- **Workflow worktrees are destroyed at completion** — salvage from the
  branch, and copy anything you need from
  `<repo_root>/.yggdrasil-worktrees/dev-brief/<wf-id>` while the run is still
  alive. (The verify-stage log under `.../dev-brief/logs/<wf-id>-verify.log`
  is NOT in a worktree and survives cleanup.)
- **Important inputs never live in /tmp only**: the durable brief pair is
  committed to the repo; the pinned copy in scratch is disposable.

## 10. Current pipeline state pointers

The live sequence state (what is landed, in flight, authored-awaiting) is
tracked in the plan doc (`awl/AWL-EXECUTION-PLAN.md`) and the operator's
session memory — this runbook is the PROCESS, those are the STATE.
