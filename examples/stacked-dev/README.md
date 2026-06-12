# Stacked dev — brief in, landed on main out

The stacked-dev workflow family takes a brief from "nothing" to "landed":
provision an isolated workspace, warm the build cache **concurrently** with
the dev agent, converge a bounded scoped verify-fix loop, pass the
authoritative gate, survive a signal-driven human/SDK review loop, and land
the stack. It exercises exactly the things Aion exists for: durability
across crashes, typed composition, real concurrency, signal waits for
human-scale pauses, and bounded retry loops with typed exhaustion.

Authored against
[`docs/briefs/stacked-dev-workflow-request.md`](../../docs/briefs/stacked-dev-workflow-request.md);
the section-7 open questions are resolved in code and indexed below.

## Composition

```
stacked_dev                         (top-level workflow.define)
│
├── provision_workspace             (activity: yg branch add + provision)
│
├── onatopp_dev  (child workflow — spawn_and_wait)
│     ├── workflow.all([warm_build, dev])      cargo build ∥ norn --print
│     └── scoped_verify_loop  (bounded by verify_fix_cap):
│           ├── scoped_checks   (activity: yg graph affected +
│           │                    yg diagnostics check --package <each>)
│           └── dev_resume      (activity: norn --print --resume w/ diagnostics)
│               └── workflow.sleep(round_backoff_ms) between rounds
│
├── gate  (child workflow — spawn_and_wait)
│     └── full_checks            (activity: yg diagnostics check --workspace)
│
├── review_loop  (bounded by review_cap):
│     ├── request_review         (activity: meridian review request)
│     ├── await_verdict          (workflow.receive("review_verdict")
│     │                           inside workflow.with_timeout(review_deadline_ms))
│     ├── dev_resume             (on RequestChanges: norn resume w/ review notes)
│     └── gate                   (re-gate each fix round)
│
└── land                         (activity: yg branch merge — into the base ref)
```

All three workflows are `[[workflow]]` entries of this one package
(`workflow.toml`), so `onatopp_dev` and `gate` are independently
dispatchable for partial runs **and** composed by `stacked_dev` through
`workflow.spawn_and_wait`. Deploy all three archives together: the engine
resolves a spawned child's type by entry module name.

Live status: `stacked_dev` answers a `stacked_dev_status` query and
`onatopp_dev` answers `onatopp_dev_status`, both `{phase, round}`,
re-registered at every stage transition so replies always reflect live
state (and replay re-arms them automatically).

## Required input — no baked defaults

Every loop cap, backoff, and deadline is a **required** input field (open
question Q5): the caller decides, the workflow bakes nothing in.
`repo_root` is the absolute path of the repository to provision from; the
worktree lands at `<repo_root>/.yggdrasil-worktrees/stacked-dev-<brief_id>`,
so every activity holds an absolute working directory.

```json
{
  "repo_root": "/abs/path/to/repo",
  "brief_id": "brief-7",
  "reviewers": ["your-member-name"],
  "base_ref": "main",
  "placement": "local",
  "isolation": "worktree",
  "brief": "Implement the widget",
  "design": "docs/design.md",
  "checklist": "docs/checklist.md",
  "stories": ["story-1", "story-2"],
  "verify_fix_cap": 3,
  "review_cap": 3,
  "round_backoff_ms": 30000,
  "review_deadline_ms": 86400000
}
```

## Running the tests

```bash
cd examples/stacked-dev
gleam test
```

The suite runs the **full pipeline in-process** under the `aion/testing`
harness: both children execute their real `execute` functions through
`spawn_and_wait` (typed child doubles registered with
`testing.mock_child`), and every activity executes its real local
implementation, which genuinely shells out. Hermeticity comes from
per-test fake-CLI shims (`yg`, `norn`, `cargo`, `meridian`) written to a private
directory placed **alone** on `PATH`; the shims emit canned JSON and record
their argv to log files the tests assert against. No real norn/meridian
install is needed — and the suite proves that a missing CLI with no shim is
a loud typed failure, never a silent skip.

Covered scenarios: the happy path (approve on round one), verify-fix
convergence on round two (diagnostics reach `norn resume`'s argv),
verify-fix exhaustion (`VerifyExhausted` with the last diagnostics), a
converged loop the authoritative gate still fails (`GateRejected`), the
RequestChanges round-trip (structured notes reach `dev_resume`, the gate
re-runs, the stack lands once), review-cap exhaustion
(`ReviewCapExhausted`), Reject, review timeout, an advisory warm-build
failure that does not fail the run, live status queries per phase, and the
loud missing-CLI failure.

## Running it live

A live run needs three processes: the server, the **worker** that serves the
eight activities, and the CLI driving the run.

```bash
# Build the three archives.
aion package examples/stacked-dev --build

# Start a server and deploy ALL THREE (children resolve by entry module).
aion server --config dev-config.toml
aion deploy examples/stacked-dev/stacked-dev.aion
aion deploy examples/stacked-dev/onatopp-dev.aion
aion deploy examples/stacked-dev/gate.aion

# Build and run the standalone activity worker (worker/ — its own crate
# against the published aion-worker SDK, NOT a workspace member). It serves
# all eight activity names by shelling to the real CLIs (yg, norn, cargo,
# meridian), so those must be on its PATH — or fake-CLI shims, exactly like
# the test suites use.
cd examples/stacked-dev/worker && cargo build
./target/debug/stacked-dev-worker --endpoint http://127.0.0.1:50051   # [server] grpc_address

# Start a run with the full required input.
aion start stacked_dev --input '{
  "repo_root": "/abs/path/to/repo",
  "brief_id": "brief-7", "reviewers": ["your-member-name"],
  "base_ref": "main",
  "placement": "local", "isolation": "worktree",
  "brief": "Implement the widget",
  "design": "docs/design.md", "checklist": "docs/checklist.md",
  "stories": ["story-1"],
  "verify_fix_cap": 3, "review_cap": 3,
  "round_backoff_ms": 30000, "review_deadline_ms": 86400000
}'

# Watch the phase advance.
aion query <workflow-id> stacked_dev_status

# When the run parks in the review wait, drive the verdict by hand:
aion signal <workflow-id> review_verdict --payload '{"decision":"approve"}'

# Structured change requests and rejections are typed payloads too:
aion signal <workflow-id> review_verdict --payload '{
  "decision": "request_changes",
  "notes": [{"file": "crates/aion-core/src/lib.rs", "line": 42,
             "note": "tighten the error taxonomy"}]
}'
aion signal <workflow-id> review_verdict --payload \
  '{"decision":"reject","reason":"wrong architecture"}'
```

The worker registers the eight activity names (`provision_workspace`,
`warm_build`, `dev`, `scoped_checks`, `dev_resume`, `full_checks`,
`request_review`, `land` — `await_verdict` is a signal, not an activity)
and mirrors the local implementations in `src/stacked_dev/locals.gleam`
invocation for invocation; its serde types in `worker/src/types.rs` are
pinned byte-compatible with the Gleam codecs by `worker/tests/wire_compat.rs`.
Note the `warm_build`/`dev` handlers exchange the tagged
`StartupTask`/`StartupResult` envelope because the two activities run
through one homogeneous `workflow.all` fan-out. The whole live loop —
archives from source, real server, this worker with shims on PATH, the
review signal, landed completion — is proven end-to-end by
`crates/aion-cli/tests/stacked_dev_live_e2e.rs`.

## How the section-7 open questions were resolved

| Q | Resolution | Where |
|---|---|---|
| Q1 scoping seam | `scoped_checks`' local impl shells to a CLI returning the affected set; the workflow stays pure and consumes `affected_modules` from the result. Empty scoping falls back **loudly** to a named workspace-wide scope. | `src/stacked_dev/locals.gleam` (`scoped_checks`), `types.CheckResult` |
| Q2 gate scope | Workspace-wide today; `GateScope = WorkspaceWide \| AffectedClosure(List(String))` keeps the typed seam, only `WorkspaceWide` exercised. | `types.GateScope`, `locals.full_checks` |
| Q3 verdict payload | Structured: `ReviewVerdict(Approve \| RequestChanges(notes) \| Reject(reason))` with per-finding `ReviewNote(file, line, note)`; `dev_resume` consumes the notes as data. | `types.ReviewDecision`, `codecs_flow.review_notes_feedback` |
| Q4 warm cache | `warm_build` returns advisory `BuildWarm(ok, duration_ms)`; a failed build forfeits the cache without failing the run. Cache sharing per isolation mode is an open `TODO(meridian)`. | `types.BuildWarm`, `locals` warm build |
| Q5 caps/backoff | `verify_fix_cap`, `review_cap`, `round_backoff_ms`, `review_deadline_ms` are REQUIRED input fields (schema-enforced). No arbitrary defaults. | `types.StackedDevInput`, `schemas/input.json` |
| Q6 one or a family | A family: three independently dispatchable `[[workflow]]` entries, with `stacked_dev` composing the children via `spawn_and_wait`. | `workflow.toml`, `src/stacked_dev.gleam` |

## TODO(meridian) seam inventory

Every Meridian-specific unknown is marked in code rather than guessed:

| Seam | Location |
|---|---|
| Exchange-VM dispatch for `Copy`/`Overlay`/`Vm` isolation | `src/stacked_dev/locals.gleam`, `provision_workspace` |
| Complete affected-closure gate scope | `src/stacked_dev/locals.gleam`, `full_checks` |
| `--profile <dev profile>` + richer prompt assembly (design-context extraction, per-R# rendering from `onatopp-dev-norn/workflow.rhai`) | `src/stacked_dev/locals.gleam`, `dev` |
| Carry the workspace root on `ResumeInput` so resume can confine file tools with `--workspace-root` | `src/stacked_dev/locals.gleam`, `dev_resume` |
| Warm-cache sharing across isolation modes | `src/stacked_dev/types.gleam`, `BuildWarm` doc |

Resolved since first authoring (now real commands, asserted in tests):
worktree provisioning (`yg branch add` + `yg branch provision --path`),
affected-set scoping (`yg graph affected --plain --direct-only`), scoped
and workspace checks (`yg diagnostics check`), norn headless invocation
(`--print --session-id/--resume --output-schema --output-format json`
with a deterministic branch-derived session id), norn's JSON envelope
(confirmed live: the schema-constrained result sits under `"output"`,
alongside usage/model/event telemetry — decoded by both `locals.gleam` and
the worker), the review request (`meridian review request <BRANCH>
--reviewer <NAME>... --as Meridian` — branch positional FIRST because the
greedy `--reviewer` flag swallows a trailing positional; reviewers a
required input; the request is always signed as the Meridian system
member; workspace from the CLI's global config), and landing (`yg branch merge <branch>` into the tree
parent — local, no PR machinery; output is `{branch, merged_into}`).

## Layout

```
workflow.toml                  three [[workflow]] entries + activity lists
schemas/                       input/output JSON Schema per entry
src/stacked_dev.gleam          top-level workflow (review loop, land)
src/onatopp_dev.gleam          dev child (startup fan-out, verify-fix loop)
src/gate.gleam                 gate child (full_checks)
src/stacked_dev/types.gleam    shared domain types
src/stacked_dev/codecs_*.gleam JSON codecs
src/stacked_dev/activities.gleam  typed activity constructors
src/stacked_dev/locals.gleam   CLI-shelling local impls (the test seam)
src/stacked_dev/cli.gleam      typed process-runner boundary
src/stacked_dev_cli_ffi.erl    Erlang port runner
test/                          hermetic shim-driven behavioral suite
worker/                        standalone Rust activity worker (own crate,
                               published aion-worker SDK; wire-compat +
                               shim-driven handler tests)
```
