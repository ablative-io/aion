# L3 — Reopen a failed workflow: design

> Status: design, pending sign-off. Captured 2026-06-16. This is the concrete
> design for L3 from `docs/WORKFLOW-RESILIENCE.md` (reopen first; L1+L2
> auto-retry follow on). Grounded in a full trace of the recovery, durability,
> status-projection, event-model, and dispatch code (see §11 touch-points).
>
> **One decision needs Tom's explicit sign-off before implementation:** the
> refinement of load-bearing invariant #4 (§5). Everything else follows from it.

## 1. Goal & scope

**Goal:** turn a workflow in terminal `Failed` status back into a running
workflow that re-executes from the step that failed, reusing the recorded
results of every step that already succeeded — so an hour of completed
`scout`/`dev`/`warm`/`scoped` work is not lost when `dev_review` dies on a
transient blip. Operator-driven: `aion reopen <workflow_id>`.

**In scope:**
- A `WorkflowReopened` event that reopens a failed run.
- The replay change that re-dispatches the reopened step (instead of replaying
  its recorded failure).
- The sanctioned single-writer path to append to a terminated workflow.
- Reuse of the existing recovery machinery to spawn + replay.
- `aion reopen <id>` end to end (CLI → client → server RPC → engine op).
- Enriched `aion list` so failed workflows show their failed step + reason.
- An explicit namespace-affinity guarantee (a remote workflow reopens on a
  remote worker) plus a test.

**Out of scope (named follow-ons):**
- L1+L2 automatic retry (classification at the boundary + engine retry driver).
  Reopen is manual; auto-retry is the next resilience deliverable.
- The namespace/task-queue split and node affinity (`docs/ROUTING-MODEL.md`).
  L3 uses the namespace dimension as it exists today.
- Bulk reopen (reopen many by filter). Single-id first.

## 2. The core problem (why naive re-run doesn't work)

When `dev_review` failed, the engine **recorded** an `ActivityFailed`
(terminal) event for it, then the workflow process crashed and a
`WorkflowFailed` event was recorded. Replay is faithful by design: if we simply
re-spawn the workflow, it replays its history, resolves the `dev_review`
activity to its recorded `ActivityFailed`, hands the same error back to the
workflow code, and crashes identically. **Re-running is not enough — the failed
step has a recorded terminal outcome that replay will keep returning.**

So reopen has to do two things: (a) make the workflow non-terminal and eligible
for recovery again, and (b) make replay treat the failed step as *needing live
re-dispatch* rather than returning its recorded failure.

## 3. Mechanism

### 3.1 `WorkflowReopened` event (reopen the run)

A new engine-internal lifecycle event:

```
WorkflowReopened {
    envelope,                     // standard recording metadata (seq, recorded_at, workflow_id)
    run_id,                       // the run being reopened (the failed run)
    reopened: Vec<CorrelationKey> // the activity correlation keys to re-dispatch
}
```

- It projects to `WorkflowStatus::Running` in `status_from_events`. Because that
  projection is already last-lifecycle-event-wins (reverse scan), a
  `WorkflowReopened` appended after `WorkflowFailed` flips the status back to
  `Running` with no change to the projection function itself. This is the same
  supersession mechanism continue-as-new already relies on.
- It re-includes the workflow in `list_active` (which filters on
  `status_from_events == Running`) and in startup recovery, so the existing
  recovery path will pick it up.
- `reopened` names the exact steps to re-run (see §3.2). It is computed by the
  reopen operation, not typed by the operator.
- **Type note:** `reopened` is `Vec<ActivityId>`, not `Vec<CorrelationKey>`.
  `Event` lives in the leaf crate `aion-core`, which must not depend on the
  engine's `CorrelationKey` (in the `aion` durability module). `ActivityId` is
  the `aion-core` representation of an activity's run-scoped ordinal, and the
  cursor matches it against `CorrelationKey::Activity(ordinal)` — both derive
  from the same scheduling position, and the cursor is run-scoped, so the match
  is exact.

### 3.2 Reopen the failed step in replay (the cursor change)

`reopened` lists the correlation keys of the activities that ended in a
**terminal** failure in this run and have **no later successful attempt**.
(Usually exactly one — `dev_review`. A concurrent fan-out could yield several;
activities that were merely *in flight* at crash time — scheduled, no terminal —
already re-dispatch on replay via the existing `collect_all` recovery and do
**not** need reopening.)

The history cursor (`crates/aion/src/durability/cursor.rs`) is taught one rule:
**a `WorkflowReopened` event that names correlation key K is a reset point for
K.** When the cursor walks the events for K, anything recorded before the reset
point is a superseded attempt; the walk continues past the recorded terminal
failure to the next `ActivityScheduled` for K, or — if there is none yet —
reports the key as exhausted so the resolver returns `ResumeLive` and the engine
dispatches a fresh attempt. The fresh attempt records a new
`ActivityScheduled`/`ActivityCompleted` (or `ActivityFailed`) under K, which
becomes K's outcome.

This is the same shape as the existing retry-chain walk
(`walks_retry_failures_to_eventual_activity_success`), except the "skip the
failure and look for the next attempt" signal is the explicit `WorkflowReopened`
reset marker rather than a `Retryable` kind. Because the marker is itself a
recorded, append-only event, **every future replay is deterministic forever**:
the cursor will always skip attempt 1 past the reset and resolve K to attempt 2.

### 3.3 Single-writer acquisition for a terminated workflow

Today there is **no** sanctioned path to append any event to a terminated
workflow — every append goes through the one `Recorder` held by the workflow's
registry handle, and a failed workflow either has no handle (API-terminated) or
is not reconstructed after restart (`list_active` excludes it). The reopen
operation creates that path, reusing existing pieces:

1. Take a per-workflow reopen lock (serialises concurrent reopen attempts and
   excludes a live resident — a terminal workflow has none).
2. Read history; verify the workflow is genuinely terminal-`Failed` for the
   target run (reject otherwise — see §8).
3. Construct `Recorder::resume_at(workflow_id, store, head)` positioned at the
   current history head. This is exactly what startup recovery constructs at
   `register_recovered_resident`; it is the legitimate single writer.
4. Append `WorkflowReopened` through that recorder.
5. Hand that **same** recorder to the resident-registration flow (§3.4), so
   there is one continuous single writer from reopen through the reopened run —
   no second writer is ever created. The single-writer invariant holds.

### 3.4 Reuse the recovery path to spawn + replay

After the `WorkflowReopened` append, the workflow projects `Running`, so it is
exactly what the startup recovery path already knows how to handle. Reopen
invokes the **same** `register_recovered_resident` flow used on engine restart:
re-derive the namespace from history (§7), resolve the pinned `.aion` package
version, spawn a fresh BEAM process at the entrypoint, register the handle as
`Resident`, and let lazy per-NIF replay return recorded results for the
completed steps and hit `ResumeLive` at the reopened step. This is the "exact
same reopen logic we already have" — reopen is a small front-end (append the
marker) on top of the recovery machinery, not a parallel implementation.

## 4. The reopen operation, end to end

`aion reopen <workflow_id> [--run-id <id>]`:

1. CLI subcommand → client `reopen()` → new server `ReopenWorkflow` RPC →
   engine reopen op.
2. Engine op: lock → validate terminal-Failed → compute `reopened` from history
   → `Recorder::resume_at` → append `WorkflowReopened` → `register_recovered_resident`
   with that recorder → return the run that is now Running.
3. The reopened process replays; reopened step(s) re-dispatch live to a worker in
   the workflow's namespace (§7); completed steps return recorded results.
4. On success the run records its normal terminal event (`WorkflowCompleted`);
   on another failure it records `WorkflowFailed` again and can be reopened
   again.

The operator types only the id. No namespace, no step, no worker — all derived.

## 5. The one decision needing sign-off: refining invariant #4

Invariant #4 (CLAUDE.md) says: *"Status is a projection… each terminal status
has exactly one corresponding terminal event."* Reopen requires refining the
second clause, and this is the only change that touches the durability core.

**Why same-run (not a new run).** Replay only returns recorded results for the
**current run** — correlation ordinals are scoped to the run segment. A
continue-as-new-style *new* run would start with an empty correlation history
and therefore **re-run scout/dev/warm/scoped from scratch**, defeating the
entire point (we want to *not* lose that hour). So reopen must reopen the
**same run**. That means a single run's history can legitimately contain
`WorkflowFailed` followed later by another terminal event
(`WorkflowCompleted`). The naive reading of "one terminal event per run" no
longer holds.

**The refinement.** A run's status is its **last** lifecycle event (which
`status_from_events` already computes), and a `WorkflowReopened` event explicitly
reopens a run after a terminal. Terminal-detection therefore means *"has a
terminal event been recorded since the last reopen point (or run start)?"* —
not *"does a terminal event exist anywhere in the run?"* The continue-as-new
mechanism already established the precedent that a later event supersedes an
earlier terminal (across runs); reopen extends the same idea within a run.

**What that requires in code (the consolidation — decision #3).** The status
projection (`status_from_events`) is already supersede-correct. Four other
terminal-detection sites are **not** — they hard-code "first/any terminal is
forever" and would treat a reopened run as permanently failed (silently blocking
its signals, completion, and close-time):
- `terminal_outcome_from_history` (`lifecycle/completion.rs`)
- `run_has_terminal_history` (`engine/delegated.rs`)
- `terminal_recorded_at` (`durability/recorder.rs`)
- the `is_terminal` gate in startup recovery / `list_active`
These get consolidated onto one supersede-correct, reset-aware check (terminal
**since the last reopen point**), so every site agrees with the projection.
This also tightens the existing `ensure_no_recorded_terminal` guard to "no
terminal since the last reopen," which is what keeps "exactly one terminal per
*lease* of the run" true and prevents double-recording within a lease.

**The ask:** approve same-run reopen with this refinement of invariant #4. If
you'd rather not stretch #4, the only alternative re-runs completed steps —
which loses the work we're trying to save. I recommend approving it.

## 6. Discovery — enriched `aion list`

`aion list --status failed` already exists but shows only id/type/time.
`aion describe <id>` already exists and dumps the full history (every step, the
`ActivityFailed` error, the `WorkflowFailed` reason) — so the drill-into-one
view needs nothing. The gap is scanning: seeing which step failed and why
across the list without describing each.

Add two projected fields to `WorkflowSummary`:
- `failed_step: Option<String>` — **the actual workflow step that failed**: the
  activity/step name, e.g. `dev_review`. This is the *step*, not the brief — the
  brief id (`stacked-dev-IP-001`) is **not** the failed step and does not belong
  in this column. (There are thousands of briefs all in the same format; the
  brief id is poor at-a-glance signal anyway. Surface the brief id / labels
  *elsewhere* — a separate context field and in `describe` — never in the
  failed-step column.)
- `failure_reason: Option<String>` — the `WorkflowFailed` error message.

**Both fields are `Option` and only populated for workflows that actually
failed.** A healthy/running/completed workflow has `None` for both, and the list
output must **not** render empty "failed step"/"reason" columns for everything —
that reads as amateurish. The columns appear only where there is a failure to
report (e.g. in `--status failed` views, or per-row only when present).

Projected by the visibility store when `WorkflowFailed` is recorded — the same
mechanism that already projects `close_time` from the terminal event. So
`aion list --status failed` reads e.g. `dev_review | norn: provider error: rate
limited`, with the brief id shown as separate context. The dashboard's
`/workflows/list` renders the same summary, so it gets the fields for free.

## 7. Namespace affinity guarantee

The reopened step re-dispatches through the normal worker registry, keyed by
`(namespace, activity_type)`. The workflow's namespace is durable (recorded as a
search attribute) and re-derived on recovery today. Reopen reuses that path, so
a `remote`-namespace workflow re-dispatches its reopened step to a `remote`
worker — never a local one. This is made an **explicit guarantee with a test**
(we hit the inverse bug — recovered remote workflows routing local — in the
2026-06-15 namespace-recovery fix, so it is pinned down deliberately). If no
worker is currently registered in the namespace, the reopened step parks and
waits for one (correct: a remote reopen waits for the remote worker), exactly as
normal dispatch does.

## 8. Edge cases

- **Reopen a non-failed workflow** (Running/Completed/Cancelled/TimedOut):
  reject with a typed error. Only terminal-`Failed` is reopenable in this cut.
  (Whether to allow reopening Cancelled/TimedOut later is a follow-on.)
- **Reopen races a live resident:** the reopen lock + the "terminal has no
  resident" fact prevent a second writer; if somehow resident, reject.
- **Double reopen:** two `aion reopen` calls for the same workflow — the lock
  serialises; the second sees the workflow already Running (reopened) and is a
  no-op/reject, not a second `WorkflowReopened`.
- **Reopened run fails again:** records a fresh `WorkflowFailed`; reopenable
  again. History accumulates `Failed → Reopened → Failed → …`; the projection is
  always the last event.
- **Server restart after reopen:** the `WorkflowReopened` is durable, so startup
  recovery treats the workflow as Running and reopens the step exactly as the
  in-process reopen would — no special-casing.
- **Concurrent fan-out failure:** `reopened` carries every terminal-failed key
  with no later success; in-flight (non-terminal) siblings re-dispatch via
  existing recovery. Verified against the `collect_all` recovery path.

## 9. Invariant compliance

1. **Type-erased events:** `WorkflowReopened` carries plain fields
   (`run_id`, `Vec<CorrelationKey>`), no generic. ✓
2. **Determinism:** the reopen is a recorded event; all future replays are
   deterministic. No wall-clock/entropy introduced. ✓
3. **Single writer:** one continuous `Recorder` from reopen through the reopened
   run (§3.3); no direct `EventStore::append`. ✓
4. **Status is a projection:** upheld and refined per §5; every terminal-
   detection site consolidated to agree with the projection. ✓ (needs sign-off)
5. **Content-hash module namespacing:** reopen resolves the same pinned package
   version recovery already resolves; no change. ✓

## 10. Decisions (recap)

1. **Same-run reopen** (not new run) — required for recorded-result reuse (§5).
2. **Explicitly named reopen point** — `reopened` keys written into the event,
   auto-detected from history (§3.2).
3. **Consolidate terminal detection** onto one supersede/reset-aware check —
   the durability-core change, sign-off in §5.
4. **Operator-driven `aion reopen <id>`** — id only; covers all failure buckets
   with no dependency on classification.

## 11. Touch-point map (for the brief)

Adding the `WorkflowReopened` variant is serde-transparent at the store/wire
layers (no proto/libSQL codec/Gleam/NIF arm), but forces exhaustive-match arms:
- `aion-core`: `event.rs` (define + `envelope()` arm), `status.rs`
  (`status_from_events` → Running), `filter.rs`, `WorkflowSummary` +
  `failed_step`/`failure_reason`.
- `aion-store`: `run_chain.rs`; visibility projection + `list_active` reset
  awareness; optional conformance case.
- `aion-store-libsql`: `append/metadata.rs` (`event_kind`, queryable flag),
  visibility row for the two new summary fields.
- `aion` engine: `durability/cursor.rs` (reset rule + family label),
  `resolver.rs` (arms), `lifecycle/completion.rs`, `lifecycle/visibility.rs`,
  `engine/delegated.rs`, `engine/api.rs`, `engine/startup.rs`,
  `durability/recorder.rs` (emit builder + reset-aware terminal helpers),
  `continue_as_new.rs`; the new reopen op (likely `lifecycle/` — name to avoid
  colliding with residency `resume`/signal `resume`, e.g. `reopen.rs`).
- `aion-server`: `ReopenWorkflow` RPC (proto + grpc/http handlers),
  `stream/selector.rs`, summary serialization.
- `aion-cli` / `aion-client`: `reopen` subcommand + client op; enrich `list`
  output with the two fields.
- Regenerate the dashboard generated types (`generated_types.rs` test).

## 12. Resolved / open

Resolved (signed off 2026-06-16):
- **Same-run reopen + the §5 refinement of invariant #4** — approved.
- **Reopen op module name = `reopen`** (avoids colliding with residency
  transition `resume` and signal `resume`).
- **`failed_step` = the actual failed step** (e.g. `dev_review`), never the
  brief id; failure fields populated/shown only for failed workflows (§6).

Open:
- Confirm reopenable statuses = `{Failed}` only for the first cut (allowing
  Cancelled/TimedOut later is a follow-on).

## 13. Reopen is a true continuation for agent steps (observability note)

Re-dispatching the failed step is, at the **engine** level, a fresh activity
invocation — but for the agent steps it is a **continuation, not a cold
restart**, by construction of the handlers:
- the worktree is preserved on failure (teardown only frees the build cache),
  so uncommitted changes remain on disk under `.yggdrasil-worktrees/<branch>`;
- the dev/dev_review handlers invoke norn with `--session-id <branch>
  --resume-if-exists`, and the branch is deterministic from the brief, so the
  re-dispatched step reconnects to the **same** norn session with its context;
- replay feeds the activity its **same recorded input**, so it lands in the same
  branch/worktree/session deterministically.

So the agent picks up where it left off. The separation: the engine returns the
workflow to the failed step; whether that step *continues* or *restarts* is a
property of the **activity handler's** resumability, which we control per
activity. The expensive agent steps are already reopenable. (Caveat for the
worker variants now in flight: a Claude-backed step must provide the equivalent
deterministic, reopenable session for reopen to stay a continuation rather than a
cold restart — see `docs/ROUTING-MODEL.md` worker flavours.)
