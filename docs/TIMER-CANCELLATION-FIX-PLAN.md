# Implementation Plan: Cancel a workflow's in-flight timers at cancellation time

Status: proposal (read-only investigation). Author: investigation agent. Date: 2026-06-22.

## Problem recap

On server restart, `aion-server` failed to boot:

```
timer recovery failed: timer recovery fire operation failed:
timer engine operation failed: workflow <id> is unknown
```

Root cause: workflow **cancellation does not clean up the workflow's in-flight
durable timers**. A cancelled workflow's `TimerStarted` stays "live" in history
forever (no matching `TimerFired`/`TimerCancelled`), the durable timer row is
never deleted, and on recovery the engine tries to fire it against a workflow
that no longer exists → `EngineSeamError::UnknownWorkflow`.

Already committed (533374aa) is the **recovery-robustness DEFENSE**:
`recover_due` now skips orphaned timers whose workflow is `UnknownWorkflow`
instead of aborting startup (`crates/aion/src/time/recovery.rs:96-130`). That
neutralizes existing orphans. This plan covers the **root-cause fix** so new
orphans are never produced.

## Evidence map (what the code actually does today)

### The cancel path

- `Engine::cancel` — `crates/aion/src/engine/api.rs:278-299`. Opens a shutdown
  gate operation, then delegates to `terminate::cancel` with a
  `TerminateWorkflowContext { runtime, store, visibility_store, registry }`.
  Note the context has **no timer service** and no `nif_state`.
- `terminate::cancel` — `crates/aion/src/lifecycle/terminate.rs:96-132`. The
  single durable cancel transition. Sequence:
  1. `registered_handle` (must be live in registry) — line 102.
  2. **Under the recorder lock** (`handle.recorder().lock().await`, line 109):
     `ensure_no_recorded_terminal` (110) then
     `record_workflow_cancelled` (111-113).
  3. `runtime.cancel_pid(handle.pid())` to kill the process (115).
  4. `upsert_workflow_visibility` (125), notify completion (127-129),
     `registry.remove` (130).
  - The comment at lines 104-106 explains *why* `WorkflowCancelled` is recorded
    **before** the kill: the exit monitor records `WorkflowFailed` for any kill
    it sees without a terminal already in history. This ordering constraint must
    be preserved.
  - **There is no timer handling anywhere in this function.** That is the bug.
- This is the only durable cancel path. `Engine::cancel` (api.rs) and the
  schedule evaluator's cancellation both funnel through it. (Supervision's
  `cancel_workflow_by_link_propagation`, `crates/aion/src/supervision/policy.rs:54`,
  only tears down linked *activity* children at the runtime level and records no
  durable workflow event — out of scope.)

### How timers live and die

- `TimerService::cancel` — `crates/aion/src/time/timer_service.rs:118-151`.
  This is a **complete, production-tested timer-cancel implementation** and it
  already does exactly what we need, race-guarded:
  - `wait_for_terminal_update_slot` (124, impl 178-191): a `DashSet` guard that
    serializes `cancel` vs `fire_timer` for the same `(workflow, timer)` so a
    concurrent wheel fire and a cancel can't both record a terminal.
  - `timer_is_live` check (136) → already-fired / already-cancelled are
    idempotent no-ops.
  - For a **resident** workflow it disarms the live wheel via
    `engine.disarm_timer(process, &timer_id)` (140-142) — i.e. it performs
    mechanism (b) itself.
  - Records `TimerCancelled` via `engine.record_workflow_event` (144-148) —
    mechanism (a).
- `timer_is_live` — `timer_service.rs:228-255`. Scans the **active run segment**
  (from the last `WorkflowStarted`) for a `TimerStarted` with no matching
  `TimerFired`/`TimerCancelled`. A `TimerCancelled` flips this to `false`.
- `fire_timer_guarded` — `timer_service.rs:193-217`. Bails out immediately when
  `timer_is_live` is false (199-201). **This is why mechanism (a) alone is
  sufficient for recovery correctness**: once `TimerCancelled` is in history, any
  later fire (wheel or recovery) is a no-op before it ever reaches the engine
  seam / `UnknownWorkflow`.
- Recovery filter `expired_timers` is purely history-driven via `fire_timer`'s
  liveness check; durable rows are never physically deleted (store has no delete
  — see below).
- `disarm_timer` wiring:
  - **Production path** is wired: `TimerNifBridge::disarm_timer` aborts the
    pending tokio task — `crates/aion/src/runtime/nif_timer_bridge.rs:266-275`.
  - The `EngineHandle for Engine` impl in
    `crates/aion/src/engine/seam_handle.rs:94-103` returns the "not wired here"
    error, but **that handle is not the timer path**. The production
    `TimerService` is built on the `TimerNifBridge` handle (see below), so its
    `disarm_timer` and `record_workflow_event` are fully wired. The `seam_handle`
    stub is a red herring for this fix.

### The recovery seam error origin

- `TimerNifBridge::record_workflow_event` —
  `crates/aion/src/runtime/nif_timer_bridge.rs:277-318`. Finds the handle in the
  registry (292-302); if absent → `EngineSeamError::UnknownWorkflow` (300-302).
  It then acquires `handle.recorder().lock()` (304-305) to append. This is the
  exact error that bricked startup, and it is also why the cleanup must run
  **while the workflow is still in the registry** (i.e. before
  `terminate::cancel`'s `registry.remove`).

### Production timer-service wiring (how to reach it from cancel)

- `installed_timer_service(nif_state)` returns the production
  `Arc<TimerService>` — `crates/aion/src/runtime/nif_timer_bridge.rs:345-349`.
  Used already at startup recovery, `crates/aion/src/engine/startup.rs:34-44`.
- `RuntimeHandle::nif_state()` is `pub(crate)` —
  `crates/aion/src/runtime/handle.rs:138`.
- `Engine` holds `runtime: Arc<RuntimeHandle>` — `crates/aion/src/engine/api.rs:41`.

So from `Engine::cancel` we can obtain the production `TimerService` via
`installed_timer_service(self.runtime.nif_state())` — the same service the live
author-cancel path and recovery use. No new plumbing into `terminate.rs`
required.

### The store has no delete-timer

- `ReadableEventStore` exposes `schedule_timer` (store.rs:111-116) and
  `expired_timers` (store.rs:119) and nothing else timer-related;
  `WritableEventStore` only has `append`. There is no `delete_timer`. Mechanism
  (c) would require a new trait method implemented across `InMemoryStore` and
  `aion-store-libsql`.

## Decision: mechanism

**Recommended: route cancellation cleanup through the existing
`TimerService::cancel` for each outstanding live timer.** This is mechanism (a)
**and** (b) at once, already implemented, already race-guarded, and requires no
new store API.

Rationale:

- **(a) record `TimerCancelled`** is the architecturally-consistent choice: the
  whole timer-liveness model is history-derived (`timer_is_live`,
  `outstanding_future_timers`), so a recorded `TimerCancelled` makes the timer
  dead everywhere — recovery (`fire_timer` no-ops at `timer_service.rs:199`),
  re-arm (`outstanding_future_timers` removes it, `recovery.rs:162`), and replay
  — with zero new surface. The committed `recover_due` defense and the
  `cancelled_timer_is_never_fired_by_recovery` test (recovery.rs:380) already
  prove the history-based model handles a cancelled timer correctly.
- **(b) disarm the wheel** is *also* needed for a **resident** workflow, but we
  get it for free: `TimerService::cancel` disarms the resident wheel entry
  (`timer_service.rs:140-142`) before recording. Calling the service rather than
  appending `TimerCancelled` by hand means we don't leave a live wheel task that
  would wake a dying/dead process. Strictly, (a) alone is *correct* (the wheel
  fire would no-op on the liveness check), but disarm avoids a wasted wake and
  matches the author-cancel path's behavior. Using the service gives both with
  no extra code.
- **Race-safety**: `TimerService::cancel` shares the `DashSet` terminal-update
  guard with `fire_timer` (`timer_service.rs:124,168,178-191`). A hand-rolled
  direct `recorder.record_timer_cancelled` in `terminate.rs` would bypass that
  guard and could, under a concurrent wheel fire, produce a double terminal
  (`TimerCancelled` + `TimerFired`) for one timer. Reusing the service avoids
  that entirely.
- **(c) add a store delete-timer**: rejected. It adds cross-backend API surface
  and is redundant — the history model already filters dead timers, and physical
  deletion is an unnecessary durability/consistency liability. (See the
  self-heal discussion below for why we also don't add it on the recovery side.)

### Where to hook in

**Recommended: in `Engine::cancel` (`crates/aion/src/engine/api.rs:278-299`),
before calling `terminate::cancel`.** Concretely, the new step:

1. Resolve the production timer service:
   `installed_timer_service(self.runtime.nif_state())`. If the bridge is not
   configured, log a warning and proceed with the existing cancel (do not fail
   cancel because timer cleanup is unavailable; the `recover_due` defense remains
   the backstop).
2. Read the workflow's history (`self.store.read_history(id)`), enumerate
   outstanding live timers in the active run segment, and call
   `timer_service.cancel(workflow_id, timer_id)` for each (each call is an
   idempotent no-op for already-terminal timers).
3. Then call `terminate::cancel(...)` exactly as today.

Why `Engine::cancel` rather than inside `terminate::cancel`:

- `Engine` already owns `runtime` and can reach `installed_timer_service`;
  `terminate.rs` is a dependency-light lifecycle module (store/registry/runtime/
  visibility only) and threading the NIF timer service into it widens its surface.
- **Ordering / deadlock safety**: `TimerService::cancel` records via the NIF
  bridge, which acquires `handle.recorder().lock()`
  (`nif_timer_bridge.rs:304-305`). `terminate::cancel` *also* holds that same
  recorder lock while recording `WorkflowCancelled`
  (`terminate.rs:109-114`). The tokio `Mutex` is **not reentrant**, so timer
  cleanup MUST happen outside that lock — doing it before `terminate::cancel` (a
  separate call that acquires and releases the lock per timer) guarantees this.
- At this point the workflow is still **resident and in the registry**, so the
  NIF bridge's registry lookup (`nif_timer_bridge.rs:292-302`) succeeds (no
  `UnknownWorkflow`) and resident disarm works.

Ordering result in history: `... TimerStarted ... TimerCancelled (per timer) ...
WorkflowCancelled`. `WorkflowCancelled` stays last (terminal-detection and
replay see a clean tail), and `ensure_no_recorded_terminal`
(`terminate.rs:110`) still passes because no terminal exists yet when
`WorkflowCancelled` is recorded.

### Enumerating outstanding timers

Reuse the existing pattern. Today `outstanding_future_timers`
(`recovery.rs:150-172`) computes live timers but then filters `fire_at > now`
(line 170) — wrong for cancel, which must cancel **all** live timers including
already-due-but-unfired ones. Recommendation: extract the core into a shared
helper and apply the `fire_at > now` filter only in the re-arm caller:

```
fn outstanding_timers(history: &[Event]) -> Vec<(TimerId, DateTime<Utc>)>
```

Scope it to the **active run segment** (last `WorkflowStarted`) to match
`timer_is_live`'s segmentation (`timer_service.rs:234-238`); a prior-run timer
would be a no-op through the service anyway, but segment-scoping keeps it tidy
and cheap. Place the helper in `time/` (e.g. `time/recovery.rs` or a small
`time/timers.rs`), have `outstanding_future_timers` call it then filter, and
have `Engine::cancel` call it unfiltered.

Note we don't strictly need `fire_at` for cancel (only the `TimerId`), but
returning the same shape keeps one helper.

## Edge cases

- **No timers**: enumeration yields empty; behavior is byte-for-byte the same as
  today (`start_then_cancel_records_started_then_cancelled`, api.rs:613, still
  holds: `[WorkflowStarted, WorkflowCancelled]`).
- **Already-terminal / already-cancelled workflow**: `terminate::cancel`'s
  `ensure_no_recorded_terminal` already rejects a second cancel (terminate.rs:110,
  139-151). If we run timer cleanup *before* that check, a redundant
  `TimerService::cancel` on an already-cancelled timer is an idempotent no-op
  (`timer_is_live` false). To avoid recording stray `TimerCancelled` for a
  workflow whose cancel will then be rejected, **either** (preferred) re-order so
  enumeration only cancels timers that are still live (they will be, for a live
  workflow), **or** gate the timer cleanup behind a cheap terminal check first.
  Simplest: since timers are only ever live for a non-terminal run, and a
  terminal run's timers are already terminal, the per-timer `timer_is_live` guard
  makes cleanup-before-check safe regardless. Recommend keeping cleanup before
  `terminate::cancel` and relying on idempotency.
- **Cancelling twice (idempotency)**: first cancel records `TimerCancelled` +
  `WorkflowCancelled`; second cancel's timer cleanup finds all timers terminal
  (no-ops) and `terminate::cancel` returns the terminal-already-recorded error.
  Net: no duplicate timer events. Mirrors existing
  `cancel_timer_after_cancel_is_idempotent_noop` (named.rs:281).
- **Resident vs non-resident**: `TimerService::cancel` resolves residency
  internally (`timer_service.rs:140`); resident → disarm + record, non-resident →
  record only. Both record `TimerCancelled`, which is the part recovery needs.
- **Replay determinism**: `TimerCancelled`/`WorkflowCancelled` are
  engine-recorded *command* events, not products of workflow code replay. The
  cancelled run's process is killed and never re-driven, so in-workflow replay
  determinism is not affected. Recorded timestamps should be a single
  `Utc::now()` captured once for the cancel operation (consistent with the
  existing single-`Utc::now()` call at terminate.rs:112).
- **Concurrent wheel fire during cancel**: handled by the `TimerService` DashSet
  guard (`timer_service.rs:124/168`) — fire and cancel cannot both record a
  terminal for the same timer.
- **New timer started in the tiny window after enumeration but before
  `cancel_pid`**: theoretically possible (the process is still alive until the
  kill). This residual race is the backstop case for the committed `recover_due`
  skip defense — acceptable, and should be called out in code comments. A
  workflow turn is engine-driven and short; the window is negligible in practice.
- **Interaction with tolerant `recover_due`**: complementary. After this fix the
  common path produces no orphans, so `recover_due` should normally never hit the
  `UnknownWorkflow` skip; the skip remains as defense for (i) pre-existing
  orphans in live DBs and (ii) the new-timer race above.

## Should the recovery DEFENSE also self-heal (delete the orphan row)?

**Recommendation: keep skip-only; do not add a delete.** Rationale:

- The store has no `delete_timer` (store.rs); adding one means new trait surface
  implemented in both `InMemoryStore` and `aion-store-libsql`, plus a
  `WriteToken`-style authority question for a non-append mutation. That is real
  scope for marginal value.
- The orphan is harmless once `recover_due` skips it: `expired_timers` returns it
  each sweep and `fire_timer` no-ops (UnknownWorkflow → skip). The only cost is a
  small repeated log + a no-op fire per sweep for as many orphans as exist.
- With the root-cause fix in place, orphan creation stops, so the live-DB orphan
  set is bounded and shrinks to "whatever exists today". A one-off migration/
  maintenance task could prune them later if log noise matters, but it is not on
  the critical path.
- If self-heal is later desired, the cleanest form is a history-driven cleanup
  (a timer whose workflow history shows a terminal `WorkflowCancelled`/
  `WorkflowFailed`/`WorkflowCompleted` covering that timer's run segment is
  prunable) rather than reacting to `UnknownWorkflow`, since `UnknownWorkflow`
  conflates "purged" with "transiently not yet recovered".

## Test plan

### Unit — `Engine::cancel` / lifecycle (root-cause)

Add to `crates/aion/src/engine/api.rs` tests (the existing
`start_then_cancel_records_started_then_cancelled` fixture, api.rs:613, is the
template) or `lifecycle/terminate.rs` tests:

1. **cancel_cancels_a_live_timer**: start workflow; record a `TimerStarted` and
   `schedule_timer` for a named review-deadline timer; `Engine::cancel`. Assert
   history is `[WorkflowStarted, TimerCancelled(deadline), WorkflowCancelled]`
   (TimerCancelled before the terminal), and that `timer_is_live` is now false /
   a subsequent `fire_timer` no-ops.
2. **cancel_cancels_multiple_live_timers**: two outstanding timers → both get a
   `TimerCancelled`, `WorkflowCancelled` last.
3. **cancel_with_no_timers_unchanged**: assert exactly
   `[WorkflowStarted, WorkflowCancelled]` (regression guard for the empty case).
4. **cancel_is_idempotent_for_timers**: cancel twice; second returns the
   terminal-already-recorded error and adds no duplicate `TimerCancelled`.
5. **resident_cancel_disarms_wheel**: with a resident workflow + a fake/real
   engine handle, assert `disarm_timer` was invoked for each live timer (mirrors
   `cancel_timer_disarms_resident_wheel_and_records_cancelled`, named.rs:214).

### Integration — the production regression (recovery)

6. **cancelled_workflow_leaves_no_orphan_for_recovery** (the headline test):
   start a workflow with a durable timer, `schedule_timer`, then `Engine::cancel`;
   then run `TimerRecovery::recover_on_startup`. Assert: returns `Ok`,
   `recovered == 0`, **no** `TimerFired` recorded, **no** `UnknownWorkflow`
   surfaced. This proves the orphan is gone *at the source*, complementing the
   existing defense test
   `orphaned_timer_for_unknown_workflow_is_skipped_not_fatal` (recovery.rs:413).

### Existing tests that must stay green

- `start_then_cancel_records_started_then_cancelled` (api.rs:613) — empty-timer
  case.
- `cancelled_timer_is_never_fired_by_recovery` (recovery.rs:380).
- `cancel_timer_*` suite in `time/named.rs:214-332`.

## Scope, risk, sequencing

**Scope: small and localized.**

- `crates/aion/src/engine/api.rs` — `Engine::cancel`: fetch
  `installed_timer_service`, enumerate live timers, cancel each before
  `terminate::cancel`. (~20-30 lines + helper call.)
- `crates/aion/src/time/recovery.rs` (or a new `time/timers.rs`) — extract
  `outstanding_timers(history)` and have `outstanding_future_timers` reuse it.
- Tests as above.
- No changes to the store trait, no changes to `seam_handle.rs`, no new events
  (reuses existing `Event::TimerCancelled`).

**Risk: low, with two specific things to get right:**

1. **Deadlock**: timer cleanup must run *outside* the `terminate.rs` recorder
   lock. Mitigated by hooking in `Engine::cancel` before `terminate::cancel`.
2. **Ordering vs `cancel_pid` and the exit monitor**: keep
   `WorkflowCancelled` recorded before the kill (terminate.rs unchanged), and do
   timer cleanup before that. The residual "new timer started in the kill window"
   race is covered by the committed `recover_due` defense — document it.
3. Minor: `installed_timer_service` can fail if the bridge isn't configured
   (e.g. some test engines). Treat as a logged warning, not a cancel failure.

**Recommended sequencing:**

1. Extract `outstanding_timers(history)` helper; refactor
   `outstanding_future_timers` to reuse it. (No behavior change; land + test
   first.)
2. Wire `Engine::cancel` to cancel live timers via the production
   `TimerService` before `terminate::cancel`. Add unit tests 1-5.
3. Add the integration regression test 6.
4. Leave `recover_due` skip defense as-is (no self-heal delete). Add a code
   comment cross-referencing the root-cause fix and the residual-race rationale.

## Open questions / unknowns (honest)

- I confirmed the production `TimerService` is the `TimerNifBridge`-backed one
  and is reachable from `Engine` via `runtime.nif_state()` +
  `installed_timer_service`. I did **not** trace whether every engine
  construction path (esp. lightweight test engines) installs the timer bridge;
  hence the "log-and-proceed if unavailable" guard. Worth a quick check during
  implementation that `Engine::cancel`'s normal server build always has it
  installed (builder.rs:65 installs it for the real runtime).
- The exact home for the shared `outstanding_timers` helper (recovery.rs vs new
  module) is a style call; either is fine.
- Schedule-overlap cancellation (`CancelPrevious`) is **resolved**: the
  production evaluator is wired with `NoopScheduleCanceller`
  (`crates/aion/src/engine/api_schedule.rs:362-363`), so it performs no durable
  cancel today. `Engine::cancel` (api.rs:278) is therefore the only durable cancel
  path that can create timer orphans, and fixing it there covers the bug. If a
  real schedule canceller is later wired, it should route through `Engine::cancel`
  (or a shared timer-cleanup helper) so it inherits this fix.
```

## IMPLEMENTED (2026-06-22) + adversarial-review follow-up

Implemented hands-on per this plan: `Engine::cancel` now calls
`cancel_inflight_timers(id)` before `terminate::cancel`, routing each live timer
through `TimerService::cancel`. Enumeration via a new `live_timers_in_active_segment`
helper (forward-scan-with-remove, active-run-segment scoped). Best-effort (warn +
proceed); `recover_due` skip kept as the permanent backstop. Tests: 5 pure-function
+ 3 integration against the real timer bridge (`cancel_records_timer_cancelled_before_workflow_cancelled`,
`cancel_cancels_multiple_live_timers`, `cancelled_workflow_leaves_no_orphan_for_recovery`).
Gated green: fmt, full lib suite, clippy. An adversarial code-review confirmed
deadlock-safety, residency/registry ordering, `WorkflowCancelled`-last ordering,
and double-cancel idempotency are all sound.

### FOLLOW-UP (separate, scoped) — pre-existing `timer_is_live` `any`-semantics bug

> **✅ RESOLVED (2026-06-22, commit 761be22e).** `timer_is_live` now delegates to the shared
> `live_timers_in_active_segment` (last-event-wins, active-segment-scoped) — re-armed named timers
> are correctly judged live. The helper was UNIFIED: moved into `time/timer_service.rs` (`pub(crate)`),
> the duplicate in `engine/api.rs` deleted, so the firing/cancel guard and `Engine::cancel`'s enumerator
> are now one function and can't diverge. Independently adversarially reviewed CLEAN (replay-safe: pure
> deterministic fold, no live-vs-recovery divergence, idempotent, segment scoping byte-identical). 376 lib
> tests + 7 new. Remaining minor (tracked, not blocking): `recovery.rs:150 outstanding_future_timers`
> still uses full-history semantics — harmless pre-existing no-op-reschedule asymmetry, backstopped.

The review surfaced a **pre-existing latent bug in `TimerService::timer_is_live`**
(`time/timer_service.rs:228-255`): it computes liveness as
`any(TimerStarted) && !any(TimerFired|TimerCancelled)` over the run segment. For a
**named** timer restarted after a terminal in the same segment
(`TimerStarted(T), TimerFired(T), TimerStarted(T)`) it wrongly returns `false`.
Impact is broader than cancellation: `fire_timer_guarded` uses the same check, so a
re-armed named timer could **silently never fire**. Our new
`live_timers_in_active_segment` does NOT inherit this (it uses last-event-wins, the
correct semantics) — but since cancellation routes through `TimerService::cancel`,
which re-checks `timer_is_live`, the cancel for such a re-armed timer no-ops, leaving
a durable row. That row is harmless (recovery's `fire_timer` no-ops it via the same
check — no `UnknownWorkflow`, no crash), so it is NOT a regression and NOT a blocker
for this fix. The proper fix is to change `timer_is_live` to last-event-wins
semantics, but it touches the timer FIRING hot path and needs its own analysis of
replay/determinism (does anything rely on the `any` semantics?) + review. Deliberately
deferred. Tracked here.
