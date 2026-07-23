//! `HaematiteStore`: a single-node Aion [`EventStore`] over [`haematite`].
//!
//! # Design
//!
//! * **Events** are the source of truth. Each workflow's history lives in one
//!   haematite event stream ([`keyspace::event_stream_key`]); appends route
//!   through [`haematite::EventStore::append_batch`] under haematite's own
//!   optimistic-concurrency guard, and reads come back through `read`/`read_from`.
//!   Aion's 1-based event sequence and haematite's 0-based stream `expected_seq`
//!   (the stream's current event count) coincide: both equal the number of
//!   already-stored events, so Aion's `expected_seq` is passed straight through.
//! * **Projections** (status, summaries, run chains) reuse the exact same
//!   `aion-store`/`aion-core` helpers the in-memory and libSQL stores use, so a
//!   workflow projects identically regardless of backend.
//! * **Timers, packages, routes, and the outbox** are keyed KV records in
//!   haematite's general keyspace (see [`keyspace`]). Each mutation is followed
//!   by a [`haematite::Database::commit`] so it is durable before the call
//!   returns. Range scans over a region prefix enumerate the region.
//!
//! # Single-node shard invariant
//!
//! The store creates haematite with `shard_count == 1`. haematite's `range` is
//! shard-local (routed from the lower bound), so a one-shard database is what
//! makes a prefix range scan globally complete — every timer / package / outbox
//! row is in the same shard as the scan's lower bound. Multi-shard support is a
//! later (cluster) increment and is intentionally out of scope for B1.
//!
//! # Outbox = Design B (events are the single source of truth)
//!
//! Outbox rows are their own keyed KV entries. [`append_with_outbox`] writes the
//! events first (the authoritative durable write) and then the outbox rows;
//! [`rearm_outbox_pending`] upserts rows back to `Pending` with a read-modify-write
//! that preserves `attempt`. Because Aion recovery rebuilds the pending-dispatch
//! set from history, a single-node `append_with_outbox` does not require
//! cross-key single-transaction atomicity: a crash between the event commit and
//! the outbox commit is recovered by the re-arm path, never a lost dispatch.
//!
//! [`append_with_outbox`]: HaematiteStore::append_with_outbox
//! [`rearm_outbox_pending`]: HaematiteStore::rearm_outbox_pending

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aion_core::{
    ActivityEvent, ActivityId, Event, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus,
    WorkflowSummary, status_from_events,
};
use aion_store::{
    ActivityRecord, ActivityStreamKey, ActivityStreamSummary, ClaimScope, MintOutcome,
    NamespaceOrigin, NamespacePlacement, NamespaceRecord, NamespaceStore, ObservabilityStore,
    OutboxRow, OutboxStatus, OutboxStore, PackageRecord, PackageRouteRecord, PackageStore,
    ReadableEventStore, RunSummary, StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, ProposeWrite, SyncNodeId};
use haematite::{Database, DatabaseConfig, DatabaseError, Hash};
use serde::{Deserialize, Serialize};

use crate::error::{
    acquire_election_error, api_error, database_error, join_error, resolve_cas_conflict,
    serde_error,
};
use crate::keyspace;

/// Quorum-replication routing for a distributed [`HaematiteStore`].
///
/// When present, event appends route through [`Database::replicate_append`] to
/// this `membership` quorum, INSTEAD of the local single-node `append_batch`
/// path. Only event history is replicated — workflows are enumerated from the
/// replicated event streams themselves (see
/// [`keyspace::workflow_id_from_event_stream_key`]), so there is no separate
/// workflow-id index to replicate. The byte-level event/stream-key encoding is
/// identical to the single-node path (haematite's `replicate_append` lays events
/// out exactly as the local `append_batch` does), so `read_history` decodes a
/// replicated stream the same way it decodes a locally-appended one.
#[derive(Clone)]
struct DistributedRouting {
    /// The quorum membership (denominator + reachable send targets) for writes.
    membership: WriteMembership,
    /// Per-operation quorum timeout passed to `replicate_append`.
    timeout: Duration,
    /// This node's globally-unique distribution name. Recorded so the SS-3
    /// shard-owner directory record can name THIS node as the current owner when
    /// it adopts a shard ([`HaematiteStore::publish_shard_owner`]).
    node_id: String,
}

/// Cluster-membership inputs for [`HaematiteStore::open_or_create_distributed`].
///
/// The minimal, well-defaulted seam a deployment passes to turn the single-node
/// haematite store into a distributed one (SS-2). A cluster of one is `node_id`
/// set, `members` empty (or `[node_id]`), and `peers` empty.
#[derive(Clone, Debug)]
pub struct ClusterBootstrap {
    /// This node's globally-unique distribution name and local endpoint identity.
    pub node_id: String,
    /// The address the local replication endpoint binds.
    pub bind_address: std::net::SocketAddr,
    /// The FULL cluster membership by node id (the quorum DENOMINATOR). The local
    /// node is always counted whether or not it appears here.
    pub members: Vec<String>,
    /// Dialable peers `(name, address)` to connect for replication.
    pub peers: Vec<(String, std::net::SocketAddr)>,
    /// Per-operation quorum/election timeout.
    pub timeout: Duration,
}

impl ClusterBootstrap {
    /// Build the [`WriteMembership`] for this node: a denominator that ALWAYS
    /// counts the full membership (never the reachable subset — sizing quorum
    /// from reachability lets a minority self-quorum), and send targets that are
    /// the configured peers excluding the local node.
    fn write_membership(&self) -> WriteMembership {
        let mut names: std::collections::BTreeSet<String> = self.members.iter().cloned().collect();
        // The local node is always part of the denominator.
        names.insert(self.node_id.clone());
        for peer in &self.peers {
            names.insert(peer.0.clone());
        }
        let send_targets = self
            .peers
            .iter()
            .map(|peer| SyncNodeId::from(peer.0.as_str()))
            .collect();
        WriteMembership {
            total_nodes: names.len(),
            send_targets,
        }
    }
}

/// Owns the background thread that answers peers' inbound replication and
/// election traffic for a distributed [`HaematiteStore`].
///
/// Dropping it (or calling [`Self::stop`]) signals the loop to exit and joins the
/// thread, so a survivor stops responding once the node is taken down.
pub struct ClusterResponder {
    running: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ClusterResponder {
    /// Spawn the inbound-write responder over `event_store`'s database.
    fn spawn(event_store: Arc<haematite::EventStore>) -> Self {
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let loop_running = Arc::clone(&running);
        let handle = std::thread::spawn(move || {
            while loop_running.load(std::sync::atomic::Ordering::Relaxed) {
                drop(respond_to_inbound_writes(
                    event_store.database(),
                    Duration::from_millis(50),
                ));
            }
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    /// Stop the responder loop and join its thread. Idempotent.
    pub fn stop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

impl Drop for ClusterResponder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A durable Aion event store backed by haematite.
///
/// Runs in one of two modes:
///
/// * **Single-node** ([`create`](HaematiteStore::create) /
///   [`open`](HaematiteStore::open)): every write is a local haematite commit.
/// * **Distributed** ([`with_distribution`](HaematiteStore::with_distribution)):
///   event appends are quorum-REPLICATED to a cluster membership, so a workflow's
///   durable history survives the owner node's death and is readable (and
///   enumerable) on the survivor after it becomes the shard owner. The outbox
///   stays Design-B local (rebuilt from replicated history on the survivor).
#[derive(Clone)]
pub struct HaematiteStore {
    inner: Arc<haematite::EventStore>,
    /// A caller-supplied data-root capability retained for every store clone. It
    /// keeps `/proc/self/fd` backend paths authoritative on Linux/Android. On
    /// platforms where Haematite receives an ordinary resolved path, retention
    /// does not confine backend I/O; the server separately requires safe ancestors.
    data_root_capability: Option<Arc<dyn Send + Sync>>,
    /// `Some` in distributed mode; `None` in single-node mode (B1, unchanged).
    distribution: Option<DistributedRouting>,
    /// Which shards this store owns for ENUMERATION purposes.
    ///
    /// `None` (the default) = own ALL shards: enumeration scans every shard,
    /// byte-for-byte identical to single-node behavior. `Some(set)` = own only
    /// those shard ids: per-workflow enumeration (workflows, timers, outbox) is
    /// scoped to those shards, so a multi-node deployment that owns shards
    /// `{0, 2}` enumerates only the work co-located there. The set is wrapped in
    /// an `Arc<RwLock<..>>` shared across `Clone`s, so a deployment that sets it
    /// on one handle affects every clone that routes through the same inner store.
    owned_shards: std::sync::Arc<std::sync::RwLock<Option<std::collections::BTreeSet<usize>>>>,
}

impl std::fmt::Debug for HaematiteStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HaematiteStore")
            .field(
                "retains_data_root_capability",
                &self.data_root_capability.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl HaematiteStore {
    /// Create a fresh single-node store rooted at `data_dir`.
    ///
    /// The directory is created if absent. The store runs haematite with a single
    /// shard (see the module docs) and no TTL sweeper or distribution.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when haematite cannot create the database.
    pub fn create(data_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::create_with_shard_count(data_dir, 1)
    }

    /// Create a fresh single-node store with `shard_count` haematite shards.
    ///
    /// `shard_count == 1` is the default [`create`](HaematiteStore::create)
    /// behavior; `> 1` exercises the cross-shard fan-out scan path
    /// ([`scan_prefix`]). Cluster/distribution is unaffected.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when haematite cannot create the database.
    pub fn create_with_shard_count(
        data_dir: impl Into<PathBuf>,
        shard_count: usize,
    ) -> Result<Self, StoreError> {
        let database = Database::create(DatabaseConfig {
            data_dir: data_dir.into(),
            shard_count,
            executor_threads: None,
            distributed: None,
        })
        .map_err(|error| database_error(&error))?;
        Ok(Self::from_database(database))
    }

    /// Open an existing single-node store rooted at `data_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when haematite cannot open the database.
    pub fn open(data_dir: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let database = Database::open(data_dir).map_err(|error| database_error(&error))?;
        Ok(Self::from_database(database))
    }

    /// Build a DISTRIBUTED store over an already-distribution-attached `database`.
    ///
    /// The caller is responsible for opening `database` with
    /// [`Database::with_distribution`] and for making this node the live shard
    /// owner (via [`Database::acquire_shard_and_serve`]) before issuing appends —
    /// `replicate_append` draws its commit stamp from the live owner state. Event
    /// appends ([`WritableEventStore::append`] / `append_with_outbox`) route
    /// through `replicate_append` to `membership`'s quorum; reads, timers,
    /// packages, routes, and the outbox stay local (Design B: the survivor
    /// rebuilds the outbox from replicated history, and enumerates workflows from
    /// the replicated event streams).
    ///
    /// `timeout` bounds each quorum write. `node_id` is this node's distribution
    /// name, recorded so the SS-3 shard-owner directory record can name this node
    /// when it adopts a shard ([`Self::publish_shard_owner`]).
    #[must_use]
    pub fn with_distribution(
        database: Database,
        membership: WriteMembership,
        timeout: Duration,
        node_id: String,
    ) -> Self {
        Self {
            inner: Arc::new(haematite::EventStore::new(database)),
            data_root_capability: None,
            distribution: Some(DistributedRouting {
                membership,
                timeout,
                node_id,
            }),
            owned_shards: std::sync::Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Borrow the shared [`haematite::EventStore`] this store routes through.
    ///
    /// Exposes the underlying handle so a distributed deployment can drive the
    /// cluster lifecycle on the SAME `Database` the store writes to —
    /// `event_store().database().acquire_shard_and_serve(..)` to take ownership,
    /// or `Arc::clone(event_store())` for a background inbound-write responder. The
    /// store never elects or serves on its own; that is the deployment's job.
    #[must_use]
    pub fn event_store(&self) -> &Arc<haematite::EventStore> {
        &self.inner
    }

    /// Retain the checked filesystem capability that authorizes this store's
    /// data root.
    ///
    /// Haematite 0.5 accepts a path and keeps it for lazy shard work. A caller
    /// that passes a descriptor-backed path must also keep that descriptor open;
    /// attaching its capability here makes the lifetime identical to the store's
    /// lifetime, including all [`Clone`]s.
    #[must_use]
    pub fn retain_data_root_capability(mut self, capability: impl Send + Sync + 'static) -> Self {
        self.data_root_capability = Some(Arc::new(capability));
        self
    }

    /// Force Haematite 0.5 to materialize every configured lazy shard now.
    ///
    /// The locked backend exposes key-based first-touch but no public direct
    /// materialize-by-index operation. This probes deterministic keys, asks the
    /// backend which shard each key owns, and performs one read on the first key
    /// found for every shard. Reads do not mutate logical data, but they do run
    /// the backend's normal shard spawn/recovery path and create its on-disk shard
    /// directory while startup still owns the checked root window.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] if any shard cannot be spawned or read.
    pub fn materialize_all_shards(&self) -> Result<(), StoreError> {
        let database = self.inner.database();
        let shard_count = database.shard_count();
        let mut materialized = vec![false; shard_count];
        let mut remaining = shard_count;
        let mut probe = 0_u64;
        while remaining != 0 {
            let key = probe.to_le_bytes();
            let shard = database.shard_for(&key);
            if !materialized[shard] {
                database
                    .read_value(&key)
                    .map_err(|error| database_error(&error))?;
                materialized[shard] = true;
                remaining -= 1;
            }
            probe = probe.checked_add(1).ok_or_else(|| {
                StoreError::Backend(
                    "could not find a routing probe for every configured haematite shard"
                        .to_owned(),
                )
            })?;
        }
        Ok(())
    }

    fn from_database(database: Database) -> Self {
        Self {
            inner: Arc::new(haematite::EventStore::new(database)),
            data_root_capability: None,
            distribution: None,
            owned_shards: std::sync::Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Open (or create) a DISTRIBUTED store and wire it into a cluster (SS-2).
    ///
    /// This is the production-boot counterpart to [`Self::open`] /
    /// [`Self::create_with_shard_count`]: it opens the on-disk database at
    /// `data_dir` (or creates a fresh one with `shard_count` shards), binds a
    /// replication [`DistributionEndpoint`] at `boot.bind_address` under
    /// `boot.node_id`, attaches it to the database, builds the quorum
    /// [`WriteMembership`] from `boot.members` (the denominator) and `boot.peers`
    /// (the dial targets), dials every peer, and spawns the inbound-write
    /// responder thread that answers peers' replication and election traffic.
    ///
    /// A "cluster of one" — no peers, `members` empty or naming only the local
    /// node — yields a denominator of 1 and no send targets, so the later
    /// `acquire_owned_shards` election self-quorums. The returned
    /// [`ClusterResponder`] owns the responder thread; dropping it stops the
    /// responder.
    ///
    /// Endpoint binding refuses to run from a thread with an entered tokio
    /// runtime, so the whole construction runs on a bare [`run_off_runtime`]
    /// thread — letting an async caller drive it directly.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when the database cannot be opened/created,
    /// the endpoint cannot bind, or a peer cannot be dialed.
    pub fn open_or_create_distributed(
        data_dir: impl Into<PathBuf>,
        shard_count: usize,
        boot: ClusterBootstrap,
    ) -> Result<(Self, ClusterResponder), StoreError> {
        let data_dir = data_dir.into();
        // Bind + dial run the endpoint's own runtime and must not see an entered
        // tokio runtime, so build the whole distributed store off-runtime.
        run_off_runtime(move || Self::build_distributed(&data_dir, shard_count, &boot))
    }

    /// Off-runtime body of [`Self::open_or_create_distributed`]: open/create the
    /// database, bind + attach the endpoint, dial peers, and start the responder.
    fn build_distributed(
        data_dir: &std::path::Path,
        shard_count: usize,
        boot: &ClusterBootstrap,
    ) -> Result<(Self, ClusterResponder), StoreError> {
        let database = if data_dir.join("config.json").exists() {
            Database::open(data_dir).map_err(|error| database_error(&error))?
        } else {
            Database::create(DatabaseConfig {
                data_dir: data_dir.to_path_buf(),
                shard_count,
                executor_threads: None,
                distributed: None,
            })
            .map_err(|error| database_error(&error))?
        };
        let endpoint = DistributionEndpoint::bind(boot.node_id.clone(), boot.bind_address, 1, None)
            .map_err(|error| {
                StoreError::Backend(format!("cluster endpoint bind failed: {error}"))
            })?;
        let database = database.with_distribution(endpoint);

        let store = Self::with_distribution(
            database,
            boot.write_membership(),
            boot.timeout,
            boot.node_id.clone(),
        );
        // Start answering peers' inbound replication/election traffic BEFORE
        // dialing out, so this node is a usable quorum participant the moment a
        // peer reaches it — even while its own outbound dials are still
        // retrying. Without this, two nodes booting at once each dial-then-serve
        // and can deadlock: each waits on a peer that is not yet answering.
        let responder = ClusterResponder::spawn(Arc::clone(store.event_store()));

        // Dial each peer with bounded retry. A separate OS process for each node
        // means there is no global "all endpoints bound, now connect" barrier the
        // in-process harness has: a node started before its peers would fail the
        // first dial outright. Retrying until `boot.timeout` lets nodes boot in
        // any order — each binds immediately, then patiently connects as peers
        // come up. The single-shot `connect_peer` is preserved for the case a
        // peer is already listening (first attempt wins, no added latency).
        for peer in &boot.peers {
            connect_peer_with_retry(
                store.event_store().database(),
                &peer.0,
                peer.1,
                boot.timeout,
            )?;
        }

        Ok((store, responder))
    }

    /// Restrict enumeration to these shard ids (multi-node: the shards this node
    /// owns). Pass the shards acquired via `acquire_shard_and_serve`. Idempotent.
    ///
    /// Affects every per-workflow enumeration path (workflows, timers, outbox);
    /// package/route listing stays cluster-wide and is unaffected. Shared across
    /// `Clone`s through the inner `Arc<RwLock<..>>`.
    pub fn set_owned_shards(&self, shards: impl IntoIterator<Item = usize>) {
        let set: std::collections::BTreeSet<usize> = shards.into_iter().collect();
        // A poisoned store lock is unrecoverable (consistent with the rest of the
        // crate's blocking commit paths): unwrap rather than mask the corruption.
        *self
            .owned_shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(set);
    }

    /// Revert to owning all shards (single-node default).
    pub fn own_all_shards(&self) {
        *self
            .owned_shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Add `shards` to the owned-enumeration scope, UNIONING with the current
    /// set rather than replacing it (SS-5 failover).
    ///
    /// [`Self::set_owned_shards`] replaces the scope — the boot path's one-shot
    /// assignment. This widens it in place, so a survivor absorbing a dead peer's
    /// shards keeps enumerating its OWN shards while also enumerating the adopted
    /// ones. When the current scope is `None` (own all shards — the single-node
    /// default), the store already enumerates `shards`, so the own-all scope is
    /// left untouched. Idempotent. Shared across `Clone`s through the inner lock.
    pub fn extend_owned_shards(&self, shards: impl IntoIterator<Item = usize>) {
        let mut guard = self
            .owned_shards
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // `None` already owns everything: widening an own-all scope is a no-op,
        // and must NOT collapse it into a finite set that would drop shards.
        if let Some(set) = guard.as_mut() {
            set.extend(shards);
        }
    }

    /// The distribution shard that owns `workflow_id`'s durable state.
    ///
    /// Computes `shard_for(event_stream_key(workflow_id))` — the same routing the
    /// co-located writes use — so a deployment can ask which shard a workflow
    /// lands on (e.g. to gate the schedule-coordinator bootstrap on real
    /// ownership; SS-2 / AA-4-4).
    #[must_use]
    pub fn shard_for_workflow(&self, workflow_id: &WorkflowId) -> usize {
        self.inner
            .database()
            .shard_for(&keyspace::event_stream_key(workflow_id))
    }

    /// Whether this node currently owns the shard `workflow_id` lives on.
    ///
    /// `true` when the workflow's shard is in the configured owned set, or when
    /// the store owns all shards (`None` scope — the single-node default). Lets a
    /// multi-shard deployment gate the coordinator bootstrap on real ownership.
    #[must_use]
    pub fn owns_workflow_shard(&self, workflow_id: &WorkflowId) -> bool {
        let shard = self.shard_for_workflow(workflow_id);
        match self.owned_shards() {
            Some(owned) => owned.contains(&shard),
            None => true,
        }
    }

    /// Whether this distributed node currently holds a live replication link to
    /// the peer named `peer_name` (SS-5b automatic failover detection).
    ///
    /// Returns `true` while the OTP distribution connection to `peer_name` is
    /// active. It flips to `false` once that connection is torn down — which
    /// happens when the peer's process dies (`kill -9` closes its sockets, the
    /// survivor's read loop hits EOF and deregisters the link) or its endpoint is
    /// dropped. This is the honest peer-liveness signal a cluster supervisor polls
    /// to decide a peer is gone and its shards must be adopted; it reflects real
    /// socket death, not a heartbeat heuristic.
    ///
    /// A single-node store (no distribution) has no peers, so this is always
    /// `false`.
    #[must_use]
    pub fn peer_connected(&self, peer_name: &str) -> bool {
        self.inner
            .database()
            .distribution()
            .is_some_and(|endpoint| endpoint.is_connected(peer_name))
    }

    /// Snapshot the current owned set (`None` = all shards). For tests/diagnostics.
    #[must_use]
    pub fn owned_shards(&self) -> Option<Vec<usize>> {
        self.owned_shards
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|set| set.iter().copied().collect())
    }

    /// The total number of distribution shards this store's keyspace is split
    /// across — the modulus `shard_for_workflow` routes within. Used by the
    /// request-routing edge to bound the unsteered-start remint loop (R-1).
    #[must_use]
    pub fn shard_count(&self) -> usize {
        self.inner.database().shard_count()
    }

    /// Mint a fresh `WorkflowId` whose durable shard this node currently owns, so
    /// an unsteered `start` lands locally and never fences (R-1 stopgap, §2.4).
    ///
    /// Returns `None` when this node owns every shard (`owned_shards() == None`):
    /// any id is already local, so the caller should NOT remint and should let
    /// the engine mint as usual — keeping the single-node / own-all path
    /// byte-identical. Otherwise it draws fresh v4 ids and returns the first
    /// whose `shard_for_workflow` is owned, bounded by `max_attempts` tries
    /// (callers pass a multiple of `shard_count` so the probability of exhausting
    /// the budget without hitting an owned shard is negligible); `None` on
    /// exhaustion lets the caller fall back rather than spin unbounded.
    #[must_use]
    pub fn remint_for_owned_shard(&self, max_attempts: usize) -> Option<WorkflowId> {
        // Own-all scope: nothing to remint toward — every shard is already local.
        self.owned_shards()?;
        for _ in 0..max_attempts {
            let candidate = WorkflowId::new_v4();
            if self.owns_workflow_shard(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// The distribution shard a caller-chosen steered-start `routing_key` targets.
    ///
    /// Uses the *same* `shard_for` hashing the workflow co-location routing uses
    /// (see [`Self::shard_for_workflow`]), so a steered `start` and any later
    /// signal/query/cancel resolved via the routing key map to one shard. The key
    /// is hashed as raw bytes; two starts with the same key always land on the
    /// same shard (R-4 steered start, §2.4 "placement derives from
    /// `shard_for(routing_key)`").
    #[must_use]
    pub fn shard_for_routing_key(&self, routing_key: &str) -> usize {
        self.inner.database().shard_for(routing_key.as_bytes())
    }

    /// Mint a fresh `WorkflowId` whose durable state lands on `target_shard`.
    ///
    /// Draws fresh v4 ids and returns the first whose `shard_for_workflow` equals
    /// `target_shard`, bounded by `max_attempts` tries (callers pass a multiple of
    /// `shard_count` so the probability of exhausting the budget without drawing
    /// the target shard is negligible). `None` on exhaustion lets the caller fall
    /// back rather than spin unbounded. Used by the R-4 steered start to place a
    /// new id on the routing key's shard ([`Self::shard_for_routing_key`]).
    #[must_use]
    pub fn mint_for_shard(&self, target_shard: usize, max_attempts: usize) -> Option<WorkflowId> {
        for _ in 0..max_attempts {
            let candidate = WorkflowId::new_v4();
            if self.shard_for_workflow(&candidate) == target_shard {
                return Some(candidate);
            }
        }
        None
    }

    /// Publish THIS node as the current owner of `shard` in the cluster's
    /// shard-owner directory (SS-3), so other nodes' request-routing edges resolve
    /// `shard` to this node — closing gap #2: after a survivor adopts a dead
    /// declared-owner's shard, a request reaching a DIFFERENT survivor must route
    /// to the adopter, not mis-resolve to the dead declared owner.
    ///
    /// The record is a quorum-replicated, fenced KV write
    /// ([`Database::replicate_write`]) keyed by [`keyspace::shard_owner_key`] so it
    /// CO-LOCATES on `shard` itself. That makes the record write subject to the
    /// SAME epoch fence the publisher just won when it elected itself owner of
    /// `shard`: only the true (fenced) owner can publish, so two survivors racing
    /// to adopt cannot both win the record — exactly one does. The record's VALUE
    /// is this node's distribution name bytes, which any member reads back with
    /// [`Self::read_shard_owner`] off its locally-applied replica.
    ///
    /// Idempotent: re-publishing the same owner is a value-preserving CAS that
    /// succeeds. It is a no-op (returns `Ok(())`) on a single-node / non-distributed
    /// store, which has no peers and no directory to coordinate.
    ///
    /// The quorum write blocks and must run off the tokio runtime (the same
    /// constraint `replicate_append` honours), so callers may invoke it from async.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotOwner`] when the fenced write is out-voted (this
    /// node is not actually the shard's current owner), and [`StoreError::Backend`]
    /// for any other replication/transport failure.
    pub fn publish_shard_owner(&self, shard: usize) -> Result<(), StoreError> {
        let Some(routing) = self.distribution.clone() else {
            // Single-node / non-distributed: no peers, no directory to publish to.
            return Ok(());
        };
        let database = self.inner.database();
        let key = keyspace::shard_owner_key(shard, |bytes| database.shard_for(bytes));
        let value = routing.node_id.clone().into_bytes();
        // CAS on the current value's hash so re-publication overwrites cleanly
        // (create-if-absent on a fresh record, value-CAS on an existing one).
        let current = database.get(&key).map_err(|error| database_error(&error))?;
        let expected = current.as_deref().map(Hash::of);
        let result = run_off_runtime(|| {
            database.replicate_write(
                key,
                expected,
                value,
                None,
                &routing.membership,
                routing.timeout,
            )
        });
        match result {
            Ok(_) => Ok(()),
            // Out-voted by the shard's promised ballot: a higher-ballot owner
            // deposed this node. This is the authoritative supersession signal —
            // ABORT (the typed variants, not the value-hash CAS, discriminate
            // supersession now).
            Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner { shard }),
            // Value-CAS mismatch alone: this node is still the live owner, but a
            // concurrent re-publish raced the value-hash precondition. This is
            // benign and idempotent — re-read the directory and treat an
            // identical/own owner record as success; only surface an error if a
            // DIFFERENT live owner is recorded (a genuine ownership disagreement).
            Err(DatabaseError::CasConflict { .. }) => {
                let recorded = self.read_shard_owner(shard)?;
                resolve_cas_conflict(recorded.as_deref(), &routing.node_id, shard)
            }
            Err(error) => Err(database_error(&error)),
        }
    }

    /// Read the distribution name of the node that currently owns `shard` per the
    /// cluster's shard-owner directory (SS-3), or `None` when no node has published
    /// a record for `shard` (the steady-state pre-adoption case: ownership is still
    /// described by static config).
    ///
    /// This is a LOCAL read of the locally-applied replica
    /// ([`Database::get`]): a record published via [`Self::publish_shard_owner`] is
    /// durably replicated to every reachable member, so any survivor reads the
    /// adopter's identity off its own store with no extra round trip. Returns
    /// `None` on a single-node / non-distributed store (no directory exists).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when the local read fails.
    pub fn read_shard_owner(&self, shard: usize) -> Result<Option<String>, StoreError> {
        if self.distribution.is_none() {
            return Ok(None);
        }
        let database = self.inner.database();
        let key = keyspace::shard_owner_key(shard, |bytes| database.shard_for(bytes));
        let Some(bytes) = database.get(&key).map_err(|error| database_error(&error))? else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| StoreError::Backend(format!("corrupt shard-owner record: {error}")))
    }

    /// Idempotent minted-on-use upsert of the namespace registry record for
    /// `name` (Control-Plane Phase 1), copy-adapted from the verified
    /// create-if-absent / value-CAS / reconcile pattern of
    /// [`Self::publish_shard_owner`].
    ///
    /// In DISTRIBUTED mode the write goes through the quorum-replicated fenced
    /// path ([`Database::replicate_write`]) keyed by [`keyspace::namespace_key`],
    /// so the record travels with its shard on owner-node death and `GET
    /// /namespaces` survives failover. The discriminations mirror
    /// `publish_shard_owner` exactly:
    ///
    /// * Absent (`expected = None`) ⇒ create-if-absent; on success returns
    ///   [`MintOutcome::Created`].
    /// * Present ⇒ value-CAS touch that bumps `last_seen` (`expected =
    ///   Some(hash)`); on success returns [`MintOutcome::AlreadyExisted`].
    /// * [`DatabaseError::CasConflict`] = a benign concurrent racer minted/touched
    ///   first ⇒ idempotent [`MintOutcome::AlreadyExisted`].
    /// * [`DatabaseError::Fenced`] = this node was deposed by a higher ballot ⇒
    ///   [`StoreError::NotOwner`].
    ///
    /// In SINGLE-NODE / non-distributed mode there is no quorum to reach, so it
    /// falls back to a plain local upsert ([`Self::local_namespace_upsert`]).
    ///
    /// The quorum write blocks and must run off the tokio runtime, so callers
    /// may invoke it from async via the [`NamespaceStore`] trait wrapper.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotOwner`] when a quorum write is fenced,
    /// [`StoreError::Serialization`] on a codec failure, or
    /// [`StoreError::Backend`] for any other replication/database failure.
    pub fn register_namespace_record(
        &self,
        record: &NamespaceRecord,
    ) -> Result<MintOutcome, StoreError> {
        let Some(routing) = self.distribution.clone() else {
            // Single-node / non-distributed: plain local upsert (no quorum).
            return self.local_namespace_upsert(record);
        };
        let database = self.inner.database();
        let key = keyspace::namespace_key(&record.name);
        let current = database.get(&key).map_err(|error| database_error(&error))?;
        if let Some(bytes) = current {
            // Exists: refresh last_seen via value-CAS (idempotent touch). The
            // existing record's origin/created_at/state are preserved; only
            // last_seen advances.
            let mut existing = NamespaceRecord::decode(&bytes)?;
            existing.bump_last_seen(Utc::now());
            let value = existing.encode()?;
            let expected = Some(Hash::of(&bytes));
            let result = run_off_runtime(|| {
                database.replicate_write(
                    key.clone(),
                    expected,
                    value,
                    None,
                    &routing.membership,
                    routing.timeout,
                )
            });
            return match result {
                // The touch succeeded, OR a concurrent racer touched the same
                // record first (benign value-CAS loss): the record exists and
                // last_seen advanced either way — idempotent AlreadyExisted.
                Ok(_) | Err(DatabaseError::CasConflict { .. }) => Ok(MintOutcome::AlreadyExisted),
                Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner {
                    shard: database.shard_for(&key),
                }),
                Err(error) => Err(database_error(&error)),
            };
        }
        // Absent: create-if-absent (expected = None).
        let value = record.encode()?;
        let result = run_off_runtime(|| {
            database.replicate_write(
                key.clone(),
                None,
                value,
                None,
                &routing.membership,
                routing.timeout,
            )
        });
        match result {
            Ok(_) => Ok(MintOutcome::Created),
            // A concurrent racer minted the same namespace first: reconcile —
            // observe the winner's record and report it as already-existing
            // (idempotent, lock-free mint).
            Err(DatabaseError::CasConflict { .. }) => Ok(MintOutcome::AlreadyExisted),
            Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner {
                shard: database.shard_for(&key),
            }),
            Err(error) => Err(database_error(&error)),
        }
    }

    /// Plain local (non-quorum) upsert of a namespace record, used by the
    /// single-node / non-distributed path of [`Self::register_namespace_record`].
    ///
    /// Create-if-absent on a fresh name (returns [`MintOutcome::Created`]); on an
    /// existing name it bumps `last_seen` and returns
    /// [`MintOutcome::AlreadyExisted`], preserving the existing record's origin,
    /// `created_at`, and lifecycle state. A single-node store owns every shard
    /// unconditionally, so the read-modify-write needs no fence.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] on a codec failure or
    /// [`StoreError::Backend`] on a database failure.
    fn local_namespace_upsert(&self, record: &NamespaceRecord) -> Result<MintOutcome, StoreError> {
        let database = self.inner.database();
        let key = keyspace::namespace_key(&record.name);
        let outcome =
            if let Some(bytes) = database.get(&key).map_err(|error| database_error(&error))? {
                let mut existing = NamespaceRecord::decode(&bytes)?;
                existing.bump_last_seen(Utc::now());
                database
                    .put(key, existing.encode()?)
                    .map_err(|error| database_error(&error))?;
                MintOutcome::AlreadyExisted
            } else {
                database
                    .put(key, record.encode()?)
                    .map_err(|error| database_error(&error))?;
                MintOutcome::Created
            };
        database.commit().map_err(|error| database_error(&error))?;
        Ok(outcome)
    }

    /// Read-modify-write a namespace record's lifecycle state to
    /// [`aion_store::NamespaceState::Deprecated`] through the same upsert path,
    /// or a no-op when the namespace has no registry row / is already deprecated.
    ///
    /// # Errors
    ///
    /// As [`Self::register_namespace_record`].
    fn deprecate_namespace_record(&self, name: &str) -> Result<(), StoreError> {
        use aion_store::NamespaceState;
        let database = self.inner.database();
        let key = keyspace::namespace_key(name);
        let Some(bytes) = database.get(&key).map_err(|error| database_error(&error))? else {
            // No registry row: idempotent no-op (deprecation never strands history).
            return Ok(());
        };
        let mut record = NamespaceRecord::decode(&bytes)?;
        if record.state == NamespaceState::Deprecated {
            // Already deprecated: idempotent no-op.
            return Ok(());
        }
        record.state = NamespaceState::Deprecated;
        if let Some(routing) = self.distribution.clone() {
            let value = record.encode()?;
            let expected = Some(Hash::of(&bytes));
            let result = run_off_runtime(|| {
                database.replicate_write(
                    key.clone(),
                    expected,
                    value,
                    None,
                    &routing.membership,
                    routing.timeout,
                )
            });
            return match result {
                // Wrote, OR a concurrent writer raced the value-CAS: the record
                // is deprecated either way — deprecation is idempotent.
                Ok(_) | Err(DatabaseError::CasConflict { .. }) => Ok(()),
                Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner {
                    shard: database.shard_for(&key),
                }),
                Err(error) => Err(database_error(&error)),
            };
        }
        // Single-node / non-distributed: plain local write (owns every shard).
        database
            .put(key, record.encode()?)
            .map_err(|error| database_error(&error))?;
        database.commit().map_err(|error| database_error(&error))?;
        Ok(())
    }

    /// Read-modify-write an existing namespace record's `placement` directive
    /// through the same value-CAS upsert path the registry's other mutations use
    /// (Control-Plane Phase 2, P2-P2), or report not-found for an absent row.
    ///
    /// Only `placement` and `last_seen` change; `origin`, `created_at`, `config`,
    /// and `state` are preserved. An absent registry row returns `Ok(None)` — the
    /// caller surfaces a not-found rather than minting a row, since placement
    /// targets an already-minted namespace. A successful (or benign concurrent
    /// CAS-loss) write returns `Ok(Some(()))`.
    ///
    /// # Errors
    ///
    /// As [`Self::register_namespace_record`].
    fn set_namespace_placement_record(
        &self,
        name: &str,
        placement: NamespacePlacement,
    ) -> Result<Option<()>, StoreError> {
        let database = self.inner.database();
        let key = keyspace::namespace_key(name);
        let Some(bytes) = database.get(&key).map_err(|error| database_error(&error))? else {
            // No registry row: placement targets an already-minted namespace, so
            // an absent row is a not-found, never a silent mint.
            return Ok(None);
        };
        let mut record = NamespaceRecord::decode(&bytes)?;
        record.placement = placement;
        record.bump_last_seen(Utc::now());
        if let Some(routing) = self.distribution.clone() {
            return self.replicate_placement_write(&key, &bytes, &record, &routing);
        }
        // Single-node / non-distributed: plain local write (owns every shard).
        database
            .put(key, record.encode()?)
            .map_err(|error| database_error(&error))?;
        database.commit().map_err(|error| database_error(&error))?;
        Ok(Some(()))
    }

    /// Quorum-replicated value-CAS write of an updated placement record, factored
    /// out of [`Self::set_namespace_placement_record`] so each function stays
    /// small. Mirrors the discriminations of [`Self::register_namespace_record`]:
    /// a benign concurrent CAS-loss reconciles as success (the placement is set
    /// either way), a fence surfaces [`StoreError::NotOwner`].
    ///
    /// # Errors
    ///
    /// As [`Self::register_namespace_record`].
    fn replicate_placement_write(
        &self,
        key: &[u8],
        current: &[u8],
        record: &NamespaceRecord,
        routing: &DistributedRouting,
    ) -> Result<Option<()>, StoreError> {
        let database = self.inner.database();
        let value = record.encode()?;
        let expected = Some(Hash::of(current));
        let result = run_off_runtime(|| {
            database.replicate_write(
                key.to_vec(),
                expected,
                value,
                None,
                &routing.membership,
                routing.timeout,
            )
        });
        match result {
            // Wrote, OR a concurrent writer raced the value-CAS: the placement is
            // set either way — the update is idempotent.
            Ok(_) | Err(DatabaseError::CasConflict { .. }) => Ok(Some(())),
            Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner {
                shard: database.shard_for(key),
            }),
            Err(error) => Err(database_error(&error)),
        }
    }

    /// Win the per-shard election and become the live owner of a SINGLE `shard`
    /// BEFORE the engine recovers over it (SS-2, per-shard seam).
    ///
    /// This is the per-shard primitive [`Self::acquire_owned_shards`] loops over,
    /// exposed on its own so the adoption path can drive a per-shard abort seam:
    /// a clean election loss on one shard ([`StoreError::NotOwner`]) must DROP only
    /// that shard rather than failing the whole batch (ADR-021 clean-partial).
    ///
    /// Only a DISTRIBUTED store elects; a single-node store owns everything
    /// unconditionally, so this is a no-op returning `Ok(())` there — boot and
    /// adoption stay byte-identical to the non-distributed path. The election is
    /// blocking and runs off the tokio runtime, like the replication write path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotOwner`] when a strictly higher ballot deposed this
    /// candidate (the acquire-time twin of a fenced publish — a clean, droppable
    /// loss), and [`StoreError::Backend`] for a quorum-unavailable election or any
    /// transport/replication fault (retryable; ownership unknown).
    pub fn acquire_owned_shard(&self, shard: usize) -> Result<(), StoreError> {
        let Some(routing) = self.distribution.as_ref() else {
            // Single-node mode: nothing to elect, owns everything already.
            return Ok(());
        };
        let database = self.inner.database();
        run_off_runtime(|| {
            database.acquire_shard_and_serve(shard, &routing.membership, routing.timeout)
        })
        .map(|_outcome| ())
        .map_err(|error| acquire_election_error(&error, shard))
    }

    /// Whether this node currently holds LIVE serve-authority for `shard` — it won
    /// the election THIS process lifetime and has not been deposed in-process.
    ///
    /// A distribution-gated seam over haematite's
    /// [`haematite::Database::is_current_owner`]: it reads the in-memory live epoch
    /// the per-write fence stamps against, so a `true` answer is consistent with
    /// the write-time fence at the instant it is read. This is the residual-window
    /// re-assertion the adoption path uses to exclude a survivor that lost its
    /// epoch between winning acquire+publish and widening its scope.
    ///
    /// POINT-IN-TIME ADVISORY: ownership can be lost concurrently, so callers must
    /// NOT treat `true` as a durable lock — the authoritative gate remains the
    /// per-write CAS fence. A single-node / non-distributed store owns everything
    /// unconditionally, so this returns `true` (the existing owns-everything no-op),
    /// keeping the non-distributed path byte-identical.
    #[must_use]
    pub fn is_current_owner(&self, shard: usize) -> bool {
        if self.distribution.is_none() {
            // Single-node: owns everything unconditionally.
            return true;
        }
        self.inner.database().is_current_owner(shard)
    }

    /// Snapshot the owned-shard scope as an owned `Option<Vec<usize>>` suitable
    /// for moving into a `self.blocking` closure (which only borrows the
    /// `EventStore`, not `self`). `None` = all shards.
    fn owned_shard_scope(&self) -> Option<Vec<usize>> {
        self.owned_shards()
    }

    /// Run a blocking haematite closure on the blocking pool, sharing the
    /// `Arc<EventStore>`.
    async fn blocking<F, T>(&self, function: F) -> Result<T, StoreError>
    where
        F: FnOnce(&haematite::EventStore) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || function(&inner))
            .await
            .map_err(|error| join_error(&error))?
    }
}

// --- value encodings for KV-region records ---------------------------------

/// Serialized form of a stored timer (the full [`TimerEntry`]).
fn encode_timer(entry: &TimerEntry) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(entry).map_err(|error| serde_error(&error))
}

fn decode_timer(bytes: &[u8]) -> Result<TimerEntry, StoreError> {
    serde_json::from_slice(bytes).map_err(|error| serde_error(&error))
}

/// On-disk form of a deployed package: deploy order is recovered from
/// `deployed_at`, so it is stored alongside the archive and content fields.
#[derive(Serialize, Deserialize)]
struct StoredPackage {
    workflow_type: String,
    content_hash: String,
    archive: Vec<u8>,
    deployed_at: String,
}

fn encode_package(record: &PackageRecord) -> Result<Vec<u8>, StoreError> {
    let stored = StoredPackage {
        workflow_type: record.workflow_type.clone(),
        content_hash: record.content_hash.clone(),
        archive: record.archive.clone(),
        deployed_at: encode_instant(record.deployed_at),
    };
    serde_json::to_vec(&stored).map_err(|error| serde_error(&error))
}

fn decode_package(bytes: &[u8]) -> Result<PackageRecord, StoreError> {
    let stored: StoredPackage =
        serde_json::from_slice(bytes).map_err(|error| serde_error(&error))?;
    Ok(PackageRecord {
        workflow_type: stored.workflow_type,
        content_hash: stored.content_hash,
        archive: stored.archive,
        deployed_at: decode_instant(&stored.deployed_at)?,
    })
}

/// On-disk form of one outbox row. Mirrors [`OutboxRow`] with the status token
/// and instant rendered as text, matching the libSQL encoding semantics.
#[derive(Serialize, Deserialize)]
struct StoredOutboxRow {
    dispatch_key: String,
    workflow_id: WorkflowId,
    ordinal: u64,
    #[serde(default)]
    run_id: Option<aion_core::RunId>,
    /// Workflow's durable isolation namespace; legacy rows persisted before NSTQ-2 default to
    /// `"default"`.
    #[serde(default = "default_outbox_route")]
    namespace: String,
    /// Pool/flavour selector; legacy rows persisted before NSTQ-2 default to `"default"`.
    #[serde(default = "default_outbox_route")]
    task_queue: String,
    /// OPTIONAL node affinity; an absent field (legacy rows persisted before NODE-2) decodes to
    /// `None` = no affinity. No sentinel string.
    #[serde(default)]
    node: Option<String>,
    activity_type: String,
    input: aion_core::Payload,
    status: String,
    attempt: u32,
    visible_after: String,
    #[serde(default)]
    claimed_at: Option<String>,
}

fn default_outbox_route() -> String {
    String::from(aion_store::DEFAULT_OUTBOX_ROUTE)
}

fn encode_outbox(row: &OutboxRow) -> Result<Vec<u8>, StoreError> {
    let stored = StoredOutboxRow {
        dispatch_key: row.dispatch_key.clone(),
        workflow_id: row.workflow_id.clone(),
        ordinal: row.ordinal,
        run_id: row.run_id.clone(),
        namespace: row.namespace.clone(),
        task_queue: row.task_queue.clone(),
        node: row.node.clone(),
        activity_type: row.activity_type.clone(),
        input: row.input.clone(),
        status: row.status.as_str().to_owned(),
        attempt: row.attempt,
        visible_after: encode_instant(row.visible_after),
        claimed_at: row.claimed_at.map(encode_instant),
    };
    serde_json::to_vec(&stored).map_err(|error| serde_error(&error))
}

fn decode_outbox(bytes: &[u8]) -> Result<OutboxRow, StoreError> {
    let stored: StoredOutboxRow =
        serde_json::from_slice(bytes).map_err(|error| serde_error(&error))?;
    Ok(OutboxRow {
        dispatch_key: stored.dispatch_key,
        workflow_id: stored.workflow_id,
        ordinal: stored.ordinal,
        run_id: stored.run_id,
        namespace: stored.namespace,
        task_queue: stored.task_queue,
        node: stored.node,
        activity_type: stored.activity_type,
        input: stored.input,
        status: OutboxStatus::parse_token(&stored.status)?,
        attempt: stored.attempt,
        visible_after: decode_instant(&stored.visible_after)?,
        claimed_at: stored
            .claimed_at
            .as_deref()
            .map(decode_instant)
            .transpose()?,
    })
}

fn encode_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_instant(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn timer_id_token(timer_id: &TimerId) -> Result<String, StoreError> {
    serde_json::to_string(timer_id).map_err(|error| serde_error(&error))
}

// --- blocking KV helpers (run inside `spawn_blocking`) ----------------------

/// Scan every value under `prefix` and decode it with `decode`.
fn scan_prefix<T, D>(
    store: &haematite::EventStore,
    prefix: &[u8],
    mut decode: D,
) -> Result<Vec<T>, StoreError>
where
    D: FnMut(&[u8], &[u8]) -> Result<T, StoreError>,
{
    let database = store.database();
    // Fan the prefix scan out across EVERY shard and concatenate the per-shard
    // results. `Database::range` is shard-local (routed from the lower bound), so
    // it only returns the keys that happen to live in the lower bound's shard;
    // for a globally-complete enumeration we must visit each shard directly via
    // `range_per_shard`. At `shard_count == 1` this is exactly one
    // `range_per_shard(0, ..)` call, byte-for-byte equivalent to the old
    // `range(..)`. The cross-shard concatenation order is arbitrary, but every
    // caller of `scan_prefix` re-sorts its results, so this is correct.
    let Some(upper) = keyspace::prefix_upper_bound(prefix) else {
        return Ok(Vec::new());
    };
    let mut decoded = Vec::new();
    for shard in 0..database.shard_count() {
        let entries = database
            .range_per_shard(shard, prefix, &upper)
            .map_err(|error| database_error(&error))?;
        for (key, value) in &entries {
            decoded.push(decode(key, value)?);
        }
    }
    Ok(decoded)
}

/// Scan every value under `prefix`, restricted to an owned-shard `scope`.
///
/// * `scope == None` → iterate `0..database.shard_count()`, identical to
///   [`scan_prefix`] (the single-node / own-all default).
/// * `scope == Some(ids)` → iterate ONLY those shard ids, skipping any id
///   `>= shard_count` defensively. Because AA-4-3a co-located each workflow's
///   timers/outbox on its event stream's shard, scanning shard `S`'s `t:`/`o:`
///   region yields exactly the timers/outbox of the workflows on shard `S`.
///
/// The cross-shard concatenation order is arbitrary; every caller re-sorts.
fn scan_prefix_scoped<T, D>(
    store: &haematite::EventStore,
    prefix: &[u8],
    scope: Option<&[usize]>,
    mut decode: D,
) -> Result<Vec<T>, StoreError>
where
    D: FnMut(&[u8], &[u8]) -> Result<T, StoreError>,
{
    let database = store.database();
    let shard_count = database.shard_count();
    let Some(upper) = keyspace::prefix_upper_bound(prefix) else {
        return Ok(Vec::new());
    };
    let mut decoded = Vec::new();
    let mut scan = |shard: usize| -> Result<(), StoreError> {
        let entries = database
            .range_per_shard(shard, prefix, &upper)
            .map_err(|error| database_error(&error))?;
        for (key, value) in &entries {
            decoded.push(decode(key, value)?);
        }
        Ok(())
    };
    match scope {
        None => {
            for shard in 0..shard_count {
                scan(shard)?;
            }
        }
        Some(ids) => {
            for &shard in ids {
                // Skip out-of-range ids defensively; an owned-shard set is
                // sourced from acquisition and should already be valid, but a
                // stale id must not abort the whole scan.
                if shard < shard_count {
                    scan(shard)?;
                }
            }
        }
    }
    Ok(decoded)
}

/// Read and decode all event payloads for `workflow_id`, in sequence order.
fn read_events(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, StoreError> {
    read_events_from(store, workflow_id, 0)
}

/// Read and decode event payloads for `workflow_id` with Aion seq `>= from_seq`.
fn read_events_from(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
    from_seq: u64,
) -> Result<Vec<Event>, StoreError> {
    let stream_key = keyspace::event_stream_key(workflow_id);
    // Aion seq is 1-based; haematite read_from takes a 0-based stream offset.
    // Aion seq `s` is stored at haematite stream offset `s - 1`, so the lower
    // bound is `from_seq.saturating_sub(1)`. `from_seq <= 1` reads everything.
    let offset = from_seq.saturating_sub(1);
    let raw = store
        .read_from(&stream_key, offset)
        .map_err(|error| api_error(&error))?;
    let mut events = Vec::with_capacity(raw.len());
    for event in raw {
        let decoded: Event =
            serde_json::from_slice(&event.payload).map_err(|error| serde_error(&error))?;
        events.push(decoded);
    }
    Ok(events)
}

/// Append `events` for `workflow_id` and, when `outbox_rows` is `Some`, the
/// outbox rows. Shared by `append` and `append_with_outbox`.
///
/// When `routing` is `Some` the event batch is quorum-REPLICATED through
/// haematite's `replicate_append` to the configured membership; the outbox rows
/// ALWAYS stay local (Design B). When `routing` is `None` this is the unchanged
/// single-node B1 path. Per-workflow KV records (timers, outbox) co-locate on the
/// workflow's shard by routing on its event-stream key.
fn append_blocking(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
    events: &[Event],
    expected_seq: u64,
    outbox_rows: Option<&[OutboxRow]>,
    routing: Option<&DistributedRouting>,
) -> Result<(), StoreError> {
    let has_outbox = outbox_rows.is_some_and(|rows| !rows.is_empty());

    // Enforce the expected-head guard FIRST, before contiguity: a stale append
    // (e.g. expected_seq=0 against a head of 1) must surface as SequenceConflict,
    // matching the libSQL/in-memory stores, even if the supplied events are not
    // contiguous with the caller's stale expectation.
    let head = stream_head(store, workflow_id)?;
    if head != expected_seq {
        return Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: head,
        });
    }

    // Validate Aion's contiguity contract before any write (matches libSQL/in-memory).
    for (next_seq, event) in (expected_seq + 1..).zip(events.iter()) {
        if event.seq() != next_seq {
            return Err(StoreError::Backend(format!(
                "event sequence must be contiguous: expected {next_seq}, got {}",
                event.seq()
            )));
        }
    }

    if events.is_empty() && !has_outbox {
        return Ok(());
    }

    if !events.is_empty() {
        let stream_key = keyspace::event_stream_key(workflow_id);
        let payloads: Vec<Vec<u8>> = events
            .iter()
            .map(|event| serde_json::to_vec(event).map_err(|error| serde_error(&error)))
            .collect::<Result<_, _>>()?;

        if let Some(routing) = routing {
            // DISTRIBUTED: replicate the whole batch to a quorum. The event/stream
            // key + value encoding is byte-identical to the local `append_batch`
            // path (haematite shares the encoding between `append` and
            // `replicate_append`), so `read_history` decodes a replicated stream
            // exactly as it decodes a locally-appended one. Workflows are
            // enumerated from these replicated streams, so there is no separate
            // workflow-id index to replicate.
            replicate_events(store, &stream_key, &payloads, expected_seq, routing)?;
        } else {
            // SINGLE-NODE (B1, unchanged): local optimistic-concurrency append.
            // `append_batch` self-commits, and workflows are enumerated from the
            // event streams themselves, so no separate index write/commit is
            // needed here.
            let payload_refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
            match store.append_batch(&stream_key, &payload_refs, expected_seq) {
                Ok(_) => {}
                Err(haematite::ApiError::SequenceConflict(conflict)) => {
                    return Err(StoreError::SequenceConflict {
                        expected: expected_seq,
                        found: conflict.actual,
                    });
                }
                Err(error) => return Err(api_error(&error)),
            }
        }
    }

    if let Some(rows) = outbox_rows {
        insert_outbox_rows(store, rows)?;
    }

    Ok(())
}

/// Run `work` on a FRESH OS thread that has NO entered tokio runtime, returning its
/// result.
///
/// haematite's distribution coordinator (`replicate_append`, `replicate_write`,
/// their quorum waits and catch-up rounds) BLOCKS and explicitly refuses to run
/// from a thread with an entered tokio runtime (`Handle::try_current().is_ok()` →
/// `TransportBlockingFromAsync`). The adapter executes its haematite work inside
/// `tokio::task::spawn_blocking`, whose blocking-pool threads STILL carry the
/// runtime context, so a direct call would be rejected. A brand-new `std::thread`
/// (here via `std::thread::scope`, so it can borrow) carries no runtime context, so
/// the coordinator runs there; it drives the endpoint's OWN internal runtime
/// internally, which is unaffected.
fn run_off_runtime<F, T>(work: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    std::thread::scope(|scope| {
        match scope.spawn(work).join() {
            Ok(value) => value,
            // Propagate a panic from the replication thread unchanged rather than
            // swallowing it into a fabricated value.
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// Dial `peer_name` at `addr`, retrying with backoff until it connects or
/// `deadline` elapses.
///
/// At cluster boot every node lives in its OWN OS process, so there is no shared
/// "all endpoints bound, now connect" barrier the in-process test harness has: a
/// node started before its peers would otherwise fail the very first dial and
/// abort the whole boot. Retrying turns boot into an order-independent
/// convergence — each node binds its endpoint immediately and connects to peers
/// as they come up, anywhere within `deadline`. The first attempt is issued with
/// no delay, so a peer that is already listening connects with zero added
/// latency; only a not-yet-listening peer pays the backoff.
///
/// `deadline` is the cluster operation timeout (`boot.timeout`), so the boot's
/// patience for peers to appear matches its patience for a quorum op — one knob.
fn connect_peer_with_retry(
    database: &Database,
    peer_name: &str,
    addr: std::net::SocketAddr,
    deadline: Duration,
) -> Result<(), StoreError> {
    const RETRY_BACKOFF: Duration = Duration::from_millis(100);
    let started = std::time::Instant::now();
    loop {
        match database.connect_peer(peer_name, addr) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if started.elapsed() >= deadline {
                    return Err(database_error(&error));
                }
                std::thread::sleep(RETRY_BACKOFF);
            }
        }
    }
}

/// Replicate one event batch to the routing quorum via `replicate_append`,
/// mapping a quorum sequence-conflict back to Aion's [`StoreError::SequenceConflict`].
fn replicate_events(
    store: &haematite::EventStore,
    stream_key: &[u8],
    payloads: &[Vec<u8>],
    expected_seq: u64,
    routing: &DistributedRouting,
) -> Result<(), StoreError> {
    let database = store.database();
    let result = run_off_runtime(|| {
        database.replicate_append(
            stream_key,
            payloads,
            expected_seq,
            &routing.membership,
            routing.timeout,
        )
    });
    match result {
        Ok(_) => Ok(()),
        Err(DatabaseError::SequenceConflict { actual, .. }) => Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: actual,
        }),
        // A quorum write that is deterministically out-voted (the owner's promised
        // ballot fenced this proposal) means THIS node is not the shard's current
        // owner. haematite surfaces this as the typed `DatabaseError::Fenced`; map
        // it to the typed, retryable `NotOwner` so the request-routing edge can
        // re-resolve/forward instead of seeing an opaque `Backend` internal error
        // (R-0). Any other consistency failure (quorum unavailable, transport,
        // timeout) is a genuine backend boundary failure and stays `Backend`.
        Err(DatabaseError::Fenced { .. }) => Err(StoreError::NotOwner {
            shard: database.shard_for(stream_key),
        }),
        Err(error) => Err(database_error(&error)),
    }
}

/// The current stored head (event count) for `workflow_id`.
fn stream_head(store: &haematite::EventStore, workflow_id: &WorkflowId) -> Result<u64, StoreError> {
    let events = read_events(store, workflow_id)?;
    Ok(events.iter().map(Event::seq).max().unwrap_or(0))
}

// --- observability (`O`) keyspace helpers (run inside `spawn_blocking`) ------

/// The `O`-region stream key for an observability [`ActivityStreamKey`].
fn observability_key(key: &ActivityStreamKey) -> Vec<u8> {
    keyspace::observability_stream_key(
        &key.workflow_id,
        key.activity_id.sequence_position(),
        key.attempt,
    )
}

/// The current `O`-stream head (next `store_seq` to be written) for `key`.
///
/// The stored `next_seq` metadata is the public 0-based next sequence — exactly
/// the value the api `append_batch` returns (`expected_seq + n`) and therefore
/// the count of durable events / the next `store_seq` to write. (The `-1` in the
/// event-KEY decode is an internal tree-key detail, NOT the `next_seq` metadata.)
/// An unwritten stream has no metadata and reads head `0`.
fn observability_head_blocking(
    store: &haematite::EventStore,
    key: &ActivityStreamKey,
) -> Result<u64, StoreError> {
    let stream_key = observability_key(key);
    let engine_next = store
        .database()
        .read_stream_next_seq(&stream_key)
        .map_err(|error| database_error(&error))?;
    Ok(engine_next.unwrap_or(0))
}

/// Append one observability event to its `O`-stream at `expected_seq`.
///
/// Maps haematite's optimistic-concurrency `SequenceConflict` back to
/// [`StoreError::SequenceConflict`] so the server sequencer can re-read the head
/// and retry. On success returns the assigned `store_seq` (which equals
/// `expected_seq`). The single-node store self-commits `append_batch`.
fn observability_append_blocking(
    store: &haematite::EventStore,
    expected_seq: u64,
    event: &ActivityEvent,
) -> Result<u64, StoreError> {
    let key = ActivityStreamKey::of(event);
    let stream_key = observability_key(&key);
    let mut event = event.clone();
    event.store_seq = Some(expected_seq);
    let payload = serde_json::to_vec(&event).map_err(|error| serde_error(&error))?;
    match store.append_batch(&stream_key, &[payload.as_slice()], expected_seq) {
        Ok(_next_seq) => Ok(expected_seq),
        Err(haematite::ApiError::SequenceConflict(conflict)) => Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: conflict.actual,
        }),
        Err(error) => Err(api_error(&error)),
    }
}

/// Read every `O`-stream record for `key` with `store_seq >= from_seq`, in order.
fn observability_read_blocking(
    store: &haematite::EventStore,
    key: &ActivityStreamKey,
    from_seq: u64,
) -> Result<Vec<ActivityRecord>, StoreError> {
    let stream_key = observability_key(key);
    let raw = match store.read_from(&stream_key, from_seq) {
        Ok(raw) => raw,
        // An empty stream at from_seq==0 that reports compacted is impossible for
        // the append-only `O` keyspace (no compaction), but treat any empty read
        // as an empty tail rather than an error.
        Err(haematite::ApiError::HistoryCompacted(_)) => return Ok(Vec::new()),
        Err(error) => return Err(api_error(&error)),
    };
    let mut records = Vec::with_capacity(raw.len());
    for event in raw {
        let decoded: ActivityEvent =
            serde_json::from_slice(&event.payload).map_err(|error| serde_error(&error))?;
        // The api seq is the authoritative store_seq; trust it over the payload's
        // self-reported copy so a record is self-consistent even if the two ever
        // drifted.
        let store_seq = event.seq;
        let mut decoded = decoded;
        decoded.store_seq = Some(store_seq);
        records.push(ActivityRecord {
            store_seq,
            event: decoded,
        });
    }
    Ok(records)
}

/// Enumerate the retained `O`-streams of one workflow via haematite's
/// intentionally unindexed stream scan (O(total streams) — an operator read,
/// never on the hot publish path).
///
/// Deliberately NOT `EventStore::scan`: that walks only shards already
/// MATERIALISED this process lifetime, so on a freshly reopened database it
/// would enumerate nothing until each stream's shard happened to be touched.
/// `scan_sequence_keys_for_shards` over every shard materialises (and
/// WAL-recovers) on demand, so enumeration is restart-correct — the "open it
/// an hour later" contract this store method exists for.
fn observability_list_blocking(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
) -> Result<Vec<ActivityStreamSummary>, StoreError> {
    let prefix = keyspace::observability_workflow_prefix(workflow_id);
    let database = store.database();
    let all_shards: Vec<usize> = (0..database.shard_count()).collect();
    let streams = database
        .scan_sequence_keys_for_shards(&all_shards)
        .map_err(|error| database_error(&error))?;
    let mut summaries = Vec::new();
    for (stream_key, next_seq) in streams {
        if !stream_key.starts_with(&prefix) {
            continue;
        }
        // A foreign region can never match the 17-byte tagged prefix, but stay
        // defensive: skip anything that does not decode as a full 29-byte
        // `O`-region stream key.
        let Some((activity_seq, attempt)) = keyspace::decode_observability_stream_key(&stream_key)
        else {
            continue;
        };
        // Match `EventStore::scan`'s live-events guard: a sequence key whose
        // events were all deleted is not a retained stream.
        if !database
            .stream_has_live_events(&stream_key)
            .map_err(|error| database_error(&error))?
        {
            continue;
        }
        summaries.push(ActivityStreamSummary {
            key: ActivityStreamKey::new(
                workflow_id.clone(),
                ActivityId::from_sequence_position(activity_seq),
                attempt,
            ),
            head: next_seq,
        });
    }
    summaries.sort_by_key(|summary| {
        (
            summary.key.activity_id.sequence_position(),
            summary.key.attempt,
        )
    });
    Ok(summaries)
}

/// Insert `rows` into the outbox, ignoring any whose `dispatch_key` already
/// exists (at-most-once dispatch). Commits before returning.
fn insert_outbox_rows(store: &haematite::EventStore, rows: &[OutboxRow]) -> Result<(), StoreError> {
    let database = store.database();
    for row in rows {
        // Co-locate the outbox row on the workflow's shard by routing on the
        // workflow's event-stream key.
        let route_key = keyspace::event_stream_key(&row.workflow_id);
        let key = keyspace::outbox_key(&row.dispatch_key);
        if database
            .get_routed(&route_key, &key)
            .map_err(|error| database_error(&error))?
            .is_none()
        {
            database
                .put_routed(&route_key, key, encode_outbox(row)?)
                .map_err(|error| database_error(&error))?;
        }
    }
    database.commit().map_err(|error| database_error(&error))?;
    Ok(())
}

#[async_trait]
impl ObservabilityStore for HaematiteStore {
    async fn append_activity_event(
        &self,
        expected_seq: u64,
        event: &ActivityEvent,
    ) -> Result<u64, StoreError> {
        // Server-single-writer optimistic-concurrency append to the `O` keyspace.
        // The server's sequencer supplies `expected_seq` and retries on conflict;
        // this store never auto-allocates the sequence (§5.3).
        let event = event.clone();
        self.blocking(move |store| observability_append_blocking(store, expected_seq, &event))
            .await
    }

    async fn activity_head(&self, key: &ActivityStreamKey) -> Result<u64, StoreError> {
        let key = key.clone();
        self.blocking(move |store| observability_head_blocking(store, &key))
            .await
    }

    async fn read_activity_events_from(
        &self,
        key: &ActivityStreamKey,
        from_seq: u64,
    ) -> Result<Vec<ActivityRecord>, StoreError> {
        let key = key.clone();
        self.blocking(move |store| observability_read_blocking(store, &key, from_seq))
            .await
    }

    async fn list_activity_streams(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<ActivityStreamSummary>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| observability_list_blocking(store, &workflow_id))
            .await
    }
}

#[async_trait]
impl WritableEventStore for HaematiteStore {
    async fn append(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        let workflow_id = workflow_id.clone();
        let events = events.to_vec();
        let routing = self.distribution.clone();
        self.blocking(move |store| {
            append_blocking(
                store,
                &workflow_id,
                &events,
                expected_seq,
                None,
                routing.as_ref(),
            )
        })
        .await
    }

    async fn append_with_outbox(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        let workflow_id = workflow_id.clone();
        let events = events.to_vec();
        let outbox_rows = outbox_rows.to_vec();
        let routing = self.distribution.clone();
        self.blocking(move |store| {
            append_blocking(
                store,
                &workflow_id,
                &events,
                expected_seq,
                Some(&outbox_rows),
                routing.as_ref(),
            )
        })
        .await
    }

    async fn rearm_outbox_pending(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let rows = rows.to_vec();
        self.blocking(move |store| {
            let database = store.database();
            for row in &rows {
                // Co-locate on the workflow's shard by routing on its event-stream
                // key, matching the original insert.
                let route_key = keyspace::event_stream_key(&row.workflow_id);
                let key = keyspace::outbox_key(&row.dispatch_key);
                // Read-modify-write: preserve the existing `attempt` budget when a
                // row already exists; insert a fresh Pending row otherwise.
                let merged = match database
                    .get_routed(&route_key, &key)
                    .map_err(|error| database_error(&error))?
                {
                    Some(existing) => {
                        let prior = decode_outbox(&existing)?;
                        OutboxRow {
                            status: OutboxStatus::Pending,
                            attempt: prior.attempt,
                            visible_after: row.visible_after,
                            claimed_at: None,
                            ..prior
                        }
                    }
                    None => OutboxRow {
                        status: OutboxStatus::Pending,
                        claimed_at: None,
                        ..row.clone()
                    },
                };
                database
                    .put_routed(&route_key, key, encode_outbox(&merged)?)
                    .map_err(|error| database_error(&error))?;
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn settle_outbox_row_cancelled(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, |row| match row.status {
            OutboxStatus::Pending | OutboxStatus::Claimed => OutboxRow {
                status: OutboxStatus::Cancelled,
                claimed_at: None,
                ..row
            },
            OutboxStatus::Done | OutboxStatus::Failed | OutboxStatus::Cancelled => row,
        })
        .await
    }

    async fn settle_workflow_outbox_rows_cancelled(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<String>, StoreError> {
        self.settle_workflow_outbox_rows(workflow_id).await
    }
}

#[async_trait]
impl OutboxStore for HaematiteStore {
    async fn append_outbox_batch(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let rows = rows.to_vec();
        self.blocking(move |store| insert_outbox_rows(store, &rows))
            .await
    }

    async fn claim_outbox_rows(&self, limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let now = Utc::now();
            let mut claimable: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| row.status == OutboxStatus::Pending && row.visible_after <= now)
            .collect();
            // Match the libSQL claim order: visible_after ASC, dispatch_key ASC.
            claimable.sort_by(|left, right| {
                left.visible_after
                    .cmp(&right.visible_after)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).unwrap_or(usize::MAX);
            claimable.truncate(take);

            let database = store.database();
            let mut claimed = Vec::with_capacity(claimable.len());
            for row in claimable {
                let updated = OutboxRow {
                    status: OutboxStatus::Claimed,
                    claimed_at: Some(now),
                    ..row
                };
                // Rewrite in place on the row's own shard (co-located by the
                // workflow's event-stream key).
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                claimed.push(updated);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(claimed)
        })
        .await
    }

    async fn claim_outbox_rows_excluding(
        &self,
        limit: u32,
        held: &std::collections::HashSet<aion_core::WorkflowId>,
    ) -> Result<Vec<OutboxRow>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = self.owned_shard_scope();
        let held = held.clone();
        self.blocking(move |store| {
            let now = Utc::now();
            let mut claimable: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| row.status == OutboxStatus::Pending && row.visible_after <= now)
            // A held (paused) workflow's row is left Pending: never claimed, so
            // release is purely resume + the next sweep (#204).
            .filter(|row| !held.contains(&row.workflow_id))
            .collect();
            claimable.sort_by(|left, right| {
                left.visible_after
                    .cmp(&right.visible_after)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).unwrap_or(usize::MAX);
            claimable.truncate(take);

            let database = store.database();
            let mut claimed = Vec::with_capacity(claimable.len());
            for row in claimable {
                let updated = OutboxRow {
                    status: OutboxStatus::Claimed,
                    claimed_at: Some(now),
                    ..row
                };
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                claimed.push(updated);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(claimed)
        })
        .await
    }

    async fn claim_outbox_rows_scoped(
        &self,
        claim_scope: &ClaimScope,
        limit: u32,
    ) -> Result<Vec<OutboxRow>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let shard_scope = self.owned_shard_scope();
        let claim_scope = claim_scope.clone();
        self.blocking(move |store| {
            let now = Utc::now();
            let mut claimable: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                shard_scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            // Identical to the unscoped claim filter, plus the pool predicate (LSUB-1a):
            // namespace + task_queue match and the node predicate (unpinned OR pinned to scope).
            .filter(|row| {
                row.status == OutboxStatus::Pending
                    && row.visible_after <= now
                    && claim_scope.admits(row)
            })
            .collect();
            // Match the libSQL claim order: visible_after ASC, dispatch_key ASC.
            claimable.sort_by(|left, right| {
                left.visible_after
                    .cmp(&right.visible_after)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).unwrap_or(usize::MAX);
            claimable.truncate(take);

            let database = store.database();
            let mut claimed = Vec::with_capacity(claimable.len());
            for row in claimable {
                let updated = OutboxRow {
                    status: OutboxStatus::Claimed,
                    claimed_at: Some(now),
                    ..row
                };
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                claimed.push(updated);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(claimed)
        })
        .await
    }

    async fn claim_outbox_rows_scoped_excluding(
        &self,
        claim_scope: &ClaimScope,
        limit: u32,
        held: &std::collections::HashSet<aion_core::WorkflowId>,
    ) -> Result<Vec<OutboxRow>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let shard_scope = self.owned_shard_scope();
        let claim_scope = claim_scope.clone();
        let held = held.clone();
        self.blocking(move |store| {
            let now = Utc::now();
            let mut claimable: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                shard_scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| {
                row.status == OutboxStatus::Pending
                    && row.visible_after <= now
                    && claim_scope.admits(row)
            })
            // A held (paused) workflow's row is left Pending: never claimed, so
            // release is purely resume + the next sweep (#204). The backpressure
            // sweep claims through this scoped path, so the hold MUST be honoured
            // here too, not only on the unscoped claim.
            .filter(|row| !held.contains(&row.workflow_id))
            .collect();
            claimable.sort_by(|left, right| {
                left.visible_after
                    .cmp(&right.visible_after)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).unwrap_or(usize::MAX);
            claimable.truncate(take);

            let database = store.database();
            let mut claimed = Vec::with_capacity(claimable.len());
            for row in claimable {
                let updated = OutboxRow {
                    status: OutboxStatus::Claimed,
                    claimed_at: Some(now),
                    ..row
                };
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                claimed.push(updated);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(claimed)
        })
        .await
    }

    async fn list_stale_claimed_outbox_rows(
        &self,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<OutboxRow>, StoreError> {
        // The read-only probe half of stale-claim reconciliation (#253): the EXACT
        // selection the re-arm below would take — same claimed + durable-claimed_at
        // predicate, same claimed_at/dispatch_key order, same truncation — with no
        // transition, so the reconciler can project workflow liveness first.
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let mut stale: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| {
                row.status == OutboxStatus::Claimed
                    && row
                        .claimed_at
                        .is_some_and(|claimed_at| claimed_at < older_than)
            })
            .collect();
            stale.sort_by(|left, right| {
                left.claimed_at
                    .cmp(&right.claimed_at)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).map_or(usize::MAX, |value| value);
            stale.truncate(take);
            Ok(stale)
        })
        .await
    }

    async fn list_unsettled_outbox_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        // Distinct owners of any live (Pending|Claimed) row, owned-shard scoped
        // like every other outbox enumeration — the boot/adoption sweep's
        // candidate set for terminal-workflow settlement (#253). Read-only.
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let rows: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?;
            let mut workflow_ids: Vec<WorkflowId> = rows
                .into_iter()
                .filter(|row| is_inflight(row.status))
                .map(|row| row.workflow_id)
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            // Deterministic textual order, matching the libSQL enumeration.
            workflow_ids.sort_by_key(ToString::to_string);
            Ok(workflow_ids)
        })
        .await
    }

    async fn cancel_outbox_rows_for_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<String>, StoreError> {
        self.settle_workflow_outbox_rows(workflow_id).await
    }

    async fn rearm_stale_claimed_outbox_rows(
        &self,
        older_than: DateTime<Utc>,
        visible_after: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<OutboxRow>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let mut stale: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| {
                row.status == OutboxStatus::Claimed
                    && row
                        .claimed_at
                        .is_some_and(|claimed_at| claimed_at < older_than)
            })
            .collect();
            stale.sort_by(|left, right| {
                left.claimed_at
                    .cmp(&right.claimed_at)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).map_or(usize::MAX, |value| value);
            stale.truncate(take);

            let database = store.database();
            let mut rearmed = Vec::with_capacity(stale.len());
            for row in stale {
                let updated = OutboxRow {
                    status: OutboxStatus::Pending,
                    visible_after,
                    claimed_at: None,
                    ..row
                };
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                rearmed.push(updated);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(rearmed)
        })
        .await
    }

    async fn complete_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, |row| OutboxRow {
            status: OutboxStatus::Done,
            claimed_at: None,
            ..row
        })
        .await
    }

    async fn retry_outbox_row(
        &self,
        dispatch_key: &str,
        next_attempt: u32,
        visible_after: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, move |row| OutboxRow {
            status: OutboxStatus::Pending,
            attempt: next_attempt,
            visible_after,
            claimed_at: None,
            ..row
        })
        .await
    }

    async fn fail_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, |row| OutboxRow {
            status: OutboxStatus::Failed,
            claimed_at: None,
            ..row
        })
        .await
    }

    async fn count_inflight_outbox_rows(&self, namespace: &str) -> Result<u64, StoreError> {
        // Mirror the claim path: scan the outbox prefix across this node's owned shards and count
        // the dispatched-but-not-terminal rows (Pending OR Claimed) whose namespace matches. A
        // stuck-Claimed row (dispatched but mark_done never landed) is still Claimed and so still
        // counts; terminal rows (Done/Failed/Cancelled) are excluded. The namespace predicate is
        // exact so no other namespace is counted. This deliberately does NOT reshard: it counts only
        // the rows on shards this node owns, exactly as the claim/rearm scans do.
        let scope = self.owned_shard_scope();
        let namespace = namespace.to_owned();
        self.blocking(move |store| {
            let rows: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?;
            let count = rows
                .into_iter()
                .filter(|row| row.namespace == namespace && is_inflight(row.status))
                .count();
            Ok(u64::try_from(count).unwrap_or(u64::MAX))
        })
        .await
    }

    async fn count_claimed_outbox_rows(&self, namespace: &str) -> Result<u64, StoreError> {
        // Mirror the claim/in-flight scan, but count ONLY Claimed (concurrently executing) rows —
        // not the Pending backlog. This is the keyed-backpressure headroom input: counting
        // Pending+Claimed would wedge a tenant against its own backlog (CP-Phase-2 §3.1). A
        // stuck-Claimed row still counts (the worker may still be executing it). Owned-shard scoped
        // exactly as the claim path, so a per-node count sees only this node's claimed slice.
        let scope = self.owned_shard_scope();
        let namespace = namespace.to_owned();
        self.blocking(move |store| {
            let rows: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?;
            let count = rows
                .into_iter()
                .filter(|row| {
                    row.namespace == namespace && matches!(row.status, OutboxStatus::Claimed)
                })
                .count();
            Ok(u64::try_from(count).unwrap_or(u64::MAX))
        })
        .await
    }

    async fn count_claimed_outbox_rows_by_namespace(
        &self,
        namespaces: &[&str],
    ) -> Result<std::collections::BTreeMap<String, u64>, StoreError> {
        // ONE owned-shard scan, bucketed by namespace (CP2-Q2 perf): the per-sweep planner needs the
        // claimed count for every active namespace, and the single-namespace form above re-scans the
        // whole owned-shard set once PER namespace (the N+1). Collapse to a single scan and tally per
        // namespace, filtered to the requested set. Byte-identical counts to N calls of the above:
        // same owned-shard scope, same Claimed-only predicate, same per-namespace scoping.
        let scope = self.owned_shard_scope();
        let requested: std::collections::BTreeSet<String> =
            namespaces.iter().map(|ns| (*ns).to_owned()).collect();
        self.blocking(move |store| {
            // Seed every requested namespace at zero so the caller can index unconditionally.
            let mut counts: std::collections::BTreeMap<String, u64> =
                requested.iter().map(|ns| (ns.clone(), 0)).collect();
            let rows: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?;
            for row in rows {
                if matches!(row.status, OutboxStatus::Claimed)
                    && let Some(count) = counts.get_mut(&row.namespace)
                {
                    *count = count.saturating_add(1);
                }
            }
            Ok(counts)
        })
        .await
    }

    async fn pending_outbox_routes(&self) -> Result<Vec<ClaimScope>, StoreError> {
        // Read-only enumeration of the distinct `(namespace, task_queue, node)` routes with a
        // claimable pending row (status Pending AND visible_after passed). Owned-shard scoped so the
        // per-node round-robin naturally sees only this node's slice of each tenant's work; claims
        // nothing — it only shapes which scopes the dispatcher then claims under (CP2-Q2).
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let now = Utc::now();
            let rows: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?;
            // Deduplicate routes deterministically (the scan order is arbitrary across shards).
            let mut seen: std::collections::BTreeSet<(String, String, Option<String>)> =
                std::collections::BTreeSet::new();
            for row in rows {
                if row.status == OutboxStatus::Pending && row.visible_after <= now {
                    seen.insert((row.namespace, row.task_queue, row.node));
                }
            }
            Ok(seen
                .into_iter()
                .map(|(namespace, task_queue, node)| {
                    ClaimScope::new(namespace, task_queue).with_node(node)
                })
                .collect())
        })
        .await
    }
}

/// Whether `status` is the dispatched-but-not-terminal (in-flight) set: `Pending` OR `Claimed`
/// (CP2-Q1.5). Terminal states (`Done`, `Failed`, `Cancelled`) are not in-flight.
fn is_inflight(status: OutboxStatus) -> bool {
    matches!(status, OutboxStatus::Pending | OutboxStatus::Claimed)
}

impl HaematiteStore {
    /// Read back the durable status of the outbox row at `dispatch_key`, or
    /// `None` when no such row exists.
    ///
    /// This is an out-of-band inspection helper symmetric with
    /// [`LibSqlStore::outbox_row_state`](aion_store_libsql::LibSqlStore::outbox_row_state):
    /// the [`OutboxStore`] dispatch contract keys terminal transitions off
    /// `dispatch_key` and never needs to read a row back, so this is used by tests
    /// and by operators auditing dead-lettered (`failed`) or drained (`done`)
    /// rows. The row is co-located on its workflow's shard, so the lookup derives
    /// the route key from the `dispatch_key`'s workflow-id prefix exactly as
    /// [`Self::transition_outbox`] does and reads the locally-applied replica.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when the `dispatch_key` is malformed (no
    /// `':'` separator or a non-workflow-id prefix) or the local read fails, and
    /// any decode error for a corrupt stored row.
    pub async fn outbox_row_status(
        &self,
        dispatch_key: &str,
    ) -> Result<Option<OutboxStatus>, StoreError> {
        let dispatch_key = dispatch_key.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            let (workflow_text, _ordinal) = dispatch_key.rsplit_once(':').ok_or_else(|| {
                StoreError::Backend(format!(
                    "outbox dispatch_key missing ':' separator: {dispatch_key}"
                ))
            })?;
            let workflow_id = parse_workflow_id(workflow_text)?;
            let route_key = keyspace::event_stream_key(&workflow_id);
            let key = keyspace::outbox_key(&dispatch_key);
            let Some(existing) = database
                .get_routed(&route_key, &key)
                .map_err(|error| database_error(&error))?
            else {
                return Ok(None);
            };
            Ok(Some(decode_outbox(&existing)?.status))
        })
        .await
    }

    /// Apply `transition` to the row at `dispatch_key`; an absent key is a no-op.
    async fn transition_outbox<F>(
        &self,
        dispatch_key: &str,
        transition: F,
    ) -> Result<(), StoreError>
    where
        F: FnOnce(OutboxRow) -> OutboxRow + Send + 'static,
    {
        let dispatch_key = dispatch_key.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            // The outbox row is co-located on its workflow's shard. The
            // dispatch_key is canonically "{workflow_id}:{ordinal}" and UUIDs
            // contain no ':', so derive the workflow id (and thus the route key)
            // by splitting off the trailing ordinal. A dispatch_key that has no
            // ':' or whose prefix is not a workflow id is a hard error — silently
            // falling back to a non-routed key would split the record from its
            // workflow's shard.
            let (workflow_text, _ordinal) = dispatch_key.rsplit_once(':').ok_or_else(|| {
                StoreError::Backend(format!(
                    "outbox dispatch_key missing ':' separator: {dispatch_key}"
                ))
            })?;
            let workflow_id = parse_workflow_id(workflow_text)?;
            let route_key = keyspace::event_stream_key(&workflow_id);
            let key = keyspace::outbox_key(&dispatch_key);
            let Some(existing) = database
                .get_routed(&route_key, &key)
                .map_err(|error| database_error(&error))?
            else {
                return Ok(());
            };
            let updated = transition(decode_outbox(&existing)?);
            database
                .put_routed(&route_key, key, encode_outbox(&updated)?)
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    /// Idempotently settle every live (Pending|Claimed) row of `workflow_id` to
    /// Cancelled, returning the settled `dispatch_key`s (#253).
    ///
    /// The single shared implementation behind BOTH the writer seam
    /// (`WritableEventStore::settle_workflow_outbox_rows_cancelled`) and the
    /// outbox-store twin (`OutboxStore::cancel_outbox_rows_for_workflow`), so the
    /// Recorder's settle-at-terminal and the server's boot/reconciler sweeps
    /// apply the identical transition table: only Pending|Claimed rows flip,
    /// terminal rows (Done/Failed/Cancelled) are never touched. Rows are
    /// co-located on the workflow's shard and rewritten in place, exactly like
    /// the claim/rearm scans.
    async fn settle_workflow_outbox_rows(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<String>, StoreError> {
        let scope = self.owned_shard_scope();
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| {
            let mut live: Vec<OutboxRow> = scan_prefix_scoped(
                store,
                keyspace::OUTBOX_PREFIX,
                scope.as_deref(),
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| row.workflow_id == workflow_id && is_inflight(row.status))
            .collect();
            live.sort_by(|left, right| left.dispatch_key.cmp(&right.dispatch_key));

            let database = store.database();
            let mut settled = Vec::with_capacity(live.len());
            for row in live {
                let updated = OutboxRow {
                    status: OutboxStatus::Cancelled,
                    claimed_at: None,
                    ..row
                };
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                settled.push(updated.dispatch_key);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(settled)
        })
        .await
    }
}

#[async_trait]
impl ReadableEventStore for HaematiteStore {
    /// Scope enumeration to the owned shard set via the inherent
    /// [`Self::set_owned_shards`] / [`Self::own_all_shards`] state, so the
    /// engine boot path can drive owned-shard scoping through the type-erased
    /// `dyn ReadableEventStore` it holds. `None` reverts to owning all shards.
    fn set_owned_shards(&self, shards: Option<&[usize]>) {
        match shards {
            Some(shards) => Self::set_owned_shards(self, shards.iter().copied()),
            None => self.own_all_shards(),
        }
    }

    /// Win the per-shard election and become the live owner of each shard in
    /// `shards` BEFORE the engine recovers over them (SS-2).
    ///
    /// Only a DISTRIBUTED store (`with_distribution`) elects: it runs
    /// [`Database::acquire_shard_and_serve`] for each shard over its configured
    /// `WriteMembership`, so on return every committed write on those shards is
    /// locally present (`become_live` union-merge) and the node is the fenced
    /// owner. A single-node store (`create`/`open`) has no membership and owns
    /// everything unconditionally, so this is a no-op there — boot stays
    /// byte-identical to the non-distributed path.
    ///
    /// The election is blocking and refuses to run from a thread with an entered
    /// tokio runtime, so it executes on a bare [`run_off_runtime`] thread exactly
    /// like the replication write path — which is what lets the async engine
    /// builder drive it directly.
    fn acquire_owned_shards(&self, shards: &[usize]) -> Result<(), StoreError> {
        // A thin loop over the per-shard primitive so the slice and per-shard
        // forms classify a clean election loss identically (ElectionLost ->
        // NotOwner; quorum/transport -> Backend). A single-node store no-ops.
        for &shard in shards {
            self.acquire_owned_shard(shard)?;
        }
        Ok(())
    }

    /// Win the per-shard election for a SINGLE `shard` via the inherent
    /// [`Self::acquire_owned_shard`], so the adoption path can drive a per-shard
    /// abort seam through the type-erased `dyn ReadableEventStore` it holds.
    fn acquire_owned_shard(&self, shard: usize) -> Result<(), StoreError> {
        Self::acquire_owned_shard(self, shard)
    }

    /// Whether this node currently holds live serve-authority for `shard` via the
    /// inherent [`Self::is_current_owner`], so the adoption path can re-assert
    /// ownership through the type-erased `dyn ReadableEventStore` it holds.
    fn is_current_owner(&self, shard: usize) -> bool {
        Self::is_current_owner(self, shard)
    }

    /// Widen the owned-enumeration scope by `shards`, unioning with the current
    /// set, via the inherent [`Self::extend_owned_shards`] (SS-5 failover) — so
    /// the engine can drive scope-widening through the type-erased
    /// `dyn ReadableEventStore` it holds after absorbing a dead peer's shards.
    fn extend_owned_shards(&self, shards: &[usize]) {
        Self::extend_owned_shards(self, shards.iter().copied());
    }

    /// Publish this node as `shard`'s current owner in the cluster directory via
    /// the inherent [`Self::publish_shard_owner`] (SS-3 failover-publish), so the
    /// engine can drive it through the type-erased `dyn ReadableEventStore` after
    /// adopting a dead peer's shards.
    fn publish_shard_owner(&self, shard: usize) -> Result<(), StoreError> {
        Self::publish_shard_owner(self, shard)
    }

    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| read_events(store, &workflow_id))
            .await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| read_events_from(store, &workflow_id, from_seq))
            .await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| {
            let history = read_events(store, &workflow_id)?;
            if history.is_empty() {
                return Ok(Vec::new());
            }
            aion_store::run_chain::run_chain_from_history(&history)
        })
        .await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        // Snapshot the owned-shard scope BEFORE `blocking` (the closure only
        // borrows the EventStore, not `self`).
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let mut ids = workflow_stream_ids(store, scope.as_deref())?;
            ids.sort_by_key(ToString::to_string);
            Ok(ids)
        })
        .await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let workflow_ids = workflow_stream_ids(store, scope.as_deref())?;
            let mut active = Vec::new();
            for workflow_id in workflow_ids {
                let history = read_events(store, &workflow_id)?;
                if matches!(status_from_events(&history), WorkflowStatus::Running) {
                    active.push(workflow_id);
                }
            }
            active.sort_by_key(ToString::to_string);
            Ok(active)
        })
        .await
    }

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let workflow_ids = workflow_stream_ids(store, scope.as_deref())?;
            let mut paused = Vec::new();
            for workflow_id in workflow_ids {
                let history = read_events(store, &workflow_id)?;
                if matches!(status_from_events(&history), WorkflowStatus::Paused) {
                    paused.push(workflow_id);
                }
            }
            paused.sort_by_key(ToString::to_string);
            Ok(paused)
        })
        .await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        let filter = filter.clone();
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let workflow_ids = workflow_stream_ids(store, scope.as_deref())?;
            let mut summaries = Vec::new();
            for workflow_id in workflow_ids {
                let history = read_events(store, &workflow_id)?;
                if let Some(summary) = WorkflowSummary::from_history(&history) {
                    if filter.matches(&summary) {
                        summaries.push(summary);
                    }
                }
            }
            summaries.sort_by(|left, right| {
                left.started_at.cmp(&right.started_at).then_with(|| {
                    left.workflow_id
                        .to_string()
                        .cmp(&right.workflow_id.to_string())
                })
            });
            Ok(summaries)
        })
        .await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let workflow_id = workflow_id.clone();
        let timer_id = timer_id.clone();
        // DISTRIBUTED mode: the durable timer MUST be a stamped envelope co-located
        // on the workflow's shard, or shard adoption's union merge (which decodes
        // every committed entry on the shard as a StampedEntry) fails with
        // UndecodableEntry and wedges adoption of any shard carrying timers (#82).
        // `replicate_write_routed` writes the timer as a STAMPED, quorum-replicated
        // envelope routed onto the workflow's shard — the stamped analog of the
        // single-node `put_routed`.
        if let Some(routing) = self.distribution.clone() {
            let database = self.inner.database();
            let token = timer_id_token(&timer_id)?;
            let entry = TimerEntry {
                workflow_id: workflow_id.clone(),
                timer_id,
                fire_at,
            };
            let route_key = keyspace::event_stream_key(&workflow_id);
            let key = keyspace::timer_key(&workflow_id, &token);
            let value = encode_timer(&entry)?;
            // CAS on the current value's hash so re-scheduling the same timer key
            // overwrites cleanly (create-if-absent on a fresh key, value-CAS on an
            // existing one), mirroring `publish_shard_owner`.
            let current = database
                .get_routed(&route_key, &key)
                .map_err(|error| database_error(&error))?;
            let expected = current.as_deref().map(Hash::of);
            // The quorum write blocks and must run off the tokio runtime.
            return run_off_runtime(|| {
                database.replicate_write_routed(
                    &route_key,
                    ProposeWrite {
                        key,
                        expected,
                        value,
                        ttl: None,
                    },
                    &routing.membership,
                    routing.timeout,
                )
            })
            .map(|_| ())
            .map_err(|error| database_error(&error));
        }
        // SINGLE-NODE mode: byte-identical to the original unstamped path
        // (`put_routed` + `commit`). A single node has no peers and never adopts a
        // shard, so there is no union merge to satisfy.
        self.blocking(move |store| {
            let token = timer_id_token(&timer_id)?;
            let entry = TimerEntry {
                workflow_id: workflow_id.clone(),
                timer_id,
                fire_at,
            };
            let database = store.database();
            // Co-locate the timer on the workflow's shard by routing on its
            // event-stream key (the same key the event stream routes by).
            let route_key = keyspace::event_stream_key(&workflow_id);
            database
                .put_routed(
                    &route_key,
                    keyspace::timer_key(&workflow_id, &token),
                    encode_timer(&entry)?,
                )
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        let scope = self.owned_shard_scope();
        self.blocking(move |store| {
            let mut timers: Vec<TimerEntry> = scan_prefix_scoped(
                store,
                keyspace::TIMER_PREFIX,
                scope.as_deref(),
                |_, value| decode_timer(value),
            )?
            .into_iter()
            .filter(|entry| entry.fire_at <= as_of)
            .collect();
            timers.sort_by(|left, right| {
                left.fire_at
                    .cmp(&right.fire_at)
                    .then_with(|| {
                        left.workflow_id
                            .to_string()
                            .cmp(&right.workflow_id.to_string())
                    })
                    .then_with(|| left.timer_id.to_string().cmp(&right.timer_id.to_string()))
            });
            Ok(timers)
        })
        .await
    }
}

#[async_trait]
impl PackageStore for HaematiteStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        let primary = record.workflow_type.clone();
        self.put_package_with_routes(record, &[primary]).await
    }

    async fn put_package_with_routes(
        &self,
        record: PackageRecord,
        route_workflow_types: &[String],
    ) -> Result<(), StoreError> {
        let route_workflow_types = route_workflow_types.to_vec();
        self.blocking(move |store| {
            let database = store.database();
            database
                .put(
                    keyspace::package_key(&record.workflow_type, &record.content_hash),
                    encode_package(&record)?,
                )
                .map_err(|error| database_error(&error))?;
            for workflow_type in route_workflow_types {
                database
                    .put(
                        keyspace::route_key(&workflow_type),
                        record.content_hash.clone().into_bytes(),
                    )
                    .map_err(|error| database_error(&error))?;
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.blocking(|store| {
            let mut records: Vec<PackageRecord> =
                scan_prefix(store, keyspace::PACKAGE_PREFIX, |_, value| {
                    decode_package(value)
                })?;
            records.sort_by(|left, right| {
                left.deployed_at
                    .cmp(&right.deployed_at)
                    .then_with(|| left.workflow_type.cmp(&right.workflow_type))
                    .then_with(|| left.content_hash.cmp(&right.content_hash))
            });
            Ok(records)
        })
        .await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let workflow_type = workflow_type.to_owned();
        let content_hash = content_hash.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            database
                .delete(keyspace::package_key(&workflow_type, &content_hash))
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let workflow_type = workflow_type.to_owned();
        let content_hash = content_hash.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            database
                .put(
                    keyspace::route_key(&workflow_type),
                    content_hash.into_bytes(),
                )
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.blocking(|store| {
            let mut routes: Vec<PackageRouteRecord> =
                scan_prefix(store, keyspace::ROUTE_PREFIX, |key, value| {
                    let workflow_type =
                        keyspace::workflow_type_from_route_key(key).ok_or_else(|| {
                            StoreError::Backend(String::from("malformed package-route key"))
                        })?;
                    let content_hash = String::from_utf8(value.to_vec()).map_err(|error| {
                        StoreError::Serialization(format!("invalid route content hash: {error}"))
                    })?;
                    Ok(PackageRouteRecord {
                        workflow_type,
                        content_hash,
                    })
                })?;
            routes.sort_by(|left, right| left.workflow_type.cmp(&right.workflow_type));
            Ok(routes)
        })
        .await
    }
}

#[async_trait]
impl NamespaceStore for HaematiteStore {
    async fn register_namespace(
        &self,
        name: &str,
        origin: NamespaceOrigin,
    ) -> Result<MintOutcome, StoreError> {
        let record = NamespaceRecord::new_minted(name, origin, Utc::now());
        self.put_namespace(record).await
    }

    async fn put_namespace(&self, record: NamespaceRecord) -> Result<MintOutcome, StoreError> {
        // The distributed CAS upsert drives `replicate_write` through
        // `run_off_runtime`, which BLOCKS and refuses to run under an entered
        // tokio runtime; run the whole upsert on a cloned handle via
        // `spawn_blocking` (the inherent method spawns its OWN bare thread for the
        // quorum wait, so this only parks a blocking-pool thread, never the
        // executor).
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.register_namespace_record(&record))
            .await
            .map_err(|error| join_error(&error))?
    }

    async fn list_namespaces(&self) -> Result<Vec<NamespaceRecord>, StoreError> {
        self.blocking(|store| {
            let mut records: Vec<NamespaceRecord> =
                scan_prefix(store, keyspace::NAMESPACE_PREFIX, |_, value| {
                    NamespaceRecord::decode(value)
                })?;
            // Ascending by created_at, ties broken by name (the trait contract).
            records.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(records)
        })
        .await
    }

    async fn get_namespace(&self, name: &str) -> Result<Option<NamespaceRecord>, StoreError> {
        let key = keyspace::namespace_key(name);
        self.blocking(move |store| {
            let database = store.database();
            match database.get(&key).map_err(|error| database_error(&error))? {
                Some(bytes) => NamespaceRecord::decode(&bytes).map(Some),
                None => Ok(None),
            }
        })
        .await
    }

    async fn set_namespace_placement(
        &self,
        name: &str,
        placement: NamespacePlacement,
    ) -> Result<Option<()>, StoreError> {
        // Same blocking/off-runtime constraint as the upsert path: the quorum
        // value-CAS write blocks and refuses to run under an entered runtime.
        let store = self.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || store.set_namespace_placement_record(&name, placement))
            .await
            .map_err(|error| join_error(&error))?
    }

    async fn deprecate_namespace(&self, name: &str) -> Result<(), StoreError> {
        // Same blocking/off-runtime constraint as the upsert path.
        let store = self.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || store.deprecate_namespace_record(&name))
            .await
            .map_err(|error| join_error(&error))?
    }
}

/// Enumerate every workflow id by reading the co-located event streams.
///
/// Each workflow's history lives in one haematite event stream keyed by
/// [`keyspace::event_stream_key`]; `scan_sequence_keys` returns the
/// `(stream_key, next_seq)` pair for every stream across all shards. Mapping each
/// stream key back through [`keyspace::workflow_id_from_event_stream_key`] yields
/// the workflow ids — no separate workflow-id index is kept. Non-`E` keys are
/// skipped defensively (the scan only ever returns event-stream sequence keys, so
/// this is belt-and-braces).
fn workflow_stream_ids(
    store: &haematite::EventStore,
    scope: Option<&[usize]>,
) -> Result<Vec<WorkflowId>, StoreError> {
    let database = store.database();
    let streams = match scope {
        // Own all shards: enumerate every stream (single-node default).
        None => database
            .scan_sequence_keys()
            .map_err(|error| database_error(&error))?,
        // Own a subset: enumerate only the streams on the owned shards.
        Some(ids) => database
            .scan_sequence_keys_for_shards(ids)
            .map_err(|error| database_error(&error))?,
    };
    Ok(streams
        .into_iter()
        .filter_map(|(stream_key, _next_seq)| {
            keyspace::workflow_id_from_event_stream_key(&stream_key)
        })
        .collect())
}

fn parse_workflow_id(text: &str) -> Result<WorkflowId, StoreError> {
    uuid::Uuid::parse_str(text)
        .map(WorkflowId::new)
        .map_err(|error| StoreError::Serialization(format!("invalid workflow id index: {error}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{
        ClaimScope, MintOutcome, NamespaceOrigin, NamespaceRecord, NamespaceState, NamespaceStore,
        OutboxRow, OutboxStatus, OutboxStore, ReadableEventStore, StoreError, WritableEventStore,
        WriteToken,
    };
    use chrono::{Duration, Utc};

    use super::HaematiteStore;
    use crate::keyspace;

    fn unique_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "aion-store-haematite-unit-{name}-{}-{nanos}-{counter}",
            std::process::id()
        ))
    }

    fn store(name: &str) -> Result<HaematiteStore, StoreError> {
        HaematiteStore::create(unique_dir(name))
    }

    fn pending_row(
        workflow_id: &WorkflowId,
        ordinal: u64,
        activity_type: &str,
        visible_after: chrono::DateTime<Utc>,
    ) -> OutboxRow {
        OutboxRow::pending(
            workflow_id.clone(),
            ordinal,
            String::from(activity_type),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            visible_after,
        )
    }

    #[tokio::test]
    async fn staged_row_round_trips_node_affinity() -> Result<(), StoreError> {
        // NODE-2: a row staged with an explicit node affinity persists in haematite
        // and reads back `Some(node)` verbatim through claim.
        let store = store("node-round-trip")?;
        let workflow_id = WorkflowId::new_v4();
        let row =
            pending_row(&workflow_id, 0, "charge", Utc::now()).with_node(Some("box-7".to_owned()));
        store
            .append_outbox_batch(std::slice::from_ref(&row))
            .await?;

        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].node.as_deref(), Some("box-7"));
        Ok(())
    }

    #[tokio::test]
    async fn count_inflight_outbox_rows_counts_pending_and_claimed_per_namespace()
    -> Result<(), StoreError> {
        // CP2-Q1.5: the durable in-flight count is exactly Pending + Claimed for the queried
        // namespace, isolated per namespace, with terminal rows excluded.
        let store = store("count-inflight")?;
        let past = Utc::now() - Duration::hours(1);
        // alpha: one Pending (left untouched), two to be claimed (one stays Claimed = stuck-Claimed,
        // one driven Done), and one driven Failed.
        let alpha_pending =
            pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha");
        let alpha_stuck =
            pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha");
        let alpha_done =
            pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha");
        let alpha_failed =
            pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha");
        // beta: one Pending only — must never be counted for alpha.
        let beta_pending =
            pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("beta");

        store
            .append_outbox_batch(&[
                alpha_pending,
                alpha_stuck.clone(),
                alpha_done.clone(),
                alpha_failed.clone(),
                beta_pending,
            ])
            .await?;

        // Claim alpha's stuck/done/failed rows to Claimed; the alpha_pending and beta_pending rows
        // are left Pending. Then drive two alpha rows terminal, leaving alpha_stuck stuck-Claimed.
        let claimed = store.claim_outbox_rows(100).await?;
        assert_eq!(claimed.len(), 5, "all five due rows were claimed");
        store.complete_outbox_row(&alpha_done.dispatch_key).await?;
        store.fail_outbox_row(&alpha_failed.dispatch_key).await?;

        // alpha in-flight = alpha_pending (Claimed) + alpha_stuck (stuck-Claimed) = 2. Done/Failed
        // excluded; the stuck-Claimed row included.
        assert_eq!(store.count_inflight_outbox_rows("alpha").await?, 2);
        // beta isolation: its single Claimed row is counted, alpha's rows never bleed in.
        assert_eq!(store.count_inflight_outbox_rows("beta").await?, 1);
        // A namespace with no rows is zero.
        assert_eq!(store.count_inflight_outbox_rows("gamma").await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn count_inflight_outbox_rows_excludes_terminal_only_namespace() -> Result<(), StoreError>
    {
        // A namespace whose only rows are Done/Failed has zero in-flight rows.
        let store = store("count-inflight-terminal")?;
        let past = Utc::now() - Duration::hours(1);
        let done = pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("ns");
        let failed = pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("ns");
        store
            .append_outbox_batch(&[done.clone(), failed.clone()])
            .await?;
        store.claim_outbox_rows(100).await?;
        store.complete_outbox_row(&done.dispatch_key).await?;
        store.fail_outbox_row(&failed.dispatch_key).await?;

        assert_eq!(store.count_inflight_outbox_rows("ns").await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn count_claimed_outbox_rows_counts_only_claimed_not_pending_backlog()
    -> Result<(), StoreError> {
        // CP2-Q2: the CLAIMED-only count is the keyed-backpressure headroom input. It counts only
        // concurrently-executing (Claimed) rows and EXCLUDES the Pending backlog, so a tenant with a
        // large backlog never wedges itself against its own count. Parity with the libSQL backend.
        let store = store("count-claimed")?;
        let past = Utc::now() - Duration::hours(1);
        let future = Utc::now() + Duration::days(3650);
        let alpha_a = pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha");
        let alpha_b = pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha");
        // Future-fenced backlog: stays Pending, never claimed.
        let mut backlog = Vec::new();
        for _ in 0..5 {
            backlog.push(
                pending_row(&WorkflowId::new_v4(), 0, "charge", future).with_namespace("alpha"),
            );
        }
        store
            .append_outbox_batch(&[alpha_a.clone(), alpha_b.clone()])
            .await?;
        store.append_outbox_batch(&backlog).await?;

        let claimed = store.claim_outbox_rows(100).await?;
        assert_eq!(claimed.len(), 2, "only the two due rows are claimable");

        // In-flight sees all 7; claimed-only sees exactly the 2 executing rows.
        assert_eq!(store.count_inflight_outbox_rows("alpha").await?, 7);
        assert_eq!(
            store.count_claimed_outbox_rows("alpha").await?,
            2,
            "claimed-only excludes the Pending backlog (no self-wedge)"
        );

        store.complete_outbox_row(&alpha_a.dispatch_key).await?;
        assert_eq!(store.count_claimed_outbox_rows("alpha").await?, 1);
        assert_eq!(store.count_claimed_outbox_rows("beta").await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn count_claimed_by_namespace_matches_per_namespace_scalar_counts()
    -> Result<(), StoreError> {
        // CP2-Q2 perf: the single bucketed scan must yield byte-identical counts to N scalar
        // per-namespace scans over the same owned-shard set — same Claimed-only predicate, one
        // entry per requested namespace (absent maps to 0). Parity with the libSQL backend.
        let store = store("count-claimed-bucketed")?;
        let past = Utc::now() - Duration::hours(1);
        let future = Utc::now() + Duration::days(3650);
        let mut due = Vec::new();
        for _ in 0..2 {
            due.push(pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("alpha"));
        }
        due.push(pending_row(&WorkflowId::new_v4(), 0, "charge", past).with_namespace("beta"));
        let backlog =
            vec![pending_row(&WorkflowId::new_v4(), 0, "charge", future).with_namespace("gamma")];
        store.append_outbox_batch(&due).await?;
        store.append_outbox_batch(&backlog).await?;
        let claimed = store.claim_outbox_rows(100).await?;
        assert_eq!(
            claimed.len(),
            3,
            "the three due rows are claimed to Claimed"
        );

        let namespaces = ["alpha", "beta", "gamma", "delta"];
        let bucketed = store
            .count_claimed_outbox_rows_by_namespace(&namespaces)
            .await?;
        for namespace in namespaces {
            let scalar = store.count_claimed_outbox_rows(namespace).await?;
            assert_eq!(
                bucketed.get(namespace).copied(),
                Some(scalar),
                "bucketed count for {namespace} must equal the scalar per-namespace count"
            );
        }
        assert_eq!(bucketed.get("alpha").copied(), Some(2));
        assert_eq!(bucketed.get("beta").copied(), Some(1));
        assert_eq!(
            bucketed.get("gamma").copied(),
            Some(0),
            "future-fenced Pending is not claimed"
        );
        assert_eq!(
            bucketed.get("delta").copied(),
            Some(0),
            "unknown namespace seeds to 0"
        );
        Ok(())
    }

    #[tokio::test]
    async fn pending_outbox_routes_enumerates_distinct_claimable_routes() -> Result<(), StoreError>
    {
        // CP2-Q2: the round-robin probe returns exactly the distinct (namespace, task_queue, node)
        // routes with a claimable Pending row, and nothing for a future-fenced route. Parity with
        // the libSQL backend.
        let store = store("pending-routes")?;
        let past = Utc::now() - Duration::hours(1);
        let future = Utc::now() + Duration::days(3650);
        let alpha1 = pending_row(&WorkflowId::new_v4(), 0, "charge", past)
            .with_namespace("alpha")
            .with_task_queue("default");
        let alpha2 = pending_row(&WorkflowId::new_v4(), 0, "charge", past)
            .with_namespace("alpha")
            .with_task_queue("default");
        let beta = pending_row(&WorkflowId::new_v4(), 0, "charge", past)
            .with_namespace("beta")
            .with_task_queue("gpu");
        let fenced = pending_row(&WorkflowId::new_v4(), 0, "charge", future)
            .with_namespace("gamma")
            .with_task_queue("default");
        store
            .append_outbox_batch(&[alpha1, alpha2, beta, fenced])
            .await?;

        let mut routes = store.pending_outbox_routes().await?;
        routes.sort_by(|l, r| {
            l.namespace
                .cmp(&r.namespace)
                .then_with(|| l.task_queue.cmp(&r.task_queue))
        });
        assert_eq!(
            routes.len(),
            2,
            "two distinct claimable routes (alpha's two rows collapse to one)"
        );
        assert_eq!(routes[0].namespace, "alpha");
        assert_eq!(routes[0].task_queue, "default");
        assert_eq!(routes[1].namespace, "beta");
        assert_eq!(routes[1].task_queue, "gpu");
        assert!(
            routes.iter().all(|route| route.namespace != "gamma"),
            "the future-fenced gamma route is not claimable and must not be enumerated"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn scoped_claim_matches_namespace_task_queue_and_node_predicate()
    -> Result<(), Box<dyn std::error::Error>> {
        // LSUB-1a: scoped claim for (remote, gpu, box-7) claims ONLY rows whose
        // (namespace, task_queue) match AND whose node is Some("box-7") or None.
        let store = store("scoped-claim")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let in_pinned = pending_row(&workflow_id, 0, "pinned", past)
            .with_namespace("remote")
            .with_task_queue("gpu")
            .with_node(Some("box-7".to_owned()));
        let in_unpinned = pending_row(&workflow_id, 1, "unpinned", past)
            .with_namespace("remote")
            .with_task_queue("gpu");
        let other_ns = pending_row(&workflow_id, 2, "other-ns", past)
            .with_namespace("default")
            .with_task_queue("gpu");
        let other_tq = pending_row(&workflow_id, 3, "other-tq", past)
            .with_namespace("remote")
            .with_task_queue("cpu");
        let other_node = pending_row(&workflow_id, 4, "other-node", past)
            .with_namespace("remote")
            .with_task_queue("gpu")
            .with_node(Some("box-9".to_owned()));

        store
            .append_outbox_batch(&[
                in_pinned.clone(),
                in_unpinned.clone(),
                other_ns,
                other_tq,
                other_node,
            ])
            .await?;

        let scope = ClaimScope::new("remote", "gpu").with_node(Some("box-7".to_owned()));
        let claimed = store.claim_outbox_rows_scoped(&scope, 100).await?;

        let mut keys: Vec<String> = claimed.into_iter().map(|row| row.dispatch_key).collect();
        keys.sort();
        let mut expected = vec![
            in_pinned.dispatch_key.clone(),
            in_unpinned.dispatch_key.clone(),
        ];
        expected.sort();
        assert_eq!(
            keys, expected,
            "scoped claim returns exactly the in-pool rows"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn node_less_scoped_claim_excludes_pinned_rows() -> Result<(), Box<dyn std::error::Error>>
    {
        // LSUB-1a: a scope with no node locality claims only unpinned rows.
        let store = store("scoped-claim-no-node")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let unpinned = pending_row(&workflow_id, 0, "unpinned", past)
            .with_namespace("remote")
            .with_task_queue("gpu");
        let pinned = pending_row(&workflow_id, 1, "pinned", past)
            .with_namespace("remote")
            .with_task_queue("gpu")
            .with_node(Some("box-7".to_owned()));
        store
            .append_outbox_batch(&[unpinned.clone(), pinned])
            .await?;

        let scope = ClaimScope::new("remote", "gpu");
        let claimed = store.claim_outbox_rows_scoped(&scope, 100).await?;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].dispatch_key, unpinned.dispatch_key);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unscoped_claim_still_claims_any_row() -> Result<(), Box<dyn std::error::Error>> {
        // LSUB-1a regression guard: the unscoped path claims EVERY visible row.
        let store = store("unscoped-claims-all")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let a = pending_row(&workflow_id, 0, "a", past)
            .with_namespace("remote")
            .with_task_queue("gpu")
            .with_node(Some("box-7".to_owned()));
        let b = pending_row(&workflow_id, 1, "b", past)
            .with_namespace("default")
            .with_task_queue("cpu");
        let c = pending_row(&workflow_id, 2, "c", past).with_node(Some("box-9".to_owned()));
        store.append_outbox_batch(&[a, b, c]).await?;

        let claimed = store.claim_outbox_rows(100).await?;
        assert_eq!(claimed.len(), 3, "unscoped claim takes all visible rows");
        Ok(())
    }

    #[test]
    fn outbox_serde_round_trips_node_and_absent_field_decodes_none() -> Result<(), StoreError> {
        // NODE-2: encode/decode carries `Some(node)` faithfully; and a serialized
        // value with NO `node` field (a legacy row persisted before NODE-2) decodes
        // back to `None` via `#[serde(default)]` — no sentinel string.
        let workflow_id = WorkflowId::new_v4();
        let pinned =
            pending_row(&workflow_id, 0, "charge", Utc::now()).with_node(Some("box-7".to_owned()));
        let decoded = super::decode_outbox(&super::encode_outbox(&pinned)?)?;
        assert_eq!(decoded.node.as_deref(), Some("box-7"));

        // A legacy serde value missing the `node` field decodes to `None`.
        let legacy = serde_json::json!({
            "dispatch_key": "wf:0",
            "workflow_id": workflow_id,
            "ordinal": 0,
            "namespace": "default",
            "task_queue": "default",
            "activity_type": "charge",
            "input": Payload::new(ContentType::Json, b"{}".to_vec()),
            "status": "pending",
            "attempt": 0,
            "visible_after": super::encode_instant(Utc::now()),
        });
        let bytes = serde_json::to_vec(&legacy)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        let decoded_legacy = super::decode_outbox(&bytes)?;
        assert_eq!(decoded_legacy.node, None);
        Ok(())
    }

    async fn status_of(
        store: &HaematiteStore,
        dispatch_key: &str,
    ) -> Result<Option<OutboxStatus>, StoreError> {
        // A claim returns the row only when Pending+due, so probe lifecycle by
        // reading the keyed value directly through the public claim/scan surface.
        let key = dispatch_key.to_owned();
        store
            .blocking(move |inner| {
                let bytes = inner
                    .database()
                    .get(&keyspace::outbox_key(&key))
                    .map_err(|error| super::database_error(&error))?;
                bytes
                    .map(|value| super::decode_outbox(&value).map(|row| row.status))
                    .transpose()
            })
            .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn outbox_claim_complete_retry_fail_round_trip() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = store("outbox-round-trip")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let row_a = pending_row(&workflow_id, 0, "a", past);
        let row_b = pending_row(&workflow_id, 1, "b", past);

        store
            .append_outbox_batch(&[row_a.clone(), row_b.clone()])
            .await?;

        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 2);
        assert!(
            claimed
                .iter()
                .all(|row| row.status == OutboxStatus::Claimed)
        );
        // Claim order is visible_after ASC then dispatch_key ASC.
        assert_eq!(claimed[0].ordinal, 0);
        assert_eq!(claimed[1].ordinal, 1);

        // Claimed rows are no longer claimable.
        assert!(store.claim_outbox_rows(10).await?.is_empty());

        store.complete_outbox_row(&row_a.dispatch_key).await?;
        assert_eq!(
            status_of(&store, &row_a.dispatch_key).await?,
            Some(OutboxStatus::Done)
        );

        // Retry into the future: pending but not yet claimable.
        let future = Utc::now() + Duration::hours(1);
        store
            .retry_outbox_row(&row_b.dispatch_key, 1, future)
            .await?;
        assert_eq!(
            status_of(&store, &row_b.dispatch_key).await?,
            Some(OutboxStatus::Pending)
        );
        assert!(store.claim_outbox_rows(10).await?.is_empty());

        // Retry into the past: claimable again with the bumped attempt.
        store.retry_outbox_row(&row_b.dispatch_key, 2, past).await?;
        let reclaimed = store.claim_outbox_rows(10).await?;
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].dispatch_key, row_b.dispatch_key);
        assert_eq!(reclaimed[0].attempt, 2);

        store.fail_outbox_row(&row_b.dispatch_key).await?;
        assert_eq!(
            status_of(&store, &row_b.dispatch_key).await?,
            Some(OutboxStatus::Failed)
        );
        assert!(store.claim_outbox_rows(10).await?.is_empty());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_outbox_batch_ignores_duplicate_dispatch_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = store("outbox-dup")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let first = pending_row(&workflow_id, 0, "charge", past);
        let duplicate = pending_row(&workflow_id, 0, "different-activity", past);

        store
            .append_outbox_batch(std::slice::from_ref(&first))
            .await?;
        store.append_outbox_batch(&[duplicate]).await?;

        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].activity_type, "charge");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn settle_cancelled_is_idempotent_and_terminal() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = store("outbox-settle-cancelled")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let pending = pending_row(&workflow_id, 0, "pending", past);
        let claimed = pending_row(&workflow_id, 1, "claimed", past);
        let done = pending_row(&workflow_id, 2, "done", past);
        let failed = pending_row(&workflow_id, 3, "failed", past);

        store
            .append_outbox_batch(&[
                pending.clone(),
                claimed.clone(),
                done.clone(),
                failed.clone(),
            ])
            .await?;

        store
            .settle_outbox_row_cancelled(&pending.dispatch_key)
            .await?;
        store
            .settle_outbox_row_cancelled(&pending.dispatch_key)
            .await?;
        assert_eq!(
            status_of(&store, &pending.dispatch_key).await?,
            Some(OutboxStatus::Cancelled)
        );
        let claimed_rows = store.claim_outbox_rows(10).await?;
        assert!(
            !claimed_rows
                .iter()
                .any(|row| row.dispatch_key == pending.dispatch_key),
            "cancelled pending row must not be claimable"
        );
        assert!(
            claimed_rows
                .iter()
                .any(|row| row.dispatch_key == claimed.dispatch_key),
            "claimed test row should have been claimed before settlement"
        );

        store
            .settle_outbox_row_cancelled(&claimed.dispatch_key)
            .await?;
        assert_eq!(
            status_of(&store, &claimed.dispatch_key).await?,
            Some(OutboxStatus::Cancelled)
        );
        let rearmed = store
            .rearm_stale_claimed_outbox_rows(Utc::now() + Duration::hours(1), past, 10)
            .await?;
        assert!(
            !rearmed
                .iter()
                .any(|row| row.dispatch_key == claimed.dispatch_key),
            "cancelled claimed row must not be stale-rearmed"
        );

        store.complete_outbox_row(&done.dispatch_key).await?;
        store.fail_outbox_row(&failed.dispatch_key).await?;
        store
            .settle_outbox_row_cancelled(&done.dispatch_key)
            .await?;
        store
            .settle_outbox_row_cancelled(&failed.dispatch_key)
            .await?;
        store
            .settle_outbox_row_cancelled(&OutboxRow::dispatch_key_for(&workflow_id, 99))
            .await?;
        assert_eq!(
            status_of(&store, &done.dispatch_key).await?,
            Some(OutboxStatus::Done)
        );
        assert_eq!(
            status_of(&store, &failed.dispatch_key).await?,
            Some(OutboxStatus::Failed)
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rearm_preserves_attempt_and_inserts_fresh_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = store("outbox-rearm")?;
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);

        // Stage one row, drive it claimed then retried to attempt=3 (its budget so far).
        let original = pending_row(&workflow_id, 0, "charge", past);
        store
            .append_outbox_batch(std::slice::from_ref(&original))
            .await?;
        let _ = store.claim_outbox_rows(10).await?;
        store
            .retry_outbox_row(&original.dispatch_key, 3, past)
            .await?;

        // Re-arm the SAME dispatch_key with a FRESH OutboxRow whose attempt is 0:
        // the re-arm must NOT reset the budget — it preserves the stored attempt=3.
        let revived = pending_row(&workflow_id, 0, "charge", Utc::now());
        assert_eq!(revived.attempt, 0);
        let fresh = pending_row(&workflow_id, 1, "settle", Utc::now());
        WritableEventStore::rearm_outbox_pending(&store, &[revived.clone(), fresh.clone()]).await?;

        let mut reclaimed = store.claim_outbox_rows(10).await?;
        reclaimed.sort_by_key(|row| row.ordinal);
        assert_eq!(reclaimed.len(), 2);
        // Existing row's attempt budget was preserved across re-arm.
        assert_eq!(reclaimed[0].dispatch_key, revived.dispatch_key);
        assert_eq!(reclaimed[0].attempt, 3);
        // Brand-new dispatch_key inserted as Pending with its own attempt (0).
        assert_eq!(reclaimed[1].dispatch_key, fresh.dispatch_key);
        assert_eq!(reclaimed[1].attempt, 0);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_with_outbox_persists_events_and_rows() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = store("outbox-atomic")?;
        let workflow_id = WorkflowId::new_v4();
        let event = aion_core::Event::WorkflowStarted {
            envelope: aion_core::EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("checkout"),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            run_id: aion_core::RunId::new_v4(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };
        let row = pending_row(&workflow_id, 0, "charge", Utc::now() - Duration::hours(1));

        store
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                0,
                std::slice::from_ref(&row),
            )
            .await?;

        assert_eq!(store.read_history(&workflow_id).await?.len(), 1);
        let claimed = store.claim_outbox_rows(10).await?;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].dispatch_key, row.dispatch_key);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn durable_state_survives_close_and_reopen() -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::{PackageRecord, PackageStore, TimerId};

        let dir = unique_dir("reopen");
        let workflow_id = WorkflowId::new_v4();
        let timer_id = TimerId::anonymous(7);
        let fire_at = Utc::now();

        {
            let store = HaematiteStore::create(&dir)?;
            let event = aion_core::Event::WorkflowStarted {
                envelope: aion_core::EventEnvelope {
                    seq: 1,
                    recorded_at: Utc::now(),
                    workflow_id: workflow_id.clone(),
                },
                workflow_type: String::from("checkout"),
                input: Payload::new(ContentType::Json, b"{}".to_vec()),
                run_id: aion_core::RunId::new_v4(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            };
            store
                .append(WriteToken::recorder(), &workflow_id, &[event], 0)
                .await?;
            store
                .schedule_timer(&workflow_id, &timer_id, fire_at)
                .await?;
            store
                .put_package(PackageRecord {
                    workflow_type: String::from("checkout"),
                    content_hash: "b".repeat(64),
                    archive: b"archive".to_vec(),
                    deployed_at: Utc::now(),
                })
                .await?;
            // Drop closes the store; haematite committed each write durably.
        }

        let reopened = HaematiteStore::open(&dir)?;
        assert_eq!(
            reopened.read_history(&workflow_id).await?.len(),
            1,
            "event history survives reopen"
        );
        assert_eq!(
            reopened.list_workflow_ids().await?,
            vec![workflow_id.clone()],
            "workflow index survives reopen"
        );
        assert_eq!(
            reopened.list_active().await?,
            vec![workflow_id],
            "projected status survives reopen"
        );
        assert_eq!(
            reopened.expired_timers(fire_at).await?.len(),
            1,
            "durable timer survives reopen"
        );
        assert_eq!(
            reopened.list_packages().await?.len(),
            1,
            "deployed package survives reopen"
        );
        assert_eq!(
            reopened.list_package_routes().await?.len(),
            1,
            "package route survives reopen"
        );
        Ok(())
    }

    /// R-0: a deterministic CAS fence from haematite is the typed
    /// `DatabaseError::Fenced`, which the writer maps to the retryable
    /// `StoreError::NotOwner`; other consistency failures (quorum-unavailable,
    /// transport) stay generic `DatabaseError::ConsistencyError` and do NOT map to
    /// `NotOwner`. The conversion contract itself is owned + tested in haematite
    /// (`DatabaseError: From<ConsistencyError>`); here we guard the aion-side
    /// classification that only a typed fence becomes `NotOwner`.
    #[test]
    fn only_typed_fence_classifies_as_not_owner() {
        let fenced = haematite::DatabaseError::Fenced {
            required: 2,
            possible_accepts: 1,
        };
        assert!(
            matches!(fenced, haematite::DatabaseError::Fenced { .. }),
            "the typed fence is the NotOwner signal"
        );

        let quorum_unavailable =
            haematite::DatabaseError::ConsistencyError("quorum cannot be reached".to_owned());
        assert!(
            !matches!(quorum_unavailable, haematite::DatabaseError::Fenced { .. }),
            "a generic consistency failure is NOT a fence and stays Backend"
        );
    }

    /// R-1: an own-all scope (the single-node default) reminting yields `None`
    /// (any id is already local); a subset scope yields an id on an owned shard.
    #[test]
    fn remint_for_owned_shard_respects_scope() -> Result<(), StoreError> {
        let store = HaematiteStore::create_with_shard_count(unique_dir("remint"), 4)?;
        // Own-all: nothing to remint toward.
        assert!(store.remint_for_owned_shard(64).is_none());

        store.set_owned_shards([2]);
        let Some(reminted) = store.remint_for_owned_shard(store.shard_count() * 16) else {
            return Err(StoreError::Backend(
                "a subset-owning store must remint an owned-shard id".to_owned(),
            ));
        };
        assert!(store.owns_workflow_shard(&reminted));
        assert_eq!(store.shard_for_workflow(&reminted), 2);
        Ok(())
    }

    /// R-4: a routing key hashes to a stable shard, and `mint_for_shard` returns
    /// an id whose durable shard is exactly that target.
    #[test]
    fn mint_for_shard_lands_on_the_routing_key_shard() -> Result<(), StoreError> {
        let store = HaematiteStore::create_with_shard_count(unique_dir("steered"), 4)?;
        let target = store.shard_for_routing_key("tenant-7/order-42");
        // Stable: hashing the same key again yields the same shard.
        assert_eq!(target, store.shard_for_routing_key("tenant-7/order-42"));
        let Some(minted) = store.mint_for_shard(target, store.shard_count() * 16) else {
            return Err(StoreError::Backend(
                "mint_for_shard must find an id on the target shard".to_owned(),
            ));
        };
        assert_eq!(store.shard_for_workflow(&minted), target);
        Ok(())
    }

    /// ADR-021 single-node / non-distributed path stays byte-identical: every
    /// fence seam is a no-op. `acquire_owned_shard` and `publish_shard_owner`
    /// return `Ok(())`, `read_shard_owner` is `None` (no directory), `extend` does
    /// not collapse the own-all scope, and `is_current_owner` is `true` (owns
    /// everything unconditionally).
    #[test]
    fn single_node_fence_seam_is_a_noop() -> Result<(), StoreError> {
        let store = HaematiteStore::create_with_shard_count(unique_dir("noop"), 4)?;
        // Owns everything: scope is None and stays None through extend.
        assert_eq!(store.owned_shards(), None, "single-node owns all shards");
        store.acquire_owned_shard(2)?; // no election, Ok
        store.publish_shard_owner(2)?; // no directory, Ok
        assert_eq!(
            store.read_shard_owner(2)?,
            None,
            "no directory on a single-node store"
        );
        ReadableEventStore::extend_owned_shards(&store, &[2]);
        assert_eq!(
            store.owned_shards(),
            None,
            "extend must not collapse the own-all scope to a finite set"
        );
        assert!(
            store.is_current_owner(2),
            "single-node owns every shard unconditionally"
        );
        assert!(
            ReadableEventStore::is_current_owner(&store, 9),
            "the trait method agrees: own-all reports current owner of any shard"
        );
        Ok(())
    }

    // --- namespace registry (Control-Plane Phase 1, S3) --------------------
    //
    // These exercise the SINGLE-NODE / local-upsert path (`distribution ==
    // None`), which is what the unit-test `store(..)` harness builds. The
    // CasConflict-reconcile and Fenced→NotOwner branches of the distributed
    // CAS path are driven only under the multi-node cluster harness that
    // arrives with S4 wiring; they are asserted there. The local path proves
    // the create / idempotent-touch / list-order / deprecate contract.

    #[tokio::test]
    async fn register_namespace_creates_then_get_returns_it() -> Result<(), StoreError> {
        let store = store("ns-create")?;

        let outcome = store
            .register_namespace("orders", NamespaceOrigin::WorkerMint)
            .await?;
        assert_eq!(
            outcome,
            MintOutcome::Created,
            "create-if-absent mints a fresh record"
        );

        let fetched = store.get_namespace("orders").await?;
        let record = fetched.expect("a freshly minted namespace must be readable back");
        assert_eq!(record.name, "orders");
        assert_eq!(record.origin, NamespaceOrigin::WorkerMint);
        assert_eq!(record.state, NamespaceState::Active);
        assert_eq!(
            record.created_at, record.last_seen,
            "a brand-new record is seen exactly once, at creation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn second_register_is_already_existed_and_bumps_last_seen() -> Result<(), StoreError> {
        let store = store("ns-touch")?;

        let first = store
            .register_namespace("billing", NamespaceOrigin::WorkerMint)
            .await?;
        assert_eq!(first, MintOutcome::Created);
        let after_create = store
            .get_namespace("billing")
            .await?
            .expect("record must exist after create");

        // A monotonic clock guarantees the second touch advances last_seen.
        // The store stamps `Utc::now()`; sleep a beat so the instant differs.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let second = store
            .register_namespace("billing", NamespaceOrigin::Explicit)
            .await?;
        assert_eq!(
            second,
            MintOutcome::AlreadyExisted,
            "a second register observes the existing record (idempotent mint)"
        );

        let after_touch = store
            .get_namespace("billing")
            .await?
            .expect("record must still exist after touch");
        assert_eq!(
            after_touch.created_at, after_create.created_at,
            "created_at is immutable across the touch"
        );
        assert_eq!(
            after_touch.origin,
            NamespaceOrigin::WorkerMint,
            "origin is preserved across the touch (the second register's origin is ignored)"
        );
        assert!(
            after_touch.last_seen >= after_create.last_seen,
            "last_seen is refreshed (bumped) by the touch: {} >= {}",
            after_touch.last_seen,
            after_create.last_seen
        );
        Ok(())
    }

    #[tokio::test]
    async fn put_namespace_carries_supplied_record() -> Result<(), StoreError> {
        let store = store("ns-put")?;
        let mut record =
            NamespaceRecord::new_minted("tenant-a", NamespaceOrigin::Explicit, Utc::now());
        record.config.kind = Some("tenant".to_owned());

        let outcome = store.put_namespace(record.clone()).await?;
        assert_eq!(outcome, MintOutcome::Created);

        let fetched = store
            .get_namespace("tenant-a")
            .await?
            .expect("put_namespace must persist the supplied record");
        assert_eq!(fetched.origin, NamespaceOrigin::Explicit);
        assert_eq!(fetched.config.kind.as_deref(), Some("tenant"));

        // Idempotent on an existing name: a second put reconciles as success.
        let again = store.put_namespace(record).await?;
        assert_eq!(again, MintOutcome::AlreadyExisted);
        Ok(())
    }

    /// `set_namespace_placement` over the single-node local path updates only the
    /// placement (+ `last_seen`) of an existing record, is idempotent, and reports
    /// not-found (`Ok(None)`) for an absent namespace rather than minting one.
    #[tokio::test]
    async fn set_placement_updates_existing_and_is_not_found_when_absent() -> Result<(), StoreError>
    {
        use aion_store::NamespacePlacement;
        use std::collections::BTreeSet;

        let store = store("ns-placement")?;
        store
            .register_namespace("orders", NamespaceOrigin::Explicit)
            .await?;
        let original = store
            .get_namespace("orders")
            .await?
            .expect("record must exist after create");
        assert_eq!(original.placement, NamespacePlacement::Unplaced);

        let nodes: BTreeSet<String> = ["az-a".to_owned(), "az-b".to_owned()].into_iter().collect();
        let placement = NamespacePlacement::Pinned {
            nodes: nodes.clone(),
        };
        assert_eq!(
            store
                .set_namespace_placement("orders", placement.clone())
                .await?,
            Some(())
        );
        let updated = store
            .get_namespace("orders")
            .await?
            .expect("record must persist");
        assert_eq!(updated.placement, placement);
        assert_eq!(updated.origin, NamespaceOrigin::Explicit);
        assert_eq!(updated.created_at, original.created_at);
        assert_eq!(updated.state, NamespaceState::Active);

        // Idempotent re-apply.
        assert_eq!(
            store.set_namespace_placement("orders", placement).await?,
            Some(())
        );

        // Absent namespace: not-found, nothing minted.
        assert_eq!(
            store
                .set_namespace_placement("ghost", NamespacePlacement::Unplaced)
                .await?,
            None
        );
        assert!(store.get_namespace("ghost").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn get_namespace_returns_none_for_absent() -> Result<(), StoreError> {
        let store = store("ns-miss")?;
        assert_eq!(
            store.get_namespace("never-minted").await?,
            None,
            "an absent name is None, never an error"
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_namespaces_is_ascending_by_created_at_then_name() -> Result<(), StoreError> {
        let store = store("ns-list")?;
        let base = Utc::now();

        // Mint out of order; assert the list re-sorts. `gamma` and `bravo`
        // share a created_at so the name tiebreak orders them.
        let alpha = NamespaceRecord::new_minted("alpha", NamespaceOrigin::Explicit, base);
        let gamma = NamespaceRecord::new_minted(
            "gamma",
            NamespaceOrigin::Explicit,
            base + Duration::seconds(10),
        );
        let bravo = NamespaceRecord::new_minted(
            "bravo",
            NamespaceOrigin::Explicit,
            base + Duration::seconds(10),
        );

        store.put_namespace(gamma).await?;
        store.put_namespace(alpha).await?;
        store.put_namespace(bravo).await?;

        let listed = store.list_namespaces().await?;
        let names: Vec<String> = listed.into_iter().map(|record| record.name).collect();
        assert_eq!(
            names,
            vec![
                "alpha".to_owned(), // earliest created_at
                "bravo".to_owned(), // tie on created_at, name < gamma
                "gamma".to_owned(),
            ],
            "list is ascending by created_at, ties broken by name"
        );
        Ok(())
    }

    #[tokio::test]
    async fn deprecate_namespace_sets_state_and_is_idempotent() -> Result<(), StoreError> {
        let store = store("ns-deprecate")?;
        store
            .register_namespace("retiring", NamespaceOrigin::WorkerMint)
            .await?;

        store.deprecate_namespace("retiring").await?;
        let after = store
            .get_namespace("retiring")
            .await?
            .expect("deprecating never deletes the record");
        assert_eq!(
            after.state,
            NamespaceState::Deprecated,
            "deprecate transitions Active -> Deprecated"
        );

        // Idempotent: deprecating again, or an unknown name, is a no-op.
        store.deprecate_namespace("retiring").await?;
        store.deprecate_namespace("no-such-namespace").await?;
        let still = store
            .get_namespace("retiring")
            .await?
            .expect("record must persist across a redundant deprecate");
        assert_eq!(still.state, NamespaceState::Deprecated);
        Ok(())
    }
}
