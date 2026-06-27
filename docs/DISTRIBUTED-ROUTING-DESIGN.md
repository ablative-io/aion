# Distributed Request Routing Design

**Status:** Design pass (investigate + design). No production code changed by this
document beyond writing it.

**Problem owner:** the multi-process cluster spike
(`01d923cb spike(mp-cluster): true multi-process aion-on-haematite cluster
validation`) proved the durable substrate works across two real `aion server`
processes (cross-process dispatch + quorum replication + survivor read) **but**
recorded blocker #3 verbatim:

> #3 no shard-owner request routing: a `start` whose new workflow_id hashes to a
> non-owned shard is fenced ("fenced by CAS rejects"); ~50% of submissions fail
> at random. design item.

This document designs the cluster's **request-routing layer** and reconciles it
with the existing roadmap (SS-2 / SS-3 / SS-5b / SS-6 / SS-8 / #13) so we build
the thing the roadmap already wants rather than a throwaway forward.

---

## 1. Diagnosis (where and why the fence happens)

### 1.1 The request entry path

Every client mutation/read arrives over tonic at one of four gRPC handlers in
`crates/aion-server/src/api/grpc.rs`:

| RPC | handler | delegates to |
| --- | --- | --- |
| `start_workflow` | `grpc.rs:45-63` | `handlers::start` (`grpc.rs:55`) |
| `signal` | `grpc.rs:65-78` | `handlers::signal` (`grpc.rs:70`) |
| `query` | `grpc.rs:80-93` | `handlers::query` (`grpc.rs:85`) |
| `cancel` | `grpc.rs:95-108` | `handlers::cancel` (`grpc.rs:100`) |

Each handler extracts the caller (`grpc.rs:32-34`, `caller_from_metadata`), then
calls into `crates/aion-server/src/api/handlers/workflows.rs`:

- `start` (`workflows.rs:28-68`) — scopes the namespace (`workflows.rs:33-36`),
  then calls `scoped.engine()?.start_workflow(...)` (`workflows.rs:48-57`). **The
  workflow id is minted *inside* the engine** — it is only known *after* the call
  (`handle.workflow_id()`, `workflows.rs:62-65`).
- `signal` (`workflows.rs:86-124`), `query` (`workflows.rs:132-175`), `cancel`
  (`workflows.rs:183-218`) — each resolves the target `workflow_id` **up front**
  (`required_workflow_id`, `workflows.rs:91 / 137 / 188`) before touching the
  engine.

The engine operations (`crates/aion/src/engine/api.rs:215` `start_workflow`,
`crates/aion/src/engine/delegated.rs:237` `signal`, `:273` `query`) read/append
through the store. In distributed mode the append funnels through
`crates/aion-store-haematite/src/store.rs`:

- `append_blocking` (`store.rs:752-829`) → when `routing` is `Some`,
  `replicate_events` (`store.rs:896-920`) →
  `database.replicate_append(stream_key, payloads, expected_seq, &membership, timeout)`
  (`store.rs:905-910`).

### 1.2 Where the fence fires and why it is opaque

`replicate_append` runs a quorum CAS against the shard's current owner-epoch. A
node that is **not** the elected owner of `shard_for(workflow_id)` cannot reach
quorum: the owner's promised ballot fences the proposal. The error originates in
haematite at `crates/haematite/src/sync/consistency.rs:191-197`:

```
ConsistencyError::Fenced { required, possible_accepts }
  => "fenced by CAS rejects: required {required} accepts, only {possible_accepts} still possible"
```

That is exactly the 2-node spike message ("required 2 accepts, only 1 still
possible"). It propagates back into aion as a **stringly-typed backend error**,
losing all structure:

1. `replicate_events` maps non-sequence errors via `database_error(&error)` →
   `StoreError::Backend(String)` (`store.rs:919`, `aion-store-haematite/src/error.rs:13-18`).
2. `StoreError` is a **closed enum** with no "wrong owner" variant
   (`crates/aion-store/src/error.rs:7-34` — only `SequenceConflict`, `NotFound`,
   `Backend`, `Serialization`).
3. The handler maps it to `WireError::backend` → `WireErrorCode::Backend` →
   **`Code::Internal`** (`grpc.rs:406-408`).

**Consequence:** the client sees an opaque `Internal` error with no indication
that (a) it hit the wrong node, or (b) which node owns the shard. There is no
retry/redirect signal. Because `start_workflow` mints a random id, ~50% of starts
in a 2-shard/2-node cluster land off-owner and fail at random — the spike's
finding.

### 1.3 What ownership information exists at request time

Everything needed to *decide* routing exists locally; the cluster-wide
*directory* does not.

- **Deterministic shard for a workflow** — `HaematiteStore::shard_for_workflow`
  (`store.rs:441-445`): `shard_for(event_stream_key(workflow_id))`. Pure, needs no
  cluster state, identical to the routing the write uses. This is the routing key
  function.
- **Does *this* node own it** — `HaematiteStore::owns_workflow_shard`
  (`store.rs:453-459`) against the local owned set
  (`owned_shards: Arc<RwLock<Option<BTreeSet<usize>>>>`, `store.rs:196`; `None` =
  owns all). Mutated by `set_owned_shards` (`store.rs:395`), `extend_owned_shards`
  (`store.rs:422`, the failover widening), `own_all_shards` (`store.rs:406`).
- **Is a peer alive right now** — `HaematiteStore::peer_connected(peer_name)`
  (`store.rs:475-480`): true socket liveness off the OTP distribution link, not a
  heartbeat. This is what the SS-5b supervisor polls.
- **Cluster membership config** — `ClusterConfig` / `ClusterPeer`
  (`crates/aion-server/src/config/mod.rs:154-212`). Each `ClusterPeer` carries
  `{ name, address (replication endpoint), owned_shards }` (`config/mod.rs:198-211`).
  Assembled into `ClusterBootstrap` (`store.rs:88-100`) and into
  `Vec<WatchedPeer { name, owned_shards }>` for the supervisor
  (`crates/aion-server/src/cluster.rs:76-81`, retained on `ConnectedStore`
  `state.rs:562-565`).

**The gap, precisely:** a node knows *its own* shards and *which peers it watches
for failover*, but there is **no queryable, cluster-coherent map "shard S → owner
node N at epoch E"**, and the only per-peer shard hint is **static config** whose
replication `address` is **not the peer's gRPC API address**. Failover
(`Engine::adopt_shards`, `crates/aion/src/engine/api.rs:283-314` → `acquire_owned_shards`
+ `extend_owned_shards`) mutates ownership at runtime, so even the static map goes
stale on a node death. **This missing directory is SS-3.**

---

## 2. The routing architecture

### 2.1 Where routing lives — gRPC edge, not the engine

**Decision: a thin routing pre-step at the gRPC adapter layer (`api/grpc.rs`),
backed by a `ShardDirectory` resolver held on `ServerState`.** Not in the engine,
not in the store.

Rationale:

- The four mutating/reading RPCs are the *only* externally-routable surface, and
  they already sit in one file. The `workflow_id` (the routing key) is available
  at the adapter for signal/query/cancel before any engine call, and is derivable
  for start (see §2.4). The store handle (`cluster_store: Arc<HaematiteStore>`,
  `state.rs:557-561`) and the peer set (`watched_peers`, `state.rs:562-565`) are
  already on `ServerState`.
- Keeping it at the edge means the engine and store stay **distribution-agnostic**
  — they continue to assume "I am the owner; if not, the fence stops me." The
  fence remains the *correctness backstop*; routing is the *availability layer* on
  top. We never weaken the fence.
- It composes with the existing edge concerns already there (drain gate
  `grpc.rs:49`, caller auth `grpc.rs:54`, namespace scope) as one more pre-flight
  check.

Shape (illustrative, not landed):

```
fn route(workflow_id) -> RouteDecision {
    let shard = store.shard_for_workflow(workflow_id);   // pure
    match directory.owner_of(shard) {                    // SS-3 resolver
        Local            => RouteDecision::Local,        // proceed to engine
        Remote(node)     => RouteDecision::Elsewhere(node),
        Unknown          => RouteDecision::Local,         // optimistic; fence backstops
    }
}
```

### 2.2 Redirect vs forward — **forward (server-side proxy), converging on liminal**

Two honest options:

- **Client-redirect (Kafka NOT_LEADER / Redis MOVED):** the wrong node returns a
  typed "owner is node X at gRPC addr A" error; the client re-dials. Pros: zero
  server-side connection fan-out, owner does exactly one node's work, naturally
  honest about staleness (client re-resolves on every MOVED). Cons: every SDK
  (Rust, plus the TS/other SDKs under `sdks/`) must learn the redirect dance and
  carry the directory; a thundering re-dial after failover; two round trips on the
  unlucky path.
- **Server-side forward/proxy:** any node accepts, forwards to the current owner,
  relays the reply. Pros: clients stay dumb (single dial to any node — the
  "durable agents as one binary" story); the cluster hides its own topology; one
  place to implement retry-on-stale. Cons: an extra intra-cluster hop; the
  receiving node holds a transient connection to the owner; care needed so a
  forward can't loop.

**Recommendation: server-side forward.** It matches the product north-star (a
client talks to *the cluster*, not to a node it must track) and — decisively — it
**reuses the transport the roadmap already commits to**. SS-8/#13 makes liminal
the cross-node mechanism, and liminal already exposes
`request_reply_conversation()` (`liminal/crates/liminal-sdk/src/remote/protocol.rs:64-77`)
— a correlated request/reply that is *exactly* a forwarded RPC. A client-redirect
model would put routing intelligence in N SDKs and still not exercise the liminal
path the cluster needs for dispatch (SS-8). Forwarding lets request-routing and
activity-dispatch ride one transport.

We keep a **client-redirect escape hatch** cheaply: see Decision A in §3 — a typed
`NotOwner` wire error (carrying the owner hint) is introduced regardless, because
it is both the forward's *fallback* (forward target itself is stale → return
NotOwner so a smart client or the next node retries) and the seam a redirect-aware
SDK could later consume. Forward-first does not foreclose redirect-later; it is a
strict superset of the wire contract.

### 2.3 The shard→node directory (this **is** SS-3)

The roadmap already specifies SS-3 as three deliverables
(`docs/STORAGE-SWAP-MATURITY-DESIGN.md:411-422`): (a) a quorum-backed /
CAS-versioned shard→node directory (static-seeded in v1), (b) a `start_workflow`
variant taking a routing key, (c) a structured `StoreError::NotOwner(shard)`. The
locked decision (`STORAGE-SWAP §3 (e)`, `AION-DISTRIBUTION-DESIGN.md:106-110`) is
a CAS-versioned map owned by a liminal global-name singleton coordinator (the Akka
ShardCoordinator role), **static assignment in v1**. This routing design folds
into that — it does not invent a parallel concept.

**Directory interface (consumed by routing, produced by SS-3):**

```
trait ShardDirectory {
    /// Current owner of `shard`, with the epoch the resolver believes is current.
    fn owner_of(&self, shard: usize) -> OwnerView;   // Local | Remote(NodeRef) | Unknown
}
struct NodeRef { node_id: String, grpc_addr: SocketAddr, epoch: u64 }
```

**Source of truth — staged (Decision B, §3):**

1. **v1 (now, unblocks the spike): static + self-knowledge.** The directory is
   built from `ClusterConfig`: this node's owned shards (`store.owned_shards()`)
   mark `Local`; each `ClusterPeer`'s declared `owned_shards` mark `Remote`. **This
   requires one config addition: a peer's gRPC API address.** Today `ClusterPeer`
   has only the *replication* `address` (`config/mod.rs:202`); forwarding a client
   RPC needs the peer's `grpc_address`. Add `ClusterPeer.grpc_address:
   Option<SocketAddr>`. This is the minimal change that makes the 2-node spike
   usable.
2. **v1.5 (with SS-5b already shipped): self-knowledge + live overlay.** Local
   ownership is read *live* from `store.owned_shards()` (which `adopt_shards`
   already widens on failover, `engine/api.rs:297`), so the owner's view of its own
   shards is always current. Peer liveness comes from `peer_connected`
   (`store.rs:475`). A peer believed-down → its shards resolve `Unknown` (route
   locally/optimistically; the fence + the local supervisor's pending adoption
   converge). This makes the static map *failover-aware without a new wire
   protocol* (see §2.5).
3. **v2 (SS-3 proper): the quorum-backed CAS directory.** The static map is
   replaced by the authoritative `{shard, owner_node, epoch}` record gossiped /
   queried via the liminal global-name coordinator. `owner_of` then returns the
   real epoch and the resolver caches with epoch-stamped invalidation. Routing's
   `ShardDirectory` trait is unchanged — only its implementation swaps. This is the
   convergence point: **routing consumes SS-3; it does not duplicate it.**

### 2.4 Routing `start` (the id-minting problem)

For signal/query/cancel the `workflow_id` is in the request. For `start` the id is
minted inside the engine (`engine/api.rs` via `StartWorkflowOptions::default()`,
`api.rs:237-241`; surfaced only at `workflows.rs:62`). Two paths, both already
anticipated by SS-3(b):

- **Steered start (preferred long-term):** add `routing_key` to
  `StartWorkflowOptions` so the id/placement derives from
  `shard_for(routing_key)`. The edge can then resolve the owner *before* calling
  the engine and forward if needed. This is SS-3(b) verbatim
  (`STORAGE-SWAP:415-420`).
- **Unsteered start (v1 stopgap):** the receiving node, if it owns *any* shard,
  can mint an id that hashes to a shard it owns (reject-and-remint loop bounded by
  shard count), guaranteeing the start lands locally and never fences. Crude but it
  makes the spike green with zero protocol work, and it is transparent to clients.
  Recommended only as the R-0/R-1 bridge until steered start lands.

### 2.5 Failover-aware resolution and the in-flight window

Ownership changes during a node death: SS-5b's supervisor
(`cluster.rs:103-218`, `tick` at `:160-198`) detects a peer's socket drop
(`peer_connected` flips, `store.rs:475`), debounces `confirmations` consecutive
polls (`cluster.rs:170-173`), then calls `Engine::adopt_shards`
(`engine/api.rs:283-314`) which elects + union-merges + re-residents the dead
peer's shards and **widens `owned_shards`** (`extend_owned_shards`,
`engine/api.rs:297`). During that window ownership is genuinely ambiguous.

Routing must resolve to the **current** owner and survive the gap. The story:

- **The fence is the invariant; routing is best-effort.** Routing can be stale and
  the system stays *correct* — a forward to a just-dead owner fails the quorum
  write (the owner is gone, can't ack), and a forward to a node that hasn't yet
  adopted fences. Either way the durable state is never corrupted. Routing's job is
  to turn those into *retry/redirect*, not a client-visible failure.
- **Stale-forward handling.** A forward whose target (a) is unreachable, or (b)
  returns `NotOwner`, triggers **bounded re-resolution + retry** at the receiving
  node: re-query the directory (which, under the v1.5 overlay, now sees the target
  `Unknown` via `peer_connected`), and either forward to the new owner or, if the
  new owner is *this* node post-adoption, handle locally. A hop counter caps
  forwards (e.g. 2) so a directory that is briefly inconsistent across nodes cannot
  loop; on cap-exceeded return the typed `NotOwner` so the caller retries with
  backoff. This mirrors SS-6's "a completion arriving at a non-owner must be
  forwarded to (or rejected-for-retry toward) the current owner"
  (`STORAGE-SWAP:269-271, 455-465`) — request routing and completion fan-in use
  the *same* resolve-or-reject discipline.
- **In-flight write idempotency.** Retried starts must not double-create. This is
  already handled by the existing optimistic-concurrency guard
  (`expected_seq`/`SequenceConflict`, `store.rs:766-772`) plus the outbox dedup
  story; routing adds no new duplication because a *forwarded* request executes
  exactly once on the owner (the relay carries the single result back).
- **Consistency story (summary):** the directory is **eventually consistent**;
  the epoch fence provides **linearizable** per-shard write safety underneath.
  Routing never promises to always hit the owner first try — it promises to
  *converge* to the owner without ever violating single-writer-per-shard. This is
  the same posture the locked decisions take
  (`AION-DISTRIBUTION-DESIGN.md:108-110`: TCP-liveness is "only a trigger to
  re-resolve, never ground truth").

### 2.6 Convergence with liminal / SS-8 / #13

The forwarded request should ride **liminal**, the same mechanism #13 uses for
activity dispatch, rather than a throwaway point-to-point gRPC proxy.

- #13 already establishes the seam: `OutboxRowDispatch`
  (`crates/aion-server/src/worker/outbox_dispatcher.rs:104`) with a
  `LiminalOutboxDispatch` (`worker/liminal_transport.rs:170-255`) behind the
  `liminal-transport` Cargo feature (`aion-server/Cargo.toml`, off by default).
  Liminal's `request_reply_conversation()`
  (`liminal-sdk/src/remote/protocol.rs:64-77`) is a correlated request/reply — the
  precise primitive a forwarded `signal`/`query`/`cancel`/`start` needs.
- SS-8 (`STORAGE-SWAP:478-487`) is defined as "storage maturity + transport
  maturity meet": dispatch targets derived at the storage/affinity layer, transport
  a drop-in below the trait. **Request forwarding is the same idea one layer up** —
  routing decides *which node*, liminal carries the call. If we forward over liminal
  global-name addressing (node = liminal global name,
  `AION-DISTRIBUTION-DESIGN.md:109`), then SS-8 and routing share one transport and
  one addressing scheme.
- **But liminal cross-node request/reply is gated** on liminal 13-L0/13-L1
  (`LIMINAL-SWAP-DESIGN.md:439-456`), which are not yet landed (the transport agent
  confirmed the current responder is liminal's echo participant, not a real remote
  aion worker, `worker/liminal_transport.rs:39-51`). So the **recommended path is a
  transport-abstracted forwarder**: define a `RequestForwarder` trait at the edge
  with a gRPC implementation now (reuses the existing tonic `WorkflowService`
  client against the peer's `grpc_address`) and a liminal implementation when
  13-L0/L1 land — exactly mirroring how `OutboxRowDispatch` has gRPC and liminal
  impls. This is Decision C (§3).

---

## 3. Key decisions for Tom

### Decision A — Redirect vs Forward
**Recommendation: server-side forward, with a typed `NotOwner` wire error
introduced anyway.** Forward keeps every SDK dumb (single dial to the cluster),
matches the single-binary north-star, and reuses the liminal request/reply path
SS-8 needs. The typed `NotOwner` (carrying owner node + gRPC addr + epoch) is
introduced regardless because it is the forward's stale-target fallback *and* a
future redirect-aware SDK's seam — forward-first is a strict superset of
redirect, so we are not boxed in. This also delivers SS-3(c)
(`StoreError::NotOwner(shard)`).

### Decision B — Directory source of truth (now vs SS-3)
**Recommendation: stage it.** v1 = static config (this node's owned shards +
peers' declared `owned_shards`) **plus one new field `ClusterPeer.grpc_address`**;
overlay live local-ownership (`store.owned_shards()`, already failover-updated) and
live peer liveness (`peer_connected`) so it is failover-aware with no new wire
protocol. v2 = swap the resolver impl for SS-3's quorum-backed CAS directory via
the liminal global-name coordinator. The `ShardDirectory` trait is the stable
seam; SS-3 fills in the authoritative implementation later. **Routing consumes
SS-3 rather than pre-empting it.**

### Decision C — Liminal now vs liminal later for the forward hop
**Recommendation: liminal later, behind a `RequestForwarder` trait, gRPC impl
now.** Cross-node request/reply over liminal is blocked on liminal 13-L0/13-L1
(not landed). Shipping a gRPC forwarder (peer tonic client to `grpc_address`)
unblocks the cluster immediately and the trait makes the liminal swap a one-impl
change when SS-8's prerequisites are green — the identical pattern already proven
by `OutboxRowDispatch`. This avoids both (a) blocking routing on liminal and (b)
building a gRPC forwarder we'd throw away (the trait keeps it as the
co-located/fallback transport).

---

## 4. Spike-first decomposition (R-0 … R-N)

Sequenced to make the 2-node spike usable fast, then converge on SS-3/SS-8.

- **R-0 — Structured fence error (spike-first; unblocks everything).**
  Add `StoreError::NotOwner { shard }` to the closed enum
  (`crates/aion-store/src/error.rs`) and map haematite `ConsistencyError::Fenced`
  to it in `replicate_events` (`store.rs:896-920`) instead of `Backend(String)`.
  Surface a `WireErrorCode::NotOwner` → a *retryable* gRPC code (`FailedPrecondition`
  or `Aborted`, mirroring the existing `SequenceConflict → Aborted` precedent
  `grpc.rs:394`). *This alone turns the spike's random Internal errors into a
  typed, retryable signal* and is SS-3(c). **Risk:** low — additive enum + mapping;
  the closed-enum match sites must all be updated (compiler-enforced).

- **R-1 — `shard_for`-aware local guard + unsteered-start remint (spike green).**
  At the edge, compute `store.shard_for_workflow(id)` and, for signal/query/cancel
  on a non-owned shard with no directory yet, return `NotOwner`. For `start`,
  remint to a locally-owned shard (the §2.4 stopgap) so starts never fence. **Risk:**
  low; remint is a crude bridge — flagged as temporary, removed once R-3 lands.

- **R-2 — `ShardDirectory` trait + static resolver + `ClusterPeer.grpc_address`.**
  Introduce the trait (§2.3), a static-config resolver, and the new config field.
  Build it on `ServerState` from `ClusterConfig` + live `store.owned_shards()` +
  `peer_connected` overlay (§2.5). **Risk:** medium — the config field is a public
  surface change (document the default `None` = not-forwardable, falls back to
  `NotOwner`); the liveness overlay needs the failover-window semantics test.

- **R-3 — `RequestForwarder` trait + gRPC forwarder + relay.**
  Edge forwards non-local RPCs to the owner's `grpc_address` via a tonic
  `WorkflowService` client, relays the reply, with a hop cap + re-resolve-on-stale
  (§2.5). Remove R-1's start-remint once steered start (R-4) or forward covers it.
  **Risk:** medium — loop prevention and the stale-forward retry are the
  correctness-sensitive parts; needs a failover-window integration test
  (forward → owner just died → re-resolve → new owner).

- **R-4 — Steered start (SS-3(b)).** Add `routing_key` to `StartWorkflowOptions`
  (`engine/api.rs` / `start::StartWorkflowOptions`) so placement is deterministic
  and the edge can resolve+forward `start` before the engine call. Retire the
  remint stopgap. **Risk:** medium — engine API surface; coordinate with the
  steered-placement gate test in SS-3.

- **R-5 — Failover-window hardening + real-app demo.** End-to-end test: 3-node
  cluster (needs the beamr 3-peer connect fix, spike blocker #2 — *out of scope
  here, noted as prerequisite*), kill the owner, confirm in-flight
  start/signal/query/cancel route to the survivor that adopts the shard with no
  duplicate `WorkflowStarted` and no client-visible failure beyond bounded retry.
  This is the routing half of the failover demo Tom wants. **Risk:** higher —
  depends on SS-5b (landed) and the beamr mesh fix (not landed).

- **R-6 (converge) — Liminal forwarder impl (SS-8 alignment).** When liminal
  13-L0/13-L1 land, add a `LiminalRequestForwarder` impl of the R-3 trait using
  `request_reply_conversation()`, addressed by liminal global name. Flip the
  forwarder selection the same way `select_outbox_row_dispatch`
  (`run.rs:338`) selects dispatch. **Risk:** gated entirely on liminal
  prerequisites; trait isolates it.

- **R-7 (converge) — Swap static resolver for SS-3 CAS directory.** Replace the
  static `ShardDirectory` impl with the quorum-backed, epoch-versioned directory
  from the liminal global-name coordinator (SS-3 proper). Routing code unchanged.
  **Risk:** the directory's own consensus is the hard part — owned by SS-3, not by
  routing.

**Sequencing against the roadmap:** R-0..R-3 are *new* but small and slot beneath
SS-3 (they define the seam SS-3 fills). R-0/R-1/R-2/R-3 make the spike usable
**before** SS-3's full directory exists. R-4 = SS-3(b). R-7 = SS-3(a). R-6 = SS-8
transport convergence (gated on #13's liminal prerequisites). The trait seams
(`ShardDirectory`, `RequestForwarder`) are deliberately the same *shape* as the
already-shipped `OutboxRowDispatch` seam so the convergence steps are impl swaps,
not rewrites.

---

## 5. What this deliberately does NOT change

- **The epoch fence / quorum CAS.** It stays the correctness backstop; routing is
  an availability layer above it. A mis-routed write still fences — we never make
  the store trust routing.
- **The engine and store distribution-agnosticism.** They keep assuming "I am the
  owner, else I fence." No ownership checks move into the engine hot path.
- **Single-writer-per-workflow / single-owner-per-shard.** Forwarding routes *to*
  the single owner; it does not introduce multi-writer or cross-shard atomicity
  (sagas remain out of scope per `STORAGE-SWAP §3 (c)`).
- **SS-5b failover mechanics.** `adopt_shards` / supervisor / `peer_connected` are
  reused as-is; routing *consumes* their ownership changes, it does not
  reimplement detection or adoption.
- **The default single-node path.** No `[store.cluster]` → no directory, no
  forwarder, byte-identical to today. The new `ClusterPeer.grpc_address` defaults
  to absent.
- **The dispatch/outbox path (#13).** Activity dispatch is untouched; routing
  reuses its *transport seam pattern* but is a separate concern (request edge vs
  activity egress).
- **No new external dependency.** gRPC forwarder uses the tonic client already in
  the tree; the liminal forwarder rides the existing optional `liminal-transport`
  feature. Single-binary, no-inbreeding posture preserved.

---

## Appendix — primary citations

- Spike blocker #3 (the problem): commit `01d923cb` message.
- Fence origin: `haematite/crates/haematite/src/sync/consistency.rs:191-197`.
- aion fence propagation: `aion-store-haematite/src/store.rs:896-920`,
  `aion-store-haematite/src/error.rs:13-18`.
- Closed error enum (no NotOwner today):
  `aion-store/src/error.rs:7-34`.
- Wire-code mapping (fence → Internal today): `aion-server/src/api/grpc.rs:388-409`.
- Request handlers: `aion-server/src/api/grpc.rs:45-108`,
  `aion-server/src/api/handlers/workflows.rs:28-218`.
- Ownership API: `aion-store-haematite/src/store.rs:441-445` (`shard_for_workflow`),
  `:453-459` (`owns_workflow_shard`), `:475-480` (`peer_connected`),
  `:395/:422/:484` (owned-set mutation/read).
- Cluster config / directory seed: `aion-server/src/config/mod.rs:154-212`;
  `aion-store-haematite/src/store.rs:88-100` (`ClusterBootstrap`);
  `aion-server/src/cluster.rs:76-81` (`WatchedPeer`);
  `aion-server/src/state.rs:557-565` (retained store + peers).
- Failover: `aion-server/src/cluster.rs:103-218` (supervisor),
  `aion/src/engine/api.rs:283-314` (`adopt_shards`).
- Liminal seam: `aion-server/src/worker/outbox_dispatcher.rs:104` (trait),
  `aion-server/src/worker/liminal_transport.rs:39-51,170-255`,
  `liminal/crates/liminal-sdk/src/remote/protocol.rs:64-77`
  (`request_reply_conversation`).
- Roadmap items: `docs/STORAGE-SWAP-MATURITY-DESIGN.md` SS-2 `:396-409`, SS-3
  `:411-422`, SS-5b `:439-453`, SS-6 `:455-465`, SS-8 `:478-487`;
  `docs/AION-DISTRIBUTION-DESIGN.md:106-110` (CAS directory, locked decisions);
  `docs/LIMINAL-SWAP-DESIGN.md:439-456` (13-L0/13-L1 prerequisites).
