# Dev pipeline: brief-forge + implement-and-gate

The dev-pipeline Aion workflow package, two slices so far:

- **`brief_forge`** (prospekt doctrine: `workflows/brief-forge.md`) — a
  finding/task goes in; a grounded, refuted, dispatchable brief comes out.
  This is where frontier judgment is spent deliberately so everything
  downstream can run cheaper — the brief is the compression boundary.
- **`implement_and_gate`** (doctrine: `workflows/implement-and-gate.md`) —
  a brief goes in; a reviewed-ready diff in an isolated workspace comes
  out, with the gate battery green as RECORDED FACT. The implementer is an
  agent; every gate is a command activity whose own exit status lands in
  durable history.

Generalized from [`examples/stacked-dev/`](../stacked-dev/): same package
shape (Gleam workflow + `workflow.toml` + standalone Rust norn worker),
same prompt discipline (pure projections built in Gleam, relayed by a dumb
worker), same `CliRun` exit-status-is-data gate pattern.

**Driven mode:** the brief-forge agent rounds (`scout`/`design`/`refute`)
run through the neutral agent-harness seam
([`aion-integration-norn`](../../crates/aion-integration-norn)) in norn
DRIVEN MODE (`norn --protocol jsonrpc`) rather than a `norn --print`
shell-out — every agent event lands as a durable `ActivityEvent` (live
transcript in the ops console) and running sessions accept
InjectMessage/Cancel interventions. The activity input IS the projected
prompt text; per-stage output schemas, the session template, and the model
pin are harness spawn arguments owned by the worker. The implementer
rounds still shell `norn --print` (see "Current boundaries").

## Composition: brief_forge

```
brief_forge                          (workflow.define)
│
├── scout      (agent activity, DRIVEN norn session, scout profile,
│               → schemas/scout-report.schema.json)
│
└── forge loop (bounded by refute_cap — a REQUIRED input, no baked defaults):
      ├── design   (agent activity, DRIVEN norn session, designer profile,
      │             session {workflow_id}-design resumed across rounds,
      │             → schemas/brief.schema.json)
      ├── refute   (agent activity, DRIVEN norn session, refuter profile,
      │             session {workflow_id}-refute — resumed across loop
      │             rounds, a driven-mode deviation from the fresh-per-round
      │             shell behaviour,
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
  brief and the scout report, never the designer's reasoning. (Driven-mode
  deviation: refute-loop rounds within one run resume ONE refuter session
  rather than each getting a fresh one — the harness session template has
  no per-round placeholder yet; the information-hygiene rule itself is
  owned by the prompt projection and is unaffected.)
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
for all three stages (light-mode pilot policy). Harness spawn arguments
are fixed per worker process, so the shell path's per-stage reasoning
efforts (scout `medium`, designer/refuter `high`) collapse to ONE
`--reasoning-effort` knob on the worker (default `high`, preserving the
two quality-bearing rounds).

## Required input — no baked defaults

`schemas/brief-forge.input.schema.json` (copied from prospekt doctrine,
like the three stage schemas beside it). Every cap is required;
`related_refs` entries are pasted SCOPE TEXT, never bare task IDs.

## Running it live

The worker serves over the liminal server-push transport (driven mode
needs it — the gRPC worker path has no agent-harness seam), so the server
must have the outbox commissioned with a liminal listener. The repo's
`dev-config.toml` does not carry that block; add (or use a config that
has, e.g. `examples/agent-dev/demo-config.toml`):

```toml
[outbox]
enabled = true
liminal_listen_address = "127.0.0.1:50061"
```

```bash
# Build the archives (one per [[workflow]] entry: brief-forge.aion and
# implement-and-gate.aion; --out is a single-workflow flag and no longer
# applies to this package).
aion package examples/dev-pipeline --build

# Start a server and deploy.
aion server --config <config-with-the-outbox-block>.toml
aion deploy examples/dev-pipeline/brief-forge.aion
aion deploy examples/dev-pipeline/implement-and-gate.aion

# Build and run the standalone MIXED activity worker (norn-worker/ — its
# own crate against the in-repo aion-worker SDK, NOT a workspace member,
# stacked-dev convention). scout/design/refute run through the composed
# driven-mode harness (`norn --protocol jsonrpc`); the rest are typed
# registry handlers. norn must be on PATH (or --norn-bin) and
# authenticated. --address is the server's [outbox]
# liminal_listen_address; agent activities dispatch on the `agents` task
# queue, the worker's default. Point --workspace-root at the TARGET
# REPOSITORY: it becomes each driven run's file-tool confinement
# (--workspace-root) and tool cwd (-C); omit it and runs are unconfined,
# steered only by the prompt's "Repository root:" line.
cd examples/dev-pipeline/norn-worker && cargo build
./target/debug/dev-pipeline-worker-norn \
  --address 127.0.0.1:50061 \
  --namespace dev-pipeline \
  --workspace-root /abs/path/to/target-repo

# Optional worker flags: --schemas-dir <dir> (where the embedded stage
# schemas are materialized as scout.json/design.json/refute.json for
# `--output-schema <dir>/{activity_type}.json`; default: a per-process
# directory under the system temp dir), --reasoning-effort <level>
# (default high, one knob for all driven stages), --norn-timeout
# <duration> (default 30m per driven step — a wedged run ends in norn's
# typed timed_out envelope), --norn-bin, --identity, --concurrency.

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

## Composition: implement_and_gate

```
implement_and_gate                   (workflow.define)
│
├── provision_workspace  (activity, queue "workspaces": git worktree/clone
│                         of repo_root at base_ref under
│                         <repo_root>/.dev-pipeline-workspaces/;
│                         failure is terminal)
│
├── implement            (activity, queue "agents": norn --print INSIDE the
│                         workspace, implementer profile + the brief
│                         EMBEDDED VERBATIM + out_of_scope called out,
│                         session <task_ref>-implement,
│                         → schemas/implementation-report.schema.json;
│                         model = implementer_model input when set,
│                         else the worker's pilot gpt-5.5)
│
└── gate loop (bounded by fix_cap — a REQUIRED input, no baked defaults):
      ├── run_gate per declared gate, in order (queue "workspaces"):
      │     sh -c <command> in the workspace →
      │     CliRun{exit_status, output, duration_ms} — NON-ZERO EXIT IS
      │     DATA, not an error; only a missing binary/workspace is
      │     terminal; battery short-circuits at the first red gate
      ├── all exit 0                → GatesGreen: report + gate record +
      │                               rounds + workspace_path (workspace
      │                               kept — review needs the diff)
      ├── red, fix_round < fix_cap  → implement_resume: SAME session, the
      │                               failing gate's CAPTURED output
      │                               (tail-bounded at capture, never a
      │                               paraphrase) → full replacement
      │                               report → ALL gates re-run from the
      │                               top
      └── red, fix_round = fix_cap  → GatesExhausted: full gate record
                                      surfaced to the operator (a finding,
                                      never an error crash; workspace kept
                                      for inspection)
```

Rules this workflow owns (from the doctrine page):

- **Exit status is the verdict** — the workflow cannot reach `GatesGreen`
  with a red gate; that is topology, not policy. No pipe sits in the
  judged position; no agent ever certifies a gate.
- **The implementer works the brief, nothing else** — the brief rides the
  input embedded verbatim; `out_of_scope` is called out in the prompt and
  crossing it is a stop-and-surface event.
- **Fix rounds get the captured output** — the durable `CliRun` record,
  tail-bounded at capture by the worker, never the implementer's memory of
  it.
- **The frontier escape hatch is an input** — `implementer_model`
  overrides the pilot model at harness invocation, so the tier a diff was
  written on is always in history.
- **The workspace survives both termini** — `teardown_workspace` exists as
  a declared activity seam but is deliberately never dispatched (see
  deviation note in the workflow's module doc).

### Example input

A toy brief with a two-gate battery (`fix_cap` and every other cap is a
REQUIRED input — no baked defaults):

```bash
aion start implement_and_gate --input '{
  "brief": {
    "title": "Fix the greeting typo in the hello-world example",
    "task_ref": "hello-typo-1",
    "problem": {
      "statement": "examples/hello-world prints \"Helo\" instead of \"Hello\".",
      "kind": "bug",
      "root_cause": {
        "statement": "The greeting literal in src/hello_world.gleam is misspelled.",
        "causal_chain": ["literal is \"Helo\"", "output prints \"Helo\""],
        "evidence": "src/hello_world.gleam:12"
      }
    },
    "fix_design": {
      "approach": "Correct the literal; touch nothing else.",
      "touch_points": ["src/hello_world.gleam"],
      "invariants_to_preserve": ["output format otherwise unchanged"],
      "rejected_alternatives": [
        {
          "alternative": "Rename the constant while in there",
          "why_rejected": "Unrelated churn; out of scope."
        }
      ],
      "risks": []
    },
    "acceptance_gates": [
      {
        "id": "G1",
        "statement": "The program prints Hello.",
        "kind": "command",
        "asserts": "outcome",
        "command": "cargo check"
      }
    ],
    "out_of_scope": ["any file other than src/hello_world.gleam"],
    "not_covered": []
  },
  "repo_root": "/abs/path/to/repo",
  "base_ref": "main",
  "isolation": "worktree",
  "fix_cap": 2,
  "gates": [
    { "id": "fmt", "command": "cargo fmt" },
    { "id": "check", "command": "cargo check" }
  ]
}'
```

Optional inputs: `node` (pins every workspace-bound step to the node
holding the worktree — set it on multi-node clusters), `implementer_model`
(the frontier escape hatch).

### Worker queues

One worker process serves ONE task queue. `scout`/`design`/`refute`
(driven) and `implement`/`implement_resume` (shell) dispatch on `agents`
(the binary's default); `provision_workspace`/`run_gate`/
`teardown_workspace` dispatch on `workspaces` — so a full
implement_and_gate deployment runs TWO instances of the same binary on the
node holding the workspaces (the harness config and every handler are
wired in both; only the queue subscription differs):

```bash
./target/debug/dev-pipeline-worker-norn --address 127.0.0.1:50061 \
  --namespace dev-pipeline \
  --workspace-root /abs/path/to/target-repo     # serves the agents queue
./target/debug/dev-pipeline-worker-norn --address 127.0.0.1:50061 \
  --namespace dev-pipeline --task-queue workspaces
```

## Layout

```
workflow.toml                       two [[workflow]] entries (brief_forge,
                                    implement_and_gate)
schemas/                            the prospekt doctrine schemas (both
                                    inputs + scout-report + brief +
                                    refutation + implementation-report,
                                    copied verbatim) + the two authored
                                    output schemas
src/brief_forge.gleam               entry: scout → capped design⇄refute
                                    loop → stamp/surface
src/implement_and_gate.gleam        entry: provision → implement → gate
                                    battery ⇄ capped fix loop →
                                    green/exhausted
src/dev_pipeline/types.gleam        schema-mirroring domain types
src/dev_pipeline/codecs.gleam       JSON codecs (reports + input/output/error)
src/dev_pipeline/prompts.gleam      profile bodies + pure prompt projections
src/dev_pipeline/activities.gleam   typed activity constructors (queues
                                    "agents" + "workspaces", optional node
                                    pinning)
src/dev_pipeline/locals.gleam       loud typed no-local-impl seam
norn-worker/                        standalone MIXED Rust worker over the
                                    liminal push transport:
                                    scout/design/refute through the
                                    driven-mode NornHarness
                                    (--output-schema materialized per
                                    activity type from include_str! of
                                    schemas/), plus typed handlers for git
                                    provisioning, the shell-path
                                    implementer rounds, and sh -c gate
                                    commands. Path-dep on the in-repo
                                    aion-worker/-integrations/-integration-
                                    norn (the harness seam is unpublished)
```

## Current boundaries

- `implement`/`implement_resume` still shell `norn --print` instead of
  riding the driven harness: harness spawn arguments are fixed per worker
  process (only `{workflow_id}`/`{activity_type}` interpolate), and the
  implementer rounds need a PER-RUN `--workspace-root` (the workspace path
  is minted at runtime by `provision_workspace`, collision suffixes and
  all) plus ONE session shared across the two activity types
  (`implement_resume` must resume `implement`'s session). Until the seam
  grows per-run spawn parameters, they stay shell — observable only by
  their terminal result, not live.
- Driven-stage norn session ids are `{workflow_id}-{activity_type}`:
  distinct runs never share sessions (an improvement over the old
  `task_ref`-derived ids), `design` keeps its context across loop rounds,
  retries resume — but refute-loop rounds within one run resume ONE
  refuter session instead of each getting a fresh one.
- No hermetic test suite yet (stacked-dev's fake-CLI shim pattern is the
  template when it lands); the `locals` seam fails loudly instead of
  shelling.
- No live status query yet (stacked-dev's `set_status` pattern applies
  directly when wanted).
- The doctrine's implementer-model-equals-review-lens rejection rule has
  no enforcement point yet — no review lens exists in this package until
  the truth-pass slice lands.
