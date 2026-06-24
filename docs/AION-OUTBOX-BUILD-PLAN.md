# Aion interim durable-outbox fan-out/fan-in (libsql) — build plan

Realizes H1 of [AION-DISTRIBUTION-DESIGN.md](./AION-DISTRIBUTION-DESIGN.md) on the existing
**libsql** backend — the early-capability milestone that delivers distributed fan-out/fan-in while
the haematite active-active backend (synchronous replication) is built underneath. Grounded in a
read-only code analysis of aion (verified spot-checks: atomic N-event `append` under an `IMMEDIATE`
tx; `ConnectedWorkerRegistry` worker model already exists).

## The big positive: the cross-process worker model already exists

The hard part is already built. `aion-worker` (receive loop + `ActivityTask` protocol),
`aion-server` `ConnectedWorkerRegistry` + `ActivityDispatcher` (gRPC dispatch to connected workers),
and `ActivityCompletionSink` all exist and work. `Recorder` is the single sequence-head (dedup
chokepoint); `store.append(token, wf, &[Event], expected_seq)` is an atomic N-event batch under a
libsql `IMMEDIATE` transaction with a `SequenceConflict` CAS guard; `PendingAwait::Collect` already
pins a contiguous ordinal range at first arrival (`nif_collect.rs::pin_or_allocate`). So this is
**not** a from-scratch distributed dispatcher — it's adding a durable outbox + store-backed dedup
around machinery that already runs.

## What's net-new (6 pieces)

1. **`outbox` table** in libsql (`aion-store-libsql/src/schema.rs`) — `dispatch_key TEXT UNIQUE`
   (= `"{workflow_id}:{ordinal}"`, the DB-level idempotency guard via `INSERT OR IGNORE`), status
   (pending/claimed/done/failed), `visible_after` for retry backoff, partial index on
   `(status, visible_after) WHERE status='pending'`.
2. **`OutboxStore` trait** (`aion-store/src/outbox.rs`) + libsql impl: `append_outbox_batch`,
   `claim_outbox_rows`, `complete_outbox_row`, `retry_outbox_row`, `fail_outbox_row`. SQLite has no
   `SELECT FOR UPDATE SKIP LOCKED`; the single-writer IMMEDIATE model gives the equivalent
   (`UPDATE ... SET status='claimed' ... RETURNING *`).
3. **`append_with_outbox`** on `LibSqlStore` — writes the events batch AND the outbox rows in the
   **same** IMMEDIATE transaction (atomicity is load-bearing). Make the outbox slice
   `Option<&[OutboxRow]>` defaulting to `None` so event-only appends are untouched.
4. **`Recorder::record_fan_out_dispatch`** — builds the N×(`ActivityScheduled`+`ActivityStarted`)
   event batch, calls `append_with_outbox`, advances `SequenceHead` by exactly `2N`. Replaces the
   `spawn_completion_task` call in `nif_collect.rs::dispatch_unscheduled` for FRESH items (stale
   recovery items keep the existing path).
5. **`OutboxDispatcher`** — a non-replayed Tokio task in `aion-server` (owns the registry): claim
   rows → dispatch via the existing `ConnectedWorkerRegistry` → on completion `complete_outbox_row`,
   on error `retry_outbox_row` (backoff) / `fail_outbox_row`. Lives entirely OUTSIDE the
   deterministic/replay domain; reads the outbox table, never workflow history.
6. **`Recorder::record_fan_out_completion`** — store-backed completion dedup: check
   `recorded_terminal` for the ordinal → if resolved return `Dropped` (no write); else append the
   terminal through the Recorder and wake the workflow PID. This is the cross-node dedup chokepoint.

## Phased build (each phase: implement → adversarial review → re-verify → land)

- **Phase 0 — schema + `OutboxStore` (LOW risk, pure additive, no behaviour change).** Idempotent
  DDL; trait + libsql impl; conformance tests (idempotent DDL, claim/complete/retry round-trip,
  duplicate `dispatch_key` ignored). *Safe first step.*
- **Phase 1 — atomic dispatch write in Recorder (MEDIUM).** `record_fan_out_dispatch` +
  `append_with_outbox`. Risk: `expected_seq` must advance by exactly `2N`. Test: seed head, dispatch,
  read back contiguous seqs + outbox rows present, one atomic op.
- **Phase 2 — `OutboxDispatcher` task (MEDIUM).** Wire into `aion-server/src/run.rs` beside the
  timer poller. Reuse the existing blocking dispatch→completion path. Risk: stale-vs-fresh dispatch
  split must never race the same ordinal (invariant: fresh→outbox owns it; stale-recovery→
  `spawn_completion_task` owns it).
- **Phase 3 — Recorder-backed completion dedup (MEDIUM-HIGH).** `record_fan_out_completion`. Risk:
  `read_history` on the completion hot path (cache via the in-memory `recorded_terminal` snapshot).
  Wire BEFORE Phase 2 is live so unmatched completions aren't dropped.
- **Phase 4 — integration test.** collect_all over N; restart mid-flight; out-of-order completions;
  duplicate completion → `Dropped`, no double-wake, no duplicate terminal in history.

## Critical unknowns (call out, test explicitly)

1. `append_with_outbox` transaction scope — don't break the event-only sequence guard (Option slice).
2. Stale-vs-fresh dispatch split at recovery — the two paths must never claim the same ordinal.
3. Completion routing without a waiting `recv` — unmatched completions must queue/route to the
   Recorder, not be silently dropped.
4. SQLite IMMEDIATE contention between the poller's claim and Recorder appends (fine single-node).
5. No retry-budget / dead-letter policy exists today — decide it before Phase 2.

## Liminal / haematite swap (later)

The outbox schema already carries `namespace`/`workflow_id`/`ordinal`/`dispatch_key` — everything
the cross-node send needs. When ready, the `OutboxDispatcher`'s local `registry.dispatch` becomes a
liminal send; `dispatch_key` maps to liminal's per-channel idempotency key. The storage backend swaps
libsql → haematite once the active-active foundation (synchronous replication + quorum fence) lands.
**No outbox schema change needed for either swap** — that's why the interim is not throwaway.
