//! Throttled per-namespace quota-state broadcaster for the ops console live
//! badge (Control-Plane Phase 2, P2-Q3).
//!
//! # Why this is periodic, not edge-triggered
//!
//! Every other [`ClusterEvent`](aion_core::ClusterEvent) is emitted at a real
//! subsystem mutation (a peer down, a shard adopted, a namespace minted). Quota
//! state is different: `in_flight` — the durable **Claimed** outbox-row count —
//! changes on *every* claim and *every* settle, so an edge-triggered emit would be
//! a per-row firehose on the hot dispatch path. Instead this task samples each
//! active namespace's REAL durable state once per fixed cadence and emits ONE
//! [`ClusterEvent::NamespaceQuotaState`](aion_core::ClusterEvent::NamespaceQuotaState)
//! per namespace. The console folds the latest snapshot per namespace, so the
//! badge tracks live load without any client polling loop (the dashboard rule).
//!
//! This is NOT the polling-as-push regression WS3 removed: there is no scrape of a
//! derived metric and no client timer. The snapshot reads the same durable
//! Claimed-row count the keyed backpressure caps against, and the ceiling the
//! backpressure resolves — the badge is a faithful window onto the live quota the
//! dispatcher already enforces, sourced server-side and pushed.
//!
//! # What it reads (REAL durable state, never the dead gauge)
//!
//! - `in_flight`: [`OutboxStore::count_claimed_outbox_rows_by_namespace`] — the
//!   durable count of currently-Claimed rows, restart-correct and namespace-
//!   stamped, NEVER the confirmed-dead `inflight_activities` gauge (P2-Q0 / #162).
//! - `ceiling`: the tenant's **cluster-wide** contract via [`QuotaCache::ceiling`]
//!   — its explicit `max_in_flight_activities` override or the platform default.
//!   The console shows the number the operator set, not the per-node proportional
//!   slice (exposing per-node math is the leaky-abstraction footgun §3.6 rejects).
//!
//! # Which namespaces
//!
//! The durable namespace registry is the authoritative tenant set, so each cadence
//! enumerates [`NamespaceStore::list_namespaces`] and emits a snapshot per
//! namespace. A namespace at its ceiling with no pending backlog still has Claimed
//! rows, so it must be sampled from the registry (not only from pending routes) or
//! its badge would stall at the moment it matters most — when it is throttled.

use std::sync::Arc;
use std::time::Duration;

use aion_core::{ClusterEvent, ClusterEventMeta};
use aion_store::{NamespaceStore, OutboxStore};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::cluster_publisher::ClusterEventPublisher;
use crate::worker::QuotaCache;

/// Emits a throttled per-namespace quota-state snapshot onto the cluster publisher
/// so the ops console renders a live in-flight/ceiling badge.
///
/// Holds only read-side handles (the durable stores, the quota cache, the
/// publisher); it never claims, settles, or mutates any row. Cheap to construct;
/// owns its cadence.
pub struct QuotaBroadcaster {
    namespace_store: Arc<dyn NamespaceStore>,
    outbox_store: Arc<dyn OutboxStore>,
    quota: QuotaCache,
    publisher: ClusterEventPublisher,
    cadence: Duration,
}

impl QuotaBroadcaster {
    /// Build a broadcaster over the durable stores, the shared quota cache, and the
    /// cluster publisher, emitting a snapshot every `cadence`.
    #[must_use]
    pub fn new(
        namespace_store: Arc<dyn NamespaceStore>,
        outbox_store: Arc<dyn OutboxStore>,
        quota: QuotaCache,
        publisher: ClusterEventPublisher,
        cadence: Duration,
    ) -> Self {
        Self {
            namespace_store,
            outbox_store,
            quota,
            publisher,
            cadence,
        }
    }

    /// Run the snapshot loop until the shared shutdown watch flips, draining on the
    /// same signal as the transports and the dispatcher.
    ///
    /// A store error on a cadence is logged and the loop continues — a transient
    /// backend hiccup must never tear down the badge feed; the next cadence
    /// re-samples. The interval uses `Skip` missed-tick behaviour so a slow sample
    /// never causes a burst of catch-up emits.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        info!(
            cadence_ms = %self.cadence.as_millis(),
            "quota-state broadcaster commissioned (throttled per-namespace in-flight/ceiling push)"
        );
        let mut interval = tokio::time::interval(self.cadence);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if *shutdown.borrow() {
                        break;
                    }
                    self.emit_snapshot().await;
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        debug!("quota-state broadcaster drained");
    }

    /// Sample every registry namespace once and emit its quota snapshot.
    ///
    /// A single bucketed Claimed-count scan covers all namespaces (the same
    /// collapsed scan the keyed backpressure uses), then each namespace's
    /// cluster-wide ceiling is resolved through the short-TTL quota cache.
    async fn emit_snapshot(&self) {
        let records = match self.namespace_store.list_namespaces().await {
            Ok(records) => records,
            Err(error) => {
                warn!(%error, "quota-state broadcaster could not list namespaces this cadence");
                return;
            }
        };
        if records.is_empty() {
            return;
        }
        let names: Vec<&str> = records.iter().map(|record| record.name.as_str()).collect();
        let claimed = match self
            .outbox_store
            .count_claimed_outbox_rows_by_namespace(&names)
            .await
        {
            Ok(claimed) => claimed,
            Err(error) => {
                warn!(%error, "quota-state broadcaster could not count claimed outbox rows");
                return;
            }
        };
        for record in &records {
            let namespace = record.name.clone();
            let in_flight = claimed.get(&namespace).copied().unwrap_or(0);
            let ceiling = self.quota.ceiling(&namespace).await;
            self.publisher
                .emit(move |meta| build_quota_state(meta, namespace, in_flight, ceiling));
        }
    }
}

/// Build one [`ClusterEvent::NamespaceQuotaState`] from a sampled snapshot, given
/// the publisher-stamped meta.
fn build_quota_state(
    meta: ClusterEventMeta,
    namespace: String,
    in_flight: u64,
    ceiling: u32,
) -> ClusterEvent {
    ClusterEvent::NamespaceQuotaState {
        meta,
        namespace,
        in_flight,
        ceiling,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::ClusterEvent;
    use aion_store::{
        ClaimScope, InMemoryStore, NamespaceOrigin, NamespaceRecord, NamespaceStore, OutboxRow,
        OutboxStatus, OutboxStore, StoreError,
    };
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use futures::StreamExt;

    use super::QuotaBroadcaster;
    use crate::cluster_publisher::ClusterEventPublisher;
    use crate::worker::QuotaCache;

    /// An outbox store whose Claimed rows are a fixed fixture, so a snapshot's
    /// `in_flight` is exactly the number of Claimed rows in each namespace.
    struct FixtureOutbox {
        rows: Vec<OutboxRow>,
    }

    #[async_trait]
    impl OutboxStore for FixtureOutbox {
        async fn append_outbox_batch(&self, _rows: &[OutboxRow]) -> Result<(), StoreError> {
            Ok(())
        }
        async fn claim_outbox_rows(&self, _limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
            Ok(Vec::new())
        }
        async fn claim_outbox_rows_scoped(
            &self,
            _scope: &ClaimScope,
            _limit: u32,
        ) -> Result<Vec<OutboxRow>, StoreError> {
            Ok(Vec::new())
        }
        async fn rearm_stale_claimed_outbox_rows(
            &self,
            _older_than: DateTime<Utc>,
            _visible_after: DateTime<Utc>,
            _limit: u32,
        ) -> Result<Vec<OutboxRow>, StoreError> {
            Ok(Vec::new())
        }
        async fn complete_outbox_row(&self, _dispatch_key: &str) -> Result<(), StoreError> {
            Ok(())
        }
        async fn retry_outbox_row(
            &self,
            _dispatch_key: &str,
            _next_attempt: u32,
            _visible_after: DateTime<Utc>,
        ) -> Result<(), StoreError> {
            Ok(())
        }
        async fn fail_outbox_row(&self, _dispatch_key: &str) -> Result<(), StoreError> {
            Ok(())
        }
        async fn count_inflight_outbox_rows(&self, _namespace: &str) -> Result<u64, StoreError> {
            Ok(0)
        }
        async fn count_claimed_outbox_rows(&self, namespace: &str) -> Result<u64, StoreError> {
            let count = self
                .rows
                .iter()
                .filter(|row| {
                    row.namespace == namespace && matches!(row.status, OutboxStatus::Claimed)
                })
                .count();
            Ok(u64::try_from(count).unwrap_or(u64::MAX))
        }
        async fn count_claimed_outbox_rows_by_namespace(
            &self,
            namespaces: &[&str],
        ) -> Result<std::collections::BTreeMap<String, u64>, StoreError> {
            let mut counts = std::collections::BTreeMap::new();
            for namespace in namespaces {
                counts.insert(
                    (*namespace).to_owned(),
                    self.count_claimed_outbox_rows(namespace).await?,
                );
            }
            Ok(counts)
        }
        async fn pending_outbox_routes(&self) -> Result<Vec<ClaimScope>, StoreError> {
            Ok(Vec::new())
        }
    }

    fn claimed_row(namespace: &str) -> OutboxRow {
        let mut row = OutboxRow::pending(
            aion_core::WorkflowId::new_v4(),
            0,
            "act".to_owned(),
            aion_core::Payload::new(aion_core::ContentType::Json, Vec::new()),
            Utc::now(),
        )
        .with_namespace(namespace);
        row.status = OutboxStatus::Claimed;
        row
    }

    async fn register(
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

    /// The one load-bearing backend assertion (P2-Q3): a quota snapshot carries the
    /// durable Claimed count as `in_flight` and the correct ceiling — the explicit
    /// override for a capped namespace, the platform default for an uncapped one.
    #[tokio::test]
    async fn snapshot_carries_durable_claimed_count_and_override_vs_default_ceiling()
    -> Result<(), Box<dyn std::error::Error>> {
        let namespace_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        // `capped` sets an explicit override (7); `uncapped` inherits the platform
        // default (100). Two Claimed rows for `capped`, one for `uncapped`.
        register(&namespace_store, "capped", Some(7)).await?;
        register(&namespace_store, "uncapped", None).await?;
        let outbox: Arc<dyn OutboxStore> = Arc::new(FixtureOutbox {
            rows: vec![
                claimed_row("capped"),
                claimed_row("capped"),
                claimed_row("uncapped"),
            ],
        });
        let quota = QuotaCache::new(Arc::clone(&namespace_store), 100, Duration::from_secs(60));
        let publisher =
            ClusterEventPublisher::new(std::num::NonZeroUsize::new(64).expect("64 is non-zero"));
        let mut subscription = publisher.subscribe(0);

        let broadcaster = QuotaBroadcaster::new(
            Arc::clone(&namespace_store),
            Arc::clone(&outbox),
            quota,
            publisher.clone(),
            Duration::from_secs(60),
        );
        broadcaster.emit_snapshot().await;

        // Collect one snapshot per namespace (registry order: capped, uncapped).
        let mut seen = std::collections::BTreeMap::new();
        for _ in 0..2 {
            let event = subscription
                .next()
                .await
                .expect("a snapshot event")
                .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
            if let ClusterEvent::NamespaceQuotaState {
                namespace,
                in_flight,
                ceiling,
                ..
            } = event
            {
                seen.insert(namespace, (in_flight, ceiling));
            } else {
                return Err(format!("expected NamespaceQuotaState, got {event:?}").into());
            }
        }

        assert_eq!(
            seen.get("capped").copied(),
            Some((2, 7)),
            "capped: durable Claimed count = 2, explicit override ceiling = 7"
        );
        assert_eq!(
            seen.get("uncapped").copied(),
            Some((1, 100)),
            "uncapped: durable Claimed count = 1, platform-default ceiling = 100"
        );
        Ok(())
    }

    /// An empty registry emits nothing (no phantom namespace, no wasted frame).
    #[tokio::test]
    async fn empty_registry_emits_no_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let namespace_store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        let outbox: Arc<dyn OutboxStore> = Arc::new(FixtureOutbox { rows: Vec::new() });
        let quota = QuotaCache::new(Arc::clone(&namespace_store), 100, Duration::from_secs(60));
        let publisher =
            ClusterEventPublisher::new(std::num::NonZeroUsize::new(8).expect("8 is non-zero"));

        let broadcaster = QuotaBroadcaster::new(
            namespace_store,
            outbox,
            quota,
            publisher.clone(),
            Duration::from_secs(60),
        );
        broadcaster.emit_snapshot().await;

        assert_eq!(
            publisher.current_seq(),
            0,
            "no namespaces means no emit and no seq advance"
        );
        Ok(())
    }
}
