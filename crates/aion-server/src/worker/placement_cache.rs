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

use std::collections::HashMap;
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
}
