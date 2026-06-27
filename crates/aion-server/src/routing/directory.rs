//! R-2 / SS-3 shard → owner directory (DISTRIBUTED-ROUTING-DESIGN §2.3).
//!
//! The [`ShardDirectory`] trait is the stable seam routing consumes; the full
//! SS-3 liminal-coordinator CAS directory later swaps the implementation without
//! touching the edge. This file ships [`StaticShardDirectory`], which layers
//! three sources, most-authoritative first:
//!
//! 1. **Live local ownership** (`store.owned_shards()`, which `adopt_shards`
//!    widens on failover) → [`OwnerView::Local`].
//! 2. **The SS-3 quorum-replicated shard-owner directory record**
//!    (`store.read_shard_owner()`): when a survivor adopts a dead owner's shard
//!    it PUBLISHES itself as the new owner (fenced, quorum-replicated), so every
//!    other survivor reads the *adopter* off its own replica and forwards there.
//!    This is the increment that closes gap #2 — without it a survivor that did
//!    not adopt the shard resolves the dead declared owner to `Unknown`, routes
//!    locally, and fails `WorkflowNotFound`.
//! 3. **Static peer config + live peer liveness** (`store.peer_connected()`) as
//!    the steady-state pre-adoption fallback.
//!
//! ## SS-3 status — minimal correct increment (not the full CAS directory)
//!
//! The published record IS quorum-backed and fenced (via haematite
//! `replicate_write` co-located on the adopted shard), which is the load-bearing
//! property: only the true election winner can publish, and the record is
//! linearizable per shard under the same epoch fence as the data. What the FULL
//! SS-3 directory (DISTRIBUTED-ROUTING-DESIGN §2.3 v2 / STORAGE-SWAP §3(e)) adds
//! on top: a single liminal global-name *coordinator* that owns the authoritative
//! `{shard, owner, epoch}` assignment for the WHOLE map (not just post-adoption
//! deltas), epoch-stamped cache invalidation in `NodeRef::epoch`, and assignment
//! at cluster formation / rebalance rather than only on failover. This increment
//! deliberately scopes to the failover-adoption delta — the one gap the kill-9
//! demo surfaced — behind the unchanged [`ShardDirectory`] trait so the full
//! coordinator is a later impl swap.

use std::net::SocketAddr;
use std::sync::Arc;

use aion_store_haematite::HaematiteStore;

/// A resolved remote shard owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRef {
    /// The owner's distribution node id (its `ClusterPeer.name`).
    pub node_id: String,
    /// The owner's gRPC client-API address for forwarding (R-3). `None` when the
    /// peer declared no `grpc_address` — then it is a known-but-not-forwardable
    /// owner and routing falls back to `NotOwner`.
    pub grpc_addr: Option<SocketAddr>,
    /// The ownership epoch the resolver believes is current. The static resolver
    /// has no epoch source, so it reports `0`; SS-3's CAS directory fills this in.
    pub epoch: u64,
}

/// The directory's view of a shard's current owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerView {
    /// This node owns the shard: proceed to the local engine.
    Local,
    /// Another node owns the shard. Carries the owner reference (which may or may
    /// not be forwardable, per `NodeRef::grpc_addr`).
    Remote(NodeRef),
    /// The owner is not known with confidence: either no peer declares the shard,
    /// or the peer that does is believed-down (its liveness link dropped). Route
    /// locally/optimistically — the epoch fence backstops correctness and the
    /// local supervisor's pending adoption converges ownership (§2.5).
    Unknown,
}

/// Resolves the current owner of a distribution shard.
///
/// Routing consumes this; SS-3 produces the authoritative implementation later.
pub trait ShardDirectory: Send + Sync {
    /// The current owner of `shard`, with the epoch the resolver believes is
    /// current.
    fn owner_of(&self, shard: usize) -> OwnerView;
}

/// One peer entry in the static directory: its name, the shards it statically
/// declares ownership of, and its (optional) gRPC forward address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryPeer {
    /// The peer's distribution node id.
    pub name: String,
    /// The shards this peer statically declares it owns.
    pub owned_shards: Vec<usize>,
    /// The peer's gRPC client-API address, if declared (R-3 forward target).
    pub grpc_addr: Option<SocketAddr>,
}

/// The static directory: static peer config + live local-ownership, the SS-3
/// quorum-replicated shard-owner overlay, and a peer-liveness overlay, all read
/// live from the cluster store.
pub struct StaticShardDirectory {
    /// The cluster store, read live for this node's owned shards, the SS-3
    /// shard-owner directory record, and peer liveness. Holding the `Arc` keeps
    /// the directory cheap to clone-by-Arc and always current without a rebuild
    /// on failover.
    store: Arc<HaematiteStore>,
    /// Peers and the shards they statically declare, with forward addresses.
    peers: Vec<DirectoryPeer>,
    /// This node's own distribution name, so a shard-owner record naming THIS
    /// node resolves `Local` (defensive — local ownership normally already
    /// reflects an adoption) and a record naming a peer can be matched to that
    /// peer's forward address. `None` leaves the SS-3 overlay's self-match
    /// disabled (the local-ownership check still covers self-adoption).
    self_node_id: Option<String>,
}

impl StaticShardDirectory {
    /// Build a directory over `store`, the configured `peers`, and this node's
    /// own distribution name `self_node_id` (used to resolve an SS-3 shard-owner
    /// record that names this node).
    #[must_use]
    pub fn new(
        store: Arc<HaematiteStore>,
        peers: Vec<DirectoryPeer>,
        self_node_id: Option<String>,
    ) -> Self {
        Self {
            store,
            peers,
            self_node_id,
        }
    }

    /// Whether this node currently owns `shard` (live, `adopt_shards`-aware).
    /// `owned_shards() == None` means own-all (single owner / pre-failover boot).
    fn owns_locally(&self, shard: usize) -> bool {
        self.store
            .owned_shards()
            .is_none_or(|owned| owned.contains(&shard))
    }

    /// Resolve `shard` from the SS-3 shard-owner directory record, if one names a
    /// CURRENT owner. Returns `None` when no record exists (steady state), the
    /// record names a peer that is not configured/forwardable, or the read fails
    /// (treated as "no overlay opinion" — the static fallback then applies). A
    /// record naming this node resolves `Local`; a record naming a live,
    /// forwardable peer resolves `Remote`.
    fn resolve_from_record(&self, shard: usize) -> Option<OwnerView> {
        // A failed read must not break routing: fall through to the static map.
        let owner = self.store.read_shard_owner(shard).ok().flatten()?;
        // The record names THIS node (it adopted the shard): serve locally.
        if self.self_node_id.as_deref() == Some(owner.as_str()) {
            return Some(OwnerView::Local);
        }
        // The record names a configured peer: forward there if it is forwardable
        // and currently live (a record can outlive its writer; liveness still
        // gates the forward target, §2.5).
        let peer = self.peers.iter().find(|peer| peer.name == owner)?;
        if self.store.peer_connected(&peer.name) {
            Some(OwnerView::Remote(NodeRef {
                node_id: peer.name.clone(),
                grpc_addr: peer.grpc_addr,
                epoch: 0,
            }))
        } else {
            // The recorded owner is itself now down: no opinion — let the static
            // map / a later adoption + re-publish converge.
            None
        }
    }
}

impl ShardDirectory for StaticShardDirectory {
    fn owner_of(&self, shard: usize) -> OwnerView {
        if self.owns_locally(shard) {
            return OwnerView::Local;
        }
        // SS-3: the quorum-replicated shard-owner record is the authoritative
        // post-adoption signal. Consulted BEFORE the static map so a survivor
        // that adopted this shard is resolved as the current owner even though
        // the static config still names the (dead) declared owner — gap #2.
        if let Some(view) = self.resolve_from_record(shard) {
            return view;
        }
        // Steady-state fallback: the peer that statically declares this shard.
        let Some(peer) = self
            .peers
            .iter()
            .find(|peer| peer.owned_shards.contains(&shard))
        else {
            // No declared owner: route optimistically; the fence backstops.
            return OwnerView::Unknown;
        };
        // A peer believed-down resolves Unknown so the edge routes locally while
        // the supervisor adopts; a live peer is the forward target (§2.5).
        if self.store.peer_connected(&peer.name) {
            OwnerView::Remote(NodeRef {
                node_id: peer.name.clone(),
                grpc_addr: peer.grpc_addr,
                epoch: 0,
            })
        } else {
            OwnerView::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{DirectoryPeer, OwnerView, ShardDirectory, StaticShardDirectory};
    use aion_store::StoreError;
    use aion_store_haematite::HaematiteStore;

    type TestResult = Result<(), StoreError>;

    fn unique_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "aion-routing-dir-{name}-{}-{nanos}-{counter}",
            std::process::id()
        ))
    }

    fn store(
        name: &str,
        shard_count: usize,
        owned: &[usize],
    ) -> Result<HaematiteStore, StoreError> {
        let store = HaematiteStore::create_with_shard_count(unique_dir(name), shard_count)?;
        store.set_owned_shards(owned.iter().copied());
        Ok(store)
    }

    /// Locally-owned shards resolve Local; the own-all scope resolves every shard
    /// Local.
    #[test]
    fn owned_shards_resolve_local() -> TestResult {
        let store = std::sync::Arc::new(store("local", 4, &[0, 1])?);
        let directory = StaticShardDirectory::new(store, Vec::new(), None);
        assert_eq!(directory.owner_of(0), OwnerView::Local);
        assert_eq!(directory.owner_of(1), OwnerView::Local);
        Ok(())
    }

    /// A shard declared by a peer with no live link resolves Unknown (route
    /// locally; the fence + supervisor converge), NOT a forwardable Remote.
    #[test]
    fn down_peer_shard_resolves_unknown() -> TestResult {
        let store = std::sync::Arc::new(store("downpeer", 4, &[0])?);
        let directory = StaticShardDirectory::new(
            store,
            vec![DirectoryPeer {
                name: "peer-1".to_owned(),
                owned_shards: vec![2, 3],
                grpc_addr: Some(
                    "127.0.0.1:6001"
                        .parse()
                        .map_err(|error| StoreError::Backend(format!("bad addr: {error}")))?,
                ),
            }],
            None,
        );
        // A single-node test store has no live distribution link, so
        // peer_connected is always false → the peer is believed-down.
        assert_eq!(directory.owner_of(2), OwnerView::Unknown);
        Ok(())
    }

    /// A shard no peer declares resolves Unknown.
    #[test]
    fn undeclared_shard_resolves_unknown() -> TestResult {
        let store = std::sync::Arc::new(store("undeclared", 4, &[0])?);
        let directory = StaticShardDirectory::new(
            store,
            vec![DirectoryPeer {
                name: "peer-1".to_owned(),
                owned_shards: vec![1],
                grpc_addr: None,
            }],
            None,
        );
        assert_eq!(directory.owner_of(3), OwnerView::Unknown);
        Ok(())
    }
}
