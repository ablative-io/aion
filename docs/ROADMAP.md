# Roadmap — open work as of 2026-06-12 (main `d8d3b35c`)

Everything below is queued, not in flight. Items are dependency-ordered
within each section. Status of the released stack: aion 0.5.0 on crates.io
(unified `aion` binary), `aion_flow` 0.3.0 on hex, beamr 0.6.0 underneath;
outside-in validated end to end.

## 1. Pending design decision: parent-close policy (cancellation cascade)

**The fact (pinned by `crates/aion/tests/nested_workflows_e2e.rs`):**
cancelling a workflow records its terminal and kills only its own process.
Child workflows are deliberately not process-linked (`src/child/spawn.rs`);
the awaiting parent's `child.await` fails with `cancelled:<reason>` (D-4
mapping, `src/runtime/nif_child_watch.rs`), but the cancelled workflow's
own descendants are **left resident** — live Running processes that nothing
reaps, surviving indefinitely until their own terminals. A grandchild
parked on a three-month timer outlives its entire cancelled tree.

**Recommendation (undecided — Tom's call):** Temporal-style per-spawn
parent-close policy, `RequestCancel | Terminate | Abandon`, as a
**required** argument on `child.spawn` / `spawn_and_wait` (the
no-arbitrary-defaults rule means authors must choose, not inherit).
`Abandon` is today's behavior made explicit. Engine work: propagate on
parent terminal (all terminals, not just cancel), recursively;
recovery must re-arm pending propagations. SDK + docs + the
`workflows.md` child section ("a per-spawn parent-close policy is under
design") update when it lands.

## 2. Proof portfolio (the "contemporary of Temporal" wave)

Goal: every public claim has a witnessable, executable receipt — not just
internal test suites. Agreed plan, in credibility-per-effort order:

1. **Claims ledger** — `docs/CLAIMS.md`: every strong claim (survives
   kill -9, byte-identical replay, zero activity re-execution, mid-flight
   version pinning, queries append nothing) mapped to the named runnable
   test/demo that proves it and the exact command. The nested-e2e suite's
   full-`Vec<Event>` equality assertions are ledger-grade receipts already.
2. **Public CI on fresh clones** — no `.github/workflows` exists yet.
   Full gates (fmt, clippy -D warnings, `cargo test -p` every crate) plus
   the from-source example builds (`tests/common/example_build.rs` needs
   `gleam` on the runner). This is the credibility keystone: our worst
   release bug was tests that only passed on one machine against stale
   gitignored archives.
3. **Chaos gate** — a harness that runs the order saga while killing the
   server at *random* points N times, asserting completion with
   byte-identical history and zero re-executed activities, in CI on every
   commit.
4. **Recorded demos** — scripted asciinema: the kill -9 recovery demo, the
   v1-pinned/v2-routed deploy demo (~3 min of terminal time each, proven
   by the dogfood runs).
5. **Published benchmark numbers** — `benchmarks/million-processes` exists
   but its results were never published. Add workflows/sec, signal latency,
   recovery-time-vs-history-length curves.
6. **Honest Temporal side-by-side** — the same order saga implemented on
   both, in one repo: LOC, infra footprint (docker-compose cluster vs one
   binary), cold start, RAM, operational story. Concede what Temporal wins:
   battle-testing, horizontal scale-out, ecosystem. **Do not claim
   multi-node scale-out — we cannot demonstrate it.**

## 3. Authoring roadmap (sequenced)

1. **Schema→Gleam codegen** — `aion new` ships schemas but codecs are
   hand-derived in the template. Add a build/package step (or
   `aion codegen`) regenerating `<name>_io.gleam` (types + codecs) from
   `schemas/*.json` so the schema is the single source of truth. Pairs with
   the scaffold; do first.
2. **Dev-pipeline template** — lift `examples/stacked-dev` into an
   `aion new` template (the fourth), once codegen exists so the template's
   codecs are generated, not vendored.
3. **`aion dev`** — watch mode: rebuild + repackage + hot-redeploy on file
   change (content-hash namespacing already makes redeploy safe).
4. **Dashboard timeline** — per-run event timeline view in aion-dashboard.
5. **Elixir SDK** — BEAM-native polyglot authoring (the strategic counter
   to Temporal's client-runtime polyglot story; see §6 of the Temporal
   discussion — we never build client-side determinism cores).
6. **Declarative DSL + visual builder** — on top of the typed SDK, not
   instead of it.
7. **WASM workflow runtime** — long-term polyglot path (beamr-wasm exists;
   banked beamr items below are prerequisites).

## 4. Next publish bundle (whenever Tom is ready)

Riding in main, unpublished:
- `aion-cli` → `aion` hint-string fix in `crates/aion-cli/src/render.rs`
  (published 0.5.0 binary still prints old hints).
- `aion new` scaffold subcommand (+ templates).
- `testing.mock_child` typed child doubles in the Gleam SDK → **hex
  `aion_flow` 0.4.0** (Tom publishes hex; scaffold templates pin
  `aion_flow >= 0.3.0 and < 0.4.0` — bump the template pin in the same
  wave).
- Crates publish order: aion-server **before** aion-cli.

## 5. Meridian integration (Tom's current focus)

`examples/stacked-dev` is the contract. Meridian owes:
- Real flag/schema wiring at the eight `TODO(meridian)` seams — inventoried
  in `examples/stacked-dev/README.md`, all in
  `src/stacked_dev/locals.gleam` (+ one on `types.BuildWarm`): exchange-VM
  dispatch, provision flags, `affected-modules` subcommand, affected-closure
  gate scope, norn run/resume flags, review-request command, stack
  submit/land schemas, warm-cache sharing per isolation mode.
- A Meridian worker serving the eight activity names
  (`provision_workspace`, `warm_build`, `dev`, `scoped_checks`,
  `dev_resume`, `full_checks`, `request_review`, `land`) mirroring the
  local impls. Note: `warm_build`/`dev` use the tagged
  `StartupTask`/`StartupResult` envelope (one homogeneous `workflow.all`
  fan-out).
- Dispatch contract is `examples/stacked-dev/schemas/input.json`
  (`additionalProperties: false`; caps/backoff/deadline are required —
  Meridian chooses the values, the workflow bakes nothing in).

## 6. Known flakes and loose ends

- **stacked-dev worker: mint-or-resume for crash recovery (Tom,
  2026-06-13).** Activities are at-least-once; a crash mid-`dev` re-runs
  the step. The session id is deterministic (branch-derived) and norn
  persists sessions to disk with `--resume <ID>`, so the conversation
  survives — but the worker's dev handler currently always mints
  (`--session-id`). Rider: detect the existing session (or fall back to
  `--resume` on mint collision) so a kill -9 mid-development resumes the
  same agent session as if nothing happened. Same refinement applies to
  the Meridian dispatcher's norn activity later.
- **Engine: no-worker dispatch is terminal (found by the worker e2e).**
  An activity dispatched with no connected worker fails the run instead
  of parking as pending work. A durability engine should wait; folds into
  the task-queues/routing item in §7.

- Under heavy parallel load in one checkout:
  `payload_binary_remains_valid_through_spawn_and_is_released` failed once;
  `examples_e2e` hit one `Incompatible locked version` gleam-build race.
  Both pass in isolation and on clean runs; both pre-date the current wave.
  Watch once CI exists — uncontended runners won't mask a real defect.
- SDK test-harness limitation: the in-process double's `with_timeout` only
  expires at a zero deadline, so a genuine signal-arrives-after-deadline
  race can't be simulated in-process. Documented in the stacked-dev work;
  fix in the SDK harness when convenient.
- `.meridian/` untracked files were swept into commit `da2f07ba` by a
  `git add -A`; flagged, unconfirmed whether intentional.
- `.claude/` (agent worktree scaffolding) shows untracked in the repo —
  decide whether to gitignore.
- beamr banked non-blockers (tracked in the beamr repo): WASM tail-park
  apply, dirty-native tail mis-continue, WASM/JIT timer parity,
  `send_after` delivery, QoS busy-yield, file-io op-key leak. Relevant
  again when the WASM workflow runtime work starts.

## 7. Engine items noted but not scheduled

- Engine-side automatic retry dispatch from a `RetryPolicy` is not wired;
  examples model retries as workflow-driven bounded loops (the documented
  pattern). Decide whether engine-side retry is wanted at all, or whether
  the workflow-driven pattern is the permanent answer.
- Query at a cancelled-but-orphaned child: behavior is whatever the pinned
  cancellation semantics imply today; revisit alongside parent-close.
- **Activity progress / heartbeats.** Between `ActivityStarted` and its
  result the engine knows nothing about a running activity. Long activities
  that stream output (norn emitting JSON events during `dev` in
  stacked-dev) have no live surface: the CLI runner captures the full
  stream but only the final typed result is recorded — correct for
  durability/replay (history carries outcomes, not progress chatter), and
  workers are free to consume/forward the live stream out-of-band (the
  Meridian worker answer). The engine-level candidate is Temporal-style
  worker heartbeats: `heartbeat(details)` updating queryable live state
  without appending history, which also enables heartbeat timeouts for
  detecting hung activities. CONFIRMED WANTED (Tom, 2026-06-13).
- **Worker task queues / routing / affinity.** CONFIRMED WANTED (Tom,
  2026-06-13). Today activities dispatch by name to whichever connected
  worker serves that name — fine for one worker, insufficient for fleets:
  filesystem-coupled activity families (stacked-dev's provision → dev →
  checks → land all share a worktree path) must land on the SAME worker
  that provisioned the workspace. Temporal solves this with task queues;
  we want named queues (worker registers names+queue, workflow/spawn
  chooses queue) plus run-affinity ("subsequent activities of this run
  prefer the worker that served activity X"), and capability placement
  falls out (norn workers on the token box, check workers on CI-class
  machines, GPU activities by the GPU). Single-remote-worker deployments
  need none of this and work today — the worker owns its private disk and
  only data crosses the wire.
