//! Cluster topology and ownership events for the dashboard real-time channel (WS3).
//!
//! This module defines the *typed contract* for the second class of real-time push the dashboard
//! consumes: cluster/topology/ownership deltas that today exist only as `tracing` logs and
//! Prometheus gauges. The wire shapes live in `aion-core` (not `aion-server`) for one reason: only
//! this leaf crate depends on `ts-rs`, so this is the single place a Rust type can cross the
//! Rust -> TypeScript boundary into the dashboard's generated bindings.
//!
//! # Scope (WS3 FOUNDATION)
//!
//! This file is *types only*. It deliberately does **not** define the broadcast publisher, the
//! emit call-sites in the supervisor / registry, the subscription endpoint, or the namespace gate.
//! Those are wired in later WS3 increments. Defining the contract first lets the dashboard and the
//! server agree on shapes before any behaviour ships.
//!
//! # Honesty corrections applied to the original design
//!
//! The design table named several variants whose payloads cannot be sourced from the subsystems
//! that would emit them today. Rather than ship a type whose fields are guaranteed to be faked,
//! those variants are **descoped from Phase 1** and documented here so the omission is explicit and
//! not silently rediscovered at wiring time:
//!
//! - **`NodeMetricsSampled` — DEFERRED.** `aion-server`'s metrics (`observability/metrics.rs`) have
//!   no `node` dimension (`connected_workers` is an `IntGaugeVec` keyed by *namespace*) and there
//!   is no `workflows_running` gauge at all. A `{ node, connected_workers, workflows_running }`
//!   payload cannot be produced by "piggybacking the gauge setters". Sourcing it honestly requires
//!   a real metrics change (node-labelled gauges + a running gauge). Until then the dashboard
//!   derives `connected_workers` client-side from [`ClusterEvent::WorkerConnected`] /
//!   [`ClusterEvent::WorkerDisconnected`] deltas against the [`ClusterSnapshot`] baseline. Adding
//!   a timer that scrapes Prometheus to fill this variant is the exact polling-as-push regression
//!   WS3 exists to remove, so the variant is omitted rather than tempting that shortcut.
//! - **`ShardOwnerChanged` / `FencedCasRejected` — DEFERRED (fast-follow).** The haematite seam
//!   (`HaematiteStore::publish_ln(shard)`) carries only `shard`; the fenced-CAS reject is a
//!   `haematite::DatabaseError::Fenced { .. }` destructured with `..` and mapped to
//!   `StoreError::NotOwner { shard }`, carrying only `shard` — never the owner name or the
//!   attempted/current epochs. The proposed payloads require enriching haematite's `Fenced` error
//!   to surface owner identity and epoch, which is a cross-crate (likely cross-repo) change. The
//!   cluster map is still *honest* without these two: shard adoption is fully observable from the
//!   supervisor's `tick()` ([`ClusterEvent::ShardAdopted`] and friends). Only the store-side
//!   CAS-reject detail is delayed.
//!
//! # u64 precision across the TS boundary
//!
//! The ts-rs config exports every `u64` as TS `number` (`with_large_int("number")` in
//! `generated_types.rs`), which truncates above `2^53`. [`ClusterEventMeta::cluster_seq`] and the
//! epoch fields below are `u64`. This is the *same* accepted ceiling that already applies to
//! [`crate::EventEnvelope::seq`]; cluster sequencing follows the established project convention
//! rather than introducing a divergent string encoding. The ceiling is documented on each field so
//! the gap-detection math on the client is aware of the bound. A long-lived deployment must keep
//! `cluster_seq` below `2^53`; in practice this is never reached for a topology-event counter.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata stamped on every [`ClusterEvent`], mirroring [`crate::EventEnvelope`] for the cluster
/// channel.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct ClusterEventMeta {
    /// Monotonic sequence number assigned by the cluster publisher (a single deployment-global
    /// `AtomicU64`). The client uses this for gap detection and `after_seq` reconnect dedup, the
    /// cluster analog of [`crate::EventEnvelope::seq`].
    ///
    /// Exported to TypeScript as `number`; see the module docs for the accepted `2^53` ceiling.
    pub cluster_seq: u64,
    /// UTC wall-clock instant at which the originating subsystem observed the state change.
    pub observed_at: DateTime<Utc>,
}

/// Transport a connected worker is delivered to over.
///
/// Mirrors the live `aion_server::worker::registry::WorkerDelivery` discriminants without carrying
/// the (non-serializable) delivery channels, so it can cross the wire and the ts-rs boundary.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "transport")]
pub enum WorkerTransport {
    /// gRPC bidirectional-stream delivery (the default transport).
    Grpc,
    /// Liminal server-push delivery (feature-gated on the server).
    Liminal,
}

/// Why a worker left the connected set.
///
/// NOTE (wiring honesty): the single registry deregistration site
/// (`ConnectedWorkerRegistry::deregister` / `remove_worker`) does not today distinguish a transport
/// disconnect from an idle timeout from an explicit deregister. The emit increment MUST derive the
/// reason at the call site from real signal (e.g. a closed delivery channel vs an explicit
/// deregister RPC vs a liveness-timeout sweep) or collapse to the variant it can actually prove.
/// This enum defines the *contract*; it must not be populated with a fabricated distinction.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "reason")]
pub enum WorkerDeathReason {
    /// The worker's delivery transport dropped (stream/connection closed).
    Disconnect,
    /// The worker was removed by a liveness/idle-timeout sweep.
    Timeout,
    /// The worker explicitly deregistered.
    Deregistered,
}

/// A cluster/topology/ownership delta pushed over the dashboard real-time channel.
///
/// Tagged union on `type` to match the existing [`crate::Event`] wire shape. Every variant carries
/// [`ClusterEventMeta`] as `meta`. Cluster events are deployment-scoped, not namespace-stamped at
/// the envelope level; the `Worker*` variants carry their own `namespaces` so the server-side gate
/// can intersect them against the caller's grants (see the deferred `cluster_filter`).
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ClusterEvent {
    /// A peer was added to this node's watch set (cluster-membership grew mid-session).
    ///
    /// Distinct from [`Self::PeerConnected`]: this is a *topology* change (a new peer to watch),
    /// not a *liveness* transition. Without it, a peer that joins after the priming
    /// [`ClusterSnapshot`] would silently never appear on the map until the next snapshot.
    PeerAdded {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// The newly-watched peer's node name.
        peer_name: String,
        /// The peer's forwarding address, when known.
        forward_addr: Option<String>,
    },
    /// A watched peer transitioned from down/unknown to connected (a liveness recovery).
    ///
    /// EMIT NOTE: the supervisor `tick()` resets `consecutive_down`/`adopted` on the same tick it
    /// observes `connected`, so the recovery condition (`was_down = consecutive_down > 0 ||
    /// adopted`) MUST be captured *before* that reset and the emit driven by the captured value.
    /// Emitting after the reset produces no recovery events (every tick looks freshly connected).
    PeerConnected {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// The recovered peer's node name.
        peer_name: String,
        /// The peer's forwarding address, when known.
        forward_addr: Option<String>,
    },
    /// A watched peer was observed down on a supervisor tick.
    ///
    /// Emitted on every tick the peer is observed down; `confirmed` flips to `true` once
    /// `consecutive_down >= confirmations` (the debounce threshold that authorizes adoption).
    PeerDisconnected {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// The down peer's node name.
        peer_name: String,
        /// Consecutive ticks this peer has been observed down.
        consecutive_down: u32,
        /// Whether the debounce threshold has been crossed (adoption-eligible).
        confirmed: bool,
    },
    /// This node adopted shards previously owned by a failed peer.
    ShardAdopted {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// Shard indices adopted in this transition.
        shards: Vec<usize>,
        /// The peer the shards were adopted from.
        from_peer: String,
        /// This node's name (the new owner).
        adopted_by: String,
    },
    /// An adoption attempt failed and will be retried on a subsequent tick.
    ///
    /// HONESTY: the original design hardcoded `will_retry: true`. That is a silent lie — a peer
    /// later observed connected resets `adopted` and stops retrying, and the
    /// "handled elsewhere" path terminally stops. The retry decision is not expressible as a
    /// constant, so the field is omitted; the dashboard infers retry by observing whether a
    /// subsequent [`Self::ShardAdopted`] / [`Self::ShardAdoptionSkipped`] for the same
    /// `(from_peer, shards)` arrives, rather than trusting a promise that may never be kept
    /// (ADR-016 no-silent-failure).
    ShardAdoptionFailed {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// Shard indices the failed attempt targeted.
        shards: Vec<usize>,
        /// The peer the shards would have been adopted from.
        from_peer: String,
        /// Human-readable adoption error.
        error: String,
    },
    /// Adoption was skipped because the shards are already held by a live third-party owner.
    ShardAdoptionSkipped {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// Shard indices that were skipped.
        shards: Vec<usize>,
        /// The peer the shards would have been adopted from.
        from_peer: String,
        /// The live node currently holding the shards.
        held_by: String,
    },
    /// A worker joined the connected set.
    WorkerConnected {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// Stable worker identifier.
        worker_id: String,
        /// Namespaces this worker serves.
        namespaces: Vec<String>,
        /// Task-queue pool this worker serves within its namespaces.
        task_queue: String,
        /// Delivery transport for this worker.
        transport: WorkerTransport,
        /// Locality/node label, when the worker reported one.
        node: Option<String>,
    },
    /// A worker left the connected set.
    WorkerDisconnected {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// Stable worker identifier.
        worker_id: String,
        /// Namespaces this worker served.
        namespaces: Vec<String>,
        /// Why the worker left (see [`WorkerDeathReason`] wiring note).
        reason: WorkerDeathReason,
    },
    /// The cluster supervisor started on this node (lifecycle).
    ///
    /// Lets the calm-state view (ADR-019) distinguish "supervisor running, all peers healthy" from
    /// "supervisor not running".
    SupervisorStarted {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// This node's name.
        node: String,
    },
    /// The cluster supervisor stopped on this node (clean drain / shutdown).
    SupervisorStopped {
        /// Cluster-event metadata.
        meta: ClusterEventMeta,
        /// This node's name.
        node: String,
    },
}

/// A peer entry in the priming [`ClusterSnapshot`].
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct ClusterPeer {
    /// The peer's node name.
    pub peer_name: String,
    /// The peer's forwarding address, when known.
    pub forward_addr: Option<String>,
    /// Whether the peer is currently observed connected.
    pub connected: bool,
    /// Consecutive ticks observed down (0 when connected).
    pub consecutive_down: u32,
}

/// A shard ownership entry in the priming [`ClusterSnapshot`].
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct ClusterShard {
    /// Shard index.
    pub shard: usize,
    /// The node that currently owns the shard.
    pub owner: String,
    /// The epoch fence value at which the owner holds the shard.
    ///
    /// Exported to TypeScript as `number`; see the module docs for the accepted `2^53` ceiling.
    pub epoch: u64,
}

/// A connected-worker entry in the priming [`ClusterSnapshot`].
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct ClusterWorker {
    /// Stable worker identifier.
    pub worker_id: String,
    /// Namespaces this worker serves (already intersected with the caller's grants by the gate).
    pub namespaces: Vec<String>,
    /// Task-queue pool this worker serves.
    pub task_queue: String,
    /// Delivery transport for this worker.
    pub transport: WorkerTransport,
    /// Locality/node label, when reported.
    pub node: Option<String>,
}

/// A calm-state baseline of the whole cluster, sent as the priming reply before the live delta
/// stream so the dashboard can render an at-a-glance "all clear" before any [`ClusterEvent`]
/// arrives (ADR-019). On `cluster_lagged` the client re-requests this rather than replaying a
/// (non-durable) delta history.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub struct ClusterSnapshot {
    /// The reading node's own name (self-identity; baselines the `Peer*` deltas which describe
    /// *other* nodes).
    pub node: String,
    /// The `cluster_seq` this snapshot is consistent as-of; the client applies only deltas with a
    /// strictly greater `cluster_seq`.
    ///
    /// Exported to TypeScript as `number`; see the module docs for the accepted `2^53` ceiling.
    pub as_of_seq: u64,
    /// Watched peers and their current liveness.
    pub peers: Vec<ClusterPeer>,
    /// Shards owned by (or visible to) the reading node.
    pub shards: Vec<ClusterShard>,
    /// Connected workers, already gated to the caller's namespaces.
    pub workers: Vec<ClusterWorker>,
}

/// A command the dashboard can issue against the cluster channel (ADR-020 command seam).
///
/// Defined now for contract coherence. **Phase 1 ships only [`Self::RequestClusterSnapshot`]** (a
/// read). The mutating variants compile so the contract exists, but their handlers reject with an
/// `unimplemented` wire error — and, per ADR-020, MUST still run the full auth gate
/// (`caller.deploy_granted()`) *before* rejecting, so the seam's authorization contract is
/// exercised now and an `unimplemented` stub is never an auth-bypass-shaped hole.
///
/// Tagged on `command` (distinct from [`ClusterEvent`]'s `type`) because commands and events are
/// different directions on the wire; the dashboard's protocol parser keys command frames on
/// `command` and event frames on `type`.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "command")]
pub enum ClusterCommand {
    /// Read the current cluster baseline (peers + shards + workers). Read-only; needs only
    /// namespace-or-deploy scope. **Phase 1.**
    RequestClusterSnapshot {},
    /// Cancel a running workflow. Aspirational — handler returns `unimplemented`.
    CancelWorkflow {
        /// Owning namespace.
        namespace: String,
        /// Target workflow.
        workflow_id: String,
    },
    /// Reopen a failed/closed workflow. Aspirational — handler returns `unimplemented`.
    ReopenWorkflow {
        /// Owning namespace.
        namespace: String,
        /// Target workflow.
        workflow_id: String,
    },
    /// Redrive a single outbox row. Aspirational — handler returns `unimplemented`.
    RedriveOutboxRow {
        /// Owning namespace.
        namespace: String,
        /// Target workflow.
        workflow_id: String,
        /// Outbox row ordinal.
        ordinal: u64,
    },
    /// Drain a node (stop-new-work + finish-in-flight + safe shutdown). Aspirational.
    DrainNode {
        /// Target node name.
        node: String,
    },
    /// Planned epoch-fenced shard handoff to a target node. Aspirational.
    PlannedHandoff {
        /// Shard to move.
        shard: usize,
        /// Destination node.
        target_node: String,
    },
    /// Test-only chaos kill of a node. Aspirational (gated).
    ChaosKillNode {
        /// Target node name.
        node: String,
    },
}

/// A typed terminal error on the cluster channel.
///
/// Mirrors the workflow path's lagged contract: when a subscriber falls behind the bounded cluster
/// broadcast buffer the server sends exactly one of these and closes, carrying the skipped count so
/// the client can decide snapshot-vs-resume (it always re-requests a [`ClusterSnapshot`], since
/// there is no durable cluster history). Surfaced to the UI as a typed error, never silently
/// dropped (ADR-016).
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ClusterStreamError {
    /// The subscriber lagged past the bounded broadcast buffer; `skipped` deltas were dropped.
    ClusterLagged {
        /// Number of cluster events dropped because the subscriber fell behind.
        ///
        /// Exported to TypeScript as `number`; see the module docs for the accepted `2^53` ceiling.
        skipped: u64,
    },
}
