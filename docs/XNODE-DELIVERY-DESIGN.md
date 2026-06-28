# Cross-node work delivery — design (chosen via first-principles bake-off)

> Status: chosen 2026-06-28 via an adversarial design panel (4 approaches × 2 judges
> × 1 critic + synthesis). Supersedes the naive "copy Temporal long-poll" and the
> "per-row haematite CAS claim" ideas — both were verified WRONG against real code
> (see §4). This is the build plan for the LSUB track (aion task #13).

## 1. The corrected premise
Aion is modeled on Temporal but is NOT constrained like it. Temporal long-polls because
it has stateless workers + a stateless matching service and no live mesh. We have:
- **beamr** — live actor mesh with **monitors** (`monitor_pid` local / `monitor_remote`
  cross-node): instant, exact worker/server death detection, not poll-timeouts.
- **liminal** — pub/sub over that mesh: a worker pool is a process group (pg).
- **haematite** — **epoch-fenced shard ownership** (the landed AA-3 stack:
  `acquire_shard_and_serve`/`become_live`/`merge_adopt`, `actor.rs` fence at
  `stamp.epoch < promised => Fenced`).

CRITICAL correction (verified in code): haematite's `Database::cas`/`EventStore::cas`
(`db.rs:191`, `event_store.rs:219`) are **unfenced, single-shard, single-node scalar-u64
atomics** — the epoch fence lives ONLY on the stamped value-hash path
(`apply_durable_kind`, `actor.rs:687`). So "fenced per-row claim CAS" does not exist and
would be split-brain-unsafe across active-active servers. Mutual exclusion must come from
the real fence (shard ownership), not a per-row CAS.

## 2. The chosen design — server-arbitrated push, fence at outbox-shard ownership
- **Fence the rare event, not the hot path.** The outbox is co-located with its workflow's
  event stream — rows route by `keyspace::event_stream_key(workflow_id)` (`aion-store-haematite
  store.rs:1299`), so a server owns a workflow's outbox rows exactly when it owns that workflow's
  shard. An aion-server instance runs the outbox dispatcher for a shard ONLY if it holds that
  shard via `acquire_shard_and_serve` (epoch-fenced, quorum). The fence lives on the **stamped
  event-append-to-history** path (a deposed/zombie server is `Fenced` on its next history write,
  `store.rs:1110`) — the local outbox CLAIM write is an unfenced `put_routed` (`store.rs:1301`);
  cross-node safety comes from ownership-gating the dispatcher + the exactly-once terminal in
  history, NOT from a fenced claim. This is the real cross-node mutual exclusion, at **server**
  granularity (rare, stable) — NOT per worker, NOT per row.
- **Per-row one-of-N is free.** Single-owner-per-shard (guaranteed by the fence) means the
  server's existing LOCAL `claim_outbox_rows` (`outbox.rs:229`) is already one-of-N — no
  quorum, no CAS race, no worker contention. (Needs a `(ns,tq,node)` scope predicate added.)
- **Workers never touch haematite** (preserves the no-inbreeding rule). The owning server is
  the arbiter; it selects a worker from the beamr pg group for the channel and **pushes** the
  DispatchRequest.
- **Advisory wake (latency only).** On staging, the owner publishes a body-less generation
  wake on the liminal channel (reuse existing broadcast). Lost wake → degrade to the existing
  outbox poll. Correctness never rests on the wake or on liminal.
- **Instant failover via beamr monitors.** The owner monitors the chosen worker; on `Down`
  it re-arms the row and re-selects with no lease-timeout wait.
- **Result path unchanged.** Worker replies via liminal request/reply correlation →
  `LiminalCompletionSource::deliver` → the SAME `ServerOutboxDeliveryCallback` →
  `record_fan_out_completion` (idempotent on dispatch_key/ordinal) = exactly-once terminal.
- **Three-layer failover, fastest wins:** (1) worker death → monitor → local re-arm/re-select
  (~RTT); (2) server death → survivor wins `acquire_shard_and_serve` for the orphaned shard,
  `become_live`/`merge_adopt` restores committed dispatch baseline losslessly, deposed owner
  Fenced; (3) double-fault backstop → `rearm_stale_claimed_outbox_rows`.

## 3. Decomposition (spike-first; default build byte-identical; full clippy bar; no shims)
- **LSUB-0 (Spike 0, GATING, fork-independent):** liminal inbound server→client PUSH frame +
  worker-SDK background reader + correlated reply. (Today: connection is request→response,
  `process.rs:307-320`; remote subscribe empty, `handles.rs:185`.) Every design needs this.
- **LSUB-1:** add `(ns,tq,node)` scope to `claim_outbox_rows`; single-server cross-node
  dispatch routes by (ns,tq,node) end-to-end over LSUB-0; `record_fan_out_completion` terminal.
- **LSUB-2:** advisory wake on stage; latency→~RTT; prove correctness unchanged when wake dropped.
- **LSUB-3:** beamr monitor failover (worker kill → reassignment in monitor-RTT, not lease-TTL).
- **LSUB-4:** server-ownership fence — gate the outbox dispatcher on `acquire_shard_and_serve`;
  adversarial active-active + owner-kill test (single-owner dispatch + lossless handoff +
  deposed Fenced). [Depends on Fork A/B below.]
- **LSUB-5:** real-app cross-node failover demo — fan-out workflow, kill the owning server
  mid-dispatch, survivor adopts the shard and finishes. (The live demo.)

## 4. Rejected (and why) — all verified against real code
- **long-poll-claim (Temporal copy):** brief says don't default to it; also server-side HOLD
  is impossible (`ParticipantBehaviour::process` synchronous, `participant.rs:38`), poll capped
  at 5s `IO_TIMEOUT`, cites the non-existent fenced CAS, `claim_outbox_rows` unscoped.
- **notify-cas-claim (my reframe):** headline "epoch-fenced per-row claim CAS" doesn't exist
  (§1); and it forces a worker→haematite path (breaks no-inbreeding) or re-adds a server hop.
  Good kernel (wake + monitor + degrade-to-poll) salvaged into the chosen design.
- **credit-push:** largest unbuilt liminal surface (async push + per-subscriber credit + broker
  one-of-N, all scaffolding-only); rests on the same miscited fence; pg death detection is
  node-granular not per-pid. Credit is a worthwhile LATER optimization, not the foundation.
- **shard-owned-queues (worker = shard owner):** cardinality mismatch — elastic/scale-to-zero
  worker churn can't map onto fixed quorum-elected shards (every join/leave = an election). But
  its correctness backbone is adopted at the RIGHT granularity (server-owns-shard).

## 5. Open forks for the owner (needed before LSUB-4; LSUB-0..3 do not depend on them)
- **Fork A (biggest) — RESOLVED 2026-06-28 = A2 (outbox-resident lease + ownership-gate).** The
  per-row dispatch lease stays in the existing aion-store outbox; on owner death the survivor
  re-residents from quorum-replicated history, replay re-arms via `rearm_outbox_pending`
  (`fan_out.rs:182`) and re-dispatches. Rationale (verified in code, not asserted): the
  exactly-once *terminal* is already enforced by `record_fan_out_completion`'s
  `ordinal_is_resolved` dedup against replicated history (`fan_out.rs:240/252/296`), so
  re-dispatch yields at most a duplicate *execution* (the contract is already at-least-once +
  idempotent-terminal — worker death, `mark_done` write failure, and retry backoff already
  redeliver). A1's "lossless lease" cannot deliver exactly-once *execution* either (the
  worker-died-after-side-effect window remains) and would impose a permanent ~2× quorum write
  tax on every dispatch to shrink the rarest redelivery cause. The correct escape hatch for
  genuinely non-idempotent side-effects is an idempotency key at the side-effect boundary
  (future additive feature), NOT an orchestrator lease — so even A1's motivating case does not
  point to A1. A1's stamped-lease design is kept on file should that ever change.
- **Fork B:** availability vs correctness under partition. Gating dispatch on a haematite
  quorum election means a minority-partition server cannot dispatch even with healthy workers
  (storage-quorum-coupled liveness). Want a single-node fast path that skips the election when
  only one server is configured (common case), electing only in active-active?
- **Fork C:** outbox shard count S, and whether outbox shards co-locate with haematite data
  shards or are an independent partition (hot-pool hotspot behavior).
- **Fork D:** pool-member selection policy (round-robin / least-in-flight / node-affinity-aware)
  — must honor `Some(node)` per NODE-AFFINITY-DESIGN. (Largely decided: honor node affinity.)
- **Fork E:** ship the advisory wake (LSUB-2) in v1, or defer until poll-latency is felt (pure
  optimization, no correctness risk).
