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
    Event, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary, status_from_events,
};
use aion_store::{
    OutboxRow, OutboxStatus, OutboxStore, PackageRecord, PackageRouteRecord, PackageStore,
    ReadableEventStore, RunSummary, StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use haematite::db::respond_to_inbound_writes;
use haematite::sync::membership::WriteMembership;
use haematite::sync::{DistributionEndpoint, SyncNodeId};
use haematite::{Database, DatabaseConfig, DatabaseError};
use serde::{Deserialize, Serialize};

use crate::error::{api_error, database_error, join_error, serde_error};
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
            sweep_interval: None,
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
    /// `timeout` bounds each quorum write.
    #[must_use]
    pub fn with_distribution(
        database: Database,
        membership: WriteMembership,
        timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(haematite::EventStore::new(database)),
            distribution: Some(DistributedRouting {
                membership,
                timeout,
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

    fn from_database(database: Database) -> Self {
        Self {
            inner: Arc::new(haematite::EventStore::new(database)),
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
                sweep_interval: None,
                distributed: None,
            })
            .map_err(|error| database_error(&error))?
        };
        let endpoint = DistributionEndpoint::bind(boot.node_id.clone(), boot.bind_address, 1, None)
            .map_err(|error| {
                StoreError::Backend(format!("cluster endpoint bind failed: {error}"))
            })?;
        let database = database.with_distribution(endpoint);

        let store = Self::with_distribution(database, boot.write_membership(), boot.timeout);
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
    activity_type: String,
    input: aion_core::Payload,
    status: String,
    attempt: u32,
    visible_after: String,
    #[serde(default)]
    claimed_at: Option<String>,
}

fn encode_outbox(row: &OutboxRow) -> Result<Vec<u8>, StoreError> {
    let stored = StoredOutboxRow {
        dispatch_key: row.dispatch_key.clone(),
        workflow_id: row.workflow_id.clone(),
        ordinal: row.ordinal,
        run_id: row.run_id.clone(),
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
    let mut next_seq = expected_seq + 1;
    for event in events {
        if event.seq() != next_seq {
            return Err(StoreError::Backend(format!(
                "event sequence must be contiguous: expected {next_seq}, got {}",
                event.seq()
            )));
        }
        next_seq += 1;
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
        // owner. haematite surfaces that as a `ConsistencyError` carrying the
        // `ConsistencyError::Fenced` Display text; map it to the typed, retryable
        // `NotOwner` so the request-routing edge can re-resolve/forward instead of
        // seeing an opaque `Backend` internal error (R-0). Any other consistency
        // failure (quorum unavailable, transport, timeout) is a genuine backend
        // boundary failure and stays `Backend`.
        Err(DatabaseError::ConsistencyError(ref message)) if is_fence_message(message) => {
            Err(StoreError::NotOwner {
                shard: database.shard_for(stream_key),
            })
        }
        Err(error) => Err(database_error(&error)),
    }
}

/// Whether a haematite `ConsistencyError` Display string is the CAS-reject fence
/// (`ConsistencyError::Fenced`) — the signal that this node is not the shard's
/// current owner. haematite reports the fence as a stringly-typed
/// `DatabaseError::ConsistencyError`, so the writer side matches on the stable
/// fence marker the `Fenced` variant renders (`"fenced by CAS rejects"`). This
/// is the minimal, repo-local detection until haematite exposes a typed fence
/// variant (then this becomes a typed match).
fn is_fence_message(message: &str) -> bool {
    message.contains("fenced by CAS rejects")
}

/// The current stored head (event count) for `workflow_id`.
fn stream_head(store: &haematite::EventStore, workflow_id: &WorkflowId) -> Result<u64, StoreError> {
    let events = read_events(store, workflow_id)?;
    Ok(events.iter().map(Event::seq).max().unwrap_or(0))
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
}

impl HaematiteStore {
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
        let Some(routing) = self.distribution.as_ref() else {
            // Single-node mode: nothing to elect, owns everything already.
            return Ok(());
        };
        let database = self.inner.database();
        for &shard in shards {
            run_off_runtime(|| {
                database.acquire_shard_and_serve(shard, &routing.membership, routing.timeout)
            })
            .map_err(|error| database_error(&error))?;
        }
        Ok(())
    }

    /// Widen the owned-enumeration scope by `shards`, unioning with the current
    /// set, via the inherent [`Self::extend_owned_shards`] (SS-5 failover) — so
    /// the engine can drive scope-widening through the type-erased
    /// `dyn ReadableEventStore` it holds after absorbing a dead peer's shards.
    fn extend_owned_shards(&self, shards: &[usize]) {
        Self::extend_owned_shards(self, shards.iter().copied());
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
        self.blocking(move |store| {
            let database = store.database();
            database
                .put(
                    keyspace::package_key(&record.workflow_type, &record.content_hash),
                    encode_package(&record)?,
                )
                .map_err(|error| database_error(&error))?;
            // put_package re-points the type's route at this version.
            database
                .put(
                    keyspace::route_key(&record.workflow_type),
                    record.content_hash.clone().into_bytes(),
                )
                .map_err(|error| database_error(&error))?;
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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{
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

    /// R-0: the fence detector must recognise the exact `ConsistencyError::Fenced`
    /// Display text haematite wraps into `DatabaseError::ConsistencyError`, and
    /// must NOT misclassify other consistency failures (quorum-unavailable,
    /// transport, timeout) as a fence.
    #[test]
    fn fence_message_detector_matches_only_the_cas_reject_fence() {
        // The literal Display of `ConsistencyError::Fenced` (haematite
        // sync/consistency.rs), as wrapped by `DatabaseError::ConsistencyError`.
        let fenced = "consistency requirement failed: fenced by CAS rejects: \
            required 2 accepts, only 1 still possible";
        assert!(super::is_fence_message(fenced));

        let quorum_unavailable = "consistency requirement failed: quorum cannot be \
            reached: required 2 acknowledgments, only 1 possible";
        assert!(!super::is_fence_message(quorum_unavailable));

        let transport = "consistency requirement failed: distribution transport \
            unavailable for quorum write";
        assert!(!super::is_fence_message(transport));
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
}
