# Storage-swap maturity — haematite as a first-class DISTRIBUTED aion backend

> Status: **design + decomposition, read-only analysis.** 2026-06-27. No production
> code changed by this doc.
>
> Scope: making haematite a fully first-class **distributed** backend for aion —
> fan-out / fan-in + worker affinity — not just a co-located store. This is the
> storage-maturity counterpart to the dispatch transport swap
> ([LIMINAL-SWAP-DESIGN.md](./LIMINAL-SWAP-DESIGN.md), "#13"). Where #13 swaps the
> *wire that carries a dispatch*, this doc matures the *store that owns a workflow's
> state across nodes* and the *placement/affinity* that binds a worker to the node
> owning that state.
>
> Sibling designs this builds ON (do not re-derive them here):
> - [MULTI-SHARD-ACTIVE-ACTIVE-DESIGN.md](./MULTI-SHARD-ACTIVE-ACTIVE-DESIGN.md) — the
>   AA-4-x series, **largely LANDED** (see §"Already done").
> - [AION-DISTRIBUTION-DESIGN.md](./AION-DISTRIBUTION-DESIGN.md) — the H1/H2 fan-out +
>   active-active foundation, the locked decisions, the storage two-tier split.
> - [ROUTING-MODEL.md](./ROUTING-MODEL.md) — namespace ⊃ task-queue ⊃ node, the three
>   routing dimensions; "node" (Tier 3) is physical worker affinity.
> - [LIMINAL-SWAP-DESIGN.md](./LIMINAL-SWAP-DESIGN.md) — #13, the dispatch transport swap.

## TL;DR (read this first)

- **Most of the storage substrate is already built and verified.** haematite has
  quorum-replicated batch append (`replicate_append`), per-shard epoch-fence ownership
  (`Ballot`/`promised`/`owner_epoch`), Paxos-style shard election (`run_prepare_round`),
  routing-key co-location (`put_routed`/`shard_for`), and owned-shard-scoped enumeration
  (`scan_sequence_keys_for_shards`). The aion adapter (`aion-store-haematite`) already
  wires **all of these**: `set_owned_shards`, `owned_shard_scope`, routed writes,
  `replicate_append`, cross-shard fan-out scans. The 3-node/3-shard active-active
  **failover demo is green** (`aion_active_active_showcase.rs`, ship gate AA-4-6).
- **The gap to "first-class distributed backend" is NOT in the store — it is in the
  ENGINE/SERVER LIFECYCLE and PLACEMENT.** Every piece of cluster orchestration today
  lives in **test harness code**, not in the production boot path:
  - `aion-server`'s `run.rs` has **zero** shard / membership / ownership / election
    awareness — it boots a single-node engine.
  - `set_owned_shards` and `acquire_shard_and_serve` are only ever called from tests
    (`aion_active_active_showcase.rs:456,462,524,527`), never from `EngineBuilder::build`
    or `run.rs`.
  - There is **no placement API**: `Engine::start_workflow` "mints its own id and can't
    be steered" to a shard (showcase ACT 2, line 26) — you cannot direct a workflow to a
    shard/node by a routing key at the engine boundary.
  - Failover is **manual engine rebuild** in the test (ACT 4); there is no cluster
    supervisor that detects a dead node, re-elects its shards, extends the owned set, and
    re-residents workflows in production.
- **Worker AFFINITY does not exist at all yet.** Storage ownership (which node owns a
  workflow's *state*) and worker placement (which node runs its *activities*) are
  unrelated today. The outbox dispatcher picks "whatever worker is on my gRPC stream"
  (ROUTING-MODEL §2/§3). Fan-out has no notion of "dispatch this member to the node that
  owns the workflow's shard." Affinity = binding worker placement to the fenced shard
  ownership map — and that map is not even materialised cluster-wide yet.
- **This composes with #13, it does not depend on it.** #13 swaps the dispatch wire
  (gRPC → liminal) behind the `OutboxRowDispatch` trait. Affinity routing decides *which
  channel/node* a dispatch targets; #13 decides *how the bytes travel*. Affinity can ship
  on the existing gRPC transport (route by namespace/shard to a specific worker stream)
  and later ride liminal's channel addressing when #13 lands. **Recommend: build the
  storage/placement maturity on the transport you have, keep the affinity decision a
  layer ABOVE the transport trait, so #13 is a drop-in underneath.**
- **The single biggest risk:** the active-active correctness foundation that the failover
  demo *appears* to prove is **not yet safe for production run-history under partition**.
  Per the spike findings in AION-DISTRIBUTION-DESIGN §H2 (E3, verified): event-stream sync
  silently LWW-drops divergent writes, and quorum is **not wired to the write path**
  (`put_with_ttl_and_consistency` counts only the local ack). `replicate_append` is the
  newer quorum path, but the non-LWW union-merge keyspace for run history (§H2b step 2b)
  and full-membership-fed quorum (§2a) are the real prerequisites. **Maturity here means
  finishing the consensus foundation, not just wiring lifecycle.** Do not let a green
  in-process demo mask that.

---

## 1. Current-state map

### 1.1 aion's store abstraction (the traits + their assumptions)

All trait definitions live in `crates/aion-store/src/`. They are deliberately
**single-writer, CAS-guarded, polling-based, and shard-agnostic at the signature level**.

| Trait | file:line | Load-bearing methods | Assumption |
|---|---|---|---|
| `ReadableEventStore` | `store.rs:55` | `read_history` (`:62`), `read_history_from` (`:85`, O(delta) resume), `read_run_chain` (`:92`), `list_active` (`:102`), `query` (`:105`), `schedule_timer` (`:111`), `expired_timers` (`:119`) | History returned in ascending seq order; `expired_timers` pollable at any instant, no per-worker partition in the signature |
| `WritableEventStore` | `store.rs:127` | `append(token, wf, events, expected_seq)` (`:135`, CAS), `append_with_outbox(...)` (`:168`, **multi-key atomic**), `rearm_outbox_pending` (`:214`), `settle_outbox_row_cancelled` (`:233`) | **Single-writer** via compile-time `WriteToken`; `SequenceConflict` if head ≠ expected_seq; `append_with_outbox` must commit events + outbox rows atomically |
| `OutboxStore` | `outbox.rs:156` | `append_outbox_batch` (`:167`), `claim_outbox_rows(limit)` (`:179`), `rearm_stale_claimed_outbox_rows` (`:197`), `complete_outbox_row` (`:212`), `retry_outbox_row` (`:222`), `fail_outbox_row` (`:236`) | **Pull / polling** model; `claim_outbox_rows` is single-writer isolated (no two dispatchers claim the same row); all terminal ops idempotent by `dispatch_key` |
| `VisibilityStore` | `visibility.rs:14` | `record_visibility` (`:23`), `list_workflows` (`:33`), `count_workflows` (`:43`) | Separate from `EventStore` (split persistence allowed); often in-memory |
| `PackageStore` | `package.rs:50` | `put_package`, `list_packages`, `delete_package`, `put_package_route`, `list_package_routes` | **Cluster-wide, not per-workflow** — node-local replicated, not sharded |
| `EventStore` (blanket) | `store.rs:246` | `Readable + Writable + Package` | convenience super-trait |

`dispatch_key = "{workflow_id}:{ordinal}"` (`outbox.rs:110`) — the idempotency key. It
**names its owning workflow**, which is the invariant co-location depends on
(MULTI-SHARD design §"Outbox dispatch_key must name its workflow").

**What aion assumes about the store (precise):**
1. **Single-writer per workflow.** Only the Recorder appends, guarded by `WriteToken`
   and `expected_seq` CAS. This is the dedup chokepoint
   (`durability/recorder/fan_out.rs:217` `record_fan_out_completion` drops a second
   terminal for an already-resolved ordinal).
2. **Multi-key atomicity for fan-out staging.** `append_with_outbox` must commit the
   `N×(ActivityScheduled+ActivityStarted)` events **and** the N outbox rows together —
   an observer must never see an outbox row without its scheduling events.
3. **Polling, not push.** The `OutboxDispatcher` calls `claim_outbox_rows(batch_size)` on
   an interval (`outbox_dispatcher.rs:178+`); the timer service calls `expired_timers`
   globally. **Neither carries a node/shard argument in the trait** — scoping is a
   side-channel on the impl (see 1.2).
4. **No node-id / partition / affinity in the trait surface.** The traits are
   single-node-shaped. Distribution is entirely a property of the *implementation*.

### 1.2 What the haematite adapter already provides (the distributed surface, wired)

`crates/aion-store-haematite/src/store.rs` is **well past** the co-located v1 the
MULTI-SHARD doc set out to build — the AA-4-x series has landed:

- **Two modes.** `create`/`open` → single-node local commits (`distribution=None`).
  `with_distribution` (`store.rs:176`) → event appends route through
  `Database::replicate_append` to a quorum (`store.rs:622`); timers/packages/outbox stay
  node-local and are rebuilt from replicated history on a failover survivor.
- **Owned-shard scoping** (AA-4-3b): `set_owned_shards` (`store.rs:217`), `own_all_shards`,
  `owned_shards` (`store.rs:237`), `owned_shard_scope` (`store.rs:248`). Every global
  enumeration consults it: `list_active` (`store.rs:1023`), `expired_timers`
  (`store.rs:1098`), `claim_outbox_rows` (`store.rs:792`), `list_workflow_ids`.
  `None` = own all shards (== single-node, byte-identical to libsql behaviour).
- **Routing-key co-location** (AA-4-1/4-3a): per-workflow records (timers, outbox rows)
  written via `put_routed(route_key = event_stream_key(workflow_id), ...)` so a
  workflow's entire durable state lands on `shard_for(event_stream_key(wf))` — the same
  shard as its event stream (`store.rs:662`). The `w:` workflow-id index was dropped;
  workflows are discovered by walking `E||*` event-stream keys.
- **Cross-shard fan-out scan**: `scan_prefix_scoped` (`store.rs:414`) iterates
  `range_per_shard` over owned shards and merges; `scan_sequence_keys_for_shards`
  (haematite `db.rs:407`) enumerates only named shards.
- **`run_off_runtime`** (`store.rs:597`): runs the blocking distribution coordinator
  (`replicate_append`, election) off the tokio runtime thread — the
  `TransportBlockingFromAsync` constraint.

**haematite primitives the adapter sits on (all LANDED, beamr-wired, verified):**

| Primitive | haematite file:line | What it gives a distributed caller |
|---|---|---|
| `replicate_append(stream_key, payloads, expected_seq, membership, timeout)` | `db/receiver.rs:278` | Atomic batch append + quorum replication, one stamp per batch, OCC on `expected_seq`, all-or-nothing. **Wired over beamr.** |
| `replicate_write` / `replicate_delete` | `db/receiver.rs:84,169` | Single-key CAS replication over beamr |
| `Ballot { counter, node }` | `sync/ballot.rs:19` | Globally-unique per-shard epoch; lexicographic order |
| `promised` / `owner_epoch` / `persisted_max_minted` | `shard/actor.rs:38` | Actor-local durable ownership state; WAL-recovered (no TOCTOU) |
| epoch fence | `shard/actor.rs:607` | Rejects a write whose `epoch < promised` with `CasError::Fenced` |
| `run_prepare_round(shard, ballot, self_promise, membership, timeout)` | `sync/endpoint.rs:709` | Paxos-style shard election; returns quorum of `Promise`s or `ElectionError::Lost{highest_seen}` |
| `record_promise` | `shard/actor.rs:995` | Durable WAL fsync of a promised ballot |
| `acquire_shard_and_serve(shard, membership, timeout)` | `db/receiver.rs:806` | Election + `become_live` union-merge; after it returns, every committed write on that shard is locally present (recovery-safe) |
| `shard_for(key)` / `shard_count()` | `db.rs:101`, `db.rs:431` | Deterministic `blake3(key) % N` routing; whole-key hash (no prefix routing) |
| `scan_sequence_keys_for_shards(ids)` | `db.rs:407` | Scoped per-shard enumeration |

**Absent in haematite (caller's responsibility):** no in-band shard→node assignment map
(haematite stores `owner_epoch` but not "node X owns {1,3,5}"); no automatic failover; no
sync scheduler/topology (DIST-003 deferred); history pull/push logic exists
(`sync/pull.rs`, `sync/push.rs`) but the catch-up responder is only partially wired.

### 1.3 What "co-located" means today (the production reality)

Despite the adapter's distributed surface, **the production path is single-node**:

- `aion-server/src/run.rs` boots one engine, one store, **no** `set_owned_shards`, **no**
  `acquire_shard_and_serve`, no membership, no election. (Grep: `run.rs` has zero
  `shard`/`cluster`/`owned`/`acquire`/`membership` references.)
- `EngineBuilder` (`crates/aion/src/engine/builder.rs`) has exactly **one** shard-aware
  hook: `bootstrap_schedule_coordinator(bool)` (AA-4-4 gate, `:373`) so non-owners don't
  fence the coordinator stream. It has **no** owned-shard set, no election step, no
  node id.
- All active-active behaviour is **test orchestration**: `aion_active_active_showcase.rs`
  drives `acquire_shard_and_serve` (`:456,524`), `set_owned_shards` (`:462,527`), and on
  failover **rebuilds a fresh engine** over the absorber's store (`:532`). Documented
  honest gaps in that file: packages are node-local (built into every node, lines 46-48);
  the coordinator is single-owner-bootstrapped; failover is "rebuild the engine."
- **Placement is unsteerable**: `Engine::start_workflow` mints its own id (showcase ACT 2,
  line 26) — the test forces a workflow onto a shard with a deterministic store-append +
  rejection-sampled id, a back door, not an API.

So "co-located today" = **one node owns everything**; the store *can* scope to owned
shards, but nothing in production ever tells it to, nothing elects shards in production,
nothing places a workflow by key, and nothing fails over without a human/test rebuilding
the engine. The fan-out dispatcher (`outbox_dispatcher.rs:178`) picks any connected
worker — **storage ownership and worker placement are fully decoupled**.

### 1.4 Fan-out / fan-in today

- **Fan-out (staging) is real and durable**: `record_fan_out_dispatch`
  (`fan_out.rs:87`) atomically records N event-pairs + N outbox rows;
  `collect_step`/`dispatch_unscheduled` (`runtime/nif_collect.rs:98,211`) pin a contiguous
  ordinal range at first arrival (`PendingAwait::Collect`).
- **Fan-in (collection) is real and arrival-order-independent**: completions append under
  the dispatch-pinned ordinal; `record_fan_out_completion` (`fan_out.rs:217`) is the
  single-writer dedup chokepoint. SDK primitives `collect_all`/`collect_map`/`collect_race`
  exist (AT-010/AT-012), and a data-pipeline fan-out/fan-in example ships (DX-023).
- **But fan-out is single-node-shaped**: the N members all stage on the owning workflow's
  shard (correct), and all dispatch through one `OutboxDispatcher` to whatever workers are
  connected to *that node's* gRPC. There is no "dispatch member k to node owning shard
  for member k's affinity key," no cross-node result ingestion beyond the existing
  `OutboxDeliveryCallback`, and no worker-pool-per-shard.

### 1.5 The #13 dispatch swap, in one line

#13 (LIMINAL-SWAP-DESIGN.md) replaces the `OutboxRowDispatch` impl
(`outbox_dispatcher.rs:104`) — gRPC → liminal `publish`, `dispatch_key` → liminal
idempotency key — behind a feature flag, with results returning via the existing
`OutboxDeliveryCallback` (`bridge.rs:95`). A **13-0 spike is landed and green**
(`worker/liminal_transport.rs`, `tests/liminal_outbox_spike.rs`) but uses liminal's echo
participant, not a real remote worker; real cross-node receive/reply (13-L0/L1) is the
unbuilt long pole. **#13 is about the wire; this doc is about the store + placement.**

---

## 2. Target: haematite as a first-class distributed aion backend

### 2.1 Definitions (precise)

- **Shard** — a haematite partition; `shard_for(key) = blake3(key) % shard_count`. A
  workflow's *entire* durable state co-locates on `shard_for(event_stream_key(wf))`
  (already true via routed writes).
- **Shard ownership** — exactly one node holds the current epoch (`promised`/`owner_epoch`)
  for a shard, enforced by the epoch fence. **This already exists per-shard in haematite.**
- **Shard→node directory** — the cluster-visible map "shard S → owner node N @ epoch E."
  haematite does *not* materialise this; the caller (aion) must. (AION-DISTRIBUTION §H2b:
  a CAS-versioned, quorum-backed assignment map.) **This is a target, not done.**
- **Worker affinity** — the binding of *activity execution placement* to *shard ownership*:
  a workflow's activities are dispatched to a worker pool **co-resident with (or addressed
  to) the node that owns the workflow's shard**, so that (a) the dispatcher reads/claims
  outbox rows it owns, and (b) physical-affinity work (Tier-3 node, ROUTING-MODEL §1) lands
  on the device holding the workflow's external state. Affinity is **bound to the fenced
  ownership map, never the raw hash** (AION-DISTRIBUTION §Affinity) — a bare consistent
  hash is unstable across join/leave.
- **Routing key → shard → node** — the chain: `workflow_id` → `event_stream_key(wf)` →
  `shard_for(...)` (haematite, done) → owner node (via the shard→node directory, target).

### 2.2 The target picture

```
   start_workflow(routing_key)            <- NEW: caller can steer placement
        |
   shard = shard_for(event_stream_key)    <- haematite (done)
        |
   owner_node = directory[shard]          <- NEW: shard->node directory (quorum-backed)
        |
   if owner_node != self: forward start to owner   <- NEW: NotOwner forwarding (AA-4-5, deferred today)
        |
   engine on owner node:
     - owns shard S (set_owned_shards, acquire_shard_and_serve)   <- done in store/haematite, NOT wired to boot
     - claims ONLY its shards' outbox rows                        <- adapter scoping (done)
     - fan-out members stage on S (routed write)                  <- done
        |
   OutboxDispatcher (owner node) dispatches each member
        |
   AFFINITY: target the worker pool for (namespace, task_queue [, node])   <- NEW
        |
   OutboxRowDispatch  --(gRPC today / liminal after #13)-->  worker pool
        |
   result -> OutboxDeliveryCallback -> Recorder (single-writer dedup, done)
        |
   on node death: cluster supervisor re-elects dead node's shards,         <- NEW (today: test rebuilds engine)
     extends owned set, re-residents workflows, rebuilds local outbox
```

What changes from today: the **NEW** rows. Everything else is landed.

### 2.3 How fan-out / fan-in works across nodes (target)

- **Fan-out**: A workflow on shard S stages N outbox rows on S (already co-located). The
  owner node's `OutboxDispatcher` claims them (owned-shard scoped, done). For each member,
  affinity selects a worker pool: by default `(namespace, task_queue)` (ROUTING-MODEL
  Tier 2); for physical-affinity activities, the node owning S (or the node holding the
  external state, Tier 3). Dispatch goes over the current transport.
- **Fan-in**: Results return via `OutboxDeliveryCallback` to the **owner node's** Recorder,
  which appends under the dispatch-pinned ordinal with single-writer dedup (done). Because
  the workflow's state is owned by exactly one node (epoch fence), there is one
  authoritative collector — **consistency for fan-in is "single-owner serialised,"** not a
  distributed merge. A completion arriving at a non-owner must be forwarded to the owner
  (or rejected so the dispatcher retries to the owner) — the same `NotOwner` mechanism.
- **Cross-shard fan-out** (members whose *own* affinity key lands on a different shard,
  e.g. child workflows): each child is its own workflow on its own shard with its own
  owner; the parent collects child completions as signals/child-completion events under its
  pinned ordinals. **No cross-shard atomicity** (MULTI-SHARD honest gap) — model
  cross-workflow effects as sagas.

### 2.4 Consistency model

- **Control plane** (shard→node directory, epochs, membership): strong / quorum-backed,
  CAS-versioned (AION-DISTRIBUTION two-tier storage). Small, low-write.
- **Data plane** (run-history): writes are **quorum-acked via `replicate_append`** so a
  committed event survives owner death. (AION-DISTRIBUTION locked decision: run-history is
  Strong by default.) **This is the hard part still owed** — see §5 risk.
- **Fan-in**: single-owner serialised (the epoch-fenced owner is the only writer). Not
  eventual, not multi-writer-merge.

---

## 3. KEY DECISIONS for Tom (each with a recommendation)

### (a) Does affinity routing REUSE haematite's shard ownership/epoch-fence, or a separate layer?
**Recommendation: REUSE — affinity is bound to the fenced shard-ownership map, with a thin
shard→node directory aion owns on top.** haematite already gives per-shard fenced ownership
(`run_prepare_round`/`promised`/`owner_epoch`) and deterministic `shard_for`. Building a
second placement hash would (i) duplicate the routing math, (ii) drift from the fence under
join/leave (a bare hash is unstable, AION-DISTRIBUTION §Affinity). The only NEW piece is the
**cluster-visible shard→node directory** (which node currently owns shard S at epoch E) —
keep it a small quorum-backed/CAS-versioned record (AION-DISTRIBUTION §H2b), self-correcting
via the fence (a stale entry causes a `Fenced`/`NotOwner` → re-resolve). Do **not** invent a
parallel affinity registry; affinity = "send work to the owner of the workflow's shard,"
plus the Tier-2 task-queue and Tier-3 node dimensions layered as worker-pool selection.

### (b) Does fan-out dispatch ride liminal (depends on #13) or stay independent?
**Recommendation: INDEPENDENT — build affinity ABOVE the `OutboxRowDispatch` trait; let #13
swap the wire underneath.** Affinity is a *targeting* decision (which pool/node); the
transport is *delivery*. Keep affinity in the dispatcher's row→target mapping, expressed as
`(namespace, task_queue, node?)`, and let `WorkerOutboxDispatch` (gRPC) consume it today and
`LiminalOutboxDispatch` (liminal) consume the same target after #13. This means storage
maturity is **not blocked on liminal's unfinished wire** (the 13-L0/L1 long pole). When #13
lands, affinity's `(namespace, task_queue)` target becomes a liminal channel name and `node`
becomes liminal's global-name addressing — a drop-in. (Note: #13's 13-3 already plans the
`namespace`/`task_queue` → channel mapping; align with it so the two efforts converge rather
than collide on the outbox schema.)

### (c) Co-location vs true distribution — where is the boundary for v1?
**Recommendation: ship "single-owner-per-workflow, cross-node workers + failover" first;
defer true multi-owner / cross-shard work.** The honest, achievable maturity step is: every
workflow has exactly one owner node (its shard's owner); that node owns its state, claims its
outbox, collects its fan-in; workers may be remote; node death re-homes the dead node's shards
to a survivor automatically (in production, not a test). This is exactly what the AA-4-x +
showcase already prove *in a test* — maturity = **moving that lifecycle into the production
boot/supervisor path**. Defer: cross-shard atomic effects (sagas instead), dynamic resharding
(non-goal), partitioned-subset storage modes.

### (d) Consistency model for fan-in.
**Recommendation: single-owner serialised, quorum-acked writes — NOT eventual, NOT
multi-writer merge.** The epoch-fenced owner is the sole Recorder for a workflow; fan-in
completions append through it (existing single-writer dedup, `fan_out.rs:217`). Late/duplicate
completions are dropped by ordinal dedup; completions arriving at a non-owner are forwarded or
rejected-for-retry to the owner. Run-history writes go through `replicate_append` to a quorum
so a committed completion survives owner death. **Reject** the tempting "let any node collect
and merge later" — the spike proved event-stream LWW merge silently drops data (§5).

### (e) Where does the shard→node directory live, and how is failover triggered in production?
**Recommendation: a quorum-backed CAS-versioned directory owned by a single liminal
global-name coordinator (the Akka ShardCoordinator role, AION-DISTRIBUTION §H2b), with a
cluster supervisor on each node that (1) acquires its assigned shards on boot via
`acquire_shard_and_serve`, (2) calls `set_owned_shards`, (3) on a membership-loss trigger
re-resolves the directory and `acquire_shard_and_serve`s any orphaned shards, extends its
owned set, and re-residents those workflows.** The TCP-liveness diff is only a *trigger to
re-resolve*, never ground truth (AION-DISTRIBUTION §H2b). v1 can use **static assignment** at
cluster formation (MULTI-SHARD §"Per-shard ownership (static v1)") and require callers to
target the owner / retry on fence — defer the dynamic balancer and `NotOwner` forwarding
(AA-4-5).

### (f) Do the store traits need a shard/affinity argument?
**Recommendation: NO new trait arguments for enumeration; YES a small placement API on the
engine.** Keep `claim_outbox_rows`/`expired_timers`/`list_active` shard-agnostic — owned-shard
scoping as an impl side-channel (`set_owned_shards`) is already correct and keeps libsql
trivial. The NEW surface is at the **engine**: a `start_workflow` variant accepting a routing
key (so placement is steerable, closing the "can't be steered" gap), and an `owned_shards`
hook on `EngineBuilder` feeding `set_owned_shards`. This keeps the storage trait clean and
puts distribution where it belongs — the engine/server lifecycle.

---

## 4. Decomposition — spike-first, smallest-first (SS-x)

Numbering: **SS-x = aion storage-maturity increment.** Each is independently verifiable;
the gate test is named. "Already done" prerequisites are cited, not rebuilt. Dependencies on
#13 and on the unbuilt haematite consensus foundation (AION-DISTRIBUTION §2a/2b) are explicit.

> **Spike gate first (SS-0).** Before any lifecycle wiring, prove the one thing a green
> in-process demo does NOT prove.

### SS-0 — SPIKE: is `replicate_append` run-history safe under a real partition?
- **Goal:** empirically confirm whether the *current* `replicate_append` quorum path
  (`db/receiver.rs:278`) preserves run history under a partition + heal, or whether it still
  hits the §H2 E3 silent-LWW-drop on the event keyspace. The failover showcase does NOT test
  a partition with concurrent divergent writes — it kills a node cleanly.
- **Method:** extend a haematite spike (sibling to `tests/spike_fencing.rs`): 2 partitioned
  owners of the same shard each `replicate_append` to the same stream; heal; assert no
  committed event vanishes and the seq counter is not LWW-corrupted. Adversarial review.
- **Verify:** either (green) `replicate_append` + epoch fence already prevents divergent
  same-stream commits (because only the fenced owner can append) — in which case the
  union-merge keyspace (§H2b 2b) is **not** needed for the single-owner model and SS can
  proceed on the lifecycle track; or (red) it still drops — in which case SS is **blocked**
  on building the non-LWW union keyspace + full-membership quorum first.
- **Risk:** HIGH information value. **This decides whether storage maturity is "wire the
  lifecycle" (weeks) or "finish consensus first" (the §2a/2b months).**
- **Depends on:** nothing. Do this first.

### SS-1 — owned-shards on `EngineBuilder` + production boot (no election yet)
- **Goal:** `EngineBuilder::owned_shards(set)` that calls `store.set_owned_shards`, and
  `run.rs` reads a static shard-assignment config and passes it through. Single-node default
  = own all shards (byte-identical to today).
- **Seam:** `EngineBuilder` (`builder.rs`), `run.rs` boot; `HaematiteStore::set_owned_shards`
  (`store.rs:217`, done). No election — assignment is static config.
- **Verify:** aion integration test: boot two engines over two stores with disjoint static
  shard sets; each recovers/enumerates only its shards (mirror `scoping.rs`, but from the
  **production builder**, not a test back door).
- **Risk:** LOW. Pure lifecycle wiring of an existing store capability.
- **Depends on:** SS-0 green (else blocked on consensus).

### SS-2 — shard election into the boot path (`acquire_shard_and_serve` in the builder)
- **Goal:** on boot, for each assigned shard, `EngineBuilder::build` runs
  `acquire_shard_and_serve` (via `run_off_runtime`) BEFORE recovery, so the node is the
  fenced owner of its shards and `become_live` has union-merged state. Gate
  `bootstrap_schedule_coordinator` on coordinator-shard ownership (AA-4-4, done — just feed
  it from real ownership).
- **Seam:** `builder.rs` build sequence; `acquire_shard_and_serve` (`db/receiver.rs:806`,
  done); membership from static config.
- **Verify:** 3-node in-process cluster booting from the **production builder** (not the
  showcase harness): each owns its shard, recovers only its workflows, exactly one
  coordinator bootstrapped cluster-wide. (Promote the showcase ACT 1 into a builder API.)
- **Risk:** MEDIUM — moves blocking election into the engine lifecycle; the `run_off_runtime`
  constraint must hold from the builder.
- **Depends on:** SS-1.

### SS-3 — shard→node directory + steerable placement
- **Goal:** (a) a quorum-backed/CAS-versioned shard→node directory record (static-seeded in
  v1); (b) a `start_workflow` variant taking a routing key so a workflow is placed on
  `shard_for(routing_key)` deterministically (close the "can't be steered" gap, showcase
  line 26); (c) `StoreError::NotOwner(shard)` surfaced from the fence so a mis-targeted
  start/append is distinguishable.
- **Seam:** engine `start_workflow` API; directory record in the store (control plane);
  fence → `NotOwner` mapping in the adapter (`CasError::Fenced` → `StoreError::NotOwner`).
- **Verify:** test: start a workflow with a routing key that hashes to shard S; assert it is
  owned by S's owner; starting it against a non-owner returns `NotOwner`.
- **Risk:** MEDIUM. Directory consistency model must match the §2a quorum decision.
- **Depends on:** SS-2. (v1 may require caller targets the owner; AA-4-5 forwarding deferred.)

### SS-4 — worker affinity: dispatch targets the owner's pool by (namespace, task_queue)
- **Goal:** the `OutboxDispatcher`'s row→target mapping selects a worker pool keyed by
  `(namespace, task_queue)` (ROUTING-MODEL Tier 2), with the *node* dimension defaulting to
  the shard owner. Affinity is expressed ABOVE the `OutboxRowDispatch` trait so it is
  transport-agnostic (decision (b)).
- **Seam:** `outbox_dispatcher.rs` target derivation; registry key
  `(namespace, task_queue, activity_type)` (ROUTING-MODEL Tier 2 — coordinate, this overlaps
  #13's 13-3 and ROUTING-MODEL's Tier-2 brief; **do it once, shared**). Likely adds a
  `namespace` field to `OutboxRow` (the first schema add, same one 13-3 plans).
- **Verify:** test: two namespaces, two pools; a `remote` workflow's fan-out members only
  ever reach `remote` workers (makes the ROUTING-MODEL §2 misrouting bug structurally
  impossible).
- **Risk:** MEDIUM. Schema add; overlaps two other tracks — sequence/share deliberately.
- **Depends on:** SS-3. **Couple with ROUTING-MODEL Tier 2 and #13's 13-3.**

### SS-5 — automatic failover in production (cluster supervisor)
- **Goal:** a per-node supervisor: on a membership-loss trigger (TCP-liveness diff →
  re-resolve directory), `acquire_shard_and_serve` any orphaned shards, extend the owned set,
  re-resident their workflows (`Engine::recover_acquired_shard`), and rebuild the node-local
  outbox from replicated history (`rearm_outbox_pending`, done). Replaces the test's manual
  engine rebuild (showcase ACT 4).
- **Seam:** NEW supervisor in `aion-server`; `recover_acquired_shard` (design AA-4-4, verify
  it exists or build it); membership/liveness source (beamr `connected_nodes`).
- **Verify:** 3-node cluster from the production path; kill a node; a survivor auto-absorbs
  its shard and drives its in-flight workflow to completion with no duplicate
  `WorkflowStarted`, while a third node is provably uninterrupted (promote the showcase gate
  into a production-path test).
- **Risk:** MEDIUM-HIGH — failure-detector quality, flapping, double-acquire races (the fence
  makes double-acquire *safe* but the supervisor must not thrash).
- **Depends on:** SS-3 (directory). Independent of #13.

### SS-6 — fan-in correctness across nodes (forward/reject completions to the owner)
- **Goal:** a completion arriving at a non-owner (e.g. after a re-home) is forwarded to (or
  rejected-for-retry toward) the current shard owner, so the single-writer Recorder on the
  owner remains the sole collector. Run gate (`run_id`, OBX-011, done) holds across re-home.
- **Seam:** `OutboxDeliveryCallback` path (`bridge.rs:95`); `NotOwner` handling on the
  delivery side; existing run-scope gates.
- **Verify:** test: re-home a workflow mid-fan-out; a completion targeted at the old owner is
  routed to the new owner; exactly one terminal per member; continue-as-new safety preserved.
- **Risk:** MEDIUM. Interacts with #13 (a liminal completion source must also honour owner
  forwarding) — keep the forwarding logic transport-agnostic.
- **Depends on:** SS-5.

### SS-7 — packages cluster-wide consistency (remove the node-local gap)
- **Goal:** address the honest gap (showcase lines 46-48; MULTI-SHARD §Packages): packages
  are built into every node today. Make package deploy a coordinator-serialised,
  cluster-replicated operation so a re-homed workflow finds its code on the absorbing node
  without a manual build.
- **Seam:** `PackageStore` (`package.rs`); deploy path; coordinator serialisation.
- **Verify:** test: deploy a package on one node; re-home a workflow of that type to a node
  that never built it; it runs.
- **Risk:** MEDIUM. Package distribution is its own sub-problem (content-addressed transfer).
- **Depends on:** SS-5. Lower priority — the demo works around it; production needs it.

### SS-8 — ride #13's liminal transport for cross-node dispatch (CONVERGENCE)
- **Goal:** with #13 landed (13-L0/L1/13-1+), affinity targets (SS-4) become liminal channel
  names and the `node` dimension becomes liminal global-name addressing; `LiminalOutboxDispatch`
  consumes the same target SS-4 produces. Storage maturity + transport maturity meet.
- **Seam:** `LiminalOutboxDispatch` (`worker/liminal_transport.rs`, spike done); SS-4 target;
  liminal global-name registry for node affinity.
- **Verify:** real two-node cross-machine fan-out: a `remote` workflow's members run on a
  remote worker via liminal, results collected on the owner, survives a node kill.
- **Risk:** MEDIUM — inherits #13's risks (liminal wire). Gated on #13.
- **Depends on:** SS-4, SS-6, and #13 (13-L0/L1/13-1, 13-3).

**Ordering:** SS-0 (decides the track) → SS-1 → SS-2 → SS-3 → {SS-4, SS-5} → SS-6 → SS-7,
with SS-8 converging after #13. SS-4 couples with ROUTING-MODEL Tier 2 and #13's 13-3.

---

## 5. Risks / prerequisites / open questions

### Single biggest risk
**The active-active correctness foundation is not yet proven safe for production run-history
under partition, despite a green failover demo.** AION-DISTRIBUTION §H2 spike (verified):
event-stream sync silently LWW-drops divergent writes (E2), and local cas is insufficient for
active-active (E3) — two partitioned owners can both bump and the split-brain hides as silent
event loss. `replicate_append` is the newer quorum path and the epoch fence *should* mean only
one owner ever appends to a stream — **but that exact claim is unverified under a real
partition** (the demo kills cleanly). **SS-0 exists to settle this.** If SS-0 is red, storage
maturity is blocked on building the non-LWW union-merge keyspace (§H2b 2b) and
full-membership-fed quorum (§2a, "weeks, net-new") *before* any lifecycle wiring. Do not let
the green in-process demo create false confidence.

### Prerequisites (gating)
1. **SS-0 outcome** — decides whether SS is lifecycle wiring or consensus-first.
2. **beamr single-version alignment** across haematite/liminal/aion (the "inbreeding" failure
   mode) — required before aion links liminal (SS-8 / #13), not for SS-1..SS-7.
3. **Quorum on the write path** (AION-DISTRIBUTION §2a) — if SS-0 shows run history needs it.
   `replicate_append` already takes a membership/quorum; verify it is fed **full live
   membership**, not a static list (a minority must fail `QuorumUnavailable`, not self-quorum).
4. **`recover_acquired_shard`** must exist on `Engine` for SS-5 (design AA-4-4 references it;
   verify/build).

### Open questions (decide while building)
- **Directory consistency**: is the shard→node directory a quorum-backed record, a liminal
  global-name singleton, or both (AION-DISTRIBUTION §H2b says singleton coordinator)? Affects
  SS-3.
- **Static vs dynamic assignment for v1**: MULTI-SHARD recommends static at formation;
  `NotOwner` forwarding (AA-4-5) and a balancer deferred. Confirm v1 is static.
- **Node-affinity (Tier 3) mechanism**: ROUTING-MODEL Tier 3 (a workflow reopens on the device
  holding its files) has no transport primitive yet beyond liminal's global-name idea
  (LIMINAL-SWAP §6). Is node affinity in scope for this maturity pass, or deferred to a
  Tier-3 brief? (Recommend: SS-4 carries the `node` dimension as data; SS-8 makes it
  addressable; the *reopen* policy is the separate WORKFLOW-REOPEN/L3 track.)
- **Schema timing**: the `namespace`/`task_queue` outbox column is needed by SS-4, #13's 13-3,
  and ROUTING-MODEL Tier 2 simultaneously — coordinate the single schema change across all
  three so it is added once.

### Already done — do NOT rebuild (cite, don't re-derive)
- haematite: `replicate_append` (quorum batch append), `Ballot`/`promised`/`owner_epoch`
  (epoch fence), `run_prepare_round` (shard election), `acquire_shard_and_serve` (election +
  become_live union-merge), `shard_for`/`shard_count`, `scan_sequence_keys_for_shards`,
  routing-key co-location. **All wired over beamr, verified.**
- aion adapter (`aion-store-haematite`): `set_owned_shards`/`owned_shard_scope`, routed
  per-workflow writes, owned-shard-scoped `claim_outbox_rows`/`expired_timers`/`list_active`,
  `with_distribution` → `replicate_append`, `run_off_runtime`, cross-shard fan-out scan. (The
  AA-4-x series landed.)
- aion engine: `bootstrap_schedule_coordinator` gate (AA-4-4); durable fan-out
  (`record_fan_out_dispatch`) + single-writer dedup fan-in (`record_fan_out_completion`);
  `run_id`-on-the-wire continue-as-new gate (OBX-011); SDK `collect_all`/`collect_map`/
  `collect_race` (AT-010/012).
- The 3-node/3-shard active-active **failover demo is green** (`aion_active_active_showcase.rs`,
  AA-4-6) — but as a **test harness**, which is exactly the thing SS-1..SS-5 move into
  production.
- #13: the `OutboxRowDispatch`/`OutboxDeliveryCallback` seams and the 13-0 liminal spike.

---

## 6. Relationship to #13 (one paragraph)

#13 (LIMINAL-SWAP) and storage-swap maturity are **orthogonal layers that meet at the
`OutboxRowDispatch` trait**. #13 changes *how a dispatch travels* (gRPC → liminal). This doc
changes *who owns a workflow's state across nodes* and *which node/pool a dispatch is aimed
at* (placement + affinity + failover lifecycle). The deliberate design choice (decision (b))
is to keep **affinity targeting ABOVE the transport trait**, so SS-1..SS-7 ship on the
existing gRPC transport without waiting for liminal's unfinished wire (the 13-L0/L1 long
pole), and SS-8 then makes affinity ride liminal as a drop-in. They **share one seam** (the
outbox dispatch trait and the `namespace`/`task_queue` schema column — SS-4 = #13's 13-3 =
ROUTING-MODEL Tier 2; do it once). Build storage maturity in parallel with #13; converge at
SS-8.
