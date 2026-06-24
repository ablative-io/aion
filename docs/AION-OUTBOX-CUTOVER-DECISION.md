# Interim durable-outbox cutover — design decision (Phase 4)

Status: **decided — build the real dedup path** (behavior-changing step; flag-gated,
default off). Decision: put `record_fan_out_completion` on the hot completion path
now (do not shelve it), realized single-writer-safe by recording **through the
workflow's own Recorder inside its actor turn**. Supersedes the completion-side
framing in [AION-OUTBOX-BUILD-PLAN.md](./AION-OUTBOX-BUILD-PLAN.md) Phase 3/4 based
on three read-only ground-truth investigations of the live code.

## Honest claim boundary

- **This build truthfully claims:** durable, crash-safe distributed fan-out/fan-in
  with remote workers, with the real `record_fan_out_completion` dedup chokepoint on
  the hot path. Built once, cross-node-*shaped*.
- **This build does NOT claim:** the workflow actor surviving node failure /
  active-active. True cross-node single-writer needs the haematite fencing token
  (H2). When haematite lands, the same `record_fan_out_completion` primitive + the
  fence make it cross-node-correct — so this is not throwaway.

## What changed our mind: the single-writer constraint

Ground truth (verified, with citations in the investigation transcripts):

- There is **one long-lived `Arc<Mutex<Recorder>>` per workflow**
  (`crates/aion/src/registry/handle.rs:159`), reachable only through the NIF call
  path (`NifContext::new` → `resolve_handle`). There is **no message-passing API**
  to post work into a workflow PID's actor turn other than the existing activity
  delivery + wake-marker path.
- The **only** serializer of writes to one workflow's history is the **actor
  model** — each PID's turns run serially, holding that mutex during all appends
  (`crates/aion/src/runtime/nif_context.rs:266-282`). 
- `write_token` is a **zero-sized capability with no identity**
  (`crates/aion-store/src/store.rs:11-29`); it does **not** fence writers. The CAS
  (`expected_seq`) is the only backstop, and a conflict is a **hard error, never a
  retry** (`crates/aion/src/durability/recorder.rs:39-43`).

**Therefore:** a server-side completion handler that constructs a *separate*
`Recorder` and calls `record_fan_out_completion` would **race the workflow's own
turns** (e.g. a scope-expiry `ActivityCancelled`). The loser gets `SequenceConflict`
→ **the completion is lost**. The documented "record at the sink via the Recorder"
path is **not single-writer-safe**.

## The decision: record_fan_out_completion, run inside the actor turn

The single-writer-safe way to land a completion is to deliver it into the
workflow's own serial turn and have the **workflow's own long-lived Recorder**
(`Arc<Mutex<Recorder>>`, `crates/aion/src/registry/handle.rs:159`) record the
terminal. The wake/post mechanism already exists
(`deliver_activity_completion_message`,
`crates/aion/src/runtime/handle/delivery.rs:153`: it populates the result and
enqueues the `activity_complete` wake marker). The change is that the **recording**
routes through `record_fan_out_completion` instead of the bare
`record_activity_completed` append:

- `record_fan_out_completion` runs through the **same** Recorder the workflow uses,
  inside the workflow's serial turn → **exactly one writer**, no separate Recorder,
  no CAS race. This is the safe realization of "the real dedup chokepoint on the hot
  path" — not the unsafe external-Recorder variant.
- Its `recorded_terminal` dedup is the cross-node-correct primitive (it is what the
  haematite swap needs). Single-node it is belt-and-suspenders with `settle_all`'s
  history short-circuit (`crates/aion/src/runtime/nif_collect.rs:298-301`), which
  already prevents `take_and_record` from being reached for an already-resolved
  ordinal. Both together mean a redelivered completion is ignored (no second
  terminal, no double-wake effect).

Why not the bare `deliver` + `record_activity_completed` shortcut: it would work
single-node but is **not** the path we keep — it has no durable dedup primitive, so
the haematite swap would have to replace it. Putting `record_fan_out_completion` on
the path now means we build the completion path **once**.

## Cutover surface (four flag-gated pieces)

All gated on `outbox.enabled` (default `false` — server behaviour unchanged when off).

1. **Dispatch (fresh items).** Flag-on → `Recorder::record_fan_out_dispatch`
   (atomic `N×(ActivityScheduled+ActivityStarted)` events + `N` pending outbox rows
   in one IMMEDIATE tx); skip `spawn_completion_task`.
2. **Completion (the gap).** The sink's **unmatched** branch
   (`PendingActivities::complete` returns `false` today and silently drops — pinned
   by `pending_complete_unknown_returns_false`) routes to a handler that resolves
   `workflow_id`→pid and posts the completion into the workflow's turn (the existing
   `deliver` + wake-marker mechanism). During that turn `take_and_record` records the
   terminal **through `record_fan_out_completion`** on the workflow's own Recorder —
   the real dedup chokepoint, single writer. (`correlation_id ==
   activity_id.to_string() == "activity:{ordinal}"`, and the sink already carries
   `(workflow_id, activity_id)`.)
3. **Recovery (crash safety).** On `first_arrival`, re-arm (`status←pending`,
   upsert keyed by `dispatch_key = "{workflow_id}:{ordinal}"`) the outbox rows for
   every *scheduled-but-no-terminal* ordinal. This closes the crash window (a
   completion lost in the map→history gap is simply re-dispatched on restart) with
   **no external history writer**. Re-arm only touches genuinely-unresolved ordinals
   (a recorded terminal means the ordinal is no longer scheduled-no-terminal).
4. **Split resolution.** Flag-on → the outbox owns **all** fan-out dispatch (fresh
   *and* recovery). `spawn_completion_task` is not used for outbox workflows, so the
   "two paths claim the same ordinal" race (build-plan unknown #2) cannot occur.

## Semantics

- **At-least-once + dedup.** Same execution assumption activities already live under
  (`spawn_completion_task` re-dispatches stale items today, so re-execution tolerance
  is not a new requirement). Duplicate *rows* are blocked by `dispatch_key UNIQUE`;
  duplicate *completions* are absorbed by the history short-circuit.
- **`done`-on-accept is fine.** Recovery re-arms by *history* state
  (scheduled-no-terminal), independent of row status, so marking the row `done` when
  the worker accepts the push does not lose work: a crash before the terminal records
  re-arms the `done` row back to `pending`.

## What this defers (deliberately)

- **Cross-node-workflow operation** (the workflow actor on a different node than the
  completion) is deferred to the haematite foundation track. `record_fan_out_completion`
  is on the path now, but single-node its `recorded_terminal` dedup runs against a
  local store; the cross-node version needs the H2 fencing token so a single writer
  is enforced *across* nodes. The completion path itself does not change at the swap —
  only its store (libsql → haematite) and the addition of the fence.

## Open edges (flag, don't block the MVP)

- **`workflow_id`→pid resolution.** Resolved: the registry is keyed
  `(WorkflowId, RunId)` but the one-live-run-per-workflow invariant holds
  (continue-as-new inserts the new run, then removes the old; distinct RunIds), so a
  secondary `WorkflowId → (RunId, pid)` index gives an unambiguous live pid. Index
  value carries `RunId` so removal is a compare-and-delete (a newer run that already
  overwrote the entry survives the old run's removal).
- **Continue-as-new window (known interim limitation).** The worker completion
  carries no `RunId`, so a completion arriving *during* the brief window where both
  the old and new run of a `WorkflowId` are live routes to the current (new) run. The
  recovery re-arm backstops this: a misrouted/ignored completion leaves the ordinal
  `scheduled-no-terminal`, so it is re-dispatched. The cross-node/haematite version
  threads run identity properly and removes this edge.
- **Workflow eviction — RESOLVED: SAFE.** A `collect`-parked workflow is NOT evicted:
  `Residency` is only `{Resident, Suspended}`; a handle is removed from the registry
  (and the live-pid index) ONLY on a terminal transition (complete/fail/cancel/
  continue-as-new); there is no idle-eviction, sweep, or hibernation; and a parked
  workflow's beamr process stays parked (not unloaded). So `live_pid` always resolves
  a parked workflow and `deliver_outbox_completion` delivers — demonstrated by the
  e2e tests (a parked workflow receives its completion and wakes). The `Ok(false)`
  not-live branch is therefore reached only for a genuinely-departed run (terminal /
  continue-as-new'd), where dropping the late completion is correct.
- **Residual liveness risk (inherent, documented).** The real loss path is
  OutboxDispatcher reliability, not the registry: a row that exhausts its retry
  budget (e.g. all workers for the activity type down past `max_attempts`)
  dead-letters to `failed` and is only re-armed on the next engine restart (the
  re-arm UPSERT flips any prior status back to `pending`). This is the standard
  at-least-once + retry-budget tradeoff, not a cutover-specific defect. A periodic
  reconciliation sweep over `failed`/`done`-without-terminal rows would close it
  without a restart — a future hardening, not a blocker.

## Test plan (Phase 4 end-to-end, model on `concurrency_e2e.rs` gate harness)

- collect_all over N with `outbox.enabled` → exactly N terminals, no duplicates.
- **Restart mid-flight** (M of N done) → replay re-arms the N−M unresolved rows,
  re-dispatch completes them, history byte-identical to a no-crash run.
- **Duplicate completion** for an already-recorded ordinal → ignored (no second
  terminal, no double-wake effect); outbox row stays `done`.
- **Out-of-order completions** → settle correctly regardless of arrival order.

## Remaining work — REQUIRED, nothing optional

The cutover landed (flag default-off) is correct and crash-safe for the tested paths,
but the following are REQUIRED before the flag is turned ON in production. None is
"optional hardening" — they are tracked work, listed roughly in priority order.

1. **Live reconciliation sweep (no-restart re-arm).** A periodic sweep that re-arms
   outbox rows that are `failed` (dead-lettered past `max_attempts`) or
   `done`-without-a-terminal-in-history back to `pending` in a LIVE server, so a
   worker outage does not strand an activity until the next engine restart. This is
   the one residual that is a real liveness gap, not an inherent tradeoff.
2. **Cancel/settle outbox rows when collect cancels an ordinal.** On fail-fast /
   scope-expiry / collect_race loser cancellation, mark the ordinal's outbox row
   terminal so the dispatcher does NOT push a dead activity to a worker (today
   cancelled siblings' rows stay live and get dispatched; the late completion is
   dropped, but the worker still executes — wasted work).
3. **Thread `RunId` on the wire (close the continue-as-new window).** Carry `RunId`
   outbox row → `ScheduledActivity` → worker echo → sink, so the completion resolves
   `(WorkflowId, RunId)`→pid exactly instead of routing to the current live run.
   Removes the continue-as-new misroute edge (today only backstopped by recovery).
4. **Full `run_server` bootstrap integration test.** The real-transport e2e
   constructs the `OutboxDispatcher` manually; nothing exercises
   `run.rs::maybe_spawn_outbox_dispatcher` wiring `state.outbox_store()` → the spawned
   dispatcher in a real boot. (A `connect_store` unit test covers the leaf-store
   wiring point; the full boot path still needs an integration test.)
5. **Liminal cross-node swap.** Replace the OutboxDispatcher's local
   `registry.dispatch` with a liminal cross-node send; `dispatch_key` → liminal
   per-channel idempotency key. No outbox schema change needed. This is what makes the
   interim actually cross-node (today: single workflow-node + remote workers).

## Next major track (the interim is the stepping stone to this)

**Active-active haematite foundation.** Draft + review the synchronous write-ack
replication design (WriteProposal/WriteAck `SyncMessage` + correlation +
durable-apply-then-ack + writer ack collector + liveness membership) BEFORE the
multi-week build (~2–4 weeks net-new per the spike); then union event-merge, the H2
monotonic fencing token (true cross-node single-writer), and the storage swap
libsql → haematite. `record_fan_out_completion` is already the cross-node-correct
dedup primitive on the path for that swap.
