# Workflow-timeout build brief — WorkflowTimedOut becomes real (#42 Option A)

Lane opened 2026-07-23 on the operator's direct ruling (Option A, 2026-07-22
DM). Roles as BC-4/BC-5: Opus builds, Norn reviews adversarially, the Fable
seat's read of the final bytes is the gate of record. Branch:
`dev/wf-timeout`. Board task: #45.

## The two operator's laws (non-negotiable, tear-checked)

1. **No declared timeout = NO deadline.** A workflow whose `.awl` document
   declares no `timeout` runs until done — the arming path simply never
   executes. Not a sentinel, not a max-duration stand-in, no deadline object
   of any kind anywhere in the run's lifecycle. Proven by a test.
2. **The buried defaults can never arm.** Today
   `assemble.rs:122` writes `opts.timeout.unwrap_or(DEFAULT_WORKFLOW_TIMEOUT)`
   (1h, assemble.rs:25) into every manifest, and the legacy Gleam-artifact
   metadata defaults to 3600 at `implicit_children.rs:67-72`. Both must be
   neutralized so that ONLY an explicitly authored timeout can ever arm a
   deadline — including for **legacy archives already written with the
   defaulted 1h**: a test must prove a legacy-shaped manifest (defaulted
   timeout, no explicitness marker) arms nothing.

## Ground truth (scout-mapped, coordinator-verified at the bytes)

- The authored timeout survives to the manifest
  (`CompiledWorkflow.timeout: Option<Duration>` compile.rs:27 →
  `AwlAssembleOptions.timeout` prepare.rs:44-52 → `Manifest.timeout:
  Duration` manifest.rs:124, defaulted per law 2) and **dies at load**:
  `LoadedWorkflow` (loader/load.rs:28-33) and `CatalogEntry`
  (loader/catalog.rs:66-74) drop it. The engine cannot see it today.
- `Event::WorkflowTimedOut { envelope, timeout: String }` (event.rs:153) has
  ZERO emission sites and no `record_workflow_timed_out` on the recorder
  (recorder.rs has completed/failed/cancelled/continued_as_new only). The
  `timeout` field is a closed-set kind descriptor, user-visible via
  `one_motion.rs:133-135` and carried into `ReplayTerminal::TimedOut`.
- All run starts (fresh, continue-as-new, exit-monitor replacement,
  crash-recovery sweep) funnel through `start_workflow_with_options`
  (lifecycle/start.rs:106); `WorkflowStarted` is appended at start.rs:145-158.
  One arming site covers every variant.
- Durable timers: `TimerService::schedule/fire_timer/cancel`
  (time/timer_service.rs:81/165/118), wheel at nif_timer_bridge.rs:254,
  failover re-arm via `TimerRecovery` (time/recovery.rs:69,132,150) and
  adoption step 5 (engine/api.rs:409-420). `recover_due` fires EVERY expired
  row through the generic `fire_timer` (recovery.rs:96-130).
- Reserved-timer precedent: the schedule coordinator mints
  `TimerId::named("schedule:{id}")` (schedule/evaluator.rs:205-206) with its
  own fire routing (evaluator.rs:254, api_schedule.rs:280).
- Terminal refusal: `current_lease_terminal` (status.rs:116) checked under
  the per-handle recorder lock (completion.rs:78, terminate.rs:139,
  continue_as_new.rs:84). Only `Engine::cancel` cancels live timers today
  (`cancel_inflight_timers`, api.rs:517-559); completion/fail/CAN cancel
  nothing.
- SDK/worker wire carries no workflow timeout — this is engine-side only.

## Deliverables

1. **Plumb the declared timeout to the engine, Option-shaped end-to-end.**
   `Manifest`/`WorkflowEntry` timeout becomes explicitly-declared-or-absent
   (per-entry — synthesized/additional workflows carry their own), threaded
   through `StagedLoad` → `LoadedWorkflow`/`CatalogEntry` → the pinned
   workflow the start path resolves (start.rs:115-122). Remove the assembler
   default and the implicit-children 3600 default. Propose the concrete
   manifest representation in your first commit message (e.g. optional field
   with serde-absent = none, plus whatever marker makes legacy defaulted
   manifests read as "not declared") — the coordinator ratifies it before
   you build on it. Package identity: `with_explicit_timeout_identity`
   (assemble.rs:130-131) already distinguishes declared timeouts — keep
   identity behaviour coherent and say so in the commit.
2. **Arm the deadline.** In `start_workflow_with_options`, when (and only
   when) the resolved entry declares a timeout: arm a durable deadline timer
   with `fire_at = WorkflowStarted.recorded_at + timeout` (deterministic —
   never a second live-clock read), recorded so `outstanding_future_timers`
   (recovery.rs:150) re-arms it after failover/adoption with zero new
   recovery machinery. Identity: a reserved named timer following the
   `schedule:` precedent — propose the prefix (e.g. `deadline:`) and prove
   the AWL/SDK layer cannot mint names under it.
3. **Route the fire.** Demux the deadline out of the generic fire path
   (both the live wheel and `recover_due`) by its reserved id: firing
   appends `WorkflowTimedOut` via a new `record_workflow_timed_out`, taken
   under the per-handle recorder lock with a `current_lease_terminal`
   re-check so it loses cleanly to a concurrent terminal — mirroring
   `ensure_no_recorded_terminal`. On append: visibility upsert, process
   teardown, monitor/registry cleanup — study what cancel does after its
   terminal (terminate.rs:96-132) and match the discipline. Late results
   against a TimedOut run are refused by the existing terminal machinery —
   add a test proving it, change nothing to achieve it.
4. **Cancel at every terminal.** Completion, fail, cancel, and
   continue-as-new all cancel an armed deadline (under the same lock
   discipline). Deadline cancellation is permanent for that run (never
   re-armed by reopen teardown semantics — pick the `TimerCancelCause`
   accordingly and justify it against reopen.rs).
5. **Continue-as-new semantics: per-run.** The successor arms fresh from its
   own `WorkflowStarted` + its own resolved contract; the predecessor's
   deadline is cancelled at the CAN transition. Cover the scout's
   resurrection hazard: `outstanding_future_timers` is whole-history-scoped,
   so an uncancelled predecessor deadline WOULD be re-armed after failover —
   a regression test proves the cancel closes that hole.
6. **The descriptor string.** Pin the closed-set `timeout` field value
   (default proposal: `"workflow"`, consistent with the reserved timer id
   family); it is user-visible in `one_motion` output and replay terminals —
   state the chosen token in the commit and use it everywhere.
7. **Tests** (beyond the per-deliverable ones): law-1 arm-nothing (inspect
   the store: no timer row, no TimerStarted, nothing); law-2
   legacy-manifest can-never-arm; declared timeout fires → status TimedOut
   end-to-end through a real engine; deadline-vs-completion race under the
   lock; failover: deadline armed on node A fires correctly after adoption
   (use the existing adoption test idioms); ops-console stream projects
   TimedOut (selector.rs:76 — existing mapping, prove it live in a test,
   not by inspection).

## Laws (workspace, enforced by clippy -D warnings)

No `unwrap`/`expect`/`panic`/`todo` — tests included (`type TestResult =
Result<(), Box<dyn std::error::Error>>` + `?`). No
`#[allow]`/`#[expect]`/`#[ignore]`/`_var`. Files ≤500 code lines; `mod.rs`
re-exports only. Doc comments on every helper, identifiers backticked.
`cargo fmt --all` before every commit (never a check variant). **Cargo.toml:
change NOTHING** — no new dependencies, no pin movement. Never set
`CARGO_TARGET_DIR`; scratch under `target/awl-test-scratch/`, never `/tmp`.
Never pipe cargo output through grep/tail/head — redirect to a file, echo
the exit code.

## Gates (run in this worktree, redirect output to files, echo exit codes)

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets` → exit 0
3. `cargo test -p aion-rs` → exit 0
4. `cargo test -p aion-awl` → exit 0
5. `cargo test -p aion-awl-package -p aion-package` → exit 0
6. `cargo test -p aion-core` → exit 0

Commit on `dev/wf-timeout` in logical units with trailer
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. **Never push,
never merge** — landing is the coordinator's hands after the Norn rounds,
the Fable read, and Waffles' tear.

## Adjudication protocol

Any place where the two laws conflict with existing machinery, any manifest
representation question the first-commit proposal doesn't cleanly settle,
any replay/adoption behaviour that surprises you: STOP that item, record it
fully, surface it in your final summary for the Fable seat. Never weaken a
law to make a test green.
