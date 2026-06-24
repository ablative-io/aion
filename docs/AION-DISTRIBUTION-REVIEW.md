# Aion distributed fan-out/fan-in/affinity + storage — comparative design review

> Output of the 11-agent comparative review (Temporal, Orleans, Akka, Ray, Erlang/OTP, DBOS/Restate → synthesis → 3-lens adversarial critique → verdict), 2026-06-24. This is the design-INPUT artifact, not yet the design doc.

## Headline verdict — `yes-with-caveats`

Yes — but only with caveats, and the caveats are load-bearing. Aion's replay core is genuinely world-class and already matches the Temporal/Restate contract (positional-ordinal correlation, single-writer recorder, recorded clock/RNG, an arrival-order-independent fan-out skeleton). The plan, however, violates the corpus consensus at exactly two seams that would corrupt run history the first time a network partitions or a frame drops: it dispatches work and ingests completions over beamr's pg broadcast, which is explicitly at-most-once and frame-dropping (sized "for low-frequency control traffic"); and it has no monotonic fencing on shard writes, so divergent partition writes get LWW-merged by haematite sync rather than rejected, while single-active-ownership rests on beamr `global`, which itself disclaims OTP's lock and uses lexicographic-wins snapshot merge. Both fixes are the cheapest proven answer in the corpus (durable outbox before dispatch; CAS-gated epoch fence) and the primitives already exist in-tree (haematite `cas`, liminal idempotency/dedup, aion's pinned Collect). Build those and it is Temporal/Restate-class; ship it as drawn and a single netsplit or dropped completion silently merges or loses committed work.

## Where it's already world-class

- Aion's deterministic replay core is best-in-class and verified in code: replay.rs resolves Commands against history by positional ordinal, CorrelationKey is positional (Activity(n)/Child(n)/Signal{name,index}), never arrival-order-derived — exactly Temporal/Restate's 'replay re-reads the log instead of re-sending' contract.
- True single-writer-per-workflow: the Recorder is the only sequence-head authority and even async arrivals (TimerFired, SignalReceived, ChildCompleted) append through it — the Akka single-writer-per-persistenceId / Temporal single-history-shard invariant that makes replay a sound fold.
- Clock and RNG are already first-class recorded state (DeterminismContext.now() from recorded timestamps; random() via determinism NIF keyed by per-call ordinal, ADR-002) — the 'make RNG/clock recorded from day one' rule, already met.
- A correct distributed-fan-out skeleton already exists locally: PendingAwait::Collect pins a contiguous ordinal range at FIRST arrival (nif_state.rs:125-128, with the explicit 'unpinned re-allocation would corrupt the ordinal-recorded-event correlation' invariant) — the exact arrival-order-independent positional match the distributed case needs.
- haematite's fan-in surface is the RIGHT primitive, contrary to the H3 worry: event_store.rs exposes a flat append-only log (stream_key||0x00||seq, big-endian, O(delta) range reads, optimistic concurrency on expected_seq); the prolly-tree is the storage engine beneath, NOT a per-completion Merkle insertion. Content-addressing is correctly reserved for sync/branch-merge/snapshot.
- The conditional-write primitive that fixes split-brain ALREADY SHIPS: haematite `cas` (db.rs:177, event_store.rs:219) with expected/actual mismatch is exactly Temporal RangeID / Orleans version-row — it is simply not yet wired onto the ownership path. That is a wiring gap, not a missing capability.
- aion's pluggable store trait already matches the best-in-class contract (append(expected_seq) + mandatory read_history_from range read; ReadableEventStore/WritableEventStore/PackageStore/VisibilityStore split), so 'haematite as a backend, not Postgres-only' is a small adapter, and the visibility store is already separated from the hot write store (Temporal's rule).
- liminal already implements the at-least-once layer the corpus demands: per-channel idempotency key + dedup receipt with TTL (durability/channel.rs, config.rs) and durable conversations that resume rather than re-execute — the pieces exist, they are just not on the cross-node hot path yet.

## Hard-problem guidance

### [must-design-now] H1 — Determinism under distribution: routing work items and completion results over liminal/beamr pg broadcast from (or into) the deterministic workflow.

- **Best practice:** Commit the intent-to-fan-out durably FIRST (Temporal ScheduleActivityTask + transfer-task outbox in one shard txn; DBOS queue-row insert; Restate Bifrost append), then a SEPARATE, non-replayed dispatcher does the real send with at-least-once retry keyed by an idempotency token. The deterministic thread NEVER sends/receives across the cluster; worker results return as appended events re-resolved by position on replay.
- **Our gap:** The plan lets aion publish work over beamr's pg path, which is verified at-most-once and frame-dropping (sender.rs: DIST_SEND_QUEUE_CAP=1024, non-blocking try_send DROPS on full, 5s write-timeout tears the connection and drops in-flight frames; module doc justifies this for 'low-frequency control traffic (pg join/leave)' that is 'self-correcting' — a dropped WORK ITEM or COMPLETION is NOT). The deterministic ordinal for each remote result is also unassigned, so appending completions in network-arrival order will trip aion's own non-determinism resolver.
- **Recommendation:** Adopt the Temporal outbox: a workflow Command emits a durable 'dispatch N' intent appended to a haematite stream in ONE write; a separate non-replayed dispatcher reads the outbox and sends with retry. Assign each fan-out member a deterministic ordinal AT DISPATCH (mirroring Collect's pin) and have the completion carry it back as the idempotency/correlation key; the Recorder appends under the pinned ordinal regardless of arrival order and DROPS a second completion for an already-resolved ordinal (it is the single sequence head, so it is the natural dedup point). Turn liminal's existing dedup/durable-conversation strategy ON for the work and join channels. This converts at-most-once-on-the-wire into effectively-once-at-the-boundary.

### [must-design-now] H2 — Split-brain: two nodes writing the same shard under a netsplit.

- **Best practice:** Gossip/membership PROPOSES the owner; a durable monotonic fencing token (Temporal RangeID, Restate epoch-bump-as-log-message) ENFORCES single-writer via conditional writes. A stale owner's conditional write FAILS and it self-fences; the token, not the ring, is the source of truth.
- **Our gap:** haematite shard writes are NOT gated on any epoch/lease. Under netsplit both nodes append by static BLAKE3 hash, and on heal sync/merge.rs silently THREE-WAY-MERGES divergent roots via the branch ConflictPolicy (LWW; VectorClock is Unimplemented) — for an event-sourced run history this is data loss disguised as convergence: a worker result vanishes or histories interleave unreplayably. single-active-ownership additionally rests on beamr `global`, which the source explicitly says 'does not implement OTP's full global lock protocol' and instead does lexicographic-lower-node-wins snapshot merge — guaranteeing duplicate activation for the full partition duration.
- **Recommendation:** Gate every shard append on a monotonic per-shard epoch using the existing `cas`: acquiring ownership CAS-bumps shard.epoch; every write is conditional on expected-epoch; a partitioned stale owner's CAS fails and it stops. FORBID LWW-merging an event-stream keyspace — divergent same-seq writes must be a hard conflict, not silently resolved. Reserve sync/merge's LWW strictly for branch/fork reconciliation of materialised state. Pair global-name with the epoch fence: a named actor may only commit if it holds the current shard epoch (lexicographic tiebreak is fine for choosing who RE-acquires, but the loser must be unable to have committed).

### [design-before-scale] H2 — Handoff, rebalancing, and membership quality.

- **Best practice:** An authoritative, versioned shard->node assignment owner (Akka ShardCoordinator) runs a buffer-during-handoff protocol; the new owner runs AcquireShard (bumps the fence, fences the dead owner) and REPLAYS state from the durable store (state is never migrated). Membership uses node:port:epoch fresh-epoch-on-restart, direct peer probes (not through the store), and Lifeguard-style self-aware timeout inflation; reading own-Dead-status self-terminates.
- **Our gap:** Affinity is a bare static BLAKE3 hash with no coordinator and no handoff — the corpus is explicit a bare consistent hash is unstable across join/leave. liminal membership is TCP-liveness diff only (connected_nodes() polled every 250ms): no incarnation epoch, no accrual/indirect probing, no totally-ordered view, so a flap looks like a departure and a restarted node reusing its name is indistinguishable from its prior incarnation. StrongConsistency.total_nodes is a static config decoupled from live membership, so both sides of a 50/50 split could each think they form quorum.
- **Recommendation:** Introduce a CAS-versioned shard->node map in haematite owned by a liminal global-name singleton acting as the coordinator; on a membership delta the new owner must AcquireShard (CAS-bump the epoch) BEFORE accepting work and reload run state from the durable store; buffer or reject inbound work while a shard is in-handoff. Add an incarnation epoch to node identity; treat the TCP diff as a SUGGESTION that triggers re-resolution, not ground truth. Feed StrongConsistency.total_nodes from the versioned membership view so a minority deterministically fails QuorumUnavailable.

### [design-before-scale] H3 — Right durable primitive for fan-in (already mostly solved) + unbounded growth.

- **Best practice:** A flat per-join append-only log + monotonic offset cursor read as a resumable fold + async snapshot and log-trim to bound replay/recovery (Restate trims after snapshot; Temporal caps 50K/50MB; Ray caps lineage at 1GB). Large result bodies stored content-addressed by hash, referenced by a small completion event — the ONE legitimate use of content-addressing in fan-in.
- **Our gap:** The primitive itself is RIGHT (event_store.rs is a flat seq log, not prolly-insertion per completion) — so this is the cheap half. The real gap is operational: there is NO async snapshot + log-trim path (HistoryCompacted exists only as a read-time sentinel, no compaction job), so the completion ledger and per-stream history grow unbounded and recovery cost grows with total history.
- **Recommendation:** Confirm and document that the per-completion append is O(1)-append on the flat log, never prolly-insertion. Add per-stream snapshot + trim (compact resolved joins to a summary, drop trimmed events) and wire the existing HistoryCompacted signal to a real compaction job BEFORE wide fan-in is load-bearing. Store large results as content-addressed blobs referenced by hash in the small completion event.

### [must-design-now] Storage thesis honesty — control-plane vs data-plane, and the no-external-deps claim.

- **Best practice:** Two-tier durability: a small CAS/consensus control-plane (membership/ownership/epochs) separate from a high-write append-only data-plane log (Akka ddata vs journal; Restate Raft-metadata vs Bifrost). Restate — the ONLY true zero-external-dependency precedent — quorum-replicates the data-plane log so a minority cannot lose committed writes, and still wants an object store for snapshots in production.
- **Our gap:** haematite uses one content-addressed structure for everything and defaults to Eventual 60s consistency (consistency.rs:135). Replicating run history via async DIST sync means a committed event is up to 60s from any replica; if the owner dies in that window the run's latest committed history is lost on failover — defeating 'never-dying.' The quorum module only COUNTS write-acks; it does not elect or fence a leader, so it observes replication breadth but cannot prevent split-brain (the Mnesia trap). The single-binary/no-deps thesis is honestly met for single-node TODAY; multi-node is eventually-consistent replication, not a consistent distributed store.
- **Recommendation:** Split a small CAS-backed control plane (shard->node map, epochs, membership version) from the append-only run-history data plane, and make run-history writes Strong (quorum-ack via the existing wait_for_quorum) rather than Eventual so a committed event survives owner death. State the thesis precisely: 'single binary, single-node durable today; multi-node is replication, not yet a consistent store.' Either commit to Restate's model (in-binary Raft/CAS control plane + epoch-fenced log) or descope to single-owner-per-shard with read-only followers — pick one explicitly; the current middle path has a distributed DB's cost and neither's safety.

## Alternatives honestly weighed

- Build on Temporal/Cadence instead of own-stack: you get the battle-tested replay engine, RangeID fencing, Matching dedup, and 50K/50MB caps for free — but it MANDATES an external Cassandra/SQL cluster (SQLite is dev-only, no embedded-replicated mode), which directly contradicts the single-binary/no-external-deps thesis and the 'durable agents as infrastructure, one never-dying binary' pitch. Rejecting it is defensible ONLY because the thesis is the product; if the thesis were negotiable, Temporal is the lower-risk path.

- Build on DBOS (Postgres-as-everything): the cleanest exactly-once trick in the corpus (business write + durability checkpoint in ONE transaction, no outbox window) and cluster fan-out via SELECT...FOR UPDATE SKIP LOCKED on a queue table — zero new storage engine. But 'one dependency' still means inheriting Postgres as the SPOF and its HA story; the no-deps thesis is only half-met. Pragmatically the strongest INTERIM: ship distributed fan-out/fan-in on the existing libsql/Postgres backend NOW (outbox + completion stream + polling dispatcher), and treat haematite-self-networked as a later backend that must clear the fencing + snapshot bar first. This delivers the user-visible capability at a fraction of the build cost.

- Run on real Erlang/OTP instead of beamr: you get OTP `global`'s genuine cluster-wide lock (a real singleton, vs beamr's lexicographic-merge stub), mature net_kernel, and decades of hardening — but you lose Rust type-safety end-to-end, the in-process embeddable-library story, and the AOT/single-binary north-star, and OTP's own delivery is still non-guaranteed (pg is at-most-once) so you'd build the same outbox/dedup layer anyway. The type-safety + single-binary vision is the reason to stay on beamr; just don't lean on beamr `global` or pg as correctness boundaries.

- Adopt Restate's architecture wholesale (in-binary Raft control plane + quorum-replicated append log): the ONLY proven zero-external-dependency precedent for exactly this thesis, and the honest target if 'world-class multi-node single-binary' is the goal. The tradeoff is real build cost — embedded Raft for placement/epochs plus an epoch-fenced data-plane log — and even Restate wants an external object store for production snapshots. This is the 'do it properly' option; the current plan is a lighter-weight approximation that needs the fencing token + handoff to approach it.

- Descope multi-node consistency: keep haematite single-owner-per-shard with replicas as read-only followers (no merge authority). Preserves the single-binary thesis and sidesteps the entire split-brain/quorum problem at the cost of no active-active writes. Lowest-risk way to ship durable distribution without taking on a distributed-database project; can be upgraded to the fenced model later.

## Prioritized design inputs (the spine of the design doc)

1. The fan-out dispatch path MUST be: workflow emits a 'dispatch N' Command -> one atomic durable write of N outbox events to a haematite stream (each carrying a deterministic idempotency key AND a pinned ordinal allocated at dispatch) -> a SEPARATE non-replayed dispatcher reads the outbox and sends over liminal with at-least-once retry. The deterministic workflow thread MUST NOT send or receive beamr/liminal messages directly.

2. Completion ingestion MUST append under the dispatch-pinned ordinal regardless of network arrival order, and the Recorder MUST drop a second completion for an already-resolved ordinal (single-writer dedup chokepoint). Specify that the join channel never determines the correlation key.

3. Every cross-node haematite shard write MUST be a conditional update gated on a monotonic per-shard epoch acquired via the existing `cas`; specify the AcquireShard sequence (CAS-bump epoch -> fence old owner -> replay state from durable store) and explicitly FORBID LWW-merging the event-stream keyspace — divergent same-seq writes are a hard conflict.

4. Specify the authoritative, CAS-versioned shard->node assignment map and its owner (a liminal global-name singleton coordinator), plus the buffer-during-handoff protocol hung on beamr's connection-down hook. A static BLAKE3 hash alone is explicitly insufficient.

5. Specify node identity as node:port:incarnation-epoch (fresh on restart) and make ownership a versioned record, treating the TCP-liveness diff only as a trigger to re-resolve — not as ground truth.

6. Split storage into a small CAS-backed control plane (ownership/epochs/membership-version) and an append-only run-history data plane; make run-history writes Strong/quorum-acked, not the default Eventual-60s, and feed StrongConsistency.total_nodes from live membership so a minority deterministically fails QuorumUnavailable.

7. Specify async snapshot + per-stream log-trim and wire the existing HistoryCompacted sentinel to a real compaction job; specify large-result blobs as content-addressed, referenced by hash from the small completion event.

8. Turn liminal's existing idempotency-key + dedup-receipt (or durable-conversation) strategy ON for the work and join channels — it is implemented but not on the cross-node hot path today.

9. Document the layering of the two replay engines: aion's Recorder is the SINGLE source of truth for any cross-process interaction the workflow observes; beamr's RecordedMessageDelivery serves only intra-beamr determinism BELOW the aion seam and must never re-deliver a completion aion already records — one clock domain, recorded once.

10. State the single-binary/no-external-deps thesis honestly and scoped: durable single-node today; multi-node is replication that becomes a consistent store only once fencing + handoff + Strong-default land.

## Open questions for Tom (decisions that reshape the design)

- Is the single-binary / no-external-dependency thesis NEGOTIABLE? If it is the product, we commit to the Restate-style in-binary fencing/quorum build cost. If it is not, building on Temporal or DBOS is materially lower-risk. This one answer reshapes everything below it.

- Sequencing: ship distributed fan-out/fan-in on the EXISTING libsql/Postgres backend first (DBOS-style outbox, weeks not months), and gate haematite-self-networked behind fencing+snapshot — or hold the feature until haematite-self-networked is safe? The former delivers the capability far sooner; the latter keeps the stack pure.

- For multi-node haematite, do you want active-active fenced writes (full fencing token + handoff + quorum, the 'do it properly' path) or single-owner-per-shard with read-only followers (much cheaper, no split-brain surface, no active-active)? This is the single biggest scope decision.

- Consistency default for run history: are you willing to make run-history writes Strong/quorum-acked (slower writes, but committed events survive owner death) — accepting the latency — versus the current Eventual-60s default that can lose the latest committed history on owner death within the sync window?

- How much do you want to invest in failure-detector quality now (incarnation epochs, direct peer probes, Lifeguard-style timeout inflation) versus deferring it? It is 'design-before-scale,' not a day-one blocker, but it determines how violently the system misbehaves under flapping links.

- Do you want immutable full audit history (Temporal/Akka — replayable forever, unbounded without caps) or trim-after-snapshot (Restate — bounded recovery/storage, loses deep history)? This sets the snapshot/compaction design and the product story around auditability.


---
## Appendix: cross-system synthesis

This is a synthesis task — no tooling needed. I'll produce the cross-cutting synthesis directly.

# Cross-Cutting Synthesis: Distributed Durable Orchestration

Drawn from Temporal/Cadence, Orleans, Akka Cluster, Ray, Erlang/OTP, and DBOS+Restate. Organized by the four dimensions and three hard problems. Each section gives **consensus**, **divergence + tradeoffs**, **traps**, with system citations. Built to adversarially critique a concrete architecture, so the "verdict" lines are blunt.

---

## DIMENSION 1: FAN-OUT

**Consensus best practice — fan-out is durable BEFORE it runs, and the deterministic/orchestration thread never dispatches across the cluster itself.**
Every event-sourced/durable engine commits the *intent to fan out* to durable storage before any work actually executes, and decouples that commit from the real cross-cluster dispatch:
- **Temporal**: workflow emits `ScheduleActivityTask` commands → History Service atomically writes `ActivityTaskScheduled` events + Transfer Tasks (outbox) in ONE shard transaction → a *separate* Transfer Task processor RPCs Matching to enqueue for remote workers. The workflow thread never sends across the cluster.
- **DBOS**: enqueue = a committed row insert into the queue table; workers poll via `SELECT … FOR UPDATE SKIP LOCKED`. Work is durable before it runs.
- **Restate**: each invocation is an event appended to the Bifrost log (quorum-replicated) before a partition processor materializes and dispatches it.

So the universal pattern: **fan-out = "append N durable dispatch records," and a non-orchestration component does the real send.** This is the single most load-bearing agreement across the corpus.

**Divergence + tradeoffs:**
- **Command/outbox (Temporal, Restate, DBOS)** vs. **direct live dispatch (Orleans, Ray, OTP)**. Orleans (`Task.WhenAll` over N grain calls), Ray (`f.remote()` caller→raylet→worker), and OTP (`simple_one_for_one` supervised children, `multi_call`, `pg`) dispatch *live* — fast, decentralized (Ray ~200µs, ~10k tasks/sec/client; OTP location-transparent pid send), but the dispatch itself is **not durable** and inherits at-least-once + idempotency burden. The durable-execution engines pay a write per dispatch to get crash-survivability.
- **Caller-as-dispatcher (Ray)** vs. **central/coordinator routing (Temporal Matching, Akka ShardRegion)**. Ray's fully decentralized lease-request/spillback removes the hot-path bottleneck but means owner death is terminal (no handoff). Temporal/Akka route through a service/region — a manageable indirection that enables clean handoff and fencing.
- **Addressed vs. broadcast fan-out**. Akka Cluster Sharding and Orleans grain-calls are *addressed* (one logical recipient per id). Akka Distributed Pub/Sub and OTP `pg` are *broadcast*. The broadcast primitives are the dangerous ones (see traps).
- **Hard fan-out caps**. Temporal caps a parent at ~1,000 children and 50K events / 50MB history → forces `ContinueAsNew` or a tree of intermediate parents. Restate and DBOS scale via queues/partitions instead. **Any single-parent wide fan-out has a ceiling somewhere; the engines that hide it best use queues, not parent-embedded child status.**

**Universal TRAPS:**
1. **At-most-once broadcast primitives silently drop work.** Akka Distributed Pub/Sub is explicitly at-most-once ("messages can be lost over the wire"). OTP `pg`/distribution is non-guaranteed delivery. **Do NOT build work-distribution fan-out on a pub/sub channel without an app-level ack/retry/dedup layer.** This directly indicts any "publish N work items to a worker pool over a liminal channel" plan.
2. **Letting the deterministic thread dispatch directly** breaks replay (see H1). Temporal's whole design exists to prevent this.
3. **Wide fan-out bloats the parent.** Temporal's >1,000-child discouragement; Orleans StatelessWorker is the escape hatch. Embedding per-child status in the parent's hot log doesn't scale.
4. **`Promise.all`-style fan-out leaks/crashes on first failure** — DBOS mandates `Promise.allSettled`; `Promise.all` can crash the Node process and leak unresolved children. A fan-out join must tolerate partial failure structurally.

---

## DIMENSION 2: FAN-IN

**Consensus best practice — the durable fan-in primitive is an APPEND-ONLY LOG + a monotonic offset/sequence cursor, read as a resumable fold. It is NOT a barrier service and NOT a content-addressed/Merkle tree.**
This is the strongest and most unanimous signal in the entire corpus, and it is decisive for the proposed architecture:
- **Temporal**: append-only event history (monotonic event IDs) + compact mutable-state summary to resolve futures without rescanning. Deliberately chose single-writer append log over any tree/CAS — "content addressing buys them nothing for a high-write linear ledger."
- **Akka**: per-`persistenceId` append-only journal + `eventsByTag` index + Projection with a **durably stored offset**; join = resumable fold from offset.
- **OTP**: process mailbox + selective receive (ordered append log you drain) + request-id collection — confirms the *shape* (ordered append log, not keyed associative structure) but is volatile.
- **DBOS**: child outputs checkpointed as rows; join = SQL read of committed rows.
- **Restate**: completions/awakeable-resolutions are log events; single-writer partition processor folds into RocksDB; **trims the log after snapshot** (opposite of keeping immutable history).
- **Ray**: durability anchored in the small mutable ownership/metadata table + lineage, NOT in the Plasma content-addressed object bytes (which are evictable/rederivable).

**Divergence + tradeoffs:**
- **What is durable in the join**: the *facts/events* (Temporal, Akka, Restate, DBOS) vs. *metadata + re-derivation* (Ray's lineage — bytes are rebuilt, not stored). Ray trades storage for re-execution risk (fatal under nondeterminism).
- **Coordination record separated from the bulk stream**: Orleans and Ray explicitly split (a) high-write completion stream / partitioned queue with per-consumer offset, from (b) a small CAS-versioned "are all N done?" coordination record (Orleans' membership-table pattern; Ray's ownership table). Temporal fuses these (history + mutable-state summary in one shard). **Splitting them is cleaner when write rates diverge.**
- **Straggler tolerance**: Ray's `ray.wait(num_returns=k)` and OTP's `multi_call` BadNodes accounting give first-k / partial-failure fan-in natively. Temporal does `Selector`/Promise.race in the deterministic loop. A durable join must decide first-of vs. join-all and account for stragglers explicitly.
- **Durability of the join itself**: Orleans is the cautionary outlier — no auto-checkpoint, no event-sourced arrivals; durability is only as good as how often the aggregator calls `WriteStateAsync`. Everyone else makes each arrival a durable append.

**Universal TRAPS:**
1. **A content-addressed/Merkle/prolly tree is the WRONG primitive for the hot completion path.** Stated explicitly by Temporal, Akka, OTP, Ray, and DBOS+Restate independently. You pay per-write rehashing up the spine + tree rebalancing, ordering is by content-hash not arrival, and "order-independence" is a *non-goal* for a strictly-appended ledger. Reserve content-addressing for cross-node sync / branch-merge / snapshot of *materialized state*, never the per-completion write. (Note: this is literally the structure that produced haematite's history-independence bug — order-sensitivity in a positional split — so the corpus is confirming an already-observed failure mode.)
2. **In-memory-only fan-in loses the join on crash.** OTP mailbox and Orleans-without-frequent-checkpoint both lose in-flight work. The durable substrate must be the append, not the process state.
3. **Unbounded history/lineage = memory leak.** Ray's `RAY_max_lineage_bytes` (1GB default), Temporal's 50K/50MB cap. Without async snapshot + log-trim (Restate), replay cost and storage grow without bound.
4. **Late/straggler replies corrupting the join.** OTP's middleman-process pattern GCs late replies so they never pollute the caller's mailbox — a join must route post-timeout completions somewhere safe.

---

## DIMENSION 3: AFFINITY

**Consensus best practice — affinity = (deterministic key→shard hash) + (a directory/registry mapping shard/identity→node) + (single-active-instance-per-key guarantee). Affinity must be a COST optimization with a correctness-preserving fallback, never a correctness dependency.**
- **Temporal**: workflowID hashed → fixed history shard (strong state affinity, one History host) + Sticky Task Queues (per-worker LRU cache) as a *pure cost optimization* with full-replay fallback on cache miss.
- **Akka**: `extractShardId` deterministic hash + `ShardCoordinator` assigns shard→region (one node) + one active entity per id; `remember-entities` makes affinity survive node loss.
- **Orleans**: grain directory (identity→silo) gives stable single-location affinity "for free" via location transparency; placement strategies (PreferLocal, SiloRole, HashBased) choose initial silo.
- **Restate**: VO key → partition with exactly one active leader processor = single-writer-per-key without locks; key-scoped state in that partition's RocksDB.
- **OTP**: `global` (locked singleton registry) for true singletons + `pg`/app-level consistent-hash for sharded pools.
- **Ray**: actor = stateful sticky home on one node + `NodeAffinitySchedulingStrategy` soft/hard + GCS actor directory.

**Divergence + tradeoffs:**
- **Static hash alone (the easy 80%) vs. hash + coordinator (the hard 20%).** A bare deterministic key→node hash (OTP makes you supply it; Orleans HashBasedPlacement) is **NOT stable across membership changes** — Orleans explicitly notes this and mitigates via directory registration. Akka's decisive addition is the **ShardCoordinator owning a durable shard→node assignment table** that rebalances. **Verdict: a static BLAKE3 key→shard hash is necessary but insufficient; you need an authoritative owner of the shard→node map plus a handoff protocol, or affinity breaks the moment a node joins/leaves.**
- **Sticky-as-optimization (Temporal, Orleans directory) vs. sticky-as-state-location (Restate, Akka).** Temporal/Orleans can always fall back to replay/reactivation from durable store on any node. Restate/Akka co-locate live state with the leader for hot-path local reads (faster) but make handoff heavier (state replays at new node). **The safe rule, stated by Temporal and Ray: any affinity must have a full-replay-from-durable-store fallback so it's never load-bearing for correctness.**
- **Strong vs. eventually-consistent directory.** Orleans offers both (eventually-consistent fast default vs. new versioned-range-lock DHT) — and recommends offering it as a *config knob*. OTP `global` is strong-but-slow (global lock), `pg` is fast-but-eventually-consistent. **Tradeoff is fundamental; the mature systems expose both rather than fixing one policy.**
- **Co-locate chatty work**: Orleans ActivationRepartitioning (call-frequency graph), Ray data-locality + Placement Groups (PACK/SPREAD), Restate co-locates log-leader with processor-leader. Optional optimization, not core.

**Universal TRAPS:**
1. **Single-activation / single-writer is NOT guaranteed during membership churn** unless explicitly fenced. Orleans documents a **~30s duplicate-activation window** after ungraceful crash/split-brain; the default directory is eventually-consistent. This is the bridge into H2 — affinity without fencing = two writers = corrupted event-sourced replay.
2. **Hash-based placement is not stable across membership changes** (Orleans) — naive consistent-hash reshuffles ownership on every join/leave unless a directory pins it.
3. **Fixed shard/partition count is sticky.** Temporal's `numHistoryShards` and Restate's partition count are **fixed at cluster creation** — no online re-sharding. Bad initial sizing requires migration. Size up front.
4. **Centralized directory/coordinator is a momentary SPOF.** Akka ShardCoordinator (singleton, unavailable during failover); Orleans PubSubRendezvous grain (bottleneck + duplicate-activation corruption in blue/green); OTP `global` single-process contention; Ray GCS. Any single-owner-of-the-map is a failover-window hazard.
5. **Stale-state-after-split-brain is acknowledged-unsolved** (Orleans #8242): even after the loser is deactivated, the survivor may hold stale in-memory state. App must use ETags/optimistic concurrency on its own storage.

---

## DIMENSION 4: DURABLE STORAGE

**Consensus best practice — two-tier durability: (1) a small, must-converge, CAS/consensus-backed CONTROL-PLANE store for membership/ownership/epochs; (2) a high-write, single-writer, append-only DATA-PLANE log for entity history/completions. Don't use one structure for both.**
- **Akka**: CRDT/ddata (+ optional LMDB) for shard-location control metadata vs. append-only journal for entity history — explicitly two different primitives.
- **Restate**: built-in Raft metadata store (placement/epochs/segments) vs. Bifrost append log materialized into RocksDB.
- **Ray**: GCS metadata (HA via external Redis) vs. evictable Plasma object bytes.
- **Orleans**: separate pluggable providers for clustering (IMembershipTable, needs atomic CAS), grain storage, reminders, directory.
- **Temporal**: main store (history/mutable-state/queues) + separate visibility store; history shard = 1:1 persistence partition = single-writer domain.

**Divergence + tradeoffs (this is where the single-binary thesis lives or dies):**
- **External DB dependency vs. embedded single-binary.** Temporal *always* needs Cassandra/SQL (SQLite is dev-only, single-node — no embedded-replicated mode). Akka needs JVM + external journal. Orleans "outsources distributed consensus to the cloud storage layer" (needs external IMembershipTable in prod). DBOS = exactly ONE dependency (Postgres) — but Postgres IS the SPOF and you inherit its HA story; the no-deps thesis is **only half-met**. **Restate is the sole true zero-external-dependency design: the single binary IS the database** (Bifrost quorum log + RocksDB + built-in Raft, snapshots to S3-style store off critical path). **For any single-binary/no-external-deps thesis, Restate is the only real precedent — and even it wants an object store for snapshots in production.**
- **Consensus outsourced to storage CAS (Orleans, DBOS) vs. embedded consensus (Restate Raft, Ray Redis-quorum).** Outsourcing gives a non-quorum protocol surviving >50% loss (Orleans) and zero extra moving parts (DBOS transactional checkpoint) — but ties you to an external store. Embedding keeps it in-binary but adds Raft complexity.
- **Transactional checkpoint (DBOS) is the cleanest exactly-once trick**: the step's business write and its durability checkpoint commit in the SAME Postgres transaction — no dual-write/outbox window. Only works when the effect targets the same store.

**Universal TRAPS:**
1. **Mnesia-style non-resolution.** OTP Mnesia reports `inconsistent_database` and punts split-brain to the operator. **Do NOT model a durable store on a backend that doesn't auto-resolve or fence partitions.**
2. **"One dependency" still means inheriting that dependency's HA/SPOF** (DBOS/Postgres). Honest accounting required.
3. **Snapshot freshness gates recovery time.** Restate: stale snapshot = long log-suffix replay. Without async snapshot + trim, recovery and storage are unbounded.
4. **Visibility/query store must be separate** from the hot write store (Temporal) — don't index the hot path inline.

---

## HARD PROBLEM H1: Determinism-under-distribution

**CONSENSUS best practice (near-unanimous, the single most important finding) — separate the deterministic orchestrator from non-deterministic effects, and make every cross-process interaction (a fan-out send AND a fan-in result) a RECORDED EVENT in history BEFORE the orchestrator observes it. On replay, re-read the event; NEVER re-send/re-receive live.**
- **Temporal**: side-effect-free workflow thread emits commands only; real distributed message-passing happens ONLY between History/Matching/workers, never in the deterministic thread. Activity results return as appended `ActivityTaskCompleted` events; replay matches commands positionally against recorded events. Nondeterminism (UUID/clock/RNG) must go through `SideEffect`/`MutableSideEffect` or an activity.
- **DBOS/Restate**: identical contract — `@workflow`/handler must be deterministic, do no I/O; all effects via `@step`/`ctx.run`, checkpointed once, served from store on replay. **Restate's key move: a sent message/call/awakeable resolution IS a journal event, so distribution doesn't break replay** — replay re-reads the log instead of re-sending.
- **Akka**: narrower but cleaner — replay only the append-only *event* log (pure `(state,event)=>state` fold); command handlers (with side effects) are NOT re-run on recovery. **Sidesteps "deterministic replay over message passing" entirely by NOT replaying message passing — only recorded facts.** Correctness rests on single-writer-per-persistenceId.

**Divergence + tradeoffs:**
- **Replay-based (Temporal, DBOS, Restate, Akka-events) vs. checkpoint-state (Orleans) vs. lineage-re-execution (Ray) vs. no-determinism (OTP).** Orleans gives up replay entirely — buys distribution + location transparency, loses in-flight work since last `WriteStateAsync`, pushes idempotency onto the app. Ray *assumes* determinism and re-executes lineage — at-least-once, **silently duplicates side effects or hangs on nondeterminism** (the 100→50 generator bug). OTP embraces nondeterminism (only pairwise-ordered, non-guaranteed delivery; no global order). **Verdict: a replay engine is strictly stronger than Orleans/Ray here — keep it — but it is ONLY sound if real message passing is recorded as events first.**
- **Exactly-once-on-the-wire mechanism**: DBOS dedups by workflow ID; Restate filters by monotonic epoch (drops messages from superseded leaders); Temporal uses idempotent Matching dedup via task tokens. All need a dedupe/idempotency key per dispatch and per completion.
- **Off-critical-path lineage logging (Ray's Lineage Stash, SOSP'19)**: log causal ordering of completions asynchronously so deterministic replay is recoverable without synchronous per-message logging latency. Worth stealing to avoid paying a sync write on every completion.

**Universal TRAPS:**
1. **Real message passing forces at-least-once + idempotency** (Orleans, OTP, Ray all confirm). **The instant the orchestrator does a live cross-process send/receive (beamr/liminal), determinism is broken unless the message's existence AND result are journaled before observation.** This is THE critique to apply: *does the proposed architecture let the deterministic workflow process send/receive beamr messages directly?* If yes, it's broken.
2. **Non-determinism checks are shallow** (Temporal: doesn't diff activity args or timer durations) — silent nondeterminism slips through; any code change to a running workflow risks a non-determinism error → requires Patching/Worker Versioning (an operational tax; old code must stay loadable, you can't delete branches).
3. **Nondeterministic re-execution silently duplicates effects** (Ray) — unless fan-out items are idempotent/keyed and completions deduped by content-hash.
4. **Determinism is an undocumented load-bearing assumption** users forget (Ray, DBOS, Restate all flag the "no I/O/clock/RNG in orchestrator" footgun). Make RNG/clock/code-version first-class recorded values from day one.

---

## HARD PROBLEM H2: Affinity + rebalancing + split-brain

**CONSENSUS best practice — gossip/membership PROPOSES the owner; a durable monotonic fencing token (RangeID/epoch/lease) ENFORCES single-writer via conditional writes. The source of truth for ownership is the durable token, NOT the gossip ring (which is transiently inconsistent under netsplit).**
- **Temporal RangeID**: every shard row has a monotonic generation; acquiring bumps it; every persistence write is a conditional update guarded by expected RangeID. A split-brain stale owner gets a conditional-update failure and self-fences. **Gossip (Ringpop/SWIM) decides who SHOULD own; the DB RangeID enforces who MAY write.**
- **Restate epoch-bump-as-log-message**: new leader takes next monotonic epoch, appends epoch-bump to the log; old leader reads it and steps down at exactly that log position. Late events from prior epoch are deterministically filtered → provably no split-brain view. Quorum write means a minority partition can't commit.
- **Orleans membership-via-CAS**: silos write themselves into IMembershipTable; direct TCP peer probes (NOT through the store) on a consistent-hash ring; suspicion accrual + a monotonic version row → **globally totally-ordered membership views**. "Perfect failure detection by fiat": once marked Dead, a silo reading its own Dead status self-terminates and restarts with a new epoch. Non-quorum (survives >50% loss).
- **Akka**: ShardCoordinator (singleton) owns durable shard→node map in ddata; buffer-during-handoff protocol; Split Brain Resolver (keep-majority/static-quorum/lease-majority) + time-based `down-removal-margin` or external lease fencing.
- **OTP**: `node:port:epoch` with fresh epoch on restart; `prevent_overlapping_partitions` (default-on, CP-leaning: actively disconnects to force clean partition geometry); `pg` purge-on-DOWN self-healing.

**Divergence + tradeoffs:**
- **Non-quorum-via-storage-CAS (Orleans, DBOS, Temporal RangeID) vs. quorum consensus (Restate Raft, Akka static-quorum, Ray Redis-quorum).** Non-quorum survives >50% loss and is single-binary-friendly IF the store is in-stack; quorum is the textbook-safe answer but sacrifices availability past quorum loss and adds complexity. **The cheapest proven split-brain answer for the hard 20% is a monotonic fencing token on each owned unit + conditional writes — adopt this regardless of which membership protocol proposes ownership.**
- **Where consensus lives**: Restate's "strong consensus in control plane (epochs/placement), relaxed/fenced data plane" is the cleanest split — keep Raft/CAS only for the small who-owns-what metadata, fence the hot path with epochs. Mirrors Temporal (gossip + RangeID).
- **Failure detector quality**: Orleans Lifeguard (self-scoring sick nodes inflate their own probe timeouts; indirect probing via a third node) is the most sophisticated and directly steal-able. OTP `net_ticktime` is timing-based, slow (45–75s), can't tell slow from dead. Temporal `ErrShardStatusUnknown` covers the uncertain-lease window. **Direct peer probes (not through the store) + self-aware timeout inflation is the robustness win.**
- **Handoff on node death**: Akka buffer-during-handoff (mark in-handoff → buffer → reassign → drain); Orleans seal-snapshot-apply-drop (crash → rebuild by querying live nodes); Temporal new owner AcquireShard (bumps RangeID, fences dead owner) + reload mutable state. **State is NOT migrated (Akka, Orleans, Restate) — the new owner replays from durable store.** Ray is the anti-pattern: no ownership handoff, owner death is terminal (fate-sharing).

**Universal TRAPS:**
1. **Trusting gossip/membership as the source of truth for ownership = split-brain corruption.** Two writers on the same event stream = interleaved events = "might not be interpreted correctly on replay" (Akka) = fatal. **The fencing token, not the ring, must gate every write.** Apply this critique directly: *does the proposed architecture gate haematite shard writes on a monotonic lease/epoch, or does it trust liminal/beamr membership?* If the latter, it's broken under netsplit.
2. **Duplicate-activation window during churn** (Orleans ~30s) — must converge to a totally-ordered view to resolve the loser, and the survivor may still hold stale state (#8242).
3. **Every split-brain strategy has a failure mode** (Akka): keep-majority self-downs if >half crash simultaneously; static-quorum breaks past `quorum*2-1`; down-all halts everything; time-based fencing trades availability for safety. No free lunch.
4. **Coordinator/singleton failover SPOF** (Akka, Orleans rendezvous, OTP global, Ray GCS) — momentary unavailability of the whole affinity layer.
5. **Don't outsource split-brain to an external HA quorum if the goal is a dependency-free binary** — Ray's biggest regret (external HA Redis, "officially supported only under KubeRay"). Put membership/quorum inside the stack.
6. **Shard briefly unavailable + buffer overflow during handoff** (Akka) — buffered messages can drop if the buffer overflows.

---

## HARD PROBLEM H3: Fan-in durable primitive

**CONSENSUS best practice (unanimous, zero dissent) — a per-join APPEND-ONLY EVENT LOG (monotonic sequence, O(1) append, single serialization point) + a compact mutable summary/cursor to resolve the join without rescanning + async snapshot to bound replay. The completion ledger is a flat append log, NOT a content-addressed/Merkle/prolly tree and NOT a coordination/barrier service.**
Every single system reaches this independently:
- **Temporal**: single-writer append log + mutable-state summary; *deliberately rejected* tree/CAS — "content addressing buys them nothing for a high-write linear ledger and would add per-write hashing + rebalancing cost."
- **Akka**: append-only journal + `eventsByTag` + durable offset; explicitly: a Merkle/prolly tree is optimized for de-dup/structural-sharing/order-independent-merge, which is the **wrong optimization target** for a strictly-appended ledger.
- **OTP**: ordered append log (mailbox) you drain/fold, not a keyed associative structure.
- **Ray**: durable metadata table + lineage; content-addressed Plasma bytes are explicitly the WRONG place to anchor completion durability.
- **DBOS**: relational append (step/child rows) + SKIP-LOCKED; SQL read for the join.
- **Restate**: Bifrost append log + single-writer partition processor + RocksDB fold + **log trim after snapshot** (immutable history is a non-goal here).

**Divergence + tradeoffs:**
- **Fused (Temporal: history + mutable-state in one shard) vs. split (Orleans/Ray: high-write completion stream + separate small CAS-versioned "all N done?" coordination record).** Split is cleaner when the completion write rate and the coordination decision rate diverge sharply.
- **Keep immutable history (Temporal/Akka) vs. trim after snapshot (Restate).** Trimming bounds replay/storage but loses full audit history. Choose based on whether you need replayable history vs. just current state.
- **Result body inline vs. content-addressed-by-hash.** Ray and the recommended pattern: append a *small* completion event (result-hash + worker id + seq) and store the *large* result body as a content-addressed blob fetched by hash. **This is the ONE legitimate place content-addressing belongs in fan-in — as the blob store the append log references, never as the write-hot join structure itself.**
- **Straggler/partial fan-in**: Ray `ray.wait(num_returns=k)`, OTP `multi_call` BadNodes — first-k and partial-failure must be explicit in the fold.

**Universal TRAPS:**
1. **Prolly/Merkle tree on the hot completion path is the canonical mistake** — flagged independently by 5 of 6 systems. Per-write rehashing up the spine, tree rebalancing, content-hash ordering instead of arrival order, and order-independence as a non-goal. **And this is the exact structure that produced haematite's history-independence bug (order-sensitive positional split) — the corpus is confirming an already-realized failure mode.** Reserve content-addressing strictly for cross-node sync / branch-merge / snapshot of materialized state.
2. **Unbounded ledger growth** without async snapshot + trim (Ray lineage cap, Temporal 50K/50MB, Restate trim).
3. **In-memory-only join** loses on crash (OTP mailbox, Orleans without frequent checkpoint).
4. **Re-spawn on parent crash instead of re-attach** — DBOS re-attaches by child workflow ID dedup; without it, replay double-dispatches the fan-out.
5. **Late completions corrupting the fold** (OTP middleman GC pattern).

---

## ACTIONABLE CRITIQUE CHECKLIST (apply to the proposed architecture)

The corpus converges on a small set of yes/no tests. The proposed architecture is **broken** if any answer is wrong:

1. **H1 / fan-out:** Does the deterministic workflow process EVER send or receive beamr/liminal messages directly? It must NOT. It must emit a "dispatch N" command that a separate, non-replayed dispatcher commits to a haematite event stream (one atomic write: intent + outbox), then sends. Worker results return as APPENDED events re-resolved by position on replay. (Temporal, Restate, DBOS, Akka all enforce this.)
2. **H1 / idempotency:** Does every fan-out item carry a deterministic dedupe/idempotency key and every completion a dedupe-by-content-hash, so at-least-once redelivery is idempotent? It must. (Orleans/Ray/OTP cautionary tales.)
3. **H2 / fencing:** Is every haematite shard write a CONDITIONAL update gated on a monotonic lease/epoch (RangeID/Restate-epoch style), with gossip/liminal membership only *proposing* ownership? It must be — trusting beamr/liminal membership as the source of truth for ownership = guaranteed split-brain corruption under netsplit. (Temporal RangeID, Restate epoch, Orleans version-row.)
4. **H2 / handoff:** Is there an authoritative owner of the shard→node map (Akka ShardCoordinator analogue, a liminal global-name singleton) with a buffer-during-handoff protocol on beamr's connection-down hook, and does the new owner replay state from haematite (state NOT migrated)? A static BLAKE3 key→shard hash alone is insufficient — it's unstable across membership changes.
5. **H2 / membership:** Direct peer probes (not through the store) + Lifeguard-style self-aware timeout inflation + `node:port:epoch` fresh-epoch-on-restart + read-own-Dead-status-self-terminate? And is membership/quorum INSIDE the stack, not an external HA Redis? (Orleans; Ray's regret.)
6. **H3 / ledger:** Is the hot fan-in completion ledger a flat per-join append-only haematite event stream + offset cursor + async snapshot — and is the prolly-tree/content-addressing confined to cross-node sync/branch-merge/snapshot and (optionally) the result-blob store referenced by hash? If the prolly-tree is on the per-completion write path, it's the canonical mistake (and the source of the known history-independence bug).
7. **Single-writer invariant:** Is exactly-one-active-owner-per-key enforced by the affinity layer (liminal global-name + coordinator + fencing) so replay only ever folds a log it solely owns? Replay is unsound without it (Akka).
8. **Storage thesis honesty:** Is the durable store split into a small CAS/consensus control-plane (membership/ownership/epochs) and a high-write append-only data-plane (Akka/Restate two-tier), and is the "single binary, no external deps" claim actually met (Restate is the only true precedent; DBOS only half-meets it via Postgres-as-SPOF)?
9. **Anti-patterns to reject outright:** at-most-once pub/sub for work distribution (Akka pub/sub, OTP pg); `Promise.all`-style fan-out; Mnesia-style split-brain non-resolution; outsourcing split-brain to external Redis; unbounded history/lineage without trim.