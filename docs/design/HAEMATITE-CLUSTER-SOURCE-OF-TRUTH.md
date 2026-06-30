# Haematite as cluster source-of-truth — membership, discovery & placement as durable state (DESIGN)

> Status: **design pass, read-only analysis. No production code changed by this doc.**
> 2026-06-30. Task #146. Written in the lineage of the other design passes
> ([NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md](../NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md),
> [NODE-AFFINITY-DESIGN.md](../NODE-AFFINITY-DESIGN.md), the epoch-fence / ADR-021
> clean-partial fence work in `crates/aion/src/engine/fence.rs`,
> [STORAGE-SWAP-MATURITY-DESIGN.md](../STORAGE-SWAP-MATURITY-DESIGN.md)).
>
> Folds into **#116** (fan-out / affinity maturity) and unblocks **#147**
> (auto-discovery: mDNS / Tailscale / gossip layered on the seed model in §4).
>
> Sibling designs this builds ON (do not re-derive them):
> - [AION-DISTRIBUTION-DESIGN.md](../AION-DISTRIBUTION-DESIGN.md) — H1/H2 fan-out +
>   active-active foundation, the locked storage two-tier split.
> - [MULTI-SHARD-ACTIVE-ACTIVE-DESIGN.md](../MULTI-SHARD-ACTIVE-ACTIVE-DESIGN.md) —
>   the AA-4-x series (shard election, epoch fence, union-merge), largely LANDED.
> - [DISTRIBUTED-ROUTING-DESIGN.md](../DISTRIBUTED-ROUTING-DESIGN.md) /
>   [ROUTING-MODEL.md](../ROUTING-MODEL.md) — SS-3 shard→node directory (#80),
>   request forwarding.

## TL;DR (read this first)

1. **The principle is already half-built.** Two of the three "cluster facts" are
   ALREADY durable, quorum-replicated haematite state today: **shard election /
   ownership** (the per-shard epoch-fence `Ballot`/`promised`/`owner_epoch`,
   #21–27/#33) and the **shard→node directory** (`publish_shard_owner` /
   `read_shard_owner`, a fenced quorum KV write keyed to co-locate on the shard,
   #80). The loop for *placement* already closes through haematite.
2. **The one cluster fact that is NOT durable state is MEMBERSHIP.** The quorum
   denominator — `WriteMembership.total_nodes`, the full member list every Strong
   CAS write and every shard election sizes its majority against — is derived from
   **static `[store.cluster].members` TOML** read once at boot. It can only change
   by editing config files and restarting every node. That is the gap this design
   closes: membership/discovery become **durable, quorum-replicated state**, the
   same consensus haematite already runs for everything else.
3. **beamr is a SENSOR, not an authority.** beamr's new net-tick
   `HeartbeatTimeout` (and the existing read-EOF `ConnectionDownReason`s) is a
   *liveness observation*. Today it drives an immediate, **local, unilateral**
   action (the SS-5b supervisor adopts shards). Under this design the same signal,
   **after debounce**, becomes a *proposed durable membership change* that must be
   **agreed by quorum** before it evicts a peer. The loop closes THROUGH haematite,
   so no single node can unilaterally rewrite the cluster's membership.
4. **The irreducible problem is bootstrap-seed.** You cannot reach quorum to WRITE
   the first membership record until you can already reach SOME peer. Every
   consensus system has this (Raft/etcd seed lists). Keep a *tiny* static seed for
   cold formation; after the cluster forms, all membership is dynamic durable
   state. Discovery (#147) automates *finding* the seed, it does not remove it.
5. **Recommended posture: spike-first, single-change membership (not joint
   consensus) initially, and the membership delta is a fenced quorum KV write that
   shares the shard-0 epoch fence** so a membership change and a shard handoff
   cannot race into split-ownership. Detail in §2.

---

## 1. PRINCIPLE — haematite is the agreement authority; beamr is the sensor

### 1.1 The three cluster facts, and where each lives today

| Cluster fact | What it answers | Where it lives TODAY | Durable consensus? |
|---|---|---|---|
| **Membership** | Who is in the cluster (the quorum denominator) | static `[store.cluster].members` TOML → `DistributedDatabaseConfig.nodes` (read once at boot) | **NO — static config** |
| **Discovery** | How do I find/dial a peer | static `[store.cluster].peers` (name + replication addr + gRPC forward addr) | **NO — static config** |
| **Placement** | Which node owns which shard | per-shard epoch-fence election (`acquire_shard_and_serve`) + SS-3 directory (`publish_shard_owner`/`read_shard_owner`) | **YES — already quorum state** |

The thesis of #146: **promote rows 1 and 2 to the same status as row 3.** Membership
and discovery become durable, quorum-replicated haematite records, read off the
locally-applied replica like any other key, mutated only by an agreed write.

### 1.2 Why haematite is the right authority (not beamr, not a sidecar)

haematite *is* a consensus engine. It already provides, verified and landed:

- **Strong CAS quorum-on-write** (`Database::replicate_write`,
  `db/receiver.rs::replicate_write`): a value reaches peer-quorum BEFORE it commits
  locally; the proposer's local ack counts but does not self-satisfy.
- **A quorum denominator that is liveness-independent**
  (`sync/membership.rs::resolve_membership` → `WriteMembership.total_nodes` =
  `config.nodes.len()`, NEVER the reachable subset). This is the load-bearing Q3
  invariant: *liveness must never shrink the denominator*, or a minority partition
  self-quorums (split-brain).
- **Per-shard epoch-fence election** (`acquire_shard_and_serve` → `Ballot` /
  `promised` / `owner_epoch`): exactly one fenced owner per shard, a typed
  `Fenced` error is the authoritative supersession signal mapped to
  `StoreError::NotOwner`.

Membership is *the same kind of fact* as a fenced shard owner. It belongs in the
same store, under the same quorum, not in a second coordination system (no
dependency "inbreeding" — see the durable-agents north star: a single binary, no
external consensus dependency).

### 1.3 beamr's role: the liveness sensor that FEEDS haematite

beamr already exposes the honest socket-liveness signal:

- `HaematiteStore::peer_connected(name)` → `endpoint.is_connected(name)`, true
  socket liveness (read-loop EOF → deregister on a real `kill -9`).
- the new **net-tick** (`beamr` `connection.rs`): an idle link with no inbound
  bytes past `HeartbeatConfig.deadline` is marked down with
  `ConnectionDownReason::HeartbeatTimeout`, firing the same down-hook / pg-purge /
  monitor-DOWN machinery as a FIN/RST. This closes the **silent-partition
  (black-hole)** gap: a peer that vanishes WITHOUT a TCP FIN is now detected by a
  missed deadline, not only by socket death.
- a registerable `ConnectionDownHook` (`register(Fn(ConnectionDownEvent))`)
  carrying `{ node, reason }`.

**The design rule:** beamr OBSERVES, haematite DECIDES. A `ConnectionDownEvent` is
an *input that proposes* a membership change; it is never itself the change.
"Agreed node X is down" is a quorum write; only after it commits do X's shards
re-elect — and re-election is *already* haematite. The loop closes through the
store.

> Today the SS-5b supervisor (`crates/aion-server/src/cluster.rs`) short-circuits
> this: on confirmed down it calls `adopt_shards` **directly and locally**. That is
> safe for *placement* (the epoch fence still guarantees exactly-one owner — the
> ADR-021 clean-partial fence in `engine/fence.rs` drops a deposed survivor), but
> it does NOT update *membership*: the dead node stays in the quorum denominator
> forever. §2/§3 close that.

---

## 2. The MEMBERSHIP-CHANGE PROTOCOL — join / leave / death as consensus ops

### 2.1 The records (durable state shapes)

Two new keyspaces, both **quorum-replicated fenced KV** (the existing
`replicate_write` path), co-located on a fixed coordination shard (recommend
**shard 0**, the same shard the schedule-coordinator already singletons on):

- **`cluster/members`** — the authoritative member set: the quorum DENOMINATOR.
  Value = an ordered, versioned list `{ epoch: u64, members: [{ node_id,
  replication_addr, grpc_forward_addr, status }] }` where `status ∈ { Joining,
  Active, Leaving, Down }`. `epoch` is a monotonic **config epoch** (distinct from
  per-shard `owner_epoch`) that increments on every committed membership delta.
- **`cluster/member/<node_id>`** — optional per-member detail / liveness lease
  metadata, so a single member's record can be CAS'd without rewriting the whole
  set. (Phase 2; phase 1 can keep everything in the single `cluster/members`
  record for simplicity — one CAS target, trivially serializable.)

`WriteMembership.total_nodes` and `send_targets` are **derived from
`cluster/members`** (the locally-applied replica) instead of from static
`config.nodes`. The static list survives ONLY as the bootstrap seed (§4).

### 2.2 Single-change vs joint consensus — RECOMMENDATION: single-change first

The classic Raft hazard: if you swap from old config C-old to new C-new in one
step, there is an instant where two *disjoint* majorities (one under C-old, one
under C-new) can each elect/commit — two leaders, split-brain.

- **Joint consensus** (C-old,new transitional config requiring majorities of BOTH)
  is the fully-general answer and the eventual target for arbitrary multi-node
  reconfiguration.
- **Single-server change** (add/remove exactly ONE member per committed delta) is
  *provably safe without* joint consensus, because any old-majority and any
  new-majority of sets differing by one element always overlap in at least one
  node — so they cannot both commit independently.

**RECOMMENDATION: ship single-server-change first.** It covers every real
operation we have (one node joins, one node drains, one node dies) and sidesteps
the joint-consensus state machine entirely. Enforce it as an invariant: a
membership delta CAS is **rejected** if it changes the member set by more than one
node. Joint consensus is a deliberate *later* addition (#116 follow-on), additive
to the record shape (the `epoch` + versioned list already support it), NOT built
now — no zombie joint-consensus code.

### 2.3 How a node commits a membership delta

A membership change is a **CAS on `cluster/members`** keyed by the current config
`epoch` (CAS precondition = hash of the current record):

1. Read the current `cluster/members` off the local replica → `(epoch_n, set_n)`.
2. Compute `set_{n+1}` = `set_n` with exactly one node added / removed / status-
   changed (single-change invariant, §2.2).
3. `replicate_write(cluster_members_key, expected = hash(record_n),
   new = record_{n+1} with epoch_{n+1})` against the **current** membership
   denominator. The write reaches quorum or it does not.
4. On `Ok`: the new member set is durable and majority-applied → every survivor
   reads the new denominator off its own replica. On `DatabaseError::CasConflict`:
   a concurrent delta won; re-read and (if still applicable) retry against the new
   epoch. On `DatabaseError::Fenced` / quorum-unavailable: the proposer is not
   permitted to change membership right now — back off (do NOT force it).

This is **the same primitive** as `publish_shard_owner` — a fenced, quorum,
value-CAS KV write — so it inherits the same correctness proof and the same
single-writer-wins guarantee.

### 2.4 The race that MUST NOT happen — membership change vs shard handoff

The hazard: node X dies. Survivor A wants to (a) adopt X's shards (a placement
change, epoch-fenced) AND (b) evict X from membership (a denominator change). If
these race, the quorum denominator could shrink *while* a shard election is in
flight against the old denominator — and a shrunk denominator is exactly the
split-brain door (`sync/membership.rs` Q3 invariant: *never shrink the
denominator from liveness*).

**RECOMMENDATION — order and fence the two operations so they cannot interleave
badly:**

- **Placement adoption fences against the OLD denominator and is allowed to run
  FIRST, independent of membership.** This is already true and already safe: the
  ADR-021 clean-partial fence (`engine/fence.rs::plan_adopted_shards`) does
  `acquire → publish` as a unit per shard, drops a deposed shard (`NotOwner`)
  before it widens scope or recovers, and the negative-control test
  (`falsifiability_external_execution_is_exactly_once_under_fix`) proves the
  fix order executes a contested shard EXACTLY once. **Do not touch this.** Adopting
  X's shards while X is *still counted* in the denominator is conservative — a
  larger denominator only makes quorum HARDER, never split-brain-able.
- **The membership eviction of X is a SEPARATE, later, quorum-agreed delta** (§2.3),
  gated on §3's debounce/confirmation, and it is the thing that finally SHRINKS the
  denominator. Shrinking is safe precisely because it goes through quorum on the
  *current* (pre-shrink) denominator: a minority cannot commit the shrink, so a
  minority can never shrink itself into a self-quorum.
- **Single-change invariant ties them together:** because eviction removes exactly
  one node and goes through quorum-on-the-old-denominator, and adoption fences per
  shard, there is no window where two disjoint majorities exist. A node that lost
  the membership-eviction CAS simply did not change the denominator; a node that
  lost a shard's acquire/publish fence simply dropped that shard. Neither can
  produce split-ownership.

**Net rule:** *placement re-election may precede membership change and fences on the
old (larger) denominator; membership change only ever shrinks the denominator via a
quorum write on the current denominator; both are single-writer-wins CAS.* The two
are independent precisely because adoption is conservative w.r.t. the denominator.

### 2.5 Join, leave, death — the three concrete flows

- **JOIN** (new node N): N boots with a seed (§4), dials a seed peer, syncs the
  store, then proposes `add N (status=Joining)` via §2.3. Once committed, N is in
  the denominator. Placement: N owns nothing until it wins shard elections (it can
  be assigned `owned_shards`, or adopt on a later death). Recommend a `Joining →
  Active` second delta once N has caught up its replica, so a not-yet-synced joiner
  does not inflate the denominator for writes it cannot yet ack. **Open: whether to
  fold catch-up into a single Active-on-join.** (Conservative: two deltas.)
- **LEAVE** (graceful drain of N): N (or an operator command) proposes
  `status(N)=Leaving`; N's shards are handed off (planned epoch-fenced handoff, the
  `PlannedHandoff` command shape already stubbed in `cluster_event.rs`); then a
  final `remove N` delta shrinks the denominator. Drain-before-shrink means no
  in-flight work is stranded.
- **DEATH** (kill -9 / partition of N): beamr sensor → debounce → §3 → a proposed
  `status(N)=Down` then `remove N` delta. Placement adoption of N's shards happens
  via the existing SS-5b path on the old denominator (§2.4); the membership shrink
  follows as the agreed eviction. A node that comes back is a JOIN.

---

## 3. The beamr-liveness → haematite-membership SEAM

### 3.1 Who consumes the signal

The consumer is the **cluster supervisor** (`crates/aion-server/src/cluster.rs`,
the SS-5b `ClusterSupervisor`) — it already owns the debounce state machine and the
poll loop. Two ways in, both already present:

- **Pull (today):** the supervisor polls `liveness.peer_connected(name)` every
  `poll_interval` and advances a per-peer `consecutive_down` counter.
- **Push (net-tick, recommended to ADD):** register a `ConnectionDownHook` on the
  beamr manager so a `HeartbeatTimeout` / read-EOF `ConnectionDownEvent` *wakes* the
  supervisor immediately rather than waiting for the next poll. The push is an
  **edge-trigger that schedules a confirmation pass**, never an immediate eviction;
  the poll loop remains the authority on the debounce count. (This keeps the
  silent-partition detection latency at ~`deadline` instead of `deadline +
  poll_interval`.)

### 3.2 Debounce / confirmation (no unilateral eviction)

The existing `SupervisorConfig { poll_interval, confirmations }`
(`failover_poll_interval_ms` default 500ms, `failover_confirmations` default 3)
already debounces: `confirmations` CONSECUTIVE down observations before action, any
reconnect resets the counter. This stays. The change is **what "action" means**:

- TODAY: confirmed-down → `adopt_shards` locally (placement only).
- UNDER THIS DESIGN: confirmed-down → (a) adopt shards locally as now (placement,
  fenced, conservative on the old denominator) AND (b) **propose** a
  `status(X)=Down` membership delta (§2.3). The propose is a quorum write — so even
  though ONE node observed X down and initiated it, the **eviction only commits if a
  majority of the current denominator agrees** (each survivor independently confirms
  X down before voting, OR the proposer's committed-on-quorum write simply requires
  a majority to apply it; recommend the latter as the v1 — a quorum write is itself
  the agreement, no separate vote protocol needed).
- This is the anti-unilateral property: **no single node evicts a peer.** A
  proposer in a minority partition CANNOT reach quorum to commit the eviction (Q3
  invariant), so a partitioned minority cannot evict the majority — it just fences
  itself out of writes, which is correct.

### 3.3 Debounce against false-positive eviction (the asymmetry)

Adoption is reversible-ish (a returning node re-elects); **eviction shrinks the
denominator and is heavier to undo** (the node must re-JOIN). RECOMMENDATION:
require a *stricter* confirmation for the membership-eviction delta than for shard
adoption — e.g. adoption at `confirmations` (3 ticks ≈ 1.5s), but eviction only
after a longer dead-interval (a separate `eviction_confirmations`, default larger,
or a wall-clock `member_down_grace`). A node that flaps and returns should re-elect
shards (cheap) without ever having been evicted from membership (expensive). Make
this a new tunable; do NOT overload `failover_confirmations`.

### 3.4 Reason-awareness

`ConnectionDownReason` lets the seam distinguish `ManualDisconnect` (a clean
LEAVE — go straight to graceful drain + remove) from `HeartbeatTimeout` /
`ReadError` / `PeerClosed` (a DEATH — debounce hard before eviction). RECOMMENDATION:
map `ManualDisconnect` → graceful LEAVE flow (§2.5), everything else → debounced
DEATH flow. Do not treat a `WriteTimeout` (wedged peer, kernel buffer full) as an
instant death — it is exactly the transient the debounce exists for.

---

## 4. The BOOTSTRAP-SEED model

### 4.1 The irreducible cold-start problem

You cannot WRITE the first `cluster/members` record by quorum until you can already
reach a quorum — and you cannot reach a quorum until you know SOME peer to dial.
This is not a haematite wart; it is intrinsic to consensus. Raft, etcd, Consul all
solve it with an **initial seed / bootstrap list**: a tiny static hint of *where to
find at least one peer*, used ONCE to form the cluster, after which membership is
dynamic.

### 4.2 RECOMMENDATION: seed-only static config, dynamic durable thereafter

Reduce `[store.cluster]` from "the full authoritative membership" to **"a seed
hint"**:

- **`node_id` + `bind_address`** stay (this node's own identity / listen addr —
  irreducibly local, cannot come from a store this node hasn't joined yet).
- **`seeds: [addr]`** (NEW) — a small list of peer replication addresses to dial on
  cold start. NOT the quorum denominator; just "ring these to find the cluster."
  One reachable seed is enough.
- **`members` / `peers` become OPTIONAL and, when present, are treated as the
  INITIAL membership ONLY for a fresh (never-formed) cluster** — the genesis write.
  On an already-formed cluster (a `cluster/members` record exists on disk/replica),
  the durable record WINS and static `members` is ignored (mirrors the existing
  "on-disk shard count wins over config `shard_count`" rule in
  `build_haematite_store`).

Cold-start sequence for a fresh cluster:
1. Each genesis node has the same `seeds` list (or mutual seeds).
2. One node (deterministically — lowest `node_id`, or an explicit
   `--bootstrap` flag) writes the **genesis `cluster/members`** record naming the
   genesis set. Until that record exists, this is a cluster of one bootstrapping
   itself (denominator 1, self-quorum — the existing "cluster of one" valid config
   in `ClusterConfig` docs).
3. Other genesis nodes dial a seed, observe the genesis record, and JOIN (§2.5).

After genesis, **all** membership is dynamic durable state. `seeds` is consulted
only on (re)boot to re-find the cluster — and a rejoining node can also be handed
the current membership directly once it reaches one live peer.

### 4.3 How discovery (#147) layers on top

`seeds` is the seam #147 plugs into: instead of a hand-maintained static seed list,
a discovery provider supplies seed addresses dynamically:

- **mDNS** — zero-config LAN discovery; nodes announce their `bind_address`, the
  seed list is populated from observed announcements.
- **Tailscale** — query the tailnet for peers tagged as cluster members; their
  100.x addresses are seeds.
- **gossip** — a SWIM-style membership gossip that *feeds candidate seeds* but does
  NOT replace the durable `cluster/members` authority (gossip is eventually-
  consistent liveness; haematite remains the strongly-consistent denominator).

The contract for #147: a `SeedProvider` trait returning candidate dial addresses;
haematite/membership stays the authority, discovery only automates *finding the
first seed*. This is why #146 unblocks #147 — #147 has a well-defined, narrow seam
to target instead of having to invent the whole membership model.

### 4.4 Migration path — from today's full static config to seed-only

1. **Additive seed field.** Add `seeds` to `ClusterConfig`; keep `members`/`peers`
   working unchanged (no break). A config with only `members` behaves exactly as
   today (static membership = genesis membership, never changes at runtime).
2. **Read membership from the durable record when present.** Wire
   `resolve_membership` (and the supervisor's `WatchedPeer` / directory build in
   `connect_haematite_store`) to read `cluster/members` off the replica, falling
   back to static `members` only when no durable record exists (fresh cluster).
   This is the load-bearing cutover; gate it behind a flag for the spike.
3. **Enable dynamic deltas.** Turn on the §2/§3 propose-on-confirmed-down and
   join/leave flows. Now `members` config is only the genesis seed.
4. **Deprecate static `members`/`peers`.** Document `seeds` as the supported path;
   keep the static list working for single-restart clusters and tests, but the
   durable record is authoritative everywhere it exists.

No flag-day: every step is additive and the static path keeps working until the
last step, satisfying the project's replay-/restart-safety discipline.

---

## 5. OPEN QUESTIONS, phased build plan, and how it folds in

### 5.1 Open questions (decision-forcing)

1. **Vote protocol for eviction, or quorum-write-IS-the-vote?** RECOMMENDED:
   quorum-write IS the agreement for v1 (no separate Raft-style vote RPC) — the
   `replicate_write` majority is the agreement. A separate per-survivor "I also see
   X down" confirmation is a *later* hardening if false-evictions show up in
   practice. Decide before SPIKE-2.
2. **Config epoch vs per-shard owner_epoch interaction.** The membership `epoch`
   and a shard's `owner_epoch` are distinct counters. OPEN: does a committed
   membership shrink need to force a re-fence of in-flight elections, or is "fence
   on the old denominator, shrink after" (§2.4) sufficient? Lean: sufficient,
   because a shrink only makes future quorums easier and adoption already fenced on
   the larger set. Prove it in the SPIKE-2 split-brain test.
3. **Joining-node catch-up gating.** One delta (`add Active`) or two
   (`add Joining` → catch up → `Active`)? Lean: two, so an un-synced joiner never
   inflates the write denominator before it can ack. Cost: one extra delta per join.
4. **Where the genesis writer is chosen.** Deterministic (lowest `node_id`) vs
   explicit `--bootstrap`. Lean: explicit `--bootstrap` flag for a fresh cluster
   (operator intent is unambiguous; avoids two nodes both writing genesis), with a
   deterministic fallback for fully-automated discovery (#147).
5. **Eviction reversibility / re-join storms.** A flapping node that gets evicted
   then re-joins repeatedly. Mitigated by §3.3's stricter eviction confirmation;
   OPEN whether to add a re-join backoff. Defer.
6. **Snapshotting the member set for a far-behind rejoiner.** If `cluster/members`
   has churned a lot, a long-dead node's replica may be stale. The existing store
   sync handles KV catch-up; confirm the member record rides that path. Likely
   free, verify in SPIKE-3.

### 5.2 Phased build plan (spike-first, smallest-first, full clippy bar, no shims)

- **SPIKE-0 (read-only, design-validation).** A standalone test (mirror
  `haematite/tests/spike_quorum.rs`) proving the split-brain property of a
  single-change membership delta: a minority partition CANNOT commit a denominator
  shrink. Pure quorum math + the existing `resolve_membership`. **Falsifiable
  control:** a "shrink-from-reachable" variant that DOES split-brain, proving the
  test detects the bug (same discipline as `engine/fence.rs`'s
  `plan_adopted_shards_prefix_buggy` negative control).
- **CSOT-1 — the durable member record + read path.** Define `cluster/members`
  keyspace + record type; write a genesis record; wire `resolve_membership` to read
  it off the replica with static-config fallback. No deltas yet. Gate behind a flag.
- **CSOT-2 — single-change delta CAS.** `propose_membership_delta` (add/remove/
  status, single-change invariant enforced) as a fenced quorum CAS on shard 0.
  Join/leave as explicit operations. Unit + a 3-node integration that adds and
  removes a member at runtime and asserts the denominator tracked it.
- **CSOT-3 — the beamr seam.** Register the `ConnectionDownHook`; on confirmed-down
  (with the stricter eviction confirmation, §3.3) propose `status=Down` → `remove`.
  Reuse the SS-5b debounce. Integration: kill -9 a node, assert the SURVIVORS'
  durable member set shrinks by exactly one AND a partitioned minority does NOT
  evict the majority. (Compose with the existing `lsub5b_osproc_kill9_failover`
  / `ss5_failover_demo` harnesses.)
- **CSOT-4 — seed-only bootstrap.** Add `seeds`; make durable record win over
  static `members`; the `SeedProvider` trait seam for #147. Migration steps §4.4.
- **CSOT-5 — joint consensus (deferred / #116).** Only if multi-node atomic
  reconfiguration is actually needed. Additive to the record `epoch`.

Each CSOT lands green on its own (the project's "land the safe part" rule): the
read path (CSOT-1) is inert and safe to ship before deltas (CSOT-2) exist.

### 5.3 How it folds into #116 and unblocks #147

- **#116 (fan-out / affinity maturity):** dynamic membership is the precondition
  for *elastic* fan-out — adding worker-capacity nodes at runtime instead of a
  config-edit-and-restart. The node-affinity third routing dimension
  ([NODE-AFFINITY-DESIGN.md](../NODE-AFFINITY-DESIGN.md)) gains runtime-mutable
  nodes; placement (already durable) + membership (now durable) together make the
  cluster topology a single coherent durable fact. Joint consensus (CSOT-5) is the
  #116 tail.
- **#147 (auto-discovery):** this design defines the exact narrow seam #147 targets
  — the `seeds` / `SeedProvider` boundary (§4.3). #147 supplies seed addresses
  (mDNS / Tailscale / gossip); haematite/membership remains the authority. Without
  #146, #147 would have nowhere to put what it discovers; with #146, discovery is a
  pluggable provider behind a one-method trait.

---

## Appendix — grounding (the code this design read)

- **Static membership today:** `crates/aion-server/src/config/mod.rs`
  (`ClusterConfig` — `node_id`, `bind_address`, `members`, `peers`,
  `failover_poll_interval_ms` default 500, `failover_confirmations` default 3;
  `validate_cluster`); built into `ClusterBootstrap` in
  `crates/aion-server/src/state.rs::build_haematite_store` and
  `connect_haematite_store`.
- **Quorum denominator:** `haematite/crates/haematite/src/sync/membership.rs`
  (`resolve_membership` → `WriteMembership.total_nodes` = full `config.nodes.len()`,
  NEVER reachable; the Q3 "never shrink the denominator" invariant);
  `sync/consistency.rs::quorum_size`.
- **Quorum write primitive:** `haematite/crates/haematite/src/db/receiver.rs::replicate_write`
  (peer-quorum BEFORE local commit); typed `DatabaseError::{Fenced, CasConflict, …}`
  in `db/error.rs`.
- **Placement already durable:** shard election
  `crates/aion-store-haematite/src/store.rs::acquire_owned_shard` →
  `Database::acquire_shard_and_serve`; SS-3 directory
  `publish_shard_owner` / `read_shard_owner` (fenced quorum KV, co-located on
  shard); residual-window `is_current_owner`.
- **The epoch-fence ordering invariant (do not regress):**
  `crates/aion/src/engine/fence.rs::plan_adopted_shards` (acquire→publish→extend per
  shard; the `plan_adopted_shards_prefix_buggy` negative control + the
  `falsifiability_external_execution_is_exactly_once_under_fix` test).
- **The liveness sensor:** `crates/aion-server/src/cluster.rs` (`ClusterSupervisor`,
  `PeerLiveness`, debounce); `aion_store_haematite::HaematiteStore::peer_connected`;
  beamr `crates/beamr/src/distribution/connection.rs`
  (`ConnectionDownReason::HeartbeatTimeout`, `ConnectionDownEvent`,
  `ConnectionDownHook`, `HeartbeatConfig` 15s tick / 45s deadline, the net-tick
  keepalive frame).
