# Dev pipeline, slice 1: brief-forge — task in, refuted brief out

The first slice of the dev-pipeline Aion workflow package: `brief_forge`
(prospekt doctrine: `workflows/brief-forge.md`). A finding/task goes in;
a grounded, refuted, dispatchable brief comes out. This is where frontier
judgment is spent deliberately so everything downstream can run cheaper —
the brief is the compression boundary.

Generalized from [`examples/stacked-dev/`](../stacked-dev/): same package
shape (Gleam workflow + `workflow.toml` + standalone Rust norn worker),
same prompt discipline (pure projections built in Gleam, relayed by a dumb
worker), same norn invocation and envelope decoding.

## Composition

```
brief_forge                          (workflow.define)
│
├── scout      (activity: norn --print, scout profile,
│               → schemas/scout-report.schema.json)
│
└── forge loop (bounded by refute_cap — a REQUIRED input, no baked defaults):
      ├── design   (activity: norn --print, designer profile,
      │             session <task_ref>-design resumed across rounds,
      │             → schemas/brief.schema.json)
      ├── refute   (activity: norn --print, refuter profile,
      │             FRESH session <task_ref>-refute-r<N> per round,
      │             → schemas/refutation.schema.json)
      ├── design_survives            → the WORKFLOW stamps
      │                                `refutation_survived` (accept step —
      │                                never the designer) → Converged
      ├── !survives, round < cap     → next design round, refutation as input
      └── !survives, cap exhausted   → Contested: last brief + last
                                       refutation surfaced to the operator
                                       (a finding, never an error crash)
```

Rules this workflow owns (from the doctrine page):

- **Root cause before fix** — schema-enforced: `kind=bug` requires
  `problem.root_cause` (if/then in `schemas/brief.schema.json`).
- **Gates assert outcomes** — `asserts` has no legal "shape" value, and the
  refuter's `gate_audit` independently hunts pass-and-still-wrong holes.
- **The refuter sees artifacts, not vibes** — its prompt carries the draft
  brief and the scout report, never the designer's reasoning; each refute
  round runs in a fresh norn session.
- **The workflow stamps** — any designer-set `refutation_survived` is
  cleared on receipt; the accept step is plain workflow code.
- **Diagnosis is a landable terminus** — `diagnose_only` rides the input
  into the design prompt and the output verbatim.

## System instructions

The three prompt preambles are the system-instruction BODIES of the
prospekt doctrine profiles (`prospekt/doctrine/profiles/{scout,designer,
refuter}.md`, frontmatter stripped), inlined verbatim as constants in
`src/dev_pipeline/prompts.gleam` — the stacked-dev mechanism (instructions
prepended to the projected prompt; norn's `--profile` flag deliberately
unused so the package is self-contained). The worker pins `--model gpt-5.5`
for all three stages (light-mode pilot policy); reasoning efforts follow
the profiles (scout `medium`, designer/refuter `high`).

## Required input — no baked defaults

`schemas/brief-forge.input.schema.json` (copied from prospekt doctrine,
like the three stage schemas beside it). Every cap is required;
`related_refs` entries are pasted SCOPE TEXT, never bare task IDs.

## Running it live

```bash
# Build the archive.
aion package examples/dev-pipeline --build

# Start a server and deploy.
aion server --config dev-config.toml
aion deploy examples/dev-pipeline/brief-forge.aion

# Build and run the standalone activity worker (norn-worker/ — its own
# crate against the published aion-worker SDK, NOT a workspace member,
# stacked-dev convention). It serves scout/design/refute by shelling the
# real `norn` CLI, so norn must be on its PATH and authenticated. The
# activities dispatch on the `agents` task queue, so that is the worker's
# default --task-queue.
cd examples/dev-pipeline/norn-worker && cargo build
./target/debug/dev-pipeline-worker-norn \
  --endpoint http://127.0.0.1:50051 \
  --namespace dev-pipeline

# Start the pilot run: the content-hash diagnosis task, diagnose-only.
aion start brief_forge --input '{
  "task_statement": "The same workflow source deploys as 4-5 different package versions — content hash differs across deploys with no source change. Diagnose the root cause.",
  "task_ref": "content-hash-drift",
  "repo_root": "/abs/path/to/aion",
  "base_ref": "main",
  "refute_cap": 1,
  "diagnose_only": true
}'
```

The output (`schemas/brief-forge.output.schema.json`) carries the brief
object verbatim, the surviving/last refutation, the rounds used, and
`diagnose_only` passed through. `outcome` is `"converged"` or
`"contested"` — with `refute_cap: 1`, a first-round kill IS the contested
result: the operator gets the draft and the attack side by side.

## Layout

```
workflow.toml                       one [[workflow]] entry (brief_forge)
schemas/                            the four prospekt doctrine schemas
                                    (input + scout-report + brief +
                                    refutation, copied verbatim) + the
                                    authored output schema
src/brief_forge.gleam               entry: scout → capped design⇄refute
                                    loop → stamp/surface
src/dev_pipeline/types.gleam        schema-mirroring domain types
src/dev_pipeline/codecs.gleam       JSON codecs (reports + input/output/error)
src/dev_pipeline/prompts.gleam      profile bodies + pure prompt projections
src/dev_pipeline/activities.gleam   typed activity constructors
                                    (task_queue "agents")
src/dev_pipeline/locals.gleam       loud typed no-local-impl seam (slice 1)
norn-worker/                        standalone Rust worker: scout/design/
                                    refute handlers shelling norn with
                                    --output-schema per stage (include_str!
                                    of schemas/) and --model gpt-5.5
```

## Slice-1 boundaries

- No hermetic test suite yet (stacked-dev's fake-CLI shim pattern is the
  template when it lands); the `locals` seam fails loudly instead of
  shelling.
- Session ids derive from `task_ref` (`<task_ref>-scout`, `-design`,
  `-refute-r<N>`): re-running the same `task_ref` resumes the previous
  run's norn sessions rather than starting clean.
- No live status query yet (stacked-dev's `set_status` pattern applies
  directly when wanted).
