# staged-rounds: a planned, phased fan-out as one AWL workflow

Supplied material goes in; merged, reviewed work comes out. One planner
agent reads the material and returns a typed, phased plan of scope-fenced
work items. Bounded rounds then fan the released items out — one worktree,
one dev agent, and one reviewer per item, IN PARALLEL — accepted items fold
into `done` and release their dependents, rejected items cycle with the
reviewer's feedback attached, accepted branches merge, and merge conflicts
feed a bounded remediation cycle whose agent RESUMES the planner's session:
the persistent coordinator judges its own plan's conflicts.

One workflow, **activities only, no children** — the whole shape is a
single AWL document on the direct compile path (`aion deploy <file.awl>`).
Every agent runs on Norn in DRIVEN mode (jsonrpc) with a schema-constrained
output — live transcript + inject/cancel in the ops console, and the
operator's ChatGPT OAuth login (no API key is ever read or passed).

## Composition

```
staged_rounds                       (awl/staged_rounds.awl, direct-compiled)
│
├── plan
│     └── planner        (AGENT, read-only, rooted at the repo;
│                         session {workflow_id}-planner; returns the typed
│                         phased plan; fold_phase seeds ready/blocked)
│
├── execute — the bounded round loop (cap = config.max_rounds):
│     ├── fork item in state.ready → provision_item     (SHELL, parallel)
│     │     one git worktree per item under
│     │     <repo>/.staged-rounds/<workflow id>/items/<item id>,
│     │     branch staged/<workflow id>/<item id>; a dependent's worktree
│     │     gets its depends_on items' accepted branches MERGED IN (it
│     │     really starts from their work); REUSED (never reset) across
│     │     rounds so a rejected item's committed work survives
│     ├── fork p in provisioned → dev_item              (AGENT, parallel)
│     │     session {workflow_id}-dev-<item id>, resumed across that
│     │     item's feedback rounds; the WORKER commits the round's work —
│     │     agents run no git
│     ├── fork d in dev_results → review_item           (AGENT, parallel)
│     │     session {workflow_id}-review-<item id>; read-only at the item
│     │     worktree (write tools denied); reconstructs the diff itself
│     └── fold_phase                                    (SHELL, pure)
│           derive-and-check each verdict (any blocking finding ⇒ reject);
│           accepted → done + dependents released; rejected → stays ready
│           with feedback attached; evidence accumulates
│
├── merge_once            (SHELL: ordered --no-ff merges of every done
│                          branch into staged/<workflow id>/integration;
│                          clean → completed, conflict → fix_conflicts)
│
└── fix_conflicts — the bounded merge loop (cap = config.max_merge_rounds):
      ├── remediate       (AGENT — RESUMES {workflow_id}-planner, rooted at
      │                    the integration worktree with the conflicted
      │                    merge IN PROGRESS; resolves the markers; the
      │                    WORKER concludes the merge — agents run no git —
      │                    and REFUSES to conclude while any conflict
      │                    marker remains in a conflicted file)
      └── merge_branches  (SHELL, idempotent continue-style re-merge)
```

Terminal dispositions are honest: `completed` (success), `exhausted`
(rounds ran out with items still pending/blocked), `conflicted` (merge
remediation ran out). Nothing pushes anywhere: the handoff is the
integration branch plus the run's accumulated evidence in durable history.
The operator merges.

## The node table

The server routes by (namespace × task_queue × node) only — never by
activity type — so this worker opens FIVE liminal connections on the one
task queue `staged_rounds`, each on its own node. The document
`awl/staged_rounds.awl` is the authoritative node table:

| activity        | node        | connection                               |
|-----------------|-------------|------------------------------------------|
| `planner`       | `planner`   | driven agent (read-only, plan schema)     |
| `provision_item`| `shell`     | typed registry handler                    |
| `dev_item`      | `developer` | driven agent (`--max-parallel` wide)      |
| `review_item`   | `reviewer`  | driven agent (read-only, `--max-parallel`)|
| `fold_phase`    | `shell`     | typed registry handler (pure)             |
| `merge_branches`| `shell`     | typed registry handler                    |
| `remediate`     | `remediation`| driven agent (resumes the planner)       |

PARALLELISM: the engine fans items out via `workflow.map` (fail-fast); the
genuine N-wide execution knob is the worker's `max_concurrency` on the
developer and reviewer connections (`--max-parallel`, default 4).

## Check, deploy, run (direct path — no Gleam project)

```sh
aion awl check examples/staged-rounds/awl/staged_rounds.awl
aion deploy    examples/staged-rounds/awl/staged_rounds.awl

cd examples/staged-rounds/worker
cargo test                       # hermetic worker tests (real git) + the
                                 # direct-path compile proof
cargo build --release
./target/release/staged-rounds-worker \
  --profiles-dir ./profiles \
  --identity staged-rounds \
  --max-parallel 4 \
  --ready-file /tmp/staged-rounds-worker.ready
```

Start the worker manually (no launchd). Then start a run from the ops
console start form (workflow type `staged_rounds`), or:

```sh
aion start staged_rounds \
  --input-file examples/staged-rounds/awl/example-input.json
# or, one motion:
aion run examples/staged-rounds/awl/staged_rounds.awl \
  --input "$(cat examples/staged-rounds/awl/example-input.json)"
```

There is no task-queue flag: the run's task queue is derived from the
deployed document (its first `worker` name), never passed at start.
`--input` is always inline JSON; a file rides `--input-file` (start only).

## Writing the input

`awl/example-input.json` is the template. The parts that decide run
quality:

- **material.brief** — the planner's whole world; write it like you're
  briefing a capable lead who has never seen the repo.
- **material.pointers** — files/docs to read first.
- **config.gates** — the mechanical battery the dev agents must run green
  before ending any turn (and the reviewers may re-run read-only).
- **config.max_rounds / max_merge_rounds** — the bounded-cycle caps; cap
  exhaustion is a terminal disposition, never a silent success.

Work item ids are git-ref-safe slugs (`[a-z0-9-]`, no leading/trailing
`-`): they name branches. A terminal per-item activity failure fails the
run — the parallel fan-out is engine-owned fail-fast.
