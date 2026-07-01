<!-- STATUS: BUILT & LANDED (reconciled 2026-07-02). This blueprint is implemented:
durable minted-on-use namespace registry ships in `aion-store/src/namespace.rs`
(NamespaceRecord/NamespacePlacement), `aion-store-haematite/src/keyspace.rs` + quorum
CAS test `aion-store-haematite/tests/namespace_quorum_cas.rs`, and the HTTP surface
`GET/POST /namespaces` + `/namespaces/{name}/placement` at
`aion-server/src/api/http/router.rs:177-181`. Verified against those symbols. The
shard-count-default dependency is RESOLVED (4096, commit af4bad09). OPEN DECISIONS in
the final section were resolved during the build. Original DRAFT text below is retained
as the design record.
     Prior header: "DRAFT design blueprint (2026-06-30) ... NOT yet approved to build". -->

# Control-Plane Phase 1 — Durable, Haematite-Backed, Minted-on-Use Namespace Registry

> Implementation blueprint. Grounds every seam in verified source (file:line). Folds into #146 (haematite as cluster source-of-truth). Realises `CONTROL-PLANE.md` §3 (lifecycle), §4 (hard decisions), §6 (shard trap), §7 Phase 1.

---

## 1. Goal + the minted-on-use model

**Goal.** Turn the namespace from a free-form label that exists only as a per-workflow search-attribute projection (`aion.namespace`, `resolver.rs:24,284`) and an outbox column (`schema.rs:186-191`) into a **first-class durable record** in a haematite-backed registry, so that:

- `GET /namespaces` returns the **live, real set** of namespaces — retiring today's stopgap that echoes `caller.namespaces()` or the configured default (`workflows.rs:182-190`).
- The set **survives owner-node death / failover** (quorum-replicated, travels with its shard like event history).
- Namespaces come into being **with zero ceremony**: a worker registering for an unseen namespace *creates* it via an idempotent CAS upsert. No pre-provision step (the anti-Temporal bet, `CONTROL-PLANE.md` §3).

**The model (verbatim from §3, now made buildable):**

1. **Minted on first reference.** A worker registering `(namespace, task_queue [, node])` for a namespace that has no durable record upserts one. Two workers racing the same new namespace is fine — the CAS-conflict-then-reconcile branch makes it idempotent (§2 of storage map; `store.rs:634-637`).
2. **Recorded durably regardless of how it appeared.** The record is `{name, created_at, last_seen, origin, config, placement}` written through the quorum-replicated fenced path.
3. **Governable.** `auto_create = open | closed` (default **open** to preserve zero-config). `closed` rejects an unknown namespace at the mint hook instead of creating it.
4. **Existence anchored on STATE, not workers** (the critical §3 correction): a namespace exists if it has *durable state* OR a *live worker* OR an *explicit registry entry*. Worker-minting is one path to existence, never the definition — so a reaped registry row can never orphan durable history.

This is purely additive to the existing auth model: `NamespaceResolver::resolve` (`resolver.rs:481-507`) still gates access by grant; the registry adds an **existence/mint** dimension *beneath* the grant check, never replacing it.

---

## 2. The durable record: `NamespaceRecord`

Defined in a new `crates/aion-store/src/namespace.rs` (sibling to `package.rs`). Mirrors `PackageRecord` style (`package.rs:22-32`).

```rust
/// One durable namespace registry entry. The control-plane source of truth
/// for "this namespace exists", listable, failover-survivable, and the anchor
/// for future per-namespace policy (quotas, placement).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceRecord {
    /// The namespace name. Free-form, exactly as carried on the wire
    /// (StartWorkflowRequest.namespace / RegisterWorker.namespaces). Primary key.
    pub name: String,
    /// When the registry first minted this namespace (first reference).
    pub created_at: DateTime<Utc>,
    /// Most recent time a worker/start referenced it — refreshed on mint-touch.
    /// Drives staleness/observability; NEVER drives reaping while state exists.
    pub last_seen: DateTime<Utc>,
    /// How it came to exist: worker-mint, explicit POST, or inferred-from-state.
    pub origin: NamespaceOrigin,
    /// Reserved per-namespace policy blob (retention, quotas, auth scope).
    /// Phase 1 writes Default; Phase 2 fills it. Present day-one to avoid migration.
    pub config: NamespaceConfig,
    /// Reserved placement directive (node/shard-range affinity). Phase 1 = Unplaced.
    /// Present day-one so physical isolation is a later policy, not a migration (§4.3).
    pub placement: NamespacePlacement,
    /// Lifecycle state so a namespace can be retired without losing history
    /// (Temporal/Cadence both needed this — competitive analysis §7.1).
    pub state: NamespaceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceOrigin { WorkerMint, Explicit, InferredFromState }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceState { Active, Deprecated }

/// Phase-1 placeholder; extends in Phase 2/3 without changing the record shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamespaceConfig { /* retention, quota keys — empty/Default in P1 */ }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NamespacePlacement { #[default] Unplaced /* NodeAffinity{..}, ShardRange{..} later */ }
```

**Why each field:**

| Field | Why now |
|---|---|
| `name` | The key. Free-form per the routing map (`liminal_transport.rs:233-240` — namespace is free-form). |
| `created_at` | "namespace appeared" audit + ordering for `list`. |
| `last_seen` | Observability/staleness signal. Decoupled from existence (see §4 anchoring) so refresh is cheap and never a reap trigger. |
| `origin` | Distinguishes self-serve mint from explicit POST from history-inference — needed for the "loud created" event and ops console. |
| `config` (reserved) | §4.3 discipline: reserve the policy blob day-one (retention is the one field *every* mature system carries — competitive §7.1). Phase 2 fills quotas as keyed backpressure (§4.2). Adding it later = data migration; reserving now = policy flip. |
| `placement` (reserved) | §4.3 verbatim: "put a `placement` field in immediately, so physical isolation is a later *policy*, not a *migration*." |
| `state` | Competitive §7.1 steal: deprecate-before-delete so retiring a namespace never strands its durable history. Also the `tenant`/`kind` reservation lives here conceptually (Hatchet insight §7.3) — add a `kind` discriminator inside `config` if Tom wants the tenant⊃namespace split reserved too (open decision §7). |

Serialization: bincode/serde to opaque bytes, exactly as packages treat their archive (`store.rs:1888-1904` encodes then `database.put`). The store backend never parses the record beyond decode-for-list.

---

## 3. Storage seam: a **sibling `NamespaceStore` trait** (not extending PackageStore/EventStore)

**Decision (from the storage map's conclusion, confirmed against source): add a sibling trait.** Rationale verified:

- The registry's durability is **stronger** than `PackageStore`'s: packages use the **plain local** `database.put`/`commit` path (`store.rs:1888-1904`) and are *not* quorum-replicated; the registry must survive owner-node death and be readable on a survivor, so it must use the **quorum-replicated `replicate_write` CAS** path (`store.rs:612-621`). Folding into `PackageStore` would either under-serve namespaces or over-promise on packages.
- Minted-on-use CAS semantics (create-if-absent / value-CAS / reconcile-on-conflict) have **no analogue** in `PackageStore`'s unconditional `put_package`.
- A sibling trait keeps the `EventStore: ReadableEventStore + WritableEventStore + PackageStore` blanket supertrait (`store.rs:376-381`) clean. In-memory/libSQL backends **default-impl it as a no-op or local-only**, exactly mirroring how `acquire_owned_shard` (`store.rs:179-182`) / `publish_shard_owner` (`store.rs:246-249`) are default-no-op and only haematite overrides them.

### Trait (`crates/aion-store/src/namespace.rs`)

```rust
#[async_trait]
pub trait NamespaceStore: Send + Sync + 'static {
    /// Idempotent minted-on-use upsert. Create-if-absent; if a record already
    /// exists, refresh `last_seen` (value-CAS). A concurrent racer that wrote an
    /// equivalent record first is treated as success (idempotent mint).
    /// Returns whether this call CREATED the record (drives the "loud created"
    /// event) vs touched an existing one.
    async fn register_namespace(
        &self,
        name: &str,
        origin: NamespaceOrigin,
    ) -> Result<MintOutcome, StoreError>;

    /// Explicit create (POST /namespaces). Same upsert but origin=Explicit and
    /// it may carry an initial config. Idempotent on an existing name.
    async fn put_namespace(&self, record: NamespaceRecord) -> Result<MintOutcome, StoreError>;

    /// The live durable set, ascending `created_at` (ties by name) — backs
    /// GET /namespaces. Filtered by grant at the API layer, not here.
    async fn list_namespaces(&self) -> Result<Vec<NamespaceRecord>, StoreError>;

    /// Single lookup — the existence probe for the `closed` policy and the
    /// resolver's existence anchor.
    async fn get_namespace(&self, name: &str) -> Result<Option<NamespaceRecord>, StoreError>;

    /// Set state Active->Deprecated (deprecate-before-delete). Idempotent.
    async fn deprecate_namespace(&self, name: &str) -> Result<(), StoreError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintOutcome { Created, AlreadyExisted }
```

**Default no-op-ish impls** for in-memory + libSQL: simplest correct choice is a **local-only** impl (a `namespaces` keyspace/table) so those backends still satisfy `list`/`get` for single-node correctness, but without quorum replication. The blanket supertrait stays `EventStore` as-is; `NamespaceStore` is carried as a **separate `Arc<dyn NamespaceStore>`**, or — cleaner — added as a supertrait bound `EventStore: ... + NamespaceStore` once every backend has at least a local impl. Recommend the **separate Arc** in Phase 1 (smaller blast radius; libSQL/in-mem get a trivial table impl; haematite gets the real quorum impl), matching how `cluster_store: Option<Arc<HaematiteStore>>` is retained separately (`state.rs:1038`).

### Haematite key scheme (`crates/aion-store-haematite/src/keyspace.rs`)

Add a fresh region tag `n:` alongside the existing six (the tag table at `keyspace.rs:14-22`; tags are single bytes that never collide with the `E`-tagged event keyspace):

```rust
/// Prefix for the namespace-registry region (Control-Plane Phase 1).
pub(crate) const NAMESPACE_PREFIX: &[u8] = b"n:";

/// Registry key for `name`: `n: || namespace_name`.
pub(crate) fn namespace_key(name: &str) -> Vec<u8> {
    composite(NAMESPACE_PREFIX, &[name.as_bytes()])
}
pub(crate) fn namespace_from_key(key: &[u8]) -> Option<String> {
    key.strip_prefix(NAMESPACE_PREFIX)
        .and_then(|s| String::from_utf8(s.to_vec()).ok())
}
```

`list_namespaces` enumerates via `scan_prefix(NAMESPACE_PREFIX)` with `prefix_upper_bound` (`keyspace.rs:168`) and sorts in memory — exactly as `list_packages` does (`store.rs:1910`).

### Haematite CAS upsert (`crates/aion-store-haematite/src/store.rs`)

Copy-adapt `publish_shard_owner` (`store.rs:600-640`) — the verified create-if-absent/value-CAS/reconcile pattern:

```rust
pub fn register_namespace_record(&self, name: &str, origin: NamespaceOrigin)
    -> Result<MintOutcome, StoreError>
{
    let Some(routing) = self.distribution.clone() else {
        // Single-node / non-distributed: plain local upsert (no quorum to reach).
        return self.local_namespace_upsert(name, origin);
    };
    let database = self.inner.database();
    let key = keyspace::namespace_key(name);
    let current = database.get(&key).map_err(|e| database_error(&e))?;     // store.rs:610 analogue
    if let Some(bytes) = &current {
        // Exists: refresh last_seen via value-CAS (idempotent touch).
        let expected = Some(Hash::of(bytes));
        let record = bump_last_seen(decode(bytes)?);
        let value = encode(&record);
        return match run_off_runtime(|| database.replicate_write(
                    key.clone(), expected, value, None, &routing.membership, routing.timeout)) {
            Ok(_) => Ok(MintOutcome::AlreadyExisted),
            Err(DatabaseError::CasConflict { .. }) => Ok(MintOutcome::AlreadyExisted), // racer touched first
            Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner { shard: database.shard_for(&key) }),
            Err(e) => Err(database_error(&e)),
        };
    }
    // Absent: create-if-absent (expected = None).            // store.rs:611 analogue
    let record = NamespaceRecord::new_minted(name, origin /*, now*/);
    let value = encode(&record);
    match run_off_runtime(|| database.replicate_write(
                key.clone(), None, value, None, &routing.membership, routing.timeout)) { // store.rs:612-621
        Ok(_) => Ok(MintOutcome::Created),
        // Concurrent racer minted first: reconcile — treat their record as success. store.rs:634-637
        Err(DatabaseError::CasConflict { .. }) => Ok(MintOutcome::AlreadyExisted),
        Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner { shard: database.shard_for(&key) }),
        Err(e) => Err(database_error(&e)),
    }
}
```

Key discriminations (verified `store.rs:622-638`): `Fenced` = deposed by higher ballot ⇒ `NotOwner`/abort; `CasConflict` = benign concurrent racer ⇒ re-read/treat-as-success (idempotent mint). This is *precisely* the storage map's "two workers raced to mint the same namespace; the loser observes the winner's record and proceeds."

### Shard routing + quorum implications (verified §3 of storage map)

- A record keyed `n: || name` routes to `shard_for(b"n:<name>")` = `BLAKE3(key) % shard_count` (`router.rs:20-27`). Namespaces **scatter across shards by name-hash**, each record living on one shard.
- The mint is a **quorum write** through `replicate_write`: it must reach the `WriteMembership` denominator (full cluster count, never the reachable subset — `store.rs:115-131`) and is fenced by that shard's epoch (`receiver.rs:97-110`).
- On owner death, the namespace record **travels with its shard's adoption/union-merge** exactly like event streams — the survivor that wins the shard election (`acquire_owned_shard`) reads the merged record. **This is what makes `GET /namespaces` survive failover.**
- **Shard-count trap (§6, VERIFIED):** the default single-node `shard_count = 1` (`config/mod.rs:1148`) means replication does not engage; the locked decision is to **raise the default virtual shard count well above 1** (power-of-two, validate vs perf audit #47). Phase 1 of the registry *depends on* that decision being made (it's a §7 open item, not a registry blocker — single-node still works via the local-upsert branch).

### Boot wiring (`crates/aion-server/src/state.rs`)

The leaf store is already shared three ways and the concrete `HaematiteStore` retained on distributed boot (`state.rs:1033-1038`). Add a fourth share:

- `state.rs:967-1054` (`connect_haematite_store`) / `ConnectedStore` (`state.rs:817-842`): add `namespace_store: Arc<dyn NamespaceStore>` populated from the same `leaf` Arc.
- `build_with_connected_store` (`state.rs:137-250`) threads it to the `NamespaceResolver` (`state.rs:218`, `NamespaceResolver::from_config`) and the worker registry. **This closes the exact gap the storage map names** — "today namespace resolution is config/engine-derived, NOT store-durable."

---

## 4. The minted-on-use write path

There are **two mint choke-points**, both verified:

### A. Worker registration (primary minter — §3 "Worker = the minter")

`WorkerRegistry::accept_registration` (`registry.rs:286-310`) is the inbound gRPC/liminal worker-register seam. It already (1) authorizes via `guard.scope(caller, &NamespaceOperation::register_worker(registration))` (`registry.rs:298-300`, op at `guard.rs:241-245`), then (2) scopes the worker's namespace **set** via `scope_worker_namespaces` (`registry.rs:301`). The mint hook slots **between auth and registry insertion**:

```rust
// registry.rs:286-310, after scope_worker_namespaces succeeds:
let namespaces = guard.scope_worker_namespaces(caller, &registration.namespaces)?;
// MINT HOOK (Phase 1): for each authorized namespace, upsert the durable record.
for ns in &namespaces {
    match self.auto_create {
        AutoCreate::Open => {
            if let MintOutcome::Created =
                self.namespace_store.register_namespace(ns, NamespaceOrigin::WorkerMint).await?
            {
                emit_namespace_created_event(ns, NamespaceOrigin::WorkerMint); // "loud" — §5
            }
        }
        AutoCreate::Closed => {
            if self.namespace_store.get_namespace(ns).await?.is_none()
               && !self.has_durable_state(ns).await? {           // STATE anchor, see below
                return Err(ServerError::namespace_denied(
                    "namespace does not exist and auto_create is closed"));
            }
        }
    }
}
```

Because auth (`guard.scope`) already ran, the mint is **auth-scoped by construction** — satisfying §4.1 (CVE-2025-14986: open-mint + isolation only coexist if minting is auth-scoped). Auth-off ⇒ operator identity with all-namespaces (`resolver.rs:122-132`) ⇒ mints freely; auth-on ⇒ scoped right.

### B. Start workflow (secondary minter / safety net)

The start handler (`workflows.rs:50-54`) already calls `guard.scope(caller, &NamespaceOperation::start(&request))` and derives the authorized `namespace` (`workflows.rs:54`). A start into a never-before-seen namespace should also mint (so a client that starts before any worker registers still gets a durable record). Insert the same mint hook right after `let namespace = scoped.namespace().to_owned();` (`workflows.rs:54`), before `start_search_attributes` (`workflows.rs:70`). The `aion.namespace` attribute stamp (`workflows.rs:106-121`, `resolver.rs:24`) is unchanged — registry mint is *additive*, the NSTQ binding stays immutable-by-construction.

### Idempotency

Guaranteed by the CAS-conflict-reconcile branch (`store.rs:634-637`): concurrent starts/registrations for the same new namespace all converge; exactly one observes `Created` (fires the event), the rest observe `AlreadyExisted`. No lock, no pre-check race.

### The "loud namespace created" event (§3, §7.5)

On first mint (`MintOutcome::Created` only), emit a durable, observable signal so the ops console + audit log get the "a new tenant appeared" signal that Temporal/Hatchet get from explicit `RegisterNamespace` — *without* the pre-provision tax (competitive §7.5). Two options (open decision §7): (a) a dedicated event-stream/log record, or (b) the durable record write *is* the event and the dashboard's socket-first push (per memory: dashboard real-time/socket-first) observes the registry delta. Recommend (b) + a structured `tracing` event for the audit log — minimal new surface, leverages existing push channels.

### Existence anchored on STATE, not workers (§3 critical correction)

A namespace **exists** if **durable state OR live worker OR explicit entry**. Implementation:

- The registry record is the *explicit entry / minted* path.
- `has_durable_state(ns)` is the **safety anchor**: even with no registry row (e.g. a pre-upgrade namespace, or one whose row was reaped), if `aion.namespace`-attributed history exists (the existing projection, `resolver.rs:284`; haematite per-workflow column `store.rs:798-802`), the namespace is treated as existing. This is what prevents a reaped row from orphaning history.
- **Back-fill / inference:** on boot or first access, a namespace with durable state but no registry row gets an `origin: InferredFromState` record lazily minted, so `GET /namespaces` is complete even for pre-registry data. (Open decision §7: eager boot scan vs lazy-on-reference — recommend lazy to avoid a full-history scan on every boot.)

This means **reaping is safe by construction**: nothing reaps a namespace that still has state, and even a buggy reap is recoverable via inference.

---

## 5. Read / API surface

### `GET /namespaces` — retire the stopgap

Replace `list_namespaces` (`workflows.rs:182-190`), which today returns `default_namespace` for the operator or `caller.namespaces()`:

```rust
pub(crate) async fn list_namespaces(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Result<Json<Vec<NamespaceSummary>>, HttpWireError> {
    let all = state.namespace_store().list_namespaces().await?;       // real durable set
    let visible = all.into_iter()
        .filter(|r| caller.can_access(&r.name))                       // resolver.rs:189-191
        .map(NamespaceSummary::from)
        .collect();
    Ok(Json(visible))
}
```

**Auth consistency (CVE-2025-14986 lesson, §4.1):** the durable set is filtered by `caller.can_access` at the **read hop** — the operator (`all_namespaces`, `resolver.rs:185-187`) sees all; an enumerated caller sees only granted names that *also exist durably*. This fixes the stopgap's silent conflation of "namespaces that exist" with "namespaces this caller may see" (competitive §7.5). The route stays as-is (`router.rs` `/namespaces -> get(...)`).

### `POST /namespaces` — explicit create

New handler + route. Calls `namespace_store.put_namespace(record)` with `origin: Explicit`, authorized against the caller's grant (must be able to access/mint the name; operator always can). Idempotent on an existing name (returns the existing record, `200`/`AlreadyExisted`). This is the `closed`-policy escape hatch: in a locked-down prod, an operator POSTs the namespace, then workers serve it.

### `auto_create` policy hook

New config knob under `[namespaces]` (sibling to `default` at `config/mod.rs:854`, `mode` at `config/mod.rs:327-336`): `auto_create: AutoCreate` (`Open | Closed`), **default `Open`**. Read into the `WorkerRegistry` and start handler mint hooks (§4). `Closed` makes the mint hook reject-if-absent instead of create. One config field, two call sites, default preserves zero-config.

### Auth-consistency at every hop (the through-line)

- **Mint hop** (register/start): auth already runs (`guard.scope`), mint is scoped (§4A).
- **Read hop** (`GET /namespaces`): filtered by `can_access`.
- **Access hop** (resolve): unchanged (`resolver.rs:481-507`) — grant check stays.
- **Existence vs visibility never leak**: a caller without a grant gets the same `not_found`/denied shape regardless of whether the namespace exists (the existing existence-leak guard, `resolver.rs:514-534`, is preserved — the registry must not introduce a new enumeration oracle).

---

## 6. Build slices (ordered, each PR-sized + independently gated)

| # | Slice | Touches | Gate (independently verifiable) |
|---|---|---|---|
| **S1** | `NamespaceRecord` + `NamespaceStore` trait + `MintOutcome`/enums; serde encode/decode | new `crates/aion-store/src/namespace.rs`; re-export in `aion-store/src/lib.rs` | Unit: round-trip encode/decode; trait compiles; `cargo doc`. No behavior change. |
| **S2** | Local default impl for in-memory + libSQL (a `namespaces` table/keyspace; no quorum) | `crates/aion-store-libsql/src/schema.rs` (add 7th table), new libsql `namespace.rs`; in-memory impl | Unit: upsert idempotency, `list` ordering, `get` miss/hit on both backends. |
| **S3** | Haematite impl: `n:` keyspace + `register_namespace_record` copy-adapted from `publish_shard_owner`; `list`/`get`/`deprecate` | `keyspace.rs` (tag `n:`), `store.rs` (CAS upsert + scan_prefix) | Unit: create-if-absent, value-CAS touch, **CasConflict reconcile = AlreadyExisted**, Fenced = NotOwner. Multi-node test: mint on A readable on B; mint survives owner kill (mirror existing failover tests). Requires shard_count>1 (§6 decision). |
| **S4** | Boot wiring: thread `Arc<dyn NamespaceStore>` through `ConnectedStore` → `ServerState` → resolver/registry | `state.rs:817-842,967-1054,137-250` | Server boots single-node + distributed; `namespace_store()` reachable; no regression in existing boot tests. |
| **S5** | Mint-on-register wiring + `auto_create` config + state-anchor (`has_durable_state`) + inference for pre-existing-state namespaces | `registry.rs:286-310`; `config/mod.rs` (`[namespaces].auto_create`); resolver state-anchor | Integration: register worker for new ns ⇒ durable record + `Created` once under concurrency; `closed` rejects unknown but allows has-state; reaped-row-with-state still resolves. |
| **S6** | Mint-on-start safety net + "loud created" event/signal | `workflows.rs:50-70` (mint hook); event emit | Integration: start into fresh ns mints; event fires exactly once on first mint; replay/idempotent on re-start. |
| **S7** | `GET /namespaces` durable-set + grant filter (retire stopgap); `POST /namespaces` explicit create | `workflows.rs:182-190`; new POST handler; `router.rs` | API test: list returns real durable set filtered by grant; operator sees all; POST is idempotent; existence-leak guard preserved. |
| **S8** | Ops-console surface: live namespace list + "namespace created" push; created/last_seen/origin columns | aion web dashboard (socket-first per memory) | Manual: new namespace appears live on the console when a worker registers; matches `GET /namespaces`. |

Dependency chain: S1 → {S2, S3} → S4 → S5 → S6 → S7 → S8. S2 and S3 parallelize after S1. Each slice is byte-identical-on-default for backends that don't implement the real path (no-op/local-only), so nothing regresses single-node behavior until S5 flips the mint on.

---

## 7. Open decisions for Tom + risks

**Decisions:**

1. **Separate `Arc<dyn NamespaceStore>` vs `EventStore` supertrait.** Recommend separate Arc in Phase 1 (smaller blast radius; haematite-only real impl; libSQL/in-mem trivial). Fold into the supertrait later only if every backend needs it threaded ubiquitously. (Storage map leans the same way.)

2. **Shard-count default (§6, blocking for the *replicated* path).** The registry's failover survivability needs `shard_count > 1`; default is `1` (`config/mod.rs:1148`). This is already a locked-strategy decision (raise to a generous power-of-two, validate vs perf audit #47). Registry doesn't block on it (single-node uses the local-upsert branch), but the **failover demo value** does. Confirm the number.

3. **"Loud created" mechanism.** Registry-delta-over-socket (leveraging the existing real-time dashboard channels) vs a dedicated event-stream record. Recommend socket-delta + structured tracing for audit. Tom's call on whether audit needs a *durable* event row.

4. **Eager boot inference vs lazy.** Back-filling `InferredFromState` records for pre-registry namespaces: eager (full scan at boot — costly) vs lazy (on first reference — recommend). Lazy means `GET /namespaces` may miss a dormant pre-registry namespace with state-but-no-traffic until it's referenced once.

5. **`tenant`/`kind` reservation (Hatchet §7.3).** Whether to reserve a `kind` discriminator now (namespace-IS-tenant vs sub-grouping) inside the record. Cheap to add to `config`; defers a Phase-2 migration if Tom wants the tenant⊃namespace split later. Recommend reserving it.

**Risks:**

- **Immutable NSTQ binding interaction.** The `aion.namespace` attribute is immutable-by-construction (append-only history, `resolver.rs:284-300`; no mismatch-on-reopen guard). The registry must **only add existence/mint**, never re-validate or re-bind a workflow's namespace on reopen — doing so could introduce a "namespace mismatch on reopen" failure that doesn't exist today. The mint hook is strictly *additive at start/register time*; recovery/replay paths are untouched. Verified: nothing in the recovery path reads the registry.

- **Reap-orphans-history.** Mitigated by the STATE anchor (§4) — but the `has_durable_state` check must be correct or a `closed`-policy + missing-row could deny access to a namespace that has live workflows. The inference fallback (`origin: InferredFromState`) is the backstop. Test this explicitly (S5 gate).

- **Existence-leak oracle.** `GET /namespaces` and the mint path must not let an unauthorized caller probe whether a namespace exists (CVE-2025-14986 family). The read filter (`can_access`) and the preserved `not_found`-symmetry guard (`resolver.rs:514-534`) cover this; review at S7.

- **Back-compat with default-only behavior.** Today everything works with the single `"default"` namespace and no registry. Slices S1-S4 are inert (no-op/local). S5 flips mint on with `auto_create=open` default ⇒ existing single-tenant deployments transparently get a `"default"` record minted on first register/start, and `GET /namespaces` starts returning the real set including `"default"`. Verify the operator/all-namespaces console path still shows `"default"` (it will, once minted) — this *replaces* the synthetic `default_namespace` echo (`workflows.rs:187`) with a real record.

- **Per-commit fsync fan-out** from raising shard_count (§6, perf audit caveat). The registry adds one quorum write per *new* namespace (rare, mint-once), negligible vs event-append volume — but the shard-count raise it implies has broader perf implications (per memory: haematite perf audit — commit fsync-amplification is a known hot path). Validate the shard number against #47, not the registry write itself.

---

**Files to touch (consolidated):** `crates/aion-store/src/namespace.rs` (new), `crates/aion-store/src/lib.rs`, `crates/aion-store/src/store.rs` (supertrait note only if folding), `crates/aion-store-libsql/src/schema.rs` + new `namespace.rs`, `crates/aion-store-haematite/src/keyspace.rs`, `crates/aion-store-haematite/src/store.rs`, `crates/aion-server/src/state.rs`, `crates/aion-server/src/config/mod.rs`, `crates/aion-server/src/worker/registry.rs`, `crates/aion-server/src/api/handlers/workflows.rs`, `crates/aion-server/src/api/http/workflows.rs` + `router.rs`, `crates/aion-server/src/namespace/resolver.rs`, plus the web ops-console.
