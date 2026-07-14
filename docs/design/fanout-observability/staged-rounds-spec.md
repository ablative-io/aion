# STAGED-ROUNDS fan-out workflow — implementation spec

One workflow, **activities only, no children**: a planner agent produces a phased item plan; a bounded loop fans items out to parallel per-item dev agents in per-item worktrees, reviews each item's diff, and folds accepted items forward; accepted branches merge, and conflicts feed a bounded remediation cycle whose agent **resumes the planner's norn session** (dev-brief's session-resumption mechanic generalized — `examples/dev-brief/worker/src/main.rs:22-27,253-265`).

Everything below was verified against main at `/Users/tom/Developer/ablative/aion` (read-only). The implementer works in a lane worktree created with:

```
git -C /Users/tom/Developer/ablative/aion worktree add /Users/tom/Developer/ablative/aion/.worktrees/staged-rounds -b staged-rounds-fanout main
```

All cargo commands: `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target` (worker is a standalone crate — see §3 — so its builds also use that target dir explicitly). All gate output → `<worktree>/staged-rounds-gates/` with an exit-code manifest.

---

## 0. Verified platform facts (the ground the design stands on)

**Direct compile seam.** `aion_awl::compile` (parse → check → schema → MIR lower → verify → select → sidecar), `crates/aion-awl/src/compile.rs:142-183`; packaging seam `compile_and_assemble_awl` at `crates/aion-awl-package/src/prepare.rs:40-52`. CLI: `aion deploy <file.awl>` direct-compiles (`crates/aion-cli/src/main.rs:477-481`), `aion run <file.awl> --input` is the one-motion path (`crates/aion-cli/src/main.rs:484-491`, `crates/aion-cli/src/one_motion.rs:46-77`). CLI binary name is `aion`, package `aion-cli` (`crates/aion-cli/Cargo.toml:2,18-19`). `aion awl check/fmt/emit/schema` at `crates/aion-cli/src/awl.rs:28-77`.

**Shapes the direct path SUPPORTS** (all load-bearing for this design):
- Collection fork over a list with **one unbound ACTION call** per item, parallel via `workflow.map` with engine-owned fail-fast (`crates/aion-awl/src/mir/lower/forks.rs:1-34,300-341`; `crates/aion-awl/src/mir/lower/fork_action.rs:34-147`). Join binds an input-ordered list of the action's return type (`forks.rs:272-281`).
- Fork inside a loop body; multiple forks per body — the fork inventory descends into loops and counts every fork (`forks.rs:58-70`).
- Bounded loop threading ONE value with `counting`, `until` first / `max` second, rebinding of the threaded var by a bound action call, counter visible after the loop (`crates/aion-awl/src/mir/lower/loops.rs:104-215,409-481`).
- Bound action calls, `sleep`, pipes whose stages are actions or `.field` projections, `route` incl. outcome payload construction (`crates/aion-awl/src/mir/lower/flow.rs:243-291,316-360`).
- Statement-level `expr -> name` binding (a pipe with zero stages) — the statement parser routes `ident.field …` heads to `parse_pipe_statement`, whose head is a full `parse_expr` (`crates/aion-awl/src/parser/statements.rs:180-237`).
- `x |> any(pred)` / `x |> all(pred)` **in expression position** parse as `Expr::CollectionPredicate` (`crates/aion-awl/src/parser/exprs.rs:140-167`) and lower directly (`crates/aion-awl/src/mir/lower/collection_predicate.rs:45-119`); two-backend parity pinned in `crates/aion-awl/tests/feature_any_all_workflow_id.rs:35-66`.
- `workflow.id` (Str) (`crates/aion-awl/src/mir/lower/expr.rs:82-99`), string concat `+` (strings only, `expr.rs:187-194`), `is empty/present/absent`, `not`, `and/or`, comparisons, record construction with omitted optional fields → `none` (`expr.rs:314-359`), **empty** list literals (`expr.rs:133-149`).
- Declared per-action config (`node`, retry, timeout) rides through direct lowering (`crates/aion-awl/src/mir/lower/activity.rs:145-196`); effective action requirements surface on `CompiledWorkflow.actions` (`crates/aion-awl/src/compile.rs:40-53,191-217`). Document-derived task queue = first `worker` name (`compile.rs:22-25`).

**Shapes the direct path REFUSES** (the refusal ledger; the document below avoids every one):
1. Pipe **combinator stages** as statements — `filter/map/sort/count` and stage-position `any/all` (`crates/aion-awl/src/mir/lower/flow.rs:354-356`). Note: dev_brief.awl's `verdicts |> all(…) -> round_clean` (`examples/dev-brief/awl/dev_brief.awl:161,165`) parses as an expression-position CollectionPredicate, not a stage, so it is direct-safe — but keep quantifiers in guard/until/bind expressions only.
2. **Non-empty list literals** (`crates/aion-awl/src/mir/lower/expr.rs:139-141`). Only `[]` lowers.
3. Call-site config lines on calls or fork branches (`flow.rs:300-302`, `forks.rs:312-314`, pinned `crates/aion-awl/src/mir/fork_tests.rs:358-366`).
4. Fork body beyond one unbound call; indexing inside a parallel fork branch (`forks.rs:305-311,329-334`).
5. `spawn`, `wait`, substeps (`flow.rs:279-289`).
6. Indexing anywhere (`Expr::Index` falls to the `unsupported "expression"` arm, `expr.rs:129`); no arithmetic exists at all (`crates/aion-awl/src/ast/expr.rs:49-70`).
7. Unbounded loops; `route` in loop bodies; non-loop-invariant `max` (`loops.rs:114-119,222-245`; checker: `crates/aion-awl/src/checker/blocks.rs:202-222`).

If the implementer hits ANY additional refusal compiling §2's document, record the exact `CompileError` rendering (shape + line) in the lane output — the compiler lanes want that list — and apply the listed fallback (§2.4).

**Worker concurrency knob (the real one).** `WorkerConfig::builder().max_concurrency(n)` — field `crates/aion-worker/src/config.rs:134`, builder `config.rs:285-289`, required (`config.rs:376-377`); enforced by a `tokio::sync::Semaphore` in the serve loop: permit acquired per dispatched activity before spawn (`crates/aion-worker/src/runtime/loop_.rs:186,264,372-383,545-575`). dev-brief sets 4 (`examples/dev-brief/worker/src/main.rs:316`). The engine side is parallel by construction (`workflow.map` fan-out, `fork_action.rs:124-137`). **No gap to document — the knob exists**; N parallel agent activities = `max_concurrency(N)` on the agent connections.

**Node routing.** The server routes by (namespace × task_queue × node) only, never by activity type; every connection on one queue must register a distinct node and every action pins its node in the document (`examples/dev-brief/worker/src/main.rs:3-8,62-71`).

**Norn harness facts.** Arg templates expand exactly `{workflow_id}` and `{activity_type}` (`crates/aion-integration-norn/src/harness.rs:146-155`) — per-item session ids therefore CANNOT come from the template; the wrapper appends per-run args at `start()` exactly as dev-brief appends `--workspace-root` from the input (`examples/dev-brief/worker/src/harness.rs:104-144`). `AgentRunSpec` carries `workflow_id`, `activity_type`, `input` (`crates/aion-integrations/src/spec.rs:12-27`). Sessions are keyed by `--session-id` + `--resume-if-exists`, so two different activity types can share one session (the merge-resumes-planner mechanic) (`examples/dev-brief/worker/src/main.rs:253-265`).

---

## 1. Files to create (everything under `examples/staged-rounds/`)

```
examples/staged-rounds/
  README.md
  awl/staged_rounds.awl            # §2 — the document (run `aion awl fmt` over it)
  awl/example-input.json           # §2.5
  worker/
    Cargo.toml                     # §3.1 — standalone crate, [workspace] stanza
    profiles/planner.md            # §5
    profiles/developer.md
    profiles/reviewer.md
    profiles/remediator.md
    schemas/plan.schema.json       # §4.4
    schemas/item-report.schema.json
    schemas/item-verdict.schema.json
    schemas/remediation.schema.json
    src/main.rs                    # §3.2 — five connections
    src/main_args.rs               # §3.3
    src/main_shell_node.rs         # §3.4
    src/main_tests.rs
    src/lib.rs                     # re-exports only (mod.rs law applies in spirit)
    src/harness.rs                 # §3.5 — generalized ProfiledNornHarness
    src/prompts.rs (+ prompts/tests.rs)   # §4.5
    src/profiles.rs                # 4-profile load, model examples/dev-brief/worker/src/profiles.rs:25-63
    src/schemas.rs                 # include_str! consts, model examples/dev-brief/worker/src/schemas.rs
    src/types.rs                   # §4.1 serde twins of the AWL wire types
    src/shell.rs                   # copy dev-brief's Shell verbatim pattern
    src/paths.rs                   # destructive-path guard, model dev-brief paths.rs; root = <repo>/.staged-rounds
    src/commit.rs                  # §4.3 — DevWork + ConcludeMerge commits
    src/handlers/mod.rs            # re-exports only
    src/handlers/provision.rs      # §4.2 provision_item
    src/handlers/merge.rs          # §4.2 merge_branches
    src/handlers/fold_phase.rs     # §4.2 fold_phase
    src/handlers/support.rs        # clip/require_run helpers, copy dev-brief handlers/support.rs
  worker/tests/
    compile_direct.rs              # §6 — THE direct-path compile proof
    shell_activities.rs            # §6 — hermetic real-git handler tests
    schemas_valid.rs               # §6
```

No workspace-member crates are touched; no console changes (so no ops-console embed regen). Files stay ≤500 code lines each — split handlers as listed.

---

## 2. The AWL document — `examples/staged-rounds/awl/staged_rounds.awl`

Write exactly this (then `aion awl fmt` it; the printer is the formatter, `crates/aion-cli/src/awl.rs:40-49`). Every construct is on the §0 supported list; syntax mirrors the flagship `examples/dev-brief/awl/dev_brief.awl` line-for-line where possible.

```awl
//! staged_rounds: supplied material goes in; merged, reviewed work comes
//! out. One planner agent reads the material and returns a typed, phased
//! plan of scope-fenced work items. Bounded rounds then fan the released
//! items out — one worktree, one dev agent, and one reviewer per item, in
//! parallel — and fold_phase moves accepted items to done, keeps rejected
//! items pending with the reviewer's feedback attached, and releases
//! newly-unblocked next-phase items. Accepted branches merge; conflicts
//! feed a bounded remediation cycle whose agent RESUMES the planner's
//! session — the persistent coordinator judges its own plan's conflicts.
//!
//! Operator notes: work item ids must be git-ref-safe slugs (they name
//! branches); a terminal per-item activity failure fails the run (the
//! parallel fan-out is engine-owned fail-fast).
workflow staged_rounds
  timeout 12h
  input material: Material
  input config: RunConfig
  outcome completed: type Completed, route success
  outcome exhausted: type Exhausted, route failure
  outcome conflicted: type Conflicted, route failure

/// What the planner reads: a title, the large brief, and repo pointers.
type Material {
  title: String,
  brief: String,
  pointers: [String],
}

type GateCommand { name: String, argv: [String] }

/// Run configuration. Every field is explicit — no defaults.
type RunConfig {
  repo_root: String,
  base_branch: String,
  gates: [GateCommand],
  max_rounds: Int,
  max_merge_rounds: Int,
}

/// One planned work item: scope fences, a phase tag, and dependency ids.
/// `feedback` starts empty; fold_phase writes rejected items' reviewer
/// feedback into it for the next round's dev turn.
type WorkItem {
  id: String,
  title: String,
  goal: String,
  scope_in: [String],
  scope_out: [String],
  phase: Int,
  depends_on: [String],
  feedback: String,
}

type Plan { summary: String, items: [WorkItem] }

type DoneItem { item_id: String, branch: String, base_commit: String, summary: String }

/// The one loop-carried value: released/blocked/done partitions plus the
/// run's accumulated adverse evidence.
type PhaseState {
  ready: [WorkItem],
  blocked: [WorkItem],
  done: [DoneItem],
  evidence: String,
}

type ProvisionedItem {
  item: WorkItem,
  workspace_path: String,
  branch: String,
  base_commit: String,
}

type ItemClaim { criterion: String, how: String }

type ItemReport {
  item_id: String,
  summary: String,
  commits: [String],
  claims: [ItemClaim],
}

type DevItemResult {
  item: WorkItem,
  workspace_path: String,
  branch: String,
  base_commit: String,
  report: ItemReport,
}

type Finding { severity: String, title: String, evidence: String }

/// One item verdict: severity is "blocking" or "advisory", overall is
/// "accept" or "reject".
type ItemVerdict {
  item_id: String,
  overall: String,
  findings: [Finding],
  /// Present when the verdict rejects.
  reject_reason: String?,
}

type MergeConflict { item_id: String, branch: String, files: [String], detail: String }

/// The merge loop's one threaded value, returned whole by merge_branches.
type MergeState {
  integration_branch: String,
  workspace_path: String,
  merged: [String],
  conflicts: [MergeConflict],
  evidence: String,
}

type RemediationReport { summary: String, resolved: [String] }

type Completed {
  plan: String,
  rounds: Int,
  merge_rounds: Int,
  done: [DoneItem],
  integration_branch: String,
  merged: [String],
  evidence: String,
}

type Exhausted {
  rounds: Int,
  ready: [WorkItem],
  blocked: [WorkItem],
  done: [DoneItem],
  evidence: String,
}

type Conflicted {
  rounds: Int,
  merge_rounds: Int,
  done: [DoneItem],
  integration_branch: String,
  conflicts: [MergeConflict],
  evidence: String,
}

worker staged_rounds
  action planner(material: Material, repo_root: String, workspace_path: String) -> Plan
    node planner
  action provision_item(run_root: String, repo_root: String, base_branch: String, item: WorkItem) -> ProvisionedItem
    node shell
  action dev_item(work: ProvisionedItem, gates: [GateCommand]) -> DevItemResult
    node developer
  action review_item(work: DevItemResult) -> ItemVerdict
    node reviewer
  action fold_phase(prior: PhaseState, incoming: [WorkItem], dev: [DevItemResult], verdicts: [ItemVerdict]) -> PhaseState
    node shell
  action merge_branches(run_root: String, repo_root: String, base_branch: String, done: [DoneItem], prior_evidence: String, remediation: String) -> MergeState
    node shell
  action remediate(merge: MergeState, plan: Plan, workspace_path: String) -> RemediationReport
    node remediation

step plan
  config.repo_root + "/.staged-rounds/" + workflow.id -> run_root
  planner(material: material, repo_root: config.repo_root, workspace_path: config.repo_root) -> plan
  fold_phase(prior: PhaseState(ready: [], blocked: [], done: [], evidence: ""), incoming: plan.items, dev: [], verdicts: []) -> seeded

step execute
  loop state = seeded counting rounds
    fork item in state.ready
      provision_item(run_root: run_root, repo_root: config.repo_root, base_branch: config.base_branch, item: item)
    join -> provisioned
    fork p in provisioned
      dev_item(work: p, gates: config.gates)
    join -> dev_results
    fork d in dev_results
      review_item(work: d)
    join -> verdicts
    fold_phase(prior: state, incoming: [], dev: dev_results, verdicts: verdicts) -> state
    until state.ready is empty and state.blocked is empty
    max config.max_rounds

  outcome complete: when state.ready is empty and state.blocked is empty, route merge_once
  outcome spent: otherwise, route wrap_exhausted

step merge_once
  merge_branches(run_root: run_root, repo_root: config.repo_root, base_branch: config.base_branch, done: state.done, prior_evidence: state.evidence, remediation: "") -> attempt
  outcome clean: when attempt.conflicts is empty, route finish_clean
  outcome dirty: otherwise, route fix_conflicts

step fix_conflicts
  loop mstate = attempt counting merge_rounds
    remediate(merge: mstate, plan: plan, workspace_path: mstate.workspace_path) -> advice
    merge_branches(run_root: run_root, repo_root: config.repo_root, base_branch: config.base_branch, done: state.done, prior_evidence: mstate.evidence, remediation: advice.summary) -> mstate
    until mstate.conflicts is empty
    max config.max_merge_rounds

  outcome merged: when mstate.conflicts is empty, route finish_fixed
  outcome stuck: otherwise, route wrap_conflicted

step finish_clean
  route completed(plan: plan.summary, rounds: rounds, merge_rounds: 0, done: state.done, integration_branch: attempt.integration_branch, merged: attempt.merged, evidence: state.evidence)

step finish_fixed
  route completed(plan: plan.summary, rounds: rounds, merge_rounds: merge_rounds, done: state.done, integration_branch: mstate.integration_branch, merged: mstate.merged, evidence: mstate.evidence)

step wrap_exhausted
  route exhausted(rounds: rounds, ready: state.ready, blocked: state.blocked, done: state.done, evidence: state.evidence)

step wrap_conflicted
  route conflicted(rounds: rounds, merge_rounds: merge_rounds, done: state.done, integration_branch: mstate.integration_branch, conflicts: mstate.conflicts, evidence: mstate.evidence)
```

### 2.1 Why each shape is safe (mapping to reference behavior)
- Three sequential parallel forks per loop pass, each body one unbound action call, each joined list feeding the next fork's collection — fork slots are counted per statement including inside loops (`forks.rs:58-70`), joins insert `List(returns)` into scope (`forks.rs:272-281`), collections are arbitrary scope expressions evaluated pre-fan-out (`forks.rs:219-232`).
- Free names in fork branches (`run_root`, `config`) are captured sorted (`forks.rs:345-354`); no indexing, no call-site config in branches.
- `loop state = seeded` / `loop mstate = attempt` — the seed is any expression over prior bindings, evaluated once at the call site (`loops.rs:121-123`); `counting rounds/merge_rounds` destructures the pair post-loop (`loops.rs:176-215`).
- Threaded-var rebinding by `fold_phase(...) -> state` / `merge_branches(...) -> mstate` matches dev_brief's `fold_round(...) -> state` (`examples/dev-brief/awl/dev_brief.awl:160`).
- `until` composites, `is empty`, cross-step binding reads (`workspace`/`state`/`rounds` used across dev_brief steps 149-185) — all flagship-proven, and the quantifier-free `until` conditions here avoid even the expression-position predicate.
- Outcome payload construction `route completed(...)` mirrors `route accepted(...)` (`dev_brief.awl:177,181,185`). `merge_rounds: 0` is an Int literal (`expr.rs:46-50`).
- The remediate-before-merge body ordering means remediate NEVER runs on an empty conflict set: `fix_conflicts` is only entered when `attempt.conflicts` is non-empty, and each later pass re-merges after remediation; the post-body `until` exits as soon as a merge comes back clean.

### 2.2 FIRST implementation action (per lane brief)
Before writing any worker code: create the `.awl` file and prove the direct path —
1. `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target cargo run -p aion-cli --bin aion -- awl check examples/staged-rounds/awl/staged_rounds.awl` (from the worktree; full output → gates dir).
2. `… -- awl fmt examples/staged-rounds/awl/staged_rounds.awl` (formatter, never a check mode).
3. The compile-seam proof: write `worker/tests/compile_direct.rs` (§6) first and run it — it calls `compile_and_assemble_awl` directly. `check`-green does NOT imply compile-green (the checker accepts more than the lowering, `forks.rs:2-3`), so the seam test is the gate of record.

### 2.3 The three-outcome envelope
Success `completed`; failures `exhausted` (rounds ran out with items still pending/blocked) and `conflicted` (merge remediation ran out). If the checker or outcome-envelope derivation refuses two failure outcomes (not expected — outcomes derive as a union envelope, `crates/aion-awl/src/compile.rs:150-151`), fall back to ONE failure outcome `stalled` whose payload adds `kind: String` (`"exhausted"`/`"conflicted"`) and unions the fields; record the refusal.

### 2.4 Fallbacks for possible refusals (apply only on an actual refusal; record each)
- `loop mstate = attempt` (binding-seeded loop) refuses → seed `MergeState(integration_branch: "", workspace_path: "", merged: [], conflicts: [], evidence: "")` and make `remediate`'s handler treat an all-empty MergeState as "read the in-progress merge from the worktree" (the integration worktree carries the truth); pass `run_root` to remediate instead of `mstate.workspace_path`.
- `route completed(... merge_rounds: 0 ...)` payload refusal → add a mechanical no-op field via `fold_phase`-style echo; or collapse `finish_clean` into `finish_fixed` by seeding `fix_conflicts` unconditionally with `max 1` when clean (last resort; prefer recording the refusal).
- Fork-in-loop count/slot refusal (not expected; inventory descends into loops) → hoist the three forks into three chained steps and route backward is NOT available inside a loop — instead reduce to `fork … sequential` (supported, `fork_action.rs:91-121`) and record the parallelism loss loudly.

### 2.5 `awl/example-input.json`
```json
{
  "material": {
    "title": "Example: split a module",
    "brief": "…large brief text…",
    "pointers": ["src/lib.rs", "docs/DESIGN.md"]
  },
  "config": {
    "repo_root": "/abs/path/to/target/repo",
    "base_branch": "main",
    "gates": [
      {"name": "fmt", "argv": ["cargo", "fmt", "--all"]},
      {"name": "clippy", "argv": ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]},
      {"name": "test", "argv": ["cargo", "test", "--workspace"]}
    ],
    "max_rounds": 3,
    "max_merge_rounds": 2
  }
}
```
Wire conventions: AWL omits absent optionals from JSON (worker must tolerate missing keys — dev-brief pins this at `examples/dev-brief/worker/src/handlers/fold_round.rs:219-242`); unknown fields must round-trip (`fold_round.rs:247-277` uses raw `serde_json::Value` passthrough where echo fidelity matters).

---

## 3. The worker — `examples/staged-rounds/worker` (match dev-brief's stack exactly)

dev-brief's worker is a **standalone Rust crate** (not a workspace member, `[workspace]` stanza) consuming `aion-worker` (feature `liminal-transport`), `aion-integration-norn`, `aion-integrations` by path (`examples/dev-brief/worker/Cargo.toml:36,50-66`). Ours is identical plus dev-deps for the compile proof.

### 3.1 `Cargo.toml`
Copy dev-brief's (`examples/dev-brief/worker/Cargo.toml`) with: name `staged-rounds-worker`, lib `staged_rounds_worker`, same lint tables, same deps with paths `../../../crates/...`; add `[dev-dependencies] aion-awl-package = { path = "../../../crates/aion-awl-package" }` and `tempfile = "3"`. House laws still apply to code quality (no unwrap/expect/panic incl. tests, no allow/expect/ignore attributes).

### 3.2 `main.rs` — five connections, one task queue `staged_rounds`
Model on `examples/dev-brief/worker/src/main.rs:110-321` structurally:
- `const TASK_QUEUE: &str = "staged_rounds";` — MUST equal the document's first `worker` name (`compile.rs:22-25` makes it the deploy task queue).
- Nodes: `shell` (provision_item, merge_branches, fold_phase), `planner` (planner), `developer` (dev_item), `reviewer` (review_item), `remediation` (remediate). One connection per node — same-queue connections must have distinct nodes (`main.rs:62-71`). Node strings in `main.rs` MUST equal the `node` lines in the document — put a doc comment cross-referencing `awl/staged_rounds.awl` as the authoritative node table (dev-brief's analog is `activities.gleam`; we have no Gleam module, the document itself is the table).
- Four agent `Role` entries (extend dev-brief's `Role` struct, `main.rs:90-108`):

| role | activity_type | node | schema | session id (per-run) | deny tools | post-run commit |
|---|---|---|---|---|---|---|
| planner | `planner` | planner | `PLAN` | `<wf>-planner` | `write,edit,apply_patch` | none |
| developer | `dev_item` | developer | `ITEM_REPORT` | `<wf>-dev-<item_id>` | none | `DevWork` |
| reviewer | `review_item` | reviewer | `ITEM_VERDICT` | `<wf>-review-<item_id>` | `write,edit,apply_patch` (dev-brief precedent `main.rs:72-79`) | none |
| remediator | `remediate` | remediation | `REMEDIATION` | `<wf>-planner` (**the planner's session — the mechanic**) | none | `ConcludeMerge` |

- **Session ids are per-run wrapper data, not template args**: the norn template expands only `{workflow_id}`/`{activity_type}` (`crates/aion-integration-norn/src/harness.rs:146-155`), which cannot express `<item_id>`. Therefore: do NOT put `--session-id` on the inner `NornHarness` template; the wrapper appends `--session-id <derived>` and `--resume-if-exists` per run in `start()`, deriving from `spec.workflow_id` (`crates/aion-integrations/src/spec.rs:14`) + the role's extractor over the input JSON (item id for dev/review; constant `planner` suffix for planner AND remediate). This is the same per-run-append seam dev-brief uses for `--workspace-root` (`examples/dev-brief/worker/src/harness.rs:134-141`).
- Inner harness per role otherwise identical to `inner_norn_harness` (`main.rs:253-272`): `--fast`, `--reasoning-effort high`, `--append-system-prompt <profile>` (never `--system-prompt`), `--output-schema <inline json>`, `without_env("OPENAI_API_KEY")`.
- `worker_config(identity, node)` clones dev-brief's (`main.rs:309-321`) with **`max_concurrency(args.max_parallel)`** on the `developer` and `reviewer` connections (the genuine-parallelism knob, §0), `max_concurrency(2)` on planner/remediation, `max_concurrency(8)` on shell. `serve_with_redial` per connection (`crates/aion-worker/src/lib.rs:85`), shell on the main thread writing `--ready-file` (`main.rs:276-302`).

### 3.3 `main_args.rs`
Copy dev-brief's (`examples/dev-brief/worker/src/main_args.rs`) adding `--max-parallel <n>` (default 4, parse to usize, reject 0 — the SDK refuses 0 anyway, `loop_.rs:362-368`). Keep `--address`, `--identity`, `--ready-file`, `--norn-bin`, `--profiles-dir` (required).

### 3.4 `main_shell_node.rs`
Copy dev-brief's registry/adapters (`examples/dev-brief/worker/src/main_shell_node.rs:8-70`): `blocking(shell, handlers::provision_item)`, `blocking(shell, handlers::merge_branches)`, `immediate(handlers::fold_phase)`.

### 3.5 `harness.rs` — generalized `ProfiledNornHarness`
Start from dev-brief's (`examples/dev-brief/worker/src/harness.rs`) and generalize:
- `PostRunCommit { DevWork, ConcludeMerge }`.
- Role context extractor `fn(&str) -> Result<RoleContext, String>` where `RoleContext { workspace_root: String, session_suffix: String, commit: Option<CommitContext> }`:
  - planner: `workspace_root = input.workspace_path` (top level), suffix `"planner"`.
  - dev_item: `workspace_root = input.work.workspace_path`, suffix `format!("dev-{}", input.work.item.id)`, commit context `{workspace_path, item_id, branch}`.
  - review_item: `workspace_root = input.work.workspace_path`, suffix `format!("review-{}", input.work.item.id)`.
  - remediate: `workspace_root = input.workspace_path` (top level; equals `merge.workspace_path`), suffix `"planner"`, commit context for ConcludeMerge.
  Missing/blank paths or ids are loud protocol errors, never fallbacks (`harness.rs:150-175` pattern and tests `harness.rs:260-286`).
- `start()`: extract context → resolve commit plan BEFORE the run (`harness.rs:120-127`) → assemble prompt → re-encode as JSON string payload (`harness.rs:128-133`) → clone inner harness, append `--workspace-root <root>`, `--session-id <workflow_id>-<suffix>`, `--resume-if-exists` → start.
- `wait_result` commit behavior:
  - `DevWork`: commit round work in the item worktree under the machinery identity, rewrite the report's `commits` to the real head — port `commit_dev_work`/`rewrite_report_commits` (`examples/dev-brief/worker/src/harness.rs:199-242`, `commit.rs:117-205`) keyed by `item_id` instead of `brief.id`.
  - `ConcludeMerge` (new): in the integration worktree, if a merge is in progress (`.git/MERGE_HEAD` present via `git rev-parse -q --verify MERGE_HEAD`), run `git add -A` then `git commit --no-edit` (concluding the conflicted merge with the agent's resolutions); if no merge in progress but the tree is dirty, `git add -A && git commit -m "fix(staged): remediation"`; if clean, record a skip. Failure of either command is an activity failure. This is the mechanical-git doctrine: agents never run git (`harness.rs:24-32`).

---

## 4. Activity contracts and handler protocols

### 4.1 `types.rs`
Serde twins of §2's records with field names byte-equal to the AWL declarations. Conventions from dev-brief: `Option<T>` + `#[serde(default, skip_serializing_if = "Option::is_none")]` for `String?` fields (absent = omitted key); `#[serde(default)]` on lists so foreign payloads missing keys decode empty (`examples/dev-brief/worker/src/prompts.rs:230-234` precedent); where a handler ECHOES agent-produced values (fold_phase echoing verdict/report content into evidence only — it re-emits typed WorkItem/DoneItem, so plain typed structs suffice; raw `Value` passthrough is only needed if echo fidelity of agent extension fields matters — not here).

### 4.2 Shell handlers
**`provision_item(run_root, repo_root, base_branch, item) -> ProvisionedItem`** (`handlers/provision.rs`), modeled on `examples/dev-brief/worker/src/handlers/workspace.rs:29-163` with ONE deliberate semantic change:
- `workspace_path = {run_root}/items/{item.id}`; `branch = "staged/" + basename(run_root) + "/" + item.id` (basename of run_root is the workflow id → no cross-run branch collisions; ids are validated git-ref-safe slugs — refuse otherwise, terminal).
- **Reuse, don't reset**: if the worktree exists, is on `branch`, and `git status --porcelain` is clean → return it as-is with `base_commit = git merge-base <branch> <base_branch>` (round-2 re-provisioning of a rejected item MUST preserve the committed round-1 work — the dev session resumes and continues; dev-brief's `-B` reset semantics, `workspace.rs:60-76`, would destroy it).
- Otherwise: `ensure_parent_dir` (`workspace.rs:153-163`), reclaim stale clean holders of the branch / refuse dirty holders loudly (`workspace.rs:98-150` verbatim port), then `git -C <repo> worktree add --force -B <branch> <path> <base_branch>` and `rev-parse HEAD` (`workspace.rs:60-92`).
- All destructive ops guarded to `<repo_root>/.staged-rounds/` via the `paths.rs` guard (dev-brief `workspace.rs:186-192`).

**`fold_phase(prior, incoming, dev, verdicts) -> PhaseState`** (`handlers/fold_phase.rs`), pure/`immediate`, modeled on `fold_round` (`examples/dev-brief/worker/src/handlers/fold_round.rs:18-62`):
1. Start from `prior`. Add every `incoming` item (the plan seed) to a working pool alongside `prior.ready`.
2. For each `dev` result, find its verdict by `item_id` (a dev result with no verdict, a verdict with no dev result, or duplicate ids = terminal input failure — loud, never silent).
3. **Derive-and-check** (port `examples/dev-brief/worker/src/handlers/verdicts.rs:38-79`): derived overall = reject iff any finding has severity `blocking`; a verdict whose asserted `overall` disagrees, or a rejection missing `reject_reason`/a substantiating blocking finding, is treated as REJECT and the violation is appended to evidence.
4. Accepted → `DoneItem { item_id, branch, base_commit, summary: report.summary }` appended to `done`; the item leaves ready. Rejected → the item stays in ready with `feedback` = rendered adverse lines (reason + `[blocking: …]` titles, `verdicts.rs` format) — the next round's dev prompt shows it.
5. Release: move every blocked item whose `depends_on` ⊆ done item ids into ready (this preserves the invariant *blocked non-empty ⇒ ready non-empty*, so the loop can never spin on an empty round; items with dangling `depends_on` naming no planned item = terminal input failure at seed time).
6. `evidence = smart_join(prior.evidence, round_lines)` (`fold_round.rs:56-62`).

**`merge_branches(run_root, repo_root, base_branch, done, prior_evidence, remediation) -> MergeState`** (`handlers/merge.rs`), idempotent continue-style protocol:
1. `integration_branch = "staged/" + basename(run_root) + "/integration"`, `workspace_path = {run_root}/integration`. Ensure the worktree exists (create with `worktree add --force -B <integration_branch> <path> <base_branch>` ONLY when absent — never reset an existing one: remediation commits live there).
2. If `MERGE_HEAD` exists (unconcluded merge — remediation didn't finish): return the SAME conflict set re-read from `git diff --name-only --diff-filter=U`, evidence noting "remediation did not conclude the in-progress merge".
3. For each `done` item in order, skip branches already merged (`git merge-base --is-ancestor <branch> HEAD`); else `git merge --no-ff <branch> -m "merge(staged): <item_id>"`. On conflict: capture `MergeConflict { item_id, branch, files: <diff-filter=U list>, detail: clipped merge output }`, **leave the merge in progress** (remediate resolves it in place), stop, and return `conflicts = [that conflict]` plus all not-yet-merged branches implicitly pending (they'll be picked up next pass).
4. All merged → `conflicts: []`, `merged` = full branch list, evidence smart-joined from `prior_evidence` + `remediation` note + per-branch merge lines.
Guard every destructive op with `paths.rs`.

### 4.3 `commit.rs`
Port dev-brief's (`examples/dev-brief/worker/src/commit.rs:62-205` surface): `CommitContext { workspace_path, item_id }`, `commit_dev_work` (add -A, commit `chore(staged dev): item <id> round work` under a machinery author identity, return head; skip when clean), `rewrite_report_commits` (rewrite the result JSON's `report.commits` to the real head), plus the new `conclude_merge` (§3.5).

### 4.4 Output schemas (`worker/schemas/*.json`, embedded via `include_str!` — `examples/dev-brief/worker/src/schemas.rs` pattern)
Each mirrors §2's record byte-conventions (draft 2020-12, model `examples/dev-brief/schemas/dev-report.schema.json` / `lens-verdict.schema.json`):
- `plan.schema.json` → `Plan`: `summary: string`, `items: [WorkItem]` with all eight WorkItem fields required except `feedback` (default `""` — instruct the planner to emit `""`; make it required with `"const"`-free string to keep decode strict: REQUIRED, planner told to emit empty string).
- `item-report.schema.json` → `ItemReport` (`item_id`, `summary`, `commits`, `claims`).
- `item-verdict.schema.json` → `ItemVerdict` (`item_id`, `overall` enum accept/reject, `findings[{severity enum blocking/advisory, title, evidence}]`, optional `reject_reason`).
- `remediation.schema.json` → `RemediationReport` (`summary`, `resolved: [string]`).

### 4.5 Prompts (`prompts.rs`) — one dumb assemble fn per role (`examples/dev-brief/worker/src/prompts.rs:10-13,57-71` discipline)
All four render `context_section(title, context_json)` (fenced JSON, `prompts.rs:65-71`) plus role sections:

- **planner**: title "Run context: material + repo root". Sections: (a) *Your job* — read the material and the repo (workspace root = the repo, read-only); return the typed plan: 3–10 work items, each with a git-ref-safe slug `id` (lowercase, `[a-z0-9-]`), a one-sentence `goal`, `scope_in` (files/dirs the item MAY touch) and `scope_out` (hard walls), an Int `phase` (1 = no prerequisites), `depends_on` listing item ids whose merged output this item needs, `feedback: ""`. Items in the same phase run **in parallel in separate worktrees against the same base branch** — they MUST NOT touch the same files; use phases/depends_on for anything that would collide or build on other items' work. (b) *Output* — the JSON schema is enforced; emit nothing but the object.
- **developer** (dev_item): title "Run context: your work item (+ reviewer feedback when cycling)". Sections: (a) your workspace IS the item's worktree on its own branch; implement `work.item.goal` strictly inside `scope_in`, never touching `scope_out`; you run NO git — the machinery commits your work after the turn (doctrine, `harness.rs:24-32`); if `work.item.feedback` is non-empty this is a resumed session continuing YOUR previous round — address the feedback first. (b) the **gate-discipline section**: port `gate_discipline_section` verbatim reading `gates` from the context (`prompts.rs:250-284` — task #236 lives inline in briefs; keep it). (c) report claims per acceptance-relevant point.
- **reviewer** (review_item): title "Run context: item + dev report". Sections: (a) charter — adversarial single-reviewer covering correctness, scope-fence compliance (`scope_out` violations are blocking), and claim verification; any blocking finding ⇒ overall reject with a `reject_reason`. (b) workspace section ported from `reviewer_workspace_section` (`prompts.rs:543-570`): your cwd IS the item worktree at the reviewed state, you are read-only (write tools denied at the process boundary), reconstruct the diff yourself with `git diff <base_commit>` (base_commit read tolerantly from context, `prompts.rs:530-535`); you MAY run the configured gates read-only.
- **remediator** (remediate): title "Run context: merge state + the plan". Sections: (a) *You are the planner, resumed* — this session planned these items; a merge of their branches hit the conflicts listed in `merge.conflicts`; your cwd IS the integration worktree with the conflicted merge IN PROGRESS; resolve every conflict marker preserving both items' intent per the plan; do NOT run git commit — the machinery concludes the merge after your turn. (b) output schema: summary + resolved file list.

Loop-back compaction (`prompts.rs:287-403`) is a polish port for the developer role only if time allows; the full-context render is correct-first.

---

## 5. Profiles (`worker/profiles/*.md`)
Static role doctrine, loaded at startup, non-empty required (`examples/dev-brief/worker/src/profiles.rs:34-63`; generalize `PROFILE_FILES` to the four names). Content: one page each — planner (decomposition judgment, parallel-safety rules, honest phasing), developer (brief-executor discipline, scope fences are law, gates before ending any turn), reviewer (adversarial, evidence-grounded, never rubber-stamps, blocking vs advisory), remediator (conflict resolution preserving both intents, minimal edits, no new features). Model tone on `examples/dev-brief/worker/profiles/{developer,reviewer}.md`.

---

## 6. Test plan (concrete names; all hermetic — real git in tempdirs, no server, no norn)

**`worker/tests/compile_direct.rs`** (the lane's headline proof; dev-dep `aion-awl-package`):
- `staged_rounds_document_compiles_and_assembles_on_the_direct_path` — read `../awl/staged_rounds.awl`, call `compile_and_assemble_awl(source, doc_dir)` (`prepare.rs:40-52`); assert `compiled.workflow_name == "staged_rounds"`, `first_worker == Some("staged_rounds")`, `timeout == Some(12h)`, archive non-empty.
- `the_seven_actions_pin_their_nodes` — assert `compiled.actions` (via `aion_awl::action_requirements` re-derivation or the `CompiledWorkflow.actions` field) contains exactly the 7 `(task_queue: "staged_rounds", action, node)` rows of §3.2's table (`compile.rs:191-217`).
- `the_input_schema_covers_material_and_config` — spot-assert required top-level properties.

**`worker/tests/shell_activities.rs`** (drive handlers against a real scratch repo built in the test — model `examples/dev-brief/worker/tests/shell_activities.rs`):
- `provision_item_creates_a_worktree_on_the_item_branch_and_reports_the_base`
- `provision_item_reuses_a_clean_existing_worktree_preserving_commits` — provision, commit a file in the worktree, provision again, assert the commit survives and base_commit is the merge-base.
- `provision_item_refuses_a_dirty_stale_holder_by_name`
- `provision_item_refuses_a_non_slug_item_id`
- `fold_phase_seeds_ready_and_blocked_from_incoming_by_phase_and_deps`
- `fold_phase_accepts_a_clean_verdict_into_done_and_releases_dependents`
- `fold_phase_keeps_a_rejected_item_pending_with_feedback_attached`
- `fold_phase_derives_reject_from_a_blocking_finding_despite_asserted_accept`
- `fold_phase_fails_loudly_on_a_verdict_with_no_matching_dev_result`
- `fold_phase_never_leaves_blocked_nonempty_with_ready_empty`
- `merge_branches_merges_disjoint_item_branches_clean` — two branches touching different files → `conflicts: []`, both in `merged`, integration branch content verified **by execution** (read the merged files).
- `merge_branches_reports_a_conflict_and_leaves_the_merge_in_progress` — assert `MERGE_HEAD` exists and `conflicts[0].files` names the file.
- `merge_branches_resumes_after_a_concluded_resolution` — resolve + `git add -A && git commit --no-edit` by hand (simulating `ConcludeMerge`), call again → remaining branch merges, `conflicts: []`.
- `merge_branches_is_idempotent_over_already_merged_branches`
- `destructive_paths_outside_staged_rounds_root_are_refused` (paths guard).

**`worker/tests/schemas_valid.rs`**: each embedded schema parses under `jsonschema::validator_for` and a canonical sample payload validates; a scope-violating sample fails.

**Unit tests in-crate** (like dev-brief `src/*/tests`): `main_tests.rs` (arg parsing incl. `--max-parallel`, roles table nodes/suffixes), `harness` tests (workspace-root + session-suffix extraction per role incl. loud failures for missing item id; `ConcludeMerge` against a real conflicted repo in tempdir — execution proof, not mock), `prompts/tests.rs` (each role's prompt contains the fenced context; developer prompt contains the verbatim gate argv; reviewer prompt contains `git diff <base>`).

**Honesty note carried into the lane output**: engine-driven end-to-end parallelism (server dispatch → semaphore → N live norn sessions) requires a live server + deploy, which this lane is forbidden to do (production server on this machine). The knob and the engine fan-out are individually verified (SDK semaphore test at `crates/aion-worker/src/worker.rs:1951`; `workflow.map` lowering pins at `crates/aion-awl/src/mir/fork_tests.rs:33-80`). Do NOT claim live parallel runtime behavior; list the live run as the owed follow-up.

---

## 7. Gates (each to `<worktree>/staged-rounds-gates/<name>.log`, exit codes in `manifest.txt`)

1. `awl-fmt`: `cargo run -p aion-cli --bin aion -- awl fmt examples/staged-rounds/awl/staged_rounds.awl`
2. `awl-check`: `… -- awl check examples/staged-rounds/awl/staged_rounds.awl`
3. `fmt-worker`: `cargo fmt` inside `examples/staged-rounds/worker` (standalone crate; also run `cargo fmt --all` at repo root — harmless, rule is always-write)
4. `clippy-worker`: `cargo clippy --all-targets -- -D warnings` in the worker dir
5. `test-worker`: `cargo test` in the worker dir (includes `compile_direct.rs` — the direct-path proof)
6. `build-worker`: `cargo build --release` in the worker dir

All with `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target`; never pipe through grep/head/tail; cargo lock contention with sibling lanes is expected — wait it out.

---

## 8. Ordered checklist

1. Create the lane worktree (§ header). Everything below happens inside it.
2. Write `examples/staged-rounds/awl/staged_rounds.awl` exactly as §2; run gates 1–2.
3. Scaffold `worker/Cargo.toml` (§3.1) + `tests/compile_direct.rs` (§6); run it. **If it refuses, apply §2.4 fallbacks one at a time, recording each exact refusal string — this list is a lane deliverable.**
4. Port `shell.rs`, `paths.rs` (root = `.staged-rounds`), `support.rs` from dev-brief.
5. `types.rs` (§4.1), then handlers: `provision.rs`, `fold_phase.rs`, `merge.rs` (§4.2) with their `shell_activities.rs` tests green as you go.
6. `commit.rs` (§4.3) incl. `conclude_merge` + its execution tests.
7. `schemas/*.json` + `schemas.rs` + `schemas_valid.rs`.
8. `prompts.rs` + `profiles.rs` + the four `profiles/*.md` (§4.5, §5) + tests.
9. `harness.rs` (§3.5) with per-run session-id/workspace-root derivation + tests.
10. `main_args.rs`, `main_shell_node.rs`, `main.rs` (§3.2–3.4) + `main_tests.rs`.
11. `README.md`: composition diagram (model `examples/dev-brief/README.md:12-51`), the node table, deploy/run instructions — direct path only: `aion deploy examples/staged-rounds/awl/staged_rounds.awl`, `aion start staged_rounds --input-file <input.json>` (no task-queue flag exists — the queue is document-derived at deploy; `--input` is inline JSON only) or one-motion `aion run … --input '<json>'`; worker launch line with `--profiles-dir ./profiles --max-parallel 4 --ready-file <path>`; **manual start, no launchd** (Tom's standing rule).
12. `awl/example-input.json` (§2.5); validate it against the compiled input schema inside `compile_direct.rs` (add `example_input_validates_against_the_compiled_contract` using `jsonschema`).
13. Run the full gate battery (§7); write the manifest.
14. Commit on the lane branch, staging explicit paths only (`examples/staged-rounds/...`), message ending with the trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. No push, no merge, no deploy, no server restarts.
