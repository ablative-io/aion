# Multi-Shard Active-Active Aion-on-Haematite — Design

**Date:** 2026-06-25
**Status:** ✅ IMPLEMENTED (reconciled 2026-07-02). The AA-4-x build series LANDED:
AA-4-0..4 + AA-4-6 are merged (commits: AA-4-3a `e6363388`, AA-4-3b `ee50fd95`,
AA-4-4 `cc905fc0`, AA-4-6 ship-gate demo `b94b1549`; plus `Engine::adopt_shards`
survivor resume `fac5ac46`). ONLY **AA-4-5** (shard→owner directory + `start_workflow`
forwarding on `NotOwner`) remains carried — it was explicitly deferred from v1 (callers
target the owner / retry on fence). NOTE: a post-kill-9 adopted-shard quorum-membership
bug is tracked separately (#148/#157 repro `b3d7885c`) and is orthogonal to this series'
landing. Design record retained below. Original status: "Design, verified against code.
Drives the AA-4-x build series."
**Goal (v1 DoD):** A 3-node cluster, `shard_count == 3`, each node owning one shard,
running three independent workflows simultaneously (one per node). Killing any one
node causes its shard to be re-elected by a survivor and that workflow to resume,
while the other two nodes keep serving their workflows uninterrupted.

This supersedes the single-node `shard_count == 1` adapter (the lock that made
prefix scans globally complete). True active-active means different workflows are
owned by different nodes, and node death re-homes only that node's shards.

---

## Verified load-bearing facts (read firsthand, 2026-06-25)

1. **`scan_sequence_keys` already fans out cross-shard.** `Database::scan_sequence_keys`
   (`haematite/src/db.rs:394`) runs `run_indexed_parallel(handles, ...)` over every
   shard and merges. This is the model for cross-shard KV fan-out.
2. **`range` is shard-local.** `Database::range` (`haematite/src/api/kv.rs:130`) routes
   from `from` via `handle_for(from)` — one shard only. This is exactly what breaks
   under `shard_count > 1`, and why the adapter's `scan_prefix` was only globally
   complete at `shard_count == 1`.
3. **Routing hashes the WHOLE key.** `ShardRouter::shard_for` (`haematite/src/shard/router.rs:20`)
   is `blake3::hash(key) % N` over all bytes. There is **no prefix routing**.
4. **Co-location is achieved by routing-key, not byte-prefix.** `EventStore::append`
   calls `db.append_with_ttl(stream_key, …)` which routes by `stream_key` (`E||uuid`);
   the shard actor then builds physical keys `stream_key || 0x00 || seq` and the seq
   counter `stream_key || 0xff` *inside that shard*. The suffixed physical keys are
   never independently routed. Events co-locate with their counter **because both are
   routed by `stream_key`**, not because they share a byte-prefix.

## CORRECTION to the first design pass (critical)

The first design pass proposed co-locating a workflow's KV records (index/timer/outbox)
by re-encoding their keys as `E || uuid || region_byte` and claiming they would "share
the same BLAKE3 prefix, so same shard." **This is false under whole-key hashing**:
`blake3(E||uuid||0x01)` is a different digest from `blake3(E||uuid||0x00||seq)` and
lands on a different shard. Appending a region byte does NOT co-locate.

**Correct mechanism — routing-key-aware KV operations in haematite.** Generalize the
exact pattern EventStore already uses for events: route by an explicit `route_key`,
operate on a separate physical key.

```
db.put_routed(route_key, physical_key, value, ...)     // routes by route_key
db.range_routed(route_key, from, to)                   // scans [from,to) within route_key's shard
db.cas_routed(route_key, key, expected, new)
db.delete_routed(route_key, key)
```

The adapter sets `route_key = event_stream_key(workflow_id)` (`E||uuid`) for ALL of a
workflow's per-workflow records (index entry, timers, outbox rows). Every record then
lands on `blake3(E||uuid) % N` — the same shard as the event stream. A workflow's
entire durable state is on one shard, identifiable from the workflow id, which is the
invariant per-shard failover recovery depends on.

Physical KV key layout (unchanged region tags, now co-located by route_key):
```
E || uuid || 0x00 || seq   event entries        (EventStore, route_key = E||uuid)
E || uuid || 0xff          sequence counter      (EventStore)
w: || uuid_text            workflow-id index      (route_key = E||uuid)
t: || uuid_text || 0x1f..  timers                 (route_key = E||uuid)
o: || dispatch_key         outbox rows            (route_key = E||uuid)   *see note
p: || type || 0x1f..       packages               node-local replicated, NOT sharded
r: || type                 routes                 node-local replicated, NOT sharded
```
*Outbox `dispatch_key` must encode the owning workflow id so the adapter can derive
`route_key`. If a dispatch_key cannot name its workflow, co-location fails — verify
during AA-4-1.

**Global scans** (`list_active`, `expired_timers`, `claim_outbox_rows`) still need
cross-shard fan-out: range the region prefix on every owned shard handle and merge.
This is `range_per_shard` (AA-4-0) iterated over shard handles, NOT `range_routed`.
`range_routed` is for single-workflow operations; `range_per_shard` is for global
enumeration.

---

## Packages & routes

Cluster-wide, not per-workflow, so they cannot carry a workflow route_key. Treat them
as **node-local replicated state**: every node persists a full copy (shard 0 on each),
exactly as the current single-node deploy path already does. Not sharded. The deploy
order is serialized by the schedule coordinator (a single workflow on one shard).

## Workflow placement & routing (pull-not-push)

Workflow W's owner node = the node owning `shard_for(event_stream_key(W))`. Any node
may *attempt* `replicate_append` for any workflow; the epoch fence at the true shard
owner rejects a non-owner's write (`Fenced`). v1: on a fence, the caller looks up the
shard→owner directory and forwards. The directory is a replicated per-shard KV record
(shard 0) or gossip; staleness self-corrects via the fence. Add
`StoreError::NotOwner(shard_id)` to distinguish a fence from other backend errors.

## Per-shard ownership (static v1)

Static assignment at cluster formation: e.g. `shard_count == 6`, node-0 ⇒ {0,1},
node-1 ⇒ {2,3}, node-2 ⇒ {4,5}. Each node calls `acquire_shard_and_serve(S, …)` per
assigned shard on startup. Overlap is an operator misconfiguration; Paxos resolves it
(one wins, others get `ElectionLost`). No balancer in v1.

## Per-shard recovery (failover)

`acquire_shard_and_serve(S)` already guarantees (via `become_live`,
`receiver.rs:850`) that after it returns every committed write on shard S is locally
present (union-merge from all promisers). So recovery is safe to start the instant it
returns. Recovery is scoped to owned shards:

```
node startup:
  1. open database (shard actors start, owned by nobody)
  2. for each assigned shard S: acquire_shard_and_serve(S)   // via run_off_runtime!
  3. after all acquired: list_active_for_shards(owned) -> recover only those workflows
  4. serve

failover (survivor detects dead node):
  1. for each shard S the dead node owned: acquire_shard_and_serve(S)  // union-merge
  2. Engine::recover_acquired_shard(S): list_active_for_shards({S}) -> spawn those
  3. serve the newly acquired shards' workflows
```

`bootstrap_schedule_coordinator` (`startup.rs:76`) must be gated: only the node owning
the coordinator's shard bootstraps it. Timer recovery (`recover_timers_on_startup`,
`startup.rs:29`) and the outbox dispatcher must scope to owned shards.

`acquire_shard_and_serve` blocks on quorum and MUST run off a tokio-runtime thread
(same `TransportBlockingFromAsync` constraint as `replicate_append`) — reuse the
adapter's `run_off_runtime` wrapper (`store.rs:455`) in the engine builder's election
step.

---

## Build increments (AA-4-x)

Each is independently testable; gate test named. Verify-don't-trust each before merge.

- **AA-4-0 (haematite, additive):** `Database::range_per_shard(shard_id, from, to)` +
  make `run_indexed_parallel` reachable. Routes a range to a shard by index, not by
  key hash. Zero behavior change for existing callers.
  *Gate:* 3-shard db; put keys into known shards; `range_per_shard(S)` returns only S's keys.

- **AA-4-1 (haematite, additive):** routing-key-aware KV ops `put_routed` /
  `range_routed` / `cas_routed` / `delete_routed`, routing by `route_key`, operating on
  a physical key. Mirrors EventStore's existing route-by-stream-key pattern.
  *Gate:* multi-shard db; `put_routed(rk, k, v)` then `range_routed(rk, …)` finds it;
  assert it lands on `shard_for(rk)`, not `shard_for(k)`.

- **AA-4-2 (adapter):** drop the `shard_count == 1` hardcode (config it); route all
  per-workflow KV writes via `*_routed` with `route_key = event_stream_key(id)`;
  replace `scan_prefix` global scans with cross-shard fan-out (`range_per_shard` over
  handles); add `list_active_for_shards` / `expired_timers_for_shards` /
  `claim_outbox_rows_for_shards`.
  *Gate:* 2-shard store; workflows hashing to different shards; `list_active()` returns
  all; `list_active_for_shards([0])` returns only shard-0 workflows; assert every record
  of workflow W co-locates with `shard_for(event_stream_key(W))`.

- **AA-4-3 (engine):** `owned_shards` on `EngineBuilder`; scope recovery + timer
  recovery to owned shards; gate `bootstrap_schedule_coordinator` on coordinator-shard
  ownership. Testable single-node (one node owns all shards == today's behavior) or
  in-process split.
  *Gate:* two stores/engines with disjoint shard subsets; each recovers only its shards.

- **AA-4-4 (deployment + adapter):** wire `acquire_shard_and_serve` (via
  `run_off_runtime`) into `EngineBuilder::build` before recovery; add
  `Engine::recover_acquired_shard(S)` for post-startup failover.
  *Gate:* 3-node in-process cluster, kill shard-1 owner, survivor acquires+recovers
  shard 1, its workflows resume.

- **AA-4-5 (deferred from v1):** shard→owner directory + `start_workflow` forwarding on
  `NotOwner`. v1 can require callers to target the owner / retry on fence.

- **AA-4-6 (integration, ship gate):** 3 nodes, 3 shards, 3 independent workflows; kill
  one; its shard re-elects and its workflow resumes while the others never pause.

## Honest gaps (carried)

- **No cross-shard atomicity.** Atomic state change across two workflows on different
  shards is impossible (no 2PC). Multi-shard Aion gives workflow-level isolation, not
  cross-workflow atomicity. Model cross-workflow effects as sagas.
- **Keyspace re-encoding is breaking** for any existing single-shard db (none in prod).
  Migration tool deferred; note in release notes.
- **Schedule coordinator is a single-shard hotspot** — correctness fine, throughput
  bounded by one node. Acceptable for v1.
- **Outbox dispatch_key must name its workflow** for co-location — verify in AA-4-1.

## Key files

- `haematite/src/db.rs:394` `scan_sequence_keys` (fan-out model); `:415`
  `shard_handles_in_order`; `:407` `shard_count`
- `haematite/src/api/kv.rs:130` `range` (shard-local); `haematite/src/api/event_store.rs:132`
  `append_batch_with_ttl` (route-by-stream_key co-location pattern to mirror)
- `haematite/src/shard/router.rs:20` `shard_for` (whole-key hash)
- `haematite/src/db/receiver.rs:806` `acquire_shard_and_serve` / `:850` `become_live`
- `aion-store-haematite/src/keyspace.rs` (region tags); `src/store.rs:111` shard_count
  hardcode; `:293` `scan_prefix`; `:455` `run_off_runtime`
- `aion/src/engine/startup.rs:29-176` recovery paths (to become shard-scoped); `:76`
  `bootstrap_schedule_coordinator`; `aion/src/engine/builder.rs:499` `build`
