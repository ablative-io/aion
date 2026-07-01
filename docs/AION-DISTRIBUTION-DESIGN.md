# Aion distributed orchestration — design doc (fan-out / fan-in / affinity / storage)

Status: **PARTIALLY DELIVERED — foundation landed, active-active build ongoing**
(reconciled 2026-07-02). Downgraded from "DRAFT, in progress": **H1** (fan-out via a
durable outbox) is BUILT and crash-safe — see AION-OUTBOX-CUTOVER-DECISION.md (all five
cutover blockers landed) and the libsql outbox. **H2** (monotonic fencing epoch) is
spike-VALIDATED (`crates/haematite/tests/spike_fencing.rs`, 5/5 green) but the
quorum/consensus-backed build is the remaining cluster wave (#146 durable membership,
#147 auto-discovery). Sections still gated on that work stay marked ⏳. Multi-shard
active-active itself landed the AA-4-x series (see MULTI-SHARD-ACTIVE-ACTIVE-DESIGN.md).
     Original status: "DRAFT, in progress. ... empirical haematite fencing spike (in progress)."

Grounded in the comparative review
([AION-DISTRIBUTION-REVIEW.md](./AION-DISTRIBUTION-REVIEW.md)) and an empirical haematite
fencing spike. Spike-dependent sections are marked ⏳.

## Locked decisions (Tom, 2026-06-24)

1. **Single-binary / no-external-dependency thesis is NON-NEGOTIABLE.** This is the product
   ("durable agents as infrastructure — one never-dying binary"). We therefore own the
   in-binary fencing + quorum work rather than offloading to Postgres/Cassandra. Restate is the
   closest zero-external-dependency precedent and the honest "do it properly" reference target.
2. **Active-active multi-node from the start.** We are NOT shipping the cheaper
   single-owner-per-shard + read-only-followers model as v1. We build the full path: monotonic
   fencing token + AcquireShard handoff + quorum-acked run history. (This takes on a real
   distributed-database problem deliberately, eyes open.)
3. **Best way possible, fully documented, validated empirically.** Every load-bearing mechanism
   is proven with a hands-on spike against the real engine before the design hardens around it —
   "the only way we really get the info is by testing it ourselves."

## What is already world-class (do not rebuild)

The review verified these in code; the design builds ON them, unchanged:
- Aion's deterministic replay core: **positional-ordinal correlation** (`Activity(n)`,
  `Child(n)`, `Signal{name,index}`), never arrival-order — the Temporal/Restate contract.
- **Single-writer Recorder** is the only sequence-head authority; even async arrivals append
  through it. This is the dedup chokepoint we exploit below.
- Clock + RNG already recorded (DeterminismContext / determinism NIF).
- `PendingAwait::Collect` already **pins a contiguous ordinal range at first arrival** — a
  correct, arrival-order-independent fan-out skeleton already exists locally.
- haematite `event_store` is the **right** fan-in primitive: a flat append-only seq log
  (`stream_key||0x00||seq`, O(delta) range reads, optimistic concurrency on `expected_seq`).
  The prolly-tree is the storage engine beneath, not per-completion Merkle insertion.
- aion's pluggable store trait already matches best-in-class (`append(expected_seq)` +
  `read_history_from`; Readable/Writable/Package/Visibility split). "haematite as a backend"
  is a small adapter, not a rewrite.

## The two load-bearing mechanisms

### H1 — Fan-out via a durable outbox (NOT pg broadcast)

**Problem:** beamr pg send is at-most-once and frame-dropping (sized for low-frequency control
traffic; `DIST_SEND_QUEUE_CAP=1024`, non-blocking try_send drops on full, 5s write-timeout
tears the connection). A dropped work-item or completion = silently lost work. The deterministic
workflow thread must never send/receive across the cluster directly.

**Design:**
1. A workflow `Command` emits a "dispatch N" intent → **one atomic durable write** of N outbox
   events to a haematite stream. Each outbox event carries (a) a deterministic **idempotency
   key** and (b) an **ordinal pinned at dispatch** (mirroring `Collect`'s first-arrival pin).
2. A **separate, non-replayed dispatcher** reads the outbox and sends over liminal with
   at-least-once retry. This component is outside the deterministic replay domain.
3. Completion ingestion appends **under the dispatch-pinned ordinal**, regardless of network
   arrival order. The Recorder **drops a second completion for an already-resolved ordinal**
   (single-writer dedup chokepoint). The join channel never determines the correlation key.
4. Turn ON liminal's **existing** per-channel idempotency-key + dedup-receipt (TTL) on the work
   and join channels. (Implemented; just not on the cross-node hot path today.)

Net effect: converts at-most-once-on-the-wire into **effectively-once-at-the-boundary** — the
DBOS/Temporal outbox pattern, backend-agnostic.

### H2 — Single active owner via a monotonic fencing epoch ⏳

**Problem:** haematite shard writes aren't fenced → under a netsplit both nodes append by static
BLAKE3 hash, and on heal `sync/merge` silently three-way-LWW-merges divergent roots
(VectorClock unimplemented) = data loss disguised as convergence. beamr `global` disclaims OTP's
lock (lexicographic snapshot-merge → duplicate activation for the full partition).

**✅ SPIKE FINDINGS (empirical, verified — `crates/haematite/tests/spike_fencing.rs`, 5/5 green):**
- **E1: `cas` IS a clean, race-free fencing token — on a SINGLE consistency domain.** Read-compare-write
  runs inside the owning shard's single-threaded actor. Gotcha: absent (`None`) ≠ physical zero
  (`Some(0)`) — **epochs must start at 1.**
- **E2: confirmed — event-stream sync silently LWW-merges divergent writes = DATA LOSS, no error.**
  Same-seq divergence → one committed event vanishes; different-seq → events survive but the
  per-stream sequence counter is LWW-corrupted. `VectorClock` merge is unimplemented; the engine
  treats event keys as ordinary mutable KV. The divergence is reported as a *resolved* conflict and
  discarded — silently wrong run history.
- **🔴 E3 CRITICAL: local `cas` is INSUFFICIENT for active-active.** Two partitioned nodes EACH
  cas-bump their own local copy of the epoch record — **both succeed** (local cas has no knowledge of
  the other side). Worse: when both pick the same epoch number, the epoch record merges *cleanly*
  (no conflict surfaced) — the split-brain is **completely hidden**, manifesting only as silent
  event loss at the data layer.

**Design (empirically grounded):**
- The fencing epoch MUST live behind **quorum/consensus** (a single synchronously-reached
  consistency domain), **never a per-node replicated cas.** This is the load-bearing requirement.
- haematite's `StrongConsistency` / `wait_for_quorum` exist (`sync/consistency.rs`) but are **NOT
  wired to the write path** today (`put_with_ttl_and_consistency` only counts the local ack) — so
  quorum is not currently enforceable end-to-end. **Wiring quorum to the write path is a haematite
  prerequisite for active-active that does not exist yet.**
- Run-history event streams need **non-LWW, collision-free, append-only/union merge semantics**
  (node-id in the event key + a `Custom` union `ConflictPolicy`) — haematite does not provide this
  today; it would lose data on any same-key cross-node write even WITH a correct fence.
- `AcquireShard` = quorum-acked epoch bump → fence old owner → **replay state from the durable
  store** (state never migrated — the Akka rule). A stale owner's quorum-gated write fails.
- **Bottom line: active-active on haematite's CURRENT primitives is NOT safe for run history.** It
  requires building, in haematite, (i) consensus/quorum on the ownership/epoch record and (ii) a
  non-LWW collision-free event keyspace. Both are real, scoped work — and they ARE the "do it
  properly" path. This is the major scope finding.

### H2b — Handoff / rebalancing / membership ⏳

- An authoritative, **CAS-versioned shard→node assignment map** owned by a liminal global-name
  singleton coordinator (Akka ShardCoordinator role). On a membership delta, the new owner must
  `AcquireShard` (bump epoch, fence dead owner) BEFORE accepting work, then reload run state.
- Node identity = `node:port:incarnation-epoch` (fresh on restart). Ownership is a versioned
  record. The TCP-liveness diff (`connected_nodes()` polled 250ms) is only a **trigger to
  re-resolve**, never ground truth.
- Failure-detector quality (incarnation epochs, direct peer probes, Lifeguard-style timeout
  inflation) is design-before-scale, not day-one — but determines how badly we misbehave under
  flapping links.

## Storage: two-tier, control-plane vs data-plane ⏳

The review's sharpest storage point: don't use one content-addressed structure for everything at
the default Eventual-60s consistency. Split:
- **Control plane** (small, CAS/quorum-backed): shard→node map, epochs, membership version.
  Strongly consistent. This is where the fencing lives.
- **Data plane** (high-write append-only): run-history event log. **Run-history writes must be
  Strong / quorum-acked** (via haematite `wait_for_quorum`), not Eventual-60s — otherwise a
  committed event is up to 60s from any replica and dies with the owner inside that window.
- Feed `StrongConsistency.total_nodes` from **live membership**, not a static config, so a
  minority partition deterministically fails `QuorumUnavailable` (and can't self-quorum).
- **Snapshot + per-stream log-trim**: wire the existing `HistoryCompacted` sentinel to a real
  compaction job (Restate trims after snapshot). Large result blobs → content-addressed,
  referenced by hash from the small completion event. (Bounds replay/recovery + storage.)

## Affinity

- Route stateful/related work via haematite's `key→shard` consistent hash AND/OR liminal's
  global-name registry (a named actor lives on one node, reachable cluster-wide).
- A bare consistent hash is explicitly insufficient across join/leave (unstable) — affinity is
  bound to the **fenced ownership map** above, not the raw hash. A named actor may only commit
  if it holds the current shard epoch.

## Two replay engines — layering (must document explicitly)

- **aion's Recorder is the SINGLE source of truth** for any cross-process interaction the
  workflow observes.
- beamr's `RecordedMessageDelivery` serves only intra-beamr determinism BELOW the aion seam and
  must **never re-deliver a completion aion already records**. One clock domain, recorded once.

## Build sequence (proposed)

0. **Prereq:** align haematite → beamr 0.9.0 (kills the version skew: liminal currently pulls
   beamr 0.8.2 transitively via haematite + 0.9.0 direct). Mechanical; do first.
1. ✅ **Spike DONE** (verified): `cas` fences on a single domain; sync/merge silently LWW-drops
   divergent event streams; **local cas is insufficient — active-active needs quorum on the epoch
   record + union event-merge** (see findings above).
2. **haematite foundation (NEW — surfaced by the spike; the real prerequisite for active-active):**
   2a. ✅ **Spike DONE + verified in code:** wiring quorum to the write path is **NET-NEW, ~2–4
       weeks**, not wire-up. The quorum *math* exists and the write path's `mpsc` ack-receiver seam
       is already shaped to accept remote acks — but `wait_for_consistency` (api/kv.rs) feeds it a
       dropped local channel, and **no synchronous write-ack transport exists**: `SyncMessage`
       (sync/protocol/wire.rs) has only 6 pull-based anti-entropy variants, NO write/ack; membership
       is a static config list with no liveness. Build required: new `SyncMessage::WriteProposal` +
       `WriteAck` variants + wire codec; request→ack correlation over the beamr connection manager;
       durable-apply-then-ack on receivers; a writer-side ack collector feeding the existing
       receiver seam; a membership/liveness source feeding `total_nodes` (must be FULL membership,
       else a minority self-quorums). This is a real synchronous-replication path. Quorum math
       wire-up = days; the transport = weeks.
   2b. Add a non-LWW, collision-free event-stream merge: node-id in the event key + a `Custom`
       union `ConflictPolicy` for run-history streams (forbid LWW on that keyspace). Its own spike
       before building.
3. Control-plane: quorum-backed CAS-versioned shard→node map + epoch fence + `AcquireShard`
   (on the quorum domain from 2a, NOT per-node cas).
4. Data-plane: Strong/quorum-acked run-history writes over the union keyspace from 2b.
5. H1 outbox dispatch path + Recorder dedup + liminal dedup ON for hot path.
6. Snapshot + trim/compaction.
7. haematite-as-aion-backend adapter (small, given the trait already fits).

**Scope note:** steps 2a/2b are genuine distributed-systems engineering in haematite that did not
exist before this analysis. They ARE the cost of active-active + single-binary, accepted
deliberately. Each gets its own empirical spike + adversarial review before it's trusted.

## Decisions (all confirmed by Tom 2026-06-24 — "do all of those things, keep it moving")

- ✅ Single-binary / no-external-deps **non-negotiable**.
- ✅ **Active-active first** (full fencing + quorum + handoff).
- ✅ Run-history writes are **Strong / quorum-acked** by default (committed events survive owner
  death; accept the latency) — required for active-active correctness anyway.
- ✅ **Do BOTH paths:** build the active-active haematite foundation (steps 2–7) AND stand up the
  interim outbox fan-out on the existing **libsql** backend in parallel as an early-capability
  milestone. The outbox dispatch design (H1) is backend-agnostic, so the interim is mostly the H1
  work pointed at libsql; it delivers user-visible fan-out/fan-in while the haematite consensus
  foundation (2a/2b) is built and spiked properly underneath, then becomes a backend swap.
- Audit history (still open, low-priority / design-before-scale): default to **trim-after-snapshot**
  (Restate-style bounded recovery) unless Tom wants immutable-forever audit. Decide when the
  snapshot/compaction step (6) is designed.
