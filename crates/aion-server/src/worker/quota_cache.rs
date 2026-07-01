//! Short-TTL in-process cache of per-namespace concurrency quotas, read by the
//! non-replayed outbox dispatcher's keyed backpressure (Control-Plane Phase 2,
//! P2-Q2).
//!
//! The dispatcher consults a namespace's cluster-wide `max_in_flight_activities`
//! ceiling on every sweep to compute its per-node headroom. Reading it straight
//! from the durable [`NamespaceStore`] each sweep would be a per-sweep quorum read
//! on the hot claim loop. This cache front-runs `get_namespace`, holding each
//! namespace's quota for a short TTL so the steady-state path is a lock +
//! map-lookup, never a store round-trip — the same pattern as
//! [`PlacementCache`](crate::worker::PlacementCache).
//!
//! Staleness is benign: the quota is the *cluster-wide* contract, enforced
//! per-node as a proportional share with generous defaults, so a stale entry only
//! over- or under-admits slightly for at most one TTL window and self-corrects on
//! the next refresh. Backpressure never drops or fails a row — an over-admit at
//! worst defers a Pending row one extra sweep — so cache staleness can never
//! perturb correctness or replay (the claim shapes timing only, CP-Phase-2 §3.4).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aion_store::NamespaceStore;

/// One cached quota entry plus the instant it was read, for TTL expiry.
#[derive(Clone, Copy)]
struct CachedQuota {
    /// The namespace's resolved cluster-wide ceiling: its explicit
    /// `max_in_flight_activities` override, or the platform default when unset /
    /// the record is absent.
    ceiling: u32,
    fetched_at: Instant,
}

/// A short-TTL cache over [`NamespaceStore::get_namespace`]'s
/// `config.max_in_flight_activities`, resolving the platform default when a
/// namespace sets no explicit override.
///
/// Cheap to clone (shares the inner store handle + map). A miss / expired entry
/// reads the durable store once and re-caches; a backend error or an absent
/// record resolves to the generous `platform_default` (so a registry hiccup
/// admits at the default headroom rather than throttling to zero).
#[derive(Clone)]
pub struct QuotaCache {
    store: Arc<dyn NamespaceStore>,
    platform_default: u32,
    ttl: Duration,
    entries: Arc<Mutex<HashMap<String, CachedQuota>>>,
}

impl std::fmt::Debug for QuotaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuotaCache")
            .field("platform_default", &self.platform_default)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl QuotaCache {
    /// Build a cache over the durable namespace store with the given platform
    /// default ceiling and entry TTL.
    #[must_use]
    pub fn new(store: Arc<dyn NamespaceStore>, platform_default: u32, ttl: Duration) -> Self {
        Self {
            store,
            platform_default,
            ttl,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the namespace's resolved cluster-wide ceiling, serving a fresh cache
    /// hit without a store read and refreshing on a miss / expiry.
    ///
    /// A poisoned cache lock or a store-read failure falls back to the generous
    /// `platform_default` — the dispatch then admits at default headroom, so a
    /// registry hiccup never throttles a tenant to zero.
    pub async fn ceiling(&self, namespace: &str) -> u32 {
        if let Some(hit) = self.fresh_hit(namespace) {
            return hit;
        }
        let ceiling = match self.store.get_namespace(namespace).await {
            Ok(Some(record)) => record
                .config
                .max_in_flight_activities
                .unwrap_or(self.platform_default),
            // An absent registry row (or a backend error) means no explicit
            // override applies: resolve to the generous platform default.
            Ok(None) | Err(_) => self.platform_default,
        };
        self.store_entry(namespace, ceiling);
        ceiling
    }

    /// Return a still-fresh cached ceiling, or `None` on a miss / expiry / a
    /// poisoned lock (treated as a miss so the caller re-reads).
    fn fresh_hit(&self, namespace: &str) -> Option<u32> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(namespace)?;
        if entry.fetched_at.elapsed() < self.ttl {
            Some(entry.ceiling)
        } else {
            None
        }
    }

    /// Record `ceiling` for `namespace` with a fresh fetch instant. A poisoned
    /// lock is a silent no-op: the next read simply re-fetches.
    fn store_entry(&self, namespace: &str, ceiling: u32) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                namespace.to_owned(),
                CachedQuota {
                    ceiling,
                    fetched_at: Instant::now(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;
    use std::time::Duration;

    use aion_store::{InMemoryStore, NamespaceOrigin, NamespaceRecord, NamespaceStore};
    use chrono::Utc;

    use super::QuotaCache;

    /// Register `namespace` carrying an explicit `max_in_flight_activities` override.
    async fn register_with_quota(
        store: &Arc<dyn NamespaceStore>,
        namespace: &str,
        quota: Option<u32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut record =
            NamespaceRecord::new_minted(namespace, NamespaceOrigin::Explicit, Utc::now());
        record.config.max_in_flight_activities = quota;
        store.put_namespace(record).await?;
        Ok(())
    }

    #[tokio::test]
    async fn explicit_override_is_read_from_store_and_cached()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        register_with_quota(&store, "capped", Some(7)).await?;
        let cache = QuotaCache::new(Arc::clone(&store), 1024, Duration::from_secs(60));

        assert_eq!(
            cache.ceiling("capped").await,
            7,
            "the explicit override is read"
        );

        // Mutate the durable record AFTER the cache filled: a fresh hit still
        // serves the cached value (proving the second read did not hit the store).
        register_with_quota(&store, "capped", Some(99)).await?;
        assert_eq!(
            cache.ceiling("capped").await,
            7,
            "a fresh cache hit must not re-read the mutated durable record"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unset_override_resolves_to_platform_default() -> Result<(), Box<dyn std::error::Error>>
    {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        register_with_quota(&store, "uncapped", None).await?;
        let cache = QuotaCache::new(store, 1024, Duration::from_secs(60));
        assert_eq!(
            cache.ceiling("uncapped").await,
            1024,
            "a namespace with no override resolves to the generous platform default"
        );
        Ok(())
    }

    #[tokio::test]
    async fn absent_namespace_resolves_to_platform_default()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        let cache = QuotaCache::new(store, 512, Duration::from_secs(60));
        assert_eq!(
            cache.ceiling("never-seen").await,
            512,
            "an absent registry row resolves to the platform default, never zero"
        );
        Ok(())
    }

    #[tokio::test]
    async fn refreshes_after_ttl_expiry() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        // A zero TTL forces every read to be a miss, so a later durable change is
        // observed on the next read. Start with an absent record → platform default.
        let cache = QuotaCache::new(Arc::clone(&store), 1024, Duration::ZERO);
        assert_eq!(
            cache.ceiling("capped").await,
            1024,
            "an absent record resolves to the platform default"
        );

        // Register the namespace with an explicit override AFTER the first read: the
        // expired (zero-TTL) entry re-reads the durable store and picks it up.
        register_with_quota(&store, "capped", Some(7)).await?;
        assert_eq!(
            cache.ceiling("capped").await,
            7,
            "an expired entry re-reads the durable record and sees the new override"
        );
        Ok(())
    }
}
