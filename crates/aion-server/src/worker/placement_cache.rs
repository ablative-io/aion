//! Short-TTL in-process cache of per-namespace placement directives, read by the
//! non-replayed outbox dispatcher (Control-Plane Phase 2, P2-P3).
//!
//! The dispatcher consults a namespace's [`NamespacePlacement`] on every claimed
//! row to decide preferred-vs-spill worker selection. Reading it straight from
//! the durable [`NamespaceStore`] each sweep would be a per-row quorum read on the
//! hot claim loop. This cache front-runs `get_namespace`, holding each
//! namespace's placement for a short TTL so the steady-state path is a lock +
//! map-lookup, never a store round-trip.
//!
//! Staleness is benign for the `Prefer` soft-spill this slice ships: a stale entry
//! only mis-*prefers* a worker for at most one TTL window — it self-corrects on
//! the next refresh and never affects correctness or replay (placement is a
//! dispatch-time selection input, never written to the recorded row). The TTL is
//! deliberately short so an operator's `PUT /placement` takes effect promptly.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aion_store::{NamespacePlacement, NamespaceStore};

/// One cached placement entry plus the instant it was read, for TTL expiry.
#[derive(Clone)]
struct CachedPlacement {
    placement: NamespacePlacement,
    fetched_at: Instant,
}

/// A short-TTL cache over [`NamespaceStore::get_namespace`]'s placement field.
///
/// Cheap to clone (shares the inner store handle + map). A miss / expired entry
/// reads the durable store once and re-caches; a backend error degrades to
/// [`NamespacePlacement::Unplaced`] (the safe default = today's any-worker
/// behaviour) rather than failing the dispatch, since placement is a soft
/// optimization and a row must still dispatch when the registry read hiccups.
#[derive(Clone)]
pub struct PlacementCache {
    store: Arc<dyn NamespaceStore>,
    ttl: Duration,
    entries: Arc<Mutex<HashMap<String, CachedPlacement>>>,
}

impl std::fmt::Debug for PlacementCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacementCache")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl PlacementCache {
    /// Build a cache over the durable namespace store with the given entry TTL.
    #[must_use]
    pub fn new(store: Arc<dyn NamespaceStore>, ttl: Duration) -> Self {
        Self {
            store,
            ttl,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the namespace's placement, serving a fresh cache hit without a store
    /// read and refreshing on a miss / expiry.
    ///
    /// A poisoned cache lock or a store-read failure falls back to
    /// [`NamespacePlacement::Unplaced`] — the dispatch then behaves exactly as the
    /// pre-Phase-2 any-worker path, so a registry hiccup never blocks a dispatch.
    pub async fn placement(&self, namespace: &str) -> NamespacePlacement {
        if let Some(hit) = self.fresh_hit(namespace) {
            return hit;
        }
        let placement = match self.store.get_namespace(namespace).await {
            Ok(Some(record)) => record.placement,
            // An absent registry row (or a backend error) means no placement
            // directive applies: default to Unplaced (any worker).
            Ok(None) | Err(_) => NamespacePlacement::Unplaced,
        };
        self.store_entry(namespace, &placement);
        placement
    }

    /// Return a still-fresh cached placement, or `None` on a miss / expiry / a
    /// poisoned lock (which is treated as a miss so the caller re-reads).
    fn fresh_hit(&self, namespace: &str) -> Option<NamespacePlacement> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(namespace)?;
        if entry.fetched_at.elapsed() < self.ttl {
            Some(entry.placement.clone())
        } else {
            None
        }
    }

    /// Record `placement` for `namespace` with a fresh fetch instant. A poisoned
    /// lock is a silent no-op: the next read simply re-fetches.
    fn store_entry(&self, namespace: &str, placement: &NamespacePlacement) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                namespace.to_owned(),
                CachedPlacement {
                    placement: placement.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }
    }
}

/// The ordered node-filter tiers an UNPINNED row consults for the `Prefer{L}`
/// two-tier spill (Control-Plane Phase 2, P2-P3) — the SINGLE source of the
/// prefer-then-spill sequence shared by BOTH the gRPC
/// ([`WorkerOutboxDispatch`](crate::worker::WorkerOutboxDispatch)) and liminal
/// ([`RegistryLiminalDispatch`](crate::worker::RegistryLiminalDispatch)) dispatch
/// paths, so the two transports can never diverge on what "prefer labelled worker,
/// spill to any" means.
///
/// The returned sequence is a list of node filters to try IN ORDER, stopping at
/// the first that has a live worker:
///
/// - `Prefer{L}` → each label in `L` (deterministic [`BTreeSet`](std::collections::BTreeSet)
///   order) as `Some(label)`, then a final `None` spill tier (any live worker).
///   An empty `L` collapses to just the `None` spill.
/// - `Unplaced` → a single `None` tier (any live worker).
///
/// `Pinned{L}` is NOT dispatched through this fn: [`worker_selection_for`] routes it
/// to the NON-spilling [`WorkerSelection::Required`] decision (require an L-labelled
/// worker, WAIT on absence, never spill — P2-I1, #164). The `Pinned` arm here is only
/// the internal fall-through of the soft `Prefer` path and never satisfies a hard pin.
///
/// This is consulted ONLY for an unpinned row (`row.node == None`): an authored
/// `Some(N)` pin is authoritative and never enters this path. The result is a
/// pure worker-SELECTION input; it never mutates the recorded row's `node`
/// (the determinism invariant, CP-Phase-2 §2.4).
#[must_use]
pub fn preferred_node_order(placement: &NamespacePlacement) -> Vec<Option<String>> {
    match placement {
        NamespacePlacement::Prefer { nodes } => {
            // Tier 1..N: each preferred label in deterministic set order.
            // Tier N+1: the `None` spill to any live worker.
            let mut tiers: Vec<Option<String>> =
                nodes.iter().map(|label| Some(label.clone())).collect();
            tiers.push(None);
            tiers
        }
        // Unplaced today, and Pinned (which does NOT spill): a single any-worker
        // tier, byte-identical to the pre-Phase-2 unpinned dispatch. `Pinned` is
        // routed to a NON-spilling `Required` decision by `worker_selection_for`
        // BEFORE this fn is reached, so a `Pinned` value here is only the internal
        // fall-through of the soft `Prefer` path and never satisfies a hard pin.
        NamespacePlacement::Unplaced | NamespacePlacement::Pinned { .. } => vec![None],
    }
}

/// How an UNPINNED outbox row (`row.node == None`) must be dispatched given its
/// namespace's [`NamespacePlacement`] — the SINGLE decision shared by BOTH the
/// gRPC ([`WorkerOutboxDispatch`](crate::worker::WorkerOutboxDispatch)) and liminal
/// ([`RegistryLiminalDispatch`](crate::worker::RegistryLiminalDispatch)) dispatch
/// paths, so the two transports can never diverge on `Prefer` (spill) vs `Pinned`
/// (require + wait, NEVER spill) semantics.
///
/// This is consulted ONLY for an unpinned row: an authored `Some(N)` pin is
/// authoritative and never enters this path. The result is a pure
/// worker-SELECTION input; it never mutates the recorded row's `node` (the
/// determinism invariant, CP-Phase-2 §2.4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerSelection {
    /// `Unplaced`/`Prefer`: try the ordered tiers (each preferred label, then the
    /// `None` spill), stopping at the first tier with a live worker. `Unplaced`
    /// collapses to the single `None` any-worker tier.
    PreferTiers(Vec<Option<String>>),
    /// `Pinned{L}`: require a worker whose advertised node ∈ `L`, and WAIT when
    /// none is live — NEVER a `None` spill to an any-node worker (CP-Phase-2 §2.5,
    /// P2-I1). An empty required set can never be satisfied by any labelled worker,
    /// so it stalls until the namespace's placement is relaxed — the correct
    /// "isolation > availability" behaviour of a hard pin with no admissible node.
    Required(BTreeSet<String>),
}

/// Resolve the [`WorkerSelection`] for an unpinned row from its namespace
/// placement. The single seam that keeps `Prefer` (soft spill) and `Pinned`
/// (hard require + wait) identical across the gRPC and liminal transports.
#[must_use]
pub fn worker_selection_for(placement: &NamespacePlacement) -> WorkerSelection {
    match placement {
        NamespacePlacement::Pinned { nodes } => WorkerSelection::Required(nodes.clone()),
        other => WorkerSelection::PreferTiers(preferred_node_order(other)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use aion_store::{InMemoryStore, NamespaceOrigin, NamespacePlacement, NamespaceStore};

    use super::PlacementCache;

    fn labels(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[tokio::test]
    async fn reads_placement_from_store_and_serves_a_fresh_hit()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        store
            .register_namespace("orders", NamespaceOrigin::Explicit)
            .await?;
        store
            .set_namespace_placement(
                "orders",
                NamespacePlacement::Prefer {
                    nodes: labels(&["n1"]),
                },
            )
            .await?;
        let cache = PlacementCache::new(Arc::clone(&store), Duration::from_secs(60));

        let first = cache.placement("orders").await;
        assert_eq!(
            first,
            NamespacePlacement::Prefer {
                nodes: labels(&["n1"])
            }
        );

        // Mutate the durable record AFTER the cache filled: a fresh hit still
        // serves the cached value (proving the second read did not hit the store).
        store
            .set_namespace_placement("orders", NamespacePlacement::Unplaced)
            .await?;
        let cached = cache.placement("orders").await;
        assert_eq!(
            cached,
            NamespacePlacement::Prefer {
                nodes: labels(&["n1"])
            },
            "a fresh cache hit must not re-read the mutated durable record"
        );
        Ok(())
    }

    #[tokio::test]
    async fn refreshes_after_ttl_expiry() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        store
            .register_namespace("orders", NamespaceOrigin::Explicit)
            .await?;
        store
            .set_namespace_placement(
                "orders",
                NamespacePlacement::Prefer {
                    nodes: labels(&["n1"]),
                },
            )
            .await?;
        // A zero TTL forces every read to be a miss, so a mutation is observed.
        let cache = PlacementCache::new(Arc::clone(&store), Duration::ZERO);

        assert_eq!(
            cache.placement("orders").await,
            NamespacePlacement::Prefer {
                nodes: labels(&["n1"])
            }
        );
        store
            .set_namespace_placement("orders", NamespacePlacement::Unplaced)
            .await?;
        assert_eq!(
            cache.placement("orders").await,
            NamespacePlacement::Unplaced,
            "an expired entry must re-read the mutated durable record"
        );
        Ok(())
    }

    #[tokio::test]
    async fn absent_namespace_defaults_to_unplaced() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        let cache = PlacementCache::new(store, Duration::from_secs(60));
        assert_eq!(
            cache.placement("never-seen").await,
            NamespacePlacement::Unplaced,
            "an absent registry row defaults to Unplaced (any worker)"
        );
        Ok(())
    }

    // --- #163: the shared prefer-then-spill tier order (gRPC + liminal) --------

    use super::preferred_node_order;

    /// A `Prefer{L}` placement yields each label in deterministic set order as a
    /// `Some` tier, then a final `None` spill tier — the single source both the
    /// gRPC and liminal dispatch paths consult so they cannot diverge.
    #[test]
    fn prefer_order_is_each_label_then_the_none_spill() {
        let order = preferred_node_order(&NamespacePlacement::Prefer {
            nodes: labels(&["n2", "n1"]),
        });
        // BTreeSet order is sorted: n1 before n2, then the None spill.
        assert_eq!(
            order,
            vec![Some("n1".to_owned()), Some("n2".to_owned()), None],
            "each preferred label (sorted) precedes the None spill tier"
        );
    }

    /// An empty `Prefer{}` set collapses to the immediate `None` spill.
    #[test]
    fn empty_prefer_set_is_just_the_spill() {
        let order = preferred_node_order(&NamespacePlacement::Prefer {
            nodes: BTreeSet::new(),
        });
        assert_eq!(order, vec![None], "an empty prefer set is the spill case");
    }

    /// `Unplaced` yields a single any-worker tier (the pre-Phase-2 unpinned
    /// selection). `preferred_node_order` also yields a single `None` tier for
    /// `Pinned`, but that is NOT the Pinned dispatch path any more: the hard pin is
    /// resolved by [`worker_selection_for`] into a NON-spilling `Required` decision
    /// (see below), so `preferred_node_order`'s `Pinned` arm is only the internal
    /// fall-through of the soft `Prefer` path and never satisfies a hard pin.
    #[test]
    fn unplaced_is_a_single_any_worker_tier() {
        assert_eq!(
            preferred_node_order(&NamespacePlacement::Unplaced),
            vec![None]
        );
    }

    // --- #164 (P2-I1): the shared Prefer-vs-Pinned selection decision ----------

    use super::{WorkerSelection, worker_selection_for};

    /// `Prefer{L}` resolves to the ordered prefer-then-spill tiers (each label,
    /// then the `None` spill) — the soft, high-availability path.
    #[test]
    fn prefer_selects_ordered_tiers_with_a_none_spill() {
        assert_eq!(
            worker_selection_for(&NamespacePlacement::Prefer {
                nodes: labels(&["n2", "n1"]),
            }),
            WorkerSelection::PreferTiers(vec![Some("n1".to_owned()), Some("n2".to_owned()), None,]),
        );
    }

    /// `Unplaced` resolves to the single `None` any-worker tier.
    #[test]
    fn unplaced_selects_the_single_any_worker_tier() {
        assert_eq!(
            worker_selection_for(&NamespacePlacement::Unplaced),
            WorkerSelection::PreferTiers(vec![None]),
        );
    }

    /// `Pinned{L}` resolves to a NON-spilling `Required` decision over exactly the
    /// required labels — it must NEVER contain a `None` spill tier, the hard-pin
    /// invariant both transports share (CP-Phase-2 §2.5, P2-I1).
    #[test]
    fn pinned_selects_required_labels_and_never_spills() {
        let selection = worker_selection_for(&NamespacePlacement::Pinned {
            nodes: labels(&["n1", "n2"]),
        });
        assert_eq!(
            selection,
            WorkerSelection::Required(labels(&["n1", "n2"])),
            "Pinned must require its label set with NO None spill tier"
        );
    }
}
