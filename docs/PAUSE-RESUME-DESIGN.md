# Pause / Resume — design (scope pass, NOT for implementation)

> Status: **design scope only**, pending owner review. Captured 2026-07-02.
> Produced alongside the reopen build (`docs/WORKFLOW-REOPEN-DESIGN.md`) as a
> sibling front-end on the recovery machinery. **Do NOT implement until the owner
> reviews the scope and rules on the one load-bearing decision (§4).**

## 1. Goal & scope

**Goal:** let an operator deliberately **pause** a running workflow — stop it
advancing and hold it durably out of active dispatch/recovery — and later
**resume** it exactly where it was, with no lost work. Unlike reopen (which
revives a *terminal* run) and unlike continue-as-new (which starts a *new* run),
pause/resume acts on a **live, non-terminal** run and is **not** a terminal
state.

**Operator surface (mirrors `aion reopen`):**
- `aion pause <workflow-id> [--run-id]` — pause a running workflow.
- `aion resume <workflow-id> [--run-id]` — resume a paused workflow.

**In scope for the design:**
- A `WorkflowPaused` **non-terminal** lifecycle event → projected status
  `Paused`, excluded from active dispatch and startup/adoption recovery until
  resumed.
- A `WorkflowResumed` lifecycle event → back to `Running`, re-driven through the
  **same** recovery front-end reopen uses (`register_recovered_resident`).
- The touch-point map (mirror of the reopen §11 map).
- The one load-bearing decision (§4) with a recommendation.

**Explicitly out of scope (named, not designed here):**
- Bulk pause/resume by filter (single-id first, like reopen).
- Pausing a *terminal* run (that is reopen) or a *suspended-on-timer/signal* run
  as a distinct case (see §5, the residency interaction).
- Auto-pause on a condition / scheduled pause windows.

## 2. Why Pause is NOT the existing residency `Suspended`

There are two DIFFERENT "not-currently-running" concepts already in the codebase,
and Pause is neither of them — this is the crux of naming and the reason the op
module cannot be called `suspend`/`resume`:

1. **Residency `Suspended`** (`crates/aion/src/lifecycle/transition.rs`) — an
   **engine-internal** flag (Resident / Suspended) orthogonal to status. AT flips
   it when a workflow enters a durable wait (timer/signal); the workflow is still
   `Running` per the projection (invariant #4: "there is no `Suspended` status").
   It is NOT durable in event history — it is registry state re-derived on
   recovery. An operator cannot address it and it carries no operator intent.
2. **Signal `resume`** — the signal-router handoff vocabulary.

Pause is a **third thing**: a *durable, operator-intended, status-visible* hold.
It must be a recorded lifecycle event (so it survives restart and shows in
`aion list` / the ops console), and it must **suppress** active dispatch and
recovery — which residency `Suspended` deliberately does not do (a
timer-suspended workflow is still recovered and re-armed on restart). So Pause
introduces a genuinely new **status**, `Paused`, and the op module is
`lifecycle/pause.rs` (name chosen to avoid colliding with residency
`suspend`/`resume` and signal `resume`, exactly as reopen avoided `resume`).

## 3. Mechanism

### 3.1 `WorkflowPaused` (non-terminal) and `WorkflowResumed`

Two new engine-internal lifecycle events in `aion-core::Event` (plain fields, no
generic — invariant #1):

```
WorkflowPaused  { envelope, run_id }          // → status Paused (NON-terminal)
WorkflowResumed { envelope, run_id }          // → status Running
```

- `status_from_events` (reverse last-lifecycle-event-wins scan) gains two arms:
  `WorkflowPaused => Paused`, `WorkflowResumed => Running`. Because the scan is
  already last-wins, `WorkflowResumed` after `WorkflowPaused` returns to
  `Running` with no structural change — the SAME supersession mechanism reopen
  and continue-as-new use.
- **`Paused` is non-terminal.** `WorkflowStatus::is_terminal()` returns `false`
  for it. This is the key difference from every other new-status precedent:
  reopen reused existing terminal statuses; Pause adds a **new non-terminal
  status** to the closed set `{Running, Completed, Failed, Cancelled, TimedOut,
  ContinuedAsNew}` → `{… , Paused}`. Every exhaustive match on `WorkflowStatus`
  and `Event` across the workspace must gain the arm (the compiler enumerates
  them; this is the bulk of the mechanical cost).

### 3.2 Exclusion from active dispatch and recovery

`Paused` must be excluded from the "active" set the way a terminal is, **without**
being terminal:
- `EventStore::list_active` today filters `status_from_events == Running`. A
  paused run projects `Paused`, so it is **already excluded** from `list_active`
  by that exact equality — startup recovery, adoption recovery, and the
  continue-as-new sweep all iterate `list_active`, so a paused run is not
  re-spawned on restart. **This is the same free exclusion reopen got** (a
  reopened run projects `Running` and is therefore included; a paused run
  projects `Paused` and is therefore excluded). No new recovery branch is needed.
- The reset-aware terminal helpers (`current_lease_terminal` and its four
  consolidated call sites — see reopen §5) are unaffected: `Paused` is not a
  terminal, so `current_lease_terminal` still returns `None` for a paused run,
  which is correct (a paused run has recorded no terminal and can still terminate
  later). `ensure_no_recorded_terminal` therefore still permits a paused run to
  complete/fail/cancel after resume.

### 3.3 Pause the live run

`aion pause`:
1. Take the shutdown-gate operation guard (like cancel/reopen).
2. Resolve the live handle; reject if not `Running` (already Paused / terminal →
   typed `InvalidState`, reusing the reopen error).
3. Through the handle's single recorder (invariant #3), append `WorkflowPaused`.
4. **Quiesce or finish in-flight — the §4 decision.**
5. Remove/mark the handle so no new work dispatches to the run (residency
   flip to `Suspended` for the in-registry case, or `registry.remove` if the
   process is torn down — depends on §4).
6. Refresh visibility so `aion list` shows `Paused`.

### 3.4 Resume via the recovery front-end

`aion resume` is a small front-end on the **same** recovery machinery reopen
uses:
1. Read history; verify current status is `Paused` (else `InvalidState`).
2. Build ONE continuous `Recorder::resume_at(head)` (invariant #3), append
   `WorkflowResumed` through it — the run now projects `Running`.
3. Hand that recorder to `register_recovered_resident` (already parameterised to
   accept an externally-held recorder by the reopen build), which re-spawns the
   BEAM process at the entrypoint, resolves the pinned package version, and lazy
   per-NIF replay returns recorded results for completed steps and hits
   `ResumeLive` at the first unrecorded step — in the workflow's own namespace
   re-derived from history (the same namespace-affinity guarantee reopen makes).

Resume therefore reuses reopen's exact respawn-and-register path; the only new
code is the append of `WorkflowResumed` and the precondition check.

## 4. The one load-bearing decision: quiesce vs. finish-in-flight

**Question:** when an operator pauses a workflow that has activities in flight
(dispatched to workers, not yet terminal), does Pause **(A) quiesce** — kill /
detach the in-flight activities so nothing runs while paused — or **(B) let them
finish** and only withhold *new* dispatch?

**Option A — quiesce (kill in-flight, re-dispatch on resume).**
- Pause tears down the live process and any linked in-flight activities (like
  cancel does), records `WorkflowPaused`, and on resume the in-flight step
  re-dispatches from scratch via the `ResumeLive` path (identical to reopen of a
  cancelled run's in-flight step).
- Pro: a paused workflow consumes ZERO worker/agent resources — the strongest
  meaning of "paused", and the one an operator pausing to stop a runaway
  agent/cost expects.
- Con: an in-flight agent step loses its *uncommitted* progress unless it is
  reopen-style resumable (worktree preserved + `--resume-if-exists` session).
  For the norn-backed steps this is already true (see reopen §13), so the loss is
  the same bounded "continue from the session, not cold restart" reopen accepts.
- Con: at-least-once — a quiesced-then-resumed activity may run twice if it
  completed on the worker in the tear-down window; activities must be idempotent
  (already a standing requirement, WORKFLOW-RESILIENCE §4).

**Option B — finish in-flight, withhold new dispatch.**
- Pause records `WorkflowPaused` and flips residency to `Suspended` but leaves the
  live process and its in-flight activities running; only the NEXT workflow
  decision (the next `ResumeLive` after the current awaits resolve) is withheld
  until resume.
- Pro: no wasted/at-least-once activity re-execution; in-flight work completes.
- Con: "paused" does NOT stop resource use immediately — a paused workflow with a
  running hour-long agent step keeps that agent running, which contradicts the
  primary operator intent (pausing to STOP work / cost). It also needs a NEW
  "withhold next dispatch while paused" gate in the dispatch path (net-new engine
  machinery), whereas quiesce reuses the existing cancel-style teardown + the
  reopen respawn path with no new dispatch-time gate.

**Recommendation: Option A (quiesce).** It matches operator intent ("pause = stop
work now"), reuses machinery that already exists (cancel-style teardown for the
pause; reopen's respawn-and-register for the resume) rather than adding a new
dispatch-time withhold gate, and its only real cost — an in-flight step
re-running from its resumable session on resume — is exactly the bounded,
already-accepted cost reopen carries (§13 of the reopen design). Option B's
"keep the agent running while paused" is the opposite of what pausing is for.
The at-least-once caveat is a standing invariant, not new debt.

**A run paused while suspended on a signal/timer** (no in-flight activity, parked
in a durable wait) is the easy case under either option: there is nothing to
quiesce; Pause just records `WorkflowPaused` (the durable timer, if any, is torn
down like cancel does to avoid an orphaned fire, and re-armed on resume via the
recovery path — the same timer-recovery path reopen/adoption already use), and
resume re-arms the wait. This case is why the op must tear down in-flight timers
before recording, exactly as `Engine::cancel` does (`cancel_inflight_timers`).

## 5. Residency interaction (must be nailed before build)

`Paused` (status, durable) and `Suspended` (residency, registry-only) coexist and
must not be conflated:
- A paused run is **removed from the registry** (Option A) or **residency
  `Suspended`** (Option B). Either way it is NOT recovered on restart because it
  projects `Paused` and `list_active` filters on `== Running`.
- On resume, `register_recovered_resident` re-inserts a `Resident` handle with a
  reconciled `Running` status — identical to reopen. The reconcile must run
  against the post-`WorkflowResumed` history (the reopen build already fixed this
  ordering trap: reconcile against the history that INCLUDES the superseding
  event, or the cached status reverts to the superseded value).

## 6. Touch-point map (mirror of reopen §11)

- **`aion-core`:** `event.rs` (`WorkflowPaused` + `WorkflowResumed` variants +
  `envelope()` arms), `status.rs` (`WorkflowStatus::Paused`; `status_from_events`
  arms; `is_terminal() == false` for `Paused`; `current_lease_terminal` leaves
  `Paused`/`Resumed` as non-terminal, non-reset — they are neither a terminal nor
  a run-start reset, so they fall through like `SearchAttributesUpdated`),
  `filter.rs` (`WorkflowFilter` already matches on status; `Paused` needs no new
  field, but `WorkflowSummary` gains nothing — pause carries no new projected
  column).
- **`aion-store`:** `run_chain.rs` arms; visibility projection already keys on
  `status_from_events`, so `Paused` flows through; `list_active` unchanged
  (filters `== Running`, which excludes `Paused` for free).
- **`aion-store-libsql`:** `append/metadata.rs` (`event_kind` + queryable flag
  for the two events); the visibility status column already stores the projected
  status string, so `Paused` needs no schema change (unlike reopen's two new
  summary columns — pause adds none).
- **`aion` engine:** NEW `lifecycle/pause.rs` (the pause + resume ops; module
  named `pause` to avoid the residency `suspend`/`resume` and signal `resume`
  collisions), `durability/cursor.rs` + `resolver.rs` (exhaustive arms; `Paused`
  is a non-command lifecycle marker like `WorkflowReopened` — no cursor reset
  rule needed, since pause records no superseded activity failure),
  `lifecycle/completion.rs` + `engine/delegated.rs` + `durability/recorder.rs`
  (the reset-aware terminal helpers already treat only terminals as terminal, so
  `Paused` needs only the exhaustive-arm additions, NOT new reset logic),
  `engine/api.rs` (`Engine::pause_workflow` / `Engine::resume_workflow` taking
  the shutdown-gate guard), `engine/startup.rs` (NO change — a paused run is
  excluded by `list_active`, exactly like a terminal), `lifecycle/visibility.rs`
  (projects `Paused`).
- **`aion-server`:** `Pause`/`Resume` RPCs (proto + gRPC/HTTP handlers), the
  namespace-auth gate (`NamespaceOperation::Pause`/`Resume` variants + `verify`
  target-ownership arms, exactly like cancel/reopen — an operator pausing a
  foreign workflow must be denied), `stream/selector.rs`, summary serialization
  (no new fields).
- **`aion-cli` / `aion-client`:** `pause`/`resume` subcommands + client ops
  (mirror `reopen`).
- **Ops console:** the `WorkflowStatus` TS enum regenerates with `Paused` via the
  `generated_types.rs` exporter; a paused row renders a distinct badge + a
  `Resume` action (capability-gated like the NOI-6 intervention controls).

## 7. Invariant compliance (as designed)

1. **Type-erased events:** `WorkflowPaused`/`WorkflowResumed` carry plain fields
   (`run_id`), no generic. ✓
2. **Determinism:** pause and resume are recorded events; no wall-clock/entropy in
   workflow-visible paths. ✓
3. **Single writer:** resume holds one continuous `Recorder` from the
   `WorkflowResumed` append through respawn (reusing reopen's parameterised
   `register_recovered_resident`); pause appends through the live handle's one
   recorder. ✓
4. **Status is a projection:** `Paused`/`Running` are projected from the last
   lifecycle event; **this decision — adding a NON-terminal `Paused` status to
   the closed status set — is the one that needs the owner's explicit sign-off**,
   the same way reopen's refinement of invariant #4 did. Unlike reopen (which
   added no status), Pause widens the status enum, so every exhaustive `match`
   and the ops-console rendering must handle it. ✓ (needs sign-off)
5. **Content-hash module namespacing:** resume resolves the same pinned package
   version recovery resolves; no change. ✓

## 8. Open questions for the owner

1. **§4: quiesce (A) vs. finish-in-flight (B).** Recommendation: A. This is the
   load-bearing call; everything else follows.
2. **New non-terminal `Paused` status** vs. modelling pause as a durable-but-
   status-invisible residency (rejected in §2 because the operator needs it
   visible and it must suppress recovery). Confirm the new status is acceptable.
3. **Interaction with reopen:** should a `Failed`/`Cancelled` run be pausable? No
   — pause acts on a live `Running` run; a terminal run is reopened, not paused.
   Confirm.
4. **Auto-resume / max-pause-duration:** out of scope here; flag if wanted so the
   event shape can reserve for it (recommend NOT — no arbitrary limits, per
   CLAUDE.md).
