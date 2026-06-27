//! R-2 shard → owner directory (DISTRIBUTED-ROUTING-DESIGN §2.3).
//!
//! The [`ShardDirectory`] trait is the stable seam routing consumes; SS-3 later
//! swaps the implementation for a quorum-backed CAS directory without touching
//! the edge. This file ships the v1/v1.5 [`StaticShardDirectory`]: static peer
//! config overlaid with *live* local ownership (`store.owned_shards()`, which
//! `adopt_shards` widens on failover) and *live* peer liveness
//! (`store.peer_connected()`), so it is failover-aware with no new wire protocol.

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

/// The v1/v1.5 static directory: static peer config + live local-ownership and
/// peer-liveness overlay read from the cluster store.
pub struct StaticShardDirectory {
    /// The cluster store, read live for this node's owned shards and peer
    /// liveness. Holding the `Arc` keeps the directory cheap to clone-by-Arc and
    /// always current without a rebuild on failover.
    store: Arc<HaematiteStore>,
    /// Peers and the shards they statically declare, with forward addresses.
    peers: Vec<DirectoryPeer>,
}

impl StaticShardDirectory {
    /// Build a static directory over `store` and the configured `peers`.
    #[must_use]
    pub fn new(store: Arc<HaematiteStore>, peers: Vec<DirectoryPeer>) -> Self {
        Self { store, peers }
    }

    /// Whether this node currently owns `shard` (live, `adopt_shards`-aware).
    /// `owned_shards() == None` means own-all (single owner / pre-failover boot).
    fn owns_locally(&self, shard: usize) -> bool {
        self.store
            .owned_shards()
            .is_none_or(|owned| owned.contains(&shard))
    }
}

impl ShardDirectory for StaticShardDirectory {
    fn owner_of(&self, shard: usize) -> OwnerView {
        if self.owns_locally(shard) {
            return OwnerView::Local;
        }
        // Find the peer that statically declares this shard.
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
        let directory = StaticShardDirectory::new(store, Vec::new());
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
        );
        assert_eq!(directory.owner_of(3), OwnerView::Unknown);
        Ok(())
    }
}
