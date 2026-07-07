# dev-brief build notes (living document — delete before merge)

Tom's directive 2026-07-07: reliable aion-native dev workflows on norn (his
ChatGPT OAuth — zero Claude spend per run), proper briefing, proper
adversarial review, intra-brief parallelism. This package generalizes the
PROVEN `examples/remediation` skeleton (norn driven mode, mechanical shell
gates, derive-and-check adversarial verdicts, bounded fix cycles, kill-9 +
reopen survival — all live-proven 2026-07-07) from yggdrasil ledger findings
to arbitrary dev briefs on any stack repo. Briefing front half stays
`brief_forge` (dev-pipeline package) — composition, not duplication.

## Locked design

- Package `aion_dev_brief`. Task queue `dev_brief`. Node pins (LOAD-BEARING,
  same discipline as remediation): `shell`, `developer`, `reviewer` — one
  worker connection per node; server routes by (ns, tq, node) only.
- Workflows: `dev_brief` (the pipeline) + `review_lens` (spawned as a CHILD
  per lens per review round — Tom's intra-brief fan-out: lenses run
  CONCURRENTLY via child.spawn/await, each with its own workflow_id and
  therefore its own fresh norn session).
- Stages: provision worktree (branch `dev/<brief-id>`, base per config)
  → developer (driven norn, session `{workflow_id}-developer` resumed across
  cycles; WORKER commits after each turn — agents run no git)
  → run_gates (shell: the brief's CONFIGURED commands, exit-status-is-data;
  plus the diff vs base_commit for the reviewers)
  → review fan-out (N `review_lens` children concurrently; each verdict
  derive-and-check: any `blocking` finding ⇒ derived Reject; asserted overall
  must agree; Reject needs reject_reason; violations recorded AND treated as
  reject)
  → bounded fix cycle (cap = max_fix_cycles, default 3; exhaustion is a
  terminal DISPOSITION, never silent success)
  → cleanup (worktree removed only when clean; branch + commits remain).
- No ledger stage. Evidence = BriefResult (report, gate outcome, verdicts,
  mismatches) in durable history + the branch.
- Default lenses when input omits them: correctness, regressions,
  brief_compliance. Empty gates list = recorded vacuous pass (operator's
  choice, honest note in outcome).

## Module map (src/)

- dev_brief/types.gleam — data model (fresh, modeled on remediation/types)
- dev_brief/cycle.gleam — cap machine (adapted: Developer→Gate→Review)
- dev_brief/verdicts.gleam — derive-and-check + branch_safe (from checks)
- dev_brief/codecs.gleam — JSON codecs (fresh, remediation style)
- dev_brief/activities.gleam — typed constructors + node/queue pins
- dev_brief.gleam — the pipeline workflow (trampoline + lens fan-out)
- review_lens.gleam — the child workflow (one agent activity)

## Worker (worker/)

Clone-adapt remediation worker: 3 connections (shell / developer /
reviewer) on queue `dev_brief`. Shell handlers: provision_workspace (reuse),
run_gates (NEW: execute configured argv list in workspace, capture exit +
tail, diff vs base_commit), cleanup_workspace (reuse — refuses dirty).
Developer handler commits after turn (adapt commit.rs), rewrites report
`commits` to the real branch head. Profiles live IN-PACKAGE at
worker/profiles/{developer,reviewer}.md (self-contained; --profiles-dir
defaults there). Output schemas: schemas/dev-report.schema.json,
schemas/lens-verdict.schema.json (norn --output-schema).

## Status / next steps

- [x] worktree `.yggdrasil-worktrees/dev-brief` on branch `dev-brief`
- [x] scaffold copied from examples/remediation, artifacts stripped
- [ ] types.gleam
- [ ] cycle.gleam, verdicts.gleam
- [ ] codecs.gleam
- [ ] activities.gleam, dev_brief.gleam, review_lens.gleam
- [ ] gleam.toml / workflow.toml / schemas/ / test/ rewrite; `gleam test` green
- [ ] worker rewrite; cargo clippy -D warnings + tests green
- [ ] package (`aion package`), deploy to :8080 server, launchd worker
      io.ablative.aion-worker-dev-brief (mirror remediation plist)
- [ ] LIVE proof on a real small stack brief, driven through the console;
      verify lenses run in parallel ON TOM'S SCREEN before claiming it
- Bars: never trust green I didn't run; cargo fmt (never --check) before
  commit; stage explicit paths; package name for engine builds is aion-rs.
