# dev-brief: development briefs as an Aion workflow family

A brief goes in; an adversarially-reviewed branch comes out. The remediation
flow's proven skeleton (driven Norn agents, mechanical shell gates,
derive-and-check verdicts, bounded fix cycles, honest terminal dispositions)
generalized from ledger findings to arbitrary development briefs on any repo.

Every agent runs on Norn in DRIVEN mode (jsonrpc) with a schema-constrained
output — live transcript + inject/cancel in the ops console, and the
operator's ChatGPT OAuth login (no API key is ever read or passed).

## Composition

```
dev_brief                              (workflow.define)
│
├── provision_workspace   (SHELL: isolated git worktree inside the repo at
│                          .yggdrasil-worktrees/dev-brief/<id>, branch dev/<id>)
│
├── the bounded fix cycle (cap = max_fix_cycles, default 3):
│     ├── developer       (AGENT, driven; session {workflow_id}-developer
│     │                    resumed across cycles; the WORKER commits the
│     │                    round's work — agents run no git)
│     ├── run_gates       (SHELL: the brief's CONFIGURED commands, exit
│     │                    status = recorded data; {base_commit} token
│     │                    substituted in argv; + the reviewers' diff)
│     │                    red → loop back to the developer
│     ├── REVIEW FAN-OUT  (one `review_lens` CHILD WORKFLOW per lens,
│     │                    spawned CONCURRENTLY, each rooted READ-ONLY at
│     │                    the run's worktree — file-write tools denied;
│     │                    parallel agents inside one brief)
│     │                    any derived rejection → loop back with every
│     │                    verdict attached; cap exhaustion is a terminal
│     │                    DISPOSITION, never a silent success
│     └── reset_workspace (SHELL: after EVERY lens round, git clean -fd +
│                          git checkout -- . re-establish the reviewed head;
│                          any lens droppings recorded, never fatal)
│
├── verify_gates          (SHELL, on ACCEPT only, if config.verify_gates set:
│                          assert clean tree, re-run the battery as recorded
│                          evidence → result.verification; never loops back)
│
└── cleanup_workspace     (SHELL: remove the worktree; the branch and its
                           commits remain; dirty worktrees are refused. Every
                           destructive git op is guarded to the dev-brief
                           worktree root — the repo itself is unreachable)
```

Nothing pushes anywhere: the handoff is the branch plus the result's
evidence (dev report, gate runs, every lens verdict, every derive-and-check
violation) in durable history. The operator merges.

### Adversarial review, mechanically enforced

Each lens verdict is judged by DERIVE-AND-CHECK: the workflow derives the
overall from the findings (any `blocking` finding ⇒ reject), rejects a
verdict whose asserted overall disagrees, requires a `reject_reason` (and a
substantiating blocking finding) on every rejection, and records every
violation cycle-stamped on the result. An empty review round never accepts.
Default lenses: `correctness`, `regressions`, `brief_compliance` — override
per brief with your own charters.

## Writing a brief

The input schema is `schemas/brief_input.json` (`aion input dev_brief` emits
a skeleton). The parts that decide run quality:

- **objective** — what and why, concretely. This is the developer's whole
  world; write it like you're briefing a capable engineer who has never
  seen the repo.
- **pointers** — files/docs to read first. Five good pointers beat a
  paragraph of prose.
- **scope_out** — the hard walls: no-reorganization boundaries for large
  files, frozen public surfaces. The `brief_compliance` lens enforces these
  adversarially.
- **acceptance** — testable criteria. The developer must claim each one
  with evidence; the lenses check the claims against the diff.
- **config.gates** — the mechanical battery for THIS repo, e.g. for a Rust
  repo:
  ```json
  [
    {"name": "fmt",    "argv": ["cargo", "fmt", "--all"]},
    {"name": "clippy", "argv": ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]},
    {"name": "test",   "argv": ["cargo", "test", "--workspace"]}
  ]
  ```
  Write-mode formatters are fine as gates: tree mutations they leave behind
  are committed mechanically (`chore(gates): …`) so the branch stays
  complete. An empty battery is honoured as a recorded vacuous pass.

## Build, package, deploy

```sh
cd examples/dev-brief
gleam test                       # 29 workflow-side unit tests
gleam build
aion package .                   # emits dev-brief.aion + review-lens.aion
aion deploy dev-brief.aion       # BOTH archives must be deployed together:
aion deploy review-lens.aion     # the parent spawns review_lens by name

cd worker
cargo test                       # 28 hermetic worker tests (real git)
cargo build --release
./target/release/dev-brief-worker \
  --profiles-dir ./profiles \
  --identity dev-brief \
  --ready-file /tmp/dev-brief-worker.ready
```

The worker opens THREE liminal connections on task queue `dev_brief`, one
per node (`shell`, `developer`, `reviewer`). The node pin is load-bearing:
the server routes by (namespace × task_queue × node) only — see
`src/dev_brief/activities.gleam`, the authoritative node table.

## Start a run

From the ops console start form (workflow type `dev_brief`), or:

```sh
aion start dev_brief --input my-brief.json --task-queue dev_brief
```

Watch it live at the console: the developer session streams as it works,
and during each review round the lenses appear as CONCURRENT sibling
`review_lens` workflows — parallel agents inside one brief.
