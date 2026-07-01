<!-- STATUS: BUILT & LANDED (reconciled 2026-07-02). Phase 2 is implemented and wired
at boot. Verified: NamespacePlacement (Prefer/Pinned) reserved fields are now real
policy the dispatch path consults — PlacementCache + worker_selection_for + the
prefer-then-spill node-filter tiers ship in `aion-server/src/worker/placement_cache.rs`
and are consulted by BOTH transports (`worker/outbox_dispatcher.rs`,
`worker/liminal_transport.rs`); QuotaCache + Backpressure keyed admission ship in
`worker/quota_cache.rs` + `worker/backpressure.rs`. All are constructed at boot in
`aion-server/src/run.rs` (PlacementCache, QuotaCache, Backpressure). PLACEMENT SPILL IS
LIVE, not partial: `Prefer{L}` prefers labelled workers then spills to any live worker;
`Pinned{L}` routes to a NON-spilling Required decision that waits on absence. (NOTE: the
stale in-code comment `aion-store/src/namespace.rs:137` "nothing reads Prefer/Pinned yet"
is OUT OF DATE — the dispatch-time two-tier spill is wired; that comment is source, not
this doc, so left for a code pass.) Shard-count-default dependency RESOLVED (4096,
af4bad09). Original DRAFT text retained below as the design record.
     Prior header: "DRAFT design blueprint (2026-07-01) ... NOT yet approved to build". -->

# Control-Plane Phase 2 — Tenant Placement, Quotas, and the Isolation Ladder

> Implementation blueprint. Grounds every seam in verified source (file:line). Realises `CONTROL-PLANE.md` §4.2 (per-tenant quotas as keyed backpressure), §4.3 (logical isolation now, placement reserved day-one), and Pillar 3 §1.3 ("manage/scale/place compute, not just dispatch"). Builds on Phase 1's durable, minted-on-use `NamespaceRecord` (`NAMESPACE-REGISTRY-PHASE-1.md`), reusing its reserved `config` and `placement` fields so Phase 2 is a **policy flip, not a migration**.

---

## 1. Framing — what Phase 2 closes

Phase 1 made the namespace a **first-class durable record** (`NamespaceRecord { name, created_at, last_seen, origin, config, placement, state }`, `aion-store/src/namespace.rs`), minted-on-use, quorum-replicated, failover-survivable, listable. It deliberately shipped `config` and `placement` as **reserved, default-valued fields** (`NamespaceConfig::default()`, `NamespacePlacement::Unplaced`) precisely so Phase 2 could fill them without touching the record shape (`NAMESPACE-REGISTRY-PHASE-1.md` §2 table, §4.3 discipline). Phase 2 turns those two reserved fields into **real policy the dispatch path consults**, closing the two halves of the multi-tenant gap the strategy names: (a) **compute placement** — Pillar 3, the dimension *every* competitor lacks (Temporal/Inngest/Hatchet place no compute; only Restate has keyed single-writer placement and DBOS per-partition concurrency) — realised by promoting `placement` over the **already-shipped, already-replay-safe** `(namespace, task_queue, node)` routing axis; and (b) **per-tenant quotas as keyed backpressure** (§4.2) — smooth admission-with-delay over the outbox claim, never Temporal's `RESOURCE_EXHAUSTED` hard-fail. The load-bearing finding that makes this small: the hard machinery already exists. The `node` routing axis is built and tested (NODE-1..6); the outbox dispatcher already lives **outside the replay domain** and already supports scoped, headroom-capped claims (`claim_outbox_rows_scoped`, `outbox.rs:317`); the per-namespace in-flight gauge already exists (`metrics.rs:147`). **Phase 2 is overwhelmingly policy wiring over built mechanism, confined to the non-replayed dispatcher — single-node behaviour is byte-identical until a policy is set.**

---

## 2. PLACEMENT — namespace-default node policy over the existing `node` axis

### 2.1 The model — recommend soft `Prefer` default, hard `Pinned` opt-in

aion already routes `(namespace, task_queue, node)`, where `node` is a per-activity **optional within-pool locality filter**: authored as activity config (`resolve_node`, `nif_activity.rs:159-174` reads `config["node"]`), recorded into history in the same atomic batch as the scheduling event (`record_activity_scheduled_started(... node)`, `nif_activity.rs:252-272`; `fan_out.rs:67,129,146`), persisted on the outbox row (`OutboxRow.node`, `outbox.rs:160,181`), and consumed by the non-replayed dispatcher straight off the row (`outbox_dispatcher.rs:160`), matched at selection by `worker_matches_node` / `ClaimScope` (`registry.rs:805-810`, `outbox.rs:133-146`). So a per-*activity* require-this-node pin is **already built**. What is missing is the **namespace-level default/constraint** over that axis. Promote the reserved enum:

```rust
pub enum NamespacePlacement {
    Unplaced,                            // today's behaviour, unchanged default
    Prefer { nodes: BTreeSet<String> },  // SOFT: default node-label set, spill-on-absence
    Pinned { nodes: BTreeSet<String> },  // HARD: require label-set, wait-on-absence (opt-in isolation)
}
```

`nodes` are **free-form node labels** matched against the worker's advertised `WorkerHandle.node` (`registry.rs:159,187` — "a locality, not a process"). This reuses the within-pool node filter end-to-end: **no new routing key, no `PoolAddress` change** (it deliberately excludes `node`, keeping this a within-pool filter, `registry.rs:74-80`).

Rejecting the alternatives concretely: **dedicated worker pools** are already the `task_queue` axis (`PoolAddress`, `registry.rs:76-98`) — placement labels nodes, it does not mint pools. **Dedicated shard-range ownership** (binding a namespace's *data* to shards) is rejected here as a placement mechanism — it conflates compute with data placement and collides with immutable `shard_count`; it lives in the isolation ladder's L3 (§4), forward-only, not in the placement default.

### 2.2 Composition rule — per-activity pin always wins; placement fills the gap

**Effective node = compose(activity.node from history/row, namespace.placement):**

| activity `node` | `Unplaced` | `Prefer{L}` | `Pinned{L}` |
|---|---|---|---|
| `None` (unpinned) | any worker (today) | prefer L, **spill** to any | require L, **wait** |
| `Some(N)`, N ∈ L | require N | require N | require N |
| `Some(N)`, N ∉ L | require N | require N | **violation → reject at workflow-start admission** |

Two properties fall out: the **per-activity pin is authoritative when compatible** (placement only fills `None` or constrains the set, matching the existing "row's `node` is source of truth" model, `outbox_dispatcher.rs:144-160`); and the `Some(N ∉ L)` conflict under `Pinned` **must be caught at start-time admission, never at dispatch** — a dispatch-time rejection would be a non-deterministic, history-affecting decision (§2.4). `Prefer` is operationally forgiving (spills); `Pinned` rejects the *start* of a workflow whose authored activities would violate the pin — a yes/no gate before any history exists.

### 2.3 Where it applies in the real dispatch path

The single behavioural seam is `WorkerOutboxDispatch::to_scheduled` (`outbox_dispatcher.rs:151-168`) → `ActivityDispatcher::dispatch` (`dispatch.rs:98-135`). Today the dispatcher does one `workers_for(... activity.node)` and waits if empty (`dispatch.rs:113-135`). For `Prefer{L}` over an **unpinned** row, replace that with a **two-tier lookup**:

1. try `workers_for(ns, tq, type, node=label)` for labels in L (preference);
2. fall back to `workers_for(ns, tq, type, None)` (any live worker) if none on L are live.

This is a pure dispatch-time liveness/locality optimization — same category as the existing round-robin (`registry.rs:595-602`) and the LSUB-3 immediate re-claim (`outbox_dispatcher.rs:336-347`). **Critically, placement is read by the non-replayed dispatcher and never threaded into the recorded row** — the row's `node` stays exactly as authored (possibly `None`); placement is consulted *only* in worker-selection (`dispatch.rs:116`). The dispatcher reads `placement(ns)` via the Phase-1 `Arc<dyn NamespaceStore>::get_namespace` (`namespace.rs`), in-process cached with short TTL so the hot claim loop is not a per-sweep quorum read.

### 2.4 Replay-determinism — placement is a dispatch-time concern, outside history

**Claim: namespace placement cannot perturb any workflow's deterministic command stream, provided it is applied only in the non-replayed dispatcher.** Argument, grounded:

1. A command-stream mismatch raises `NonDeterminismError` and terminates the run (`CONTROL-PLANE.md` §5; `durability/resolver.rs`), so *anything affecting the recorded command stream must be history-sourced*, never live/ambient.
2. The activity's `node` is **already history-sourced** — authored config (`nif_activity.rs:159`), recorded at schedule time (`nif_activity.rs:259-272`), replayed identically (`ResolveOutcome::Recorded` short-circuits). The recorded row's `node` is immutable once written in the `append_with_outbox` batch.
3. `Prefer` acts **only in `ActivityDispatcher::dispatch`** (`dispatch.rs:116`), inside the `OutboxDispatcher` — a Tokio task explicitly *outside the deterministic replay domain* ("lives entirely OUTSIDE the deterministic replay domain… reads ONLY the outbox table — never workflow history", `outbox_dispatcher.rs:5-9`). It changes which *live worker* receives an already-recorded task. The workflow observes only the activity *result* (recorded once, replayed) — **different worker, identical recorded result, identical replay.**
4. Therefore `Prefer` is a pure liveness optimization on the dispatch leg; it touches nothing the workflow function reads.

**The one rule that keeps this safe: do NOT stamp namespace placement into the recorded row.** If placement ever influenced the `node` written at schedule time, that would be a live/ambient input to the recorded stream — replay-fatal exactly like #144's start-time queue. The row records only the workflow-authored `node`; placement is resolved fresh at dispatch. If audit later wants the default *materialized* into history, it MUST be sourced the #144 way (read once at workflow start, recorded as a start-time fact, replayed) — deferred from Phase 2. `Pinned` admission-time rejection is likewise replay-safe: a yes/no gate at *start*, before any history exists; a started workflow's authored nodes are already in history and never re-evaluated on replay.

### 2.5 Failover — soft spills for free, hard stalls by design

Placement rides the **unmodified** kill-9 substrate (#157/#108). When a placed node dies, two orthogonal axes (§4.3) act independently:

- **Data axis (unchanged):** the dead node's shard ranges are adopted by a survivor via epoch-fenced quorum election + union-merge handoff (`acquire_owned_shard`/`publish_shard_owner`, `store.rs:600,871`); its outbox rows become claimable by the adopter (`owned_shard_scope`, `outbox_dispatcher.rs:288`).
- **Compute axis (placement):** under `Prefer{L}`, if the L-labelled workers were co-located on the dead node, the §2.3 two-tier lookup **spills to any live worker** — compute fails over automatically; a preference is not a single point of failure. Under `Pinned{L}`, the row hits the existing **no-worker wait path** (`dispatch.rs:122-134`) and stays pending/retries (LSUB-3) until an L-labelled worker returns — the *correct, intended* semantics of a hard tenant pin (isolation > availability), identical to how a per-activity `Some(N)` pin behaves today.

**Nothing new is needed in the failover machinery** — the only Phase-2 addition is the `Prefer` two-tier spill in worker selection. This is the payoff of building placement as a dispatch-time policy over the existing `node` axis rather than as shard-pinning.

---

## 3. QUOTAS — per-tenant keyed backpressure over the outbox claim

### 3.1 What to limit — concurrent in-flight activities per namespace (the one dimension)

For an agent/LLM workload the scarce, slow, expensive resource is the **model/tool call**, and every one is an **activity** (`CONTROL-PLANE.md` §1.6, the activity boundary). So the meaningful pressure dimension is **concurrent in-flight activities per namespace** — not RPS, not start-rate, not queue depth. Dispatch/start-rate (Temporal's `namespaceRPS`) is the wrong unit for long-running work (a tenant at 4 starts/min can saturate 400 concurrent 90-second model calls — the exact `RESOURCE_EXHAUSTED` footgun §4.2 forbids). Queue depth is an *output* of backpressure, not a control input. Concurrent in-flight maps 1:1 onto Inngest's `concurrency: { key }`, caps *cost* directly, and is **already measured per-namespace** (`inflight_activities` `IntGaugeVec`, `metrics.rs:147-149`; incremented in `activity_dispatched`, decremented in `activity_completed`/`activity_abandoned`, `metrics.rs:141-180`).

**MVP limits exactly one thing:** `max_in_flight_activities` per namespace, enforced as **keyed backpressure** — at the ceiling, the dispatcher *holds the row pending* (does not claim it this sweep), it remains durable in the outbox, reconsidered next sweep. Nothing dropped, no `RESOURCE_EXHAUSTED` to the workflow. Reserve (config-only, unenforced) an optional `max_dispatch_rate` token-bucket for bursty-cheap tenants; do not build it (§5).

### 3.2 Where to enforce — the outbox dispatcher's claim

The seam is **`OutboxDispatcher::sweep_once` at the claim** (`outbox_dispatcher.rs:287-307`). Today it calls the unscoped `claim_outbox_rows(batch_size)` (`outbox_dispatcher.rs:297`). Backpressure is a **claim-shaping filter** there: read current per-namespace in-flight (§3.3), compute `headroom(ns) = quota(ns) − inflight(ns)`, and **claim at most `headroom(ns)` rows per namespace** via the existing `claim_outbox_rows_scoped(&ClaimScope, limit)` (`outbox.rs:317-321`) — the atomic single-writer claim semantics are byte-identical, only the `limit` becomes quota-derived. Rows over the ceiling are simply not claimed this sweep; they stay `Pending` and reconsidered on the poll/`wake` loop (`outbox_dispatcher.rs:254-282`).

This is correct for three load-bearing reasons: (1) **it cannot break exactly-once or the durable-outbox guarantees** — a claim returning fewer rows is already first-class (the whole `visible_after`/backoff machinery, `outbox_dispatcher.rs:382-398`, exists to defer claims; `dispatch_key` UNIQUE and `INSERT OR IGNORE`, `outbox.rs:11-12,281`, untouched); (2) **it is the single funnel for fan-out work** — every fan-out activity flows through the outbox (`nif_collect.rs:251-256`), so capping the claim caps real tenant concurrency in one place; (3) **the scoped-claim primitive already exists** (`outbox.rs:317`), Phase 1's `ClaimScope` already carries `(namespace, task_queue, node)`.

**Explicitly NOT worker-registration admission** (`registry.rs:323-355`) — that gates *capacity* (a worker serving five tenants) to throttle *one*, wrong granularity, and surfaces as a connection failure (the Temporal hard-fail shape). **Explicitly NOT the in-engine `spawn_completion_task` path** (`nif_activity_dispatch.rs:239-246`) — a non-fan-out activity records `ActivityScheduled`/`ActivityStarted` *synchronously in history* before async dispatch (`nif_activity_dispatch.rs:217-225`); delaying it would block the workflow process inside the replay domain. MVP scopes backpressure to the outbox/fan-out path (which dominates agent cost); see §3.4 and §5.

### 3.3 Durable + observable

**Config is durable, in the Phase-1 `NamespaceConfig` blob.** Add one field:

```rust
pub struct NamespaceConfig {
    pub kind: Option<String>,                 // Phase-1 reserved (tenant⊃namespace split)
    /// Phase 2: keyed-backpressure ceiling. None = generous platform default
    /// (NOT a low hard cap — CONTROL-PLANE §4.2). Some(n) = per-tenant override.
    pub max_in_flight_activities: Option<u32>,
}
```

**Additive serde** (an `Option`): old records decode with `None`, so it is a policy flip, not a migration — exactly what Phase 1 reserved `config` for. It is **failover-surviving for free**: the record travels with its shard's adoption/union-merge like all registry state (`NAMESPACE-REGISTRY-PHASE-1.md` §3, `store.rs:206`). The dispatcher reads `quota(ns)` via `get_namespace`, in-process cached with short TTL.

**Usage source = the DURABLE outbox table, NOT the gauge (CORRECTED by P2-Q0, §8).** The original draft proposed the `inflight_activities` gauge (`metrics.rs:147`) as the live count. **P2-Q0 verification proved that gauge is dead code** — its three mutators (`activity_dispatched`/`activity_completed`/`activity_abandoned`) have *zero* call sites on any path and never have in git history, so it is permanently `0`. The correct, restart-correct, single-writer source is the **durable outbox table**: a per-namespace count of `Pending`+`Claimed` rows (a new `count_inflight_outbox_rows(namespace)` mirroring the existing `claim_outbox_rows_scoped` scoping). Rows are already namespace-stamped (NSTQ-2), survive restart, and are correct under the stuck-`Claimed` case (`outbox_dispatcher.rs:317-327`) — unlike an in-memory per-process gauge. This is the "haematite state is the authority, not an in-memory sensor" direction. The dead gauge is tracked for wiring-or-removal separately (it covers fan-out only, which is exactly the path the fan-out quota targets).

**Observability over the existing socket channel.** Push quota state (`quota`, `inflight`, `headroom`, `throttled: bool`) as a new `ClusterEvent` variant on the **same deploy-scoped `ClusterEventPublisher`** the registry already emits `NamespaceCreated` on (`minter.rs:214-219`, `cluster_publisher.rs`). The ops-console namespace panel gets a live "throttled / N of M in flight" badge with zero new transport — the socket-first, real-data-only surface the dashboard mandate requires.

### 3.4 Replay-determinism — backpressure is invisible to replay

**Claim: delaying an outbox dispatch via the claim filter cannot perturb any workflow's command stream.** The engine has two phases split by the durable-outbox boundary:

- **Phase A (replay domain):** `record_fan_out_dispatch` *atomically* stages the `ActivityScheduled`/`ActivityStarted` events **and** the outbox rows in one batch (`fan_out.rs:152`, `nif_collect.rs:251-253`). The command stream — ordinals, scheduled events, `CorrelationKey::Activity(ordinal)` resolutions (`nif_activity_dispatch.rs:177,405-411`) — is fully determined *at staging time*, before the dispatcher runs.
- **Phase B (non-replay domain):** the `OutboxDispatcher` "reads ONLY the outbox table — never workflow history" (`outbox_dispatcher.rs:5-8`). Its **claim timing produces no events and writes nothing to history.** A row claimed now vs. three sweeps later yields the identical `ActivityCompleted` whenever the worker returns; the completion is recorded by the await path keyed on ordinal (`nif_activity_dispatch.rs:430-455`), order-independent of dispatch timing.

So backpressure only moves a completion **later in wall-clock**, never **earlier or to a different ordinal**. The engine is constitutionally latency-tolerant (durable await/suspend, `nif_activity_dispatch.rs:322-324`); no `now()`/`random()` is consumed by the claim. **The correctness keystone: enforce on the claim (Phase B), never on staging (Phase A).** This is the mirror image of the #144 lesson — #144 was dangerous because the start-time queue affected *recorded* content; backpressure lives strictly *after* the row is staged, so it has no command-stream surface. (The one path unsafe to delay is the in-engine `spawn_completion_task`, §3.2 — hence MVP scopes to fan-out.)

### 3.5 Fairness — round-robin the claim, never FIFO-drain one tenant

The noisy-neighbour problem the multi-tenant pitch rests on (`CONTROL-PLANE.md` §1.2) is solved at the *same* seam. Today one unscoped `claim_outbox_rows(batch_size)` could return `batch_size` rows all from one busy tenant, starving others. Replace it with **per-namespace scoped claims issued round-robin across namespaces with pending work**, each capped at `min(headroom(ns), fair_share)` where `fair_share = batch_size / active_namespaces`. The per-tenant `max_in_flight_activities` ceiling is the hard backstop (even a tenant winning every round-robin slot cannot exceed its quota); the round-robin guarantees a starving tenant a claim slot every sweep. This is Inngest's keyed flow-control + Hatchet's `GROUP_ROUND_ROBIN` in one seam.

### 3.6 Multi-node — cluster-wide CONTRACT, proportional enforcement (no per-node leak to the tenant)

**The professional bar: the tenant's contract is cluster-wide, never "per-node × N."** A tenant who sets `max_in_flight = 256` must get a meaningful cluster-wide guarantee, not a number silently multiplied by node count — exposing per-node math is the same class of leaky-abstraction footgun we sell against (Temporal's confusing per-namespace limits). So the **user-facing semantic is cluster-wide**; only the *enforcement* is decentralized.

**MVP enforcement = proportional share, derived from owned-shard fraction (faithful, no central bottleneck).** Each node enforces `ceil(quota(ns) × f)` where `f = |owned shards| / shard_count` — its proportional slice of the cluster-wide ceiling. This is faithful, not a guess: a namespace's outbox rows scatter by `dispatch_key` hash uniformly across shards, and each node claims only rows on shards it owns (the scoping lives in the store impl — `claim_outbox_rows` shard-filters at `store.rs:1521`, driven by `owned_shard_scope`, `store.rs:911`), so a node owning fraction `f` of shards naturally sees ~`f` of the tenant's rows and capping at `f × quota` sums to ≈`quota` cluster-wide — with **zero hot-path coordination** (each node already knows its owned shards and `shard_count`). A cluster-wide *exact* counter is deliberately rejected: it is the central bottleneck we sell against (Temporal's frontend RPS limiter). Generous defaults (§4.2) make the rounding over-admit slightly under shard skew rather than starve — the right failure direction. **Reserved follow-up (exactness, not contract change):** publish per-node in-flight into a `n:<ns>:usage` shard-local counter for an eventually-consistent aggregate read (no hot-path quorum) — tightens the approximation without changing the cluster-wide contract the tenant already sees; folds into #146. *(Per-node fraction enforcement reads the per-node `count_inflight_outbox_rows(namespace)` over owned shards — NOT the dead `inflight_activities` gauge; see the §8 `P2-Q0` resolution.)*

---

## 4. The ISOLATION ladder — logical → pool → node → data

Phase 1 made isolation **logical**: a namespace is an auth-checked correctness boundary at every hop — register (`registry.rs:335-346`), dispatch (the `(namespace, task_queue, node)` pool key, `registry.rs:76-111`), resolve. A workflow's activities cannot reach a worker outside its namespace (`same_task_queue_in_different_namespaces_is_isolated`, `registry.rs:1113-1162`). That boundary is real but **shared-fate on shared infrastructure**. Physical isolation adds **fault-domain + resource-domain separation underneath** it — and for three of four levels it is a **placement policy flip + drain, never a data migration**, because all three lower mechanisms already ship.

| Level | Guarantees | Mechanism (already shipped) | Migration cost |
|---|---|---|---|
| **L0 Shared-everything** (today) | Logical auth boundary; shared workers/nodes/shards | the Phase-1 `(ns,tq,node)` pool key | — (default, zero-config) |
| **L1 Dedicated pool** | Activities run only in workers serving *only* this namespace | `worker-serves-namespace-SET` (`registry.rs:155-176`) | policy flip + drain |
| **L2 Dedicated node(s)** | Compute pinned to specific nodes; co-tenant work never lands there | NODE within-pool filter (`worker_matches_node`, `registry.rs:805-810`) | policy flip + drain |
| **L3 Dedicated data** | Durable state lives on shards owned by tenant-dedicated nodes | shard-targeted minting (`mint_for_shard`, `store.rs:563-571`) | **forward-only**; bounded by immutable `shard_count` |

L0→L1→L2 is a monotone tightening of `NamespacePlacement`. L3 adds a data-locality guarantee and is where the architecture bites.

**L1 — dedicated pool → one admission predicate.** A worker is reachable in a namespace iff its `namespaces: BTreeSet` contains it (`registry.rs:1164-1215`). L1 = an admission rule: workers serving an isolated namespace advertise a **singleton set** `{tenant-a}`, and `mint_or_gate_namespaces` (`registry.rs:373-380`) — already the auth-scoped mint/gate hook — is extended to **place**: reject a registration whose set is not exactly `{that namespace}` when the namespace's placement is pool-dedicated. Routing is untouched (the pool is already keyed `(ns,tq,node)`; a singleton-namespace worker produces a pool no other tenant can key into).

**L2 — dedicated node(s) → namespace-level node invariant.** `NamespacePlacement::Pinned { nodes }` (§2.1) makes every dispatch in the namespace implicitly node-pinned. Two reads of `placement`: **admission** (extend `registry.rs:373-380` again) rejects a worker whose advertised `node` (`registry.rs:347`) is not in the set; **dispatch** passes `node = Some(...)` from placement instead of `None` (reusing the NODE filter, `registry.rs:639-664`) — no new routing structure. Replay-safe by §2.4 (affects worker selection, not recorded command order). **Forbid workflow-visible placement entirely in Phase 2** — placement is a control-plane/routing fact, never workflow-observable; a `current_node()` primitive would be a new non-determinism footgun (the #144 family) unless history-sourced like `now()` (`nif_determinism.rs`). Recommend L2 ⊇ L1 (a node-pinned namespace's workers are singleton-set by admission), composable but defaulting to imply pool-dedication.

**L3 — dedicated data → honest limits under immutable `shard_count`.** A workflow's *entire* durable state shards by its UUID (event stream `E || uuid`, `keyspace.rs:45-50`; timers keyed `workflow_id.to_string()`, `keyspace.rs:70-78`; both via `BLAKE3(key) % shard_count`, `router.rs:20-28`). The store exposes `mint_for_shard(target_shard, max_attempts)` (`store.rs:563-571`) — the R-4 primitive that rejection-samples ids until one lands on a chosen shard — and nodes own *sets* of shard indices (`owned_shards`, `store.rs:204`; `set_owned_shards`, `store.rs:412`). So `NamespacePlacement::ShardAffinity { shards: BTreeSet<usize> }` at **workflow start** mints the id onto a tenant-owned shard, co-locating all that workflow's state on the tenant's nodes. **Feasible today** for *new* workflows. **Hard limits (stated, not hand-waved):**

1. **You cannot carve private shards from thin air** — `shard_count` is fixed at `Database::create`, immutable (`store.rs:241-247`), no reshard path (`router.rs` only ever does `% handles.len()`). L3 = partitioning the *fixed* virtual shard space; the number of disjoint-shard tenants is **bounded by `shard_count`**. This makes the §6.1 generous-default decision **load-bearing** for L3.
2. **`mint_for_shard` is forward-only** — it steers *new* ids; it does nothing for state already on the "wrong" shard. Promoting a live L0 namespace to L3 steers only newly-started workflows; historical workflows stay scattered. **Scope decision: Phase 2 L3 is forward-only** — accept it and document it to tenants rather than build event-stream copy-migration (which would break id stability).
3. **Per-tenant replication confinement is NOT available** — `WriteMembership` quorums to the **full cluster membership** (`store.rs:115-131`), so a shard's replicas spread cluster-wide regardless of placement. Phase 2 L3 gives *primary placement*, not *replication confinement*. Be explicit with compliance tenants.

**Infeasible — do not design:** "spin up a private shard on demand," "resize a tenant's shard allocation," "split a hot tenant's shard" — all require in-place resharding haematite cannot do (the Temporal `numHistoryShards` trap, `CONTROL-PLANE.md` §6). The only honest L3 is assign-from-the-fixed-pool, choose `shard_count` generously up front, accept the tenant-count ceiling.

**The migration-free promise, proved.** L1/L2 touch only routing, never data — durable state location is a function of UUID + `shard_count` (`keyspace.rs:45-50`), neither changes when `placement` changes. Promotion L0→L1/L2 = `update NamespaceRecord.placement` (the same idempotent quorum CAS as every registry mutation) + bring up dedicated/pinned workers + drain the shared ones serving the namespace (`broadcast_drain`, `registry.rs:620-637`, already exists). In-flight workflows recover and re-dispatch onto the new pool — **no event is rewritten.** The one caveat is L3-on-existing-state (forward-only, above) — the single place the promise has an asterisk, which must be stated to the tenant.

**Security — defence-in-depth.** Logical isolation is the *correctness* boundary (must be bug-free); physical isolation is the *containment* boundary (bounds damage when correctness fails). Against the CVE-2025-14986 confused-deputy family: under L1/L2 a mis-routed cross-tenant dispatch finds an empty pool (`workers_for` returns empty, `registry.rs:592`) and **fails closed**, rather than executing in a process that also holds the other tenant's secrets — blast radius collapses from "cross-tenant code execution" to "a failed dispatch." This is exactly the multi-tenant story Temporal's own docs concede they don't have (`CONTROL-PLANE.md` §1.2).

---

## 5. Competitive steals / avoids (sourced)

| System | STEAL | AVOID |
|---|---|---|
| **Temporal** | Fairness-key virtual queues with **weighted** round-robin (per-tenant limit scaled by weight) — [task-queue-priority-fairness](https://docs.temporal.io/develop/task-queue-priority-fairness) | Per-namespace RPS as a hard global 429 (`ResourceExhausted`); `frontend.rps=2400` per-instance default that users raise 32× — [cloud/limits](https://docs.temporal.io/cloud/limits), [constants.go](https://github.com/temporalio/temporal/blob/main/common/dynamicconfig/constants.go) |
| **Inngest** | **Declarative keyed flow-control** `{limit, key, scope}` — per-tenant virtual concurrency queues, multiple composable limits per fn, GCRA throttle with per-tenant key, **delay/enqueue not drop** — [concurrency](https://www.inngest.com/docs/guides/concurrency), [throttling](https://www.inngest.com/docs/guides/throttling) | Rate-limiting default is **lossy** (drops events past the limit) — unacceptable for durable agent work; make lossy strictly opt-in — [rate-limiting](https://www.inngest.com/docs/guides/rate-limiting) |
| **Restate** | **Keyed single-writer placement** — a tenant key deterministically pins to one partition leader with epoch-fenced failover; maps 1:1 onto our shard/owner model — [architecture](https://docs.restate.dev/references/architecture) | Partition count fixed at cluster creation (no split/migration); **no per-tenant quota dimension** — don't conflate single-writer-per-key with fairness |
| **DBOS** | **Partitioned queues** — concurrency limit applies *per partition key* ("concurrency=1 per tenant"), all OSS — [queue-tutorial](https://docs.dbos.dev/python/tutorials/queue-tutorial) | Concurrency/rate is global-per-queue **unless** you opt into partitioning — silent noisy-neighbor; make per-tenant the default posture |
| **Hatchet** | **`GROUP_ROUND_ROBIN`** fairness across tenant groups + runtime CEL-keyed dynamic rate limits — [round-robin](https://docs.hatchet.run/home/features/concurrency/round-robin), [rate-limits](https://docs.hatchet.run/home/rate-limits) | Rate-limited steps are **re-queued/retried** (busy-poll pressure) not admitted — prefer GCRA smooth admission |

**Cross-cutting:** (1) **Placement is the unclaimed quadrant** — only Restate and DBOS have any real per-tenant placement primitive; Temporal/Inngest/Hatchet have none, confirming the control-plane thesis. Our shard/owner+epoch-fence model already matches Restate's; Phase 2 adds the quota dimension on top. (2) **The universal footgun is hard-rejection vs back-pressure** — Temporal 429s, Inngest drops, Hatchet re-queue-spins. The clean model is **admission-with-delay + per-tenant virtual queues** (§3), quota expressed as fairness, never as an opaque global ceiling.

---

## 6. Slice pipeline (ordered, PR-sized, byte-identical-on-default)

Each slice is inert until a policy is set (placement `Unplaced`, quota `None`), so single-node behaviour is unchanged through the whole pipeline until P2-Q3 / P2-P3 flip a tenant's policy on.

| # | Slice | Touches | Gate (independently verifiable) |
|---|---|---|---|
| **P2-P1** | Promote `NamespacePlacement` enum: add `Prefer { nodes }` + `Pinned { nodes }` (keep `Unplaced` default); extend the tagged-string serde | `aion-store/src/namespace.rs:115-121,211-235` | Unit: encode/decode round-trips all 3 variants; old `"unplaced"` bytes decode byte-identical (back-compat). No behaviour change. |
| **P2-Q1** | Add `max_in_flight_activities: Option<u32>` to `NamespaceConfig` + serde + generous platform default under `[namespaces]` | `aion-store/src/namespace.rs:99`; `aion-server/src/config/mod.rs` (sibling to `auto_create`) | Unit: additive serde, old records decode `None`; default present. No behaviour change. |
| **P2-P2** | `PUT /namespaces/{name}/placement` (operator/grant-scoped) → idempotent quorum CAS on `placement` (`register_namespace_record` pattern); placement-changed delta on the existing socket publisher | `aion-server/src/api/…`, `aion-store-haematite/src/store.rs:670+`, `minter.rs:214` | API: set/read-back via `GET /namespaces`; auth-scoped; emits delta. |
| **P2-P3** | Two-tier `Prefer` spill in dispatch: dispatcher reads `placement(ns)`, for an **unpinned** row tries preferred labels then falls back to any worker; assert the row's `node` is never mutated | `aion-server/src/worker/outbox_dispatcher.rs:151-168`, `dispatch.rs:113-135` (inject `Arc<dyn NamespaceStore>`) | Integration: unpinned row in `Prefer{n1}` → n1 worker when present, spills to n2 when absent. **Replay test:** identical history regardless of which node served it. **Determinism gate:** row `node` == authored `node`. |
| **P2-Q2** | Replace the unscoped claim with a **per-namespace, round-robin, headroom-capped** `claim_outbox_rows_scoped` loop; headroom from the `inflight_activities` gauge vs cached `quota(ns)`; per-node-share semantics, documented | `aion-server/src/worker/outbox_dispatcher.rs:287-307` (the one real change); `metrics.rs:147` (read gauge); `outbox.rs:317` (reused unchanged) | Integration: (a) tenant at ceiling holds rows `Pending`, never `Failed`/dropped, dispatches when headroom returns; (b) two tenants, one bursty — quiet one gets claims every sweep (fairness); (c) **replay-stability:** heavily-throttled fan-out → byte-identical history vs un-throttled. |
| **P2-Q3** | Quota-state `ClusterEvent` variant on the existing publisher → ops-console live "throttled / N of M" badge | `aion-core/src/cluster_event.rs`, `minter.rs:214`, ops-console panel | Manual: badge updates live when a tenant hits its ceiling; matches gauge. |
| **P2-I1** | Placement-admission at register: extend `mint_or_gate_namespaces` to mint+gate+**place** — reject a worker whose namespace-set / advertised `node` violates an L1/L2 namespace's placement (auth-scoped by construction) | `aion-server/src/worker/registry.rs:373-380` | Integration: singleton-set enforced for L1; node-set enforced for L2; reject is loud; non-isolated namespaces unaffected. |
| **P2-I2** | Ops-console isolation surface: show each namespace's level (L0–L2); operator promotes L0→L1→L2; panel shows the drain step (`broadcast_drain`) | ops-console; reuse `registry.rs:620-637` | Manual: promote a namespace, watch shared workers drain + dedicated pool take over, in-flight work recovers. |
| **P2-L3** | *(deferred — gated on §6.1 shard-count default)* `ShardAffinity { shards }` + `mint_for_shard` at start; forward-only, documented caveat | `namespace.rs`, `aion-store-haematite/src/store.rs:563-571`, start path | Integration: new workflow in a `ShardAffinity{0..7}` ns lands on those shards; existing state untouched (forward-only asserted). **Hard-blocked on shard_count>1.** |

**Dependency chain:** `P2-P1 → P2-P2 → P2-P3` (placement) and `P2-Q1 → P2-Q2 → P2-Q3` (quotas) run in parallel after their respective record-field slices; `P2-I1 → P2-I2` (isolation admission) depends on `P2-P1` (needs `Pinned`); `P2-L3` is deferred behind the shard-count decision. **First demoable value:** P2-P1→P2-P3 — a tenant labels two nodes, their unpinned work prefers those nodes, kill one node → work spills to the other and completes, visible on the ops console, riding the Sydney-demo failover substrate.

---

## 7. Open decisions for Tom + risks

**Decisions (foundational forks — recommendation given):**

1. **Soft-default vs hard-default for a placed namespace.** Recommend **`Prefer` (spill, high-availability) as the default**, `Pinned` opt-in. The alternative — placement means *isolation by default* (`Pinned`) — buys a stronger tenant-isolation pitch but accepts the stall-on-node-loss behaviour as its price. This is the core product-semantics fork. *Recommend Prefer-default; reserve Pinned for the explicit isolation tier (L2).*

2. **`Pinned` conflict handling: reject-at-start vs spill-with-warning.** When a workflow authors `node: Some(N)` but the namespace is `Pinned{L}` with N ∉ L — hard-reject the start (strict, surfaces misconfig early) or honour the activity pin and warn (forgiving). *Lean reject-at-start for the isolation story; it's a UX call.*

3. **Quota contract = cluster-wide, enforcement = proportional (REVISED — §3.6).** The tenant's `max_in_flight` is a **cluster-wide** number (never "per-node × N" — exposing per-node math is a leaky-abstraction footgun, the class of thing we outclass Temporal on). Each node enforces `ceil(quota × owned-shard-fraction)`, which sums to ≈quota cluster-wide with no central counter and no hot-path coordination (rows scatter by `dispatch_key` hash; a node sees its proportional slice). Exact cluster-wide accounting (eventually-consistent registry aggregate) is a *precision* follow-up, not a contract change. *No remaining fork here — this is the best move; the only open number is the default ceiling (decision 4).*

4. **Default ceiling value.** §4.2 demands "generous." A concrete number (e.g. 256 or 1024 concurrent activities/namespace) interacts with the raised shard-count default (#47) since fan-out fsync scales with concurrency. *Pick a generous power-of-two, validate against #47.*

5. **Forward-only L3 + workflow-visible placement forbidden.** Confirm (a) promoting a live namespace to `ShardAffinity` steers only *new* workflows (historical state stays scattered — no event-stream copy-migration), and (b) placement is control-plane-only, **never** workflow-observable (no `current_node()`), so we never mint a new non-determinism footgun. *Recommend both: accept forward-only, lock placement-is-not-workflow-visible.*

6. **Shard-count default number (cross-cutting, still open from Phase 1).** The L3 tenant-count ceiling **equals** `shard_count` (§4). If multi-tenant physical-data isolation is a near-term selling point, the default must be generous enough that "dedicated shards per tenant" is viable for a realistic tenant count. *Same decision Phase 1 deferred; L3 makes it load-bearing. Validate against perf audit #47.*

7. **Throttle visibility to the tenant.** Expose "you are being throttled" to the *client* (a status field) or keep it operator-only on the ops console? Surfacing risks clients treating soft backpressure as an error. *Lean operator-only in MVP; client-facing later behind an explicit field.*

**Risks:**

- **Placement read on the hot dispatch path.** The dispatcher must read `placement(ns)`/`quota(ns)` without a per-sweep quorum read — mitigated by in-process short-TTL cache on `get_namespace`. A stale cache under `Prefer` only mis-prefers a worker (self-correcting next sweep); under `Pinned`/quota it could briefly over- or under-admit. Test cache staleness explicitly.
- **Gauge drift as quota source.** `inflight_activities` is a Prometheus gauge that can drift on a crash (dispatched-but-never-completed before restart). Self-heals via `activity_abandoned` (`metrics.rs:175`); tracker-derived crash-exact count is a follow-up. Accept gauge for MVP.
- **Per-node-share surprise.** MVP ships a *per-node* quota; an operator setting `max_in_flight=256` on a 4-node cluster effectively permits ~1024 cluster-wide. Document the per-node semantic loudly so we never silently ship a surprising global cap.
- **L1/L2 admission strictness.** Rejecting a worker that mixes an isolated namespace with others (vs partial-admit) is the recommended clean choice but splits one registration's fate loudly. Confirm reject-whole-registration (recommended) vs admit-for-non-isolated-only.
- **Replay-safety regressions.** The entire design rests on §2.4 + §3.4 — *nothing* that affects the recorded command stream may read live/ambient placement or quota state. The determinism gates (P2-P3, P2-Q2 replay tests asserting byte-identical history) are non-negotiable CI guards; any future "materialize placement into history for audit" work MUST go the #144 history-sourced route, never live.

---

**Files Phase 2 touches (consolidated):** `crates/aion-store/src/namespace.rs:99,115-121,211-235` (extend `NamespacePlacement` + `NamespaceConfig` + serde), `crates/aion-server/src/worker/outbox_dispatcher.rs:151-168,287-307` (placement spill + quota claim — the two real behavioural changes), `crates/aion-server/src/worker/dispatch.rs:113-135` (two-tier worker selection), `crates/aion-server/src/worker/registry.rs:373-380` (placement-admission for L1/L2), `crates/aion-server/src/config/mod.rs` (default ceiling under `[namespaces]`), `crates/aion-server/src/observability/metrics.rs:147` (read the gauge), `crates/aion-core/src/cluster_event.rs` (quota-state variant), `crates/aion-server/src/namespace/minter.rs:214` (reuse publisher), `crates/aion-store-haematite/src/store.rs:563-571,670+` (`mint_for_shard` for deferred L3, placement CAS), new `PUT /namespaces/{name}/placement` handler + route, and the ops-console placement/quota panels. **Reused unchanged:** the per-activity `node` plumbing (`nif_activity.rs:159`, `fan_out.rs`, `OutboxRow.node`/`ClaimScope`, `worker_matches_node`), `claim_outbox_rows_scoped` (`outbox.rs:317`), the failover substrate (#157), and the entire engine/replay path — which is the whole point: Phase 2 is policy over already-built, already-replay-safe mechanism.

---

## 8. Adversarial critique verdict + required pre-build fixes (2026-07-01)

This blueprint was put through an independent adversarial feasibility review against the real code. Verdicts: **determinism = SOUND** (every load-bearing seam verified — placement and claim-shaping provably stay in the non-replayed dispatcher and cannot perturb a workflow's recorded command stream; `NonDeterminismError` is command-stream-only, `resolver.rs:52,91-113`; backpressure shapes the *claim* (Phase B), never the staged command order in `record_fan_out_dispatch`'s atomic `append_with_outbox`, `fan_out.rs:152`). **Feasibility = FEASIBLE-WITH-FIXES.** The core thesis survives scrutiny; the following must be folded into the slices before building.

**P2-Q0 (RESOLVED 2026-07-01 — gate cleared, design corrected).** The prerequisite was: prove `inflight_activities` is incremented **and decremented** for outbox-dispatched (fan-out) activities before it can be the quota source. **Verdict: the gauge is DEAD CODE on every path** — its three mutators (`activity_dispatched`/`activity_completed`/`activity_abandoned`, `metrics.rs:141-180`) have *zero* call sites anywhere and never have in git history; the real metrics seam `InstrumentedEventStore::record_events` (`instrumented_store.rs:59-97`) falls through `_ => {}` for every `Activity*` event. It is permanently `0`. Shipping gauge-as-source = a silent no-op quota; "fixing" it with only the increment = the monotonic-climb wedge. **RESOLUTION (folded into §3.3):** the quota source is the **durable outbox-row count** (`Pending`+`Claimed` per namespace) — restart-correct, single-writer, namespace-stamped — NOT the gauge. **New prerequisite slice before P2-Q2:** add `count_inflight_outbox_rows(namespace)` over the outbox table (mirroring `claim_outbox_rows_scoped` scoping), verified against the stuck-`Claimed` (`outbox_dispatcher.rs:317-327`) and restart cases. The dead gauge/counters are tracked as **#162** (wire into `InstrumentedEventStore` for real per-namespace in-flight observability across both paths, or delete) — an inert, plausible-looking `inflight_activities` in the tree is the exact hazard that produced this design mistake.

**Required slice corrections:**
- **P2-Q2 is not a one-line swap.** `claim_outbox_rows_scoped` (`store.rs:1568-1626`) requires a *specific* `ClaimScope { namespace, task_queue, node }` per call — it cannot "claim across all namespaces, round-robin." Per-namespace round-robin needs a **pending-namespaces enumeration primitive that does not exist today** (`sweep_once` currently does one unscoped `claim_outbox_rows`, `outbox_dispatcher.rs:287-307`). Add that probe (or a per-registered-(ns,tq) loop) as real work in P2-Q2; do not frame it as "the one change."
- **P2-I1 must move to `accept_registration`, not `mint_or_gate_namespaces`.** The mint hook (`registry.rs:373-380`) receives only the namespace *list*; L1 (singleton-set) needs the full advertised set and L2 needs the worker's advertised `node`, which is resolved in `accept_registration` at line 347 — *after* the mint hook (line 346). Put the placement-admission predicate in `accept_registration` with `node`/set in scope; leave mint auth-scoping unchanged.
- **P2-P3 `Prefer` spill semantics:** `worker_matches_node` (`registry.rs:805-810`) returns false for an *unlabelled* worker (`node=None`) when the filter is `Some(label)`, so tier-1 reaches only exactly-labelled workers and tier-2 (`None`) spills to **any worker including unlabelled** on the very first node-loss. Correct, but document this explicitly (operators may expect label-locality) and add the unlabelled-only-survivor test.

**Citation corrections (apply when implementing — anchors above are off):** `owned_shard_scope` is `aion-store-haematite/src/store.rs:911` (shard-scoping happens inside the store impl at `store.rs:1521`, not in `outbox_dispatcher.rs:288`, which is only a comment); the `BLAKE3 % shard_count` router is `haematite/crates/haematite/src/shard/router.rs:21-27` (**cross-repo**, in haematite, not aion); `OutboxRow.node` is at `outbox.rs:185` (not 181); `shard_count` immutability is by *absence of any reshard path*, not a guard at `store.rs:241-247` (that is `Database::create`).

**Added considerations (carry into the slices):** (1) `mark_done`-fails-leaves-row-`Claimed` (`outbox_dispatcher.rs:317-327`) means "in-flight via gauge" and "in-flight via Claimed rows" can diverge — pick one notion and stick to it. (2) `Pinned{L}` + per-node quota under failover (nodes die → no-worker wait *and* rows stay claimed on the adopter) is an un-modelled combined state — spec it. (3) The default-ceiling × raised-shard-count fsync coupling (Open Decision 4/6) is unquantified — add a guard against an operator setting a ceiling that overwhelms the shard-count default.

**Bottom line (reviewer):** safe to build from after P2-Q0 (gauge-wiring proof), the P2-Q2 re-scope, and the P2-I1 re-target. The determinism foundation is genuinely solid; the L3 "dedicated data" section is the most honest part (forward-only, bounded by immutable `shard_count`, correctly deferred). The quota path's dependency on gauge accuracy over an unbuilt completion path is the riskiest part and is now gated by P2-Q0.
