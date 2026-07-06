//! Per-tenant keyed backpressure at the outbox claim (Control-Plane Phase 2,
//! P2-Q2).
//!
//! # What this is
//!
//! The non-replayed [`OutboxDispatcher`](crate::worker::OutboxDispatcher) normally
//! claims one unscoped batch of pending rows per sweep. With backpressure attached
//! it instead claims **per-namespace, round-robin, headroom-capped**: a tenant at
//! its concurrency ceiling has its excess Pending rows held (left durable, NOT
//! dropped, reconsidered next sweep), and a bursty tenant cannot starve a quiet one.
//!
//! # Fairness is per-NAMESPACE, not per-route
//!
//! The batch budget is allocated PER NAMESPACE first: every active namespace (one
//! with claimable pending work) gets a guaranteed slice — `batch_size ÷ active`
//! (rounded up, ≥1), capped by that namespace's headroom — BEFORE any single tenant
//! can consume the whole batch. A bursty tenant spread across many task_queues can
//! therefore never exhaust the sweep budget on its own routes and starve a quiet
//! single-route tenant: each namespace's slice is reserved up front, and only within
//! a namespace is that slice distributed round-robin across its own routes. Any
//! budget left after every namespace has had its guaranteed slice is offered in a
//! second pass to namespaces with more pending work — fairness first, utilization
//! second.
//!
//! # The three load-bearing semantics
//!
//! 1. **CLAIMED-only headroom.** The ceiling caps *concurrent executing* activities
//!    — `Claimed` rows — never `Pending + Claimed`. Counting the Pending backlog
//!    would wedge a tenant against its own backlog (it could never claim the rows
//!    that make up the count). So `headroom = per_node_ceiling − claimed`, fed by
//!    [`OutboxStore::count_claimed_outbox_rows`], never `count_inflight_*`
//!    (CP-Phase-2 §3.1 as corrected).
//! 2. **Proportional per-node ceiling.** The tenant's quota is a *cluster-wide*
//!    contract; each node enforces `ceil(quota × owned_shard_fraction)` where the
//!    fraction is `|owned shards| / shard_count`. Rows scatter by `dispatch_key`
//!    hash uniformly across shards and a node claims only rows on shards it owns,
//!    so the per-node ceilings sum to ≈quota cluster-wide with NO central counter
//!    (CP-Phase-2 §3.6).
//! 3. **Exactly-once preserved.** Backpressure only shapes the `limit` and `scope`
//!    of the existing atomic [`OutboxStore::claim_outbox_rows_scoped`]; a smaller
//!    limit is already first-class (the backoff/visibility machinery defers claims
//!    routinely). It touches no dedup (`dispatch_key` UNIQUE / INSERT OR IGNORE) and
//!    no ack/settle path — a held row stays exactly `Pending`.

use std::collections::BTreeMap;

use aion_store::{ClaimScope, OutboxRow, OutboxStore};
use std::sync::Arc;
use tracing::warn;

use crate::worker::QuotaCache;

/// This node's owned-shard fraction of the cluster's virtual shard space.
///
/// `owned / total` is the proportional slice of every tenant's cluster-wide quota
/// this node enforces (CP-Phase-2 §3.6). A single-node / own-all deployment has
/// `owned == total` (fraction 1), so each per-node ceiling equals the full quota
/// and behaviour is byte-identical to no per-node split.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnedShardFraction {
    owned: u32,
    total: u32,
}

impl OwnedShardFraction {
    /// Build a fraction from this node's owned-shard count and the cluster shard
    /// count.
    ///
    /// A `total` of zero is meaningless (the keyspace always has ≥1 shard); it is
    /// clamped to 1 so the fraction is well-defined. `owned` is clamped to `total`
    /// — a node never owns more than the whole keyspace — so the fraction is always
    /// in `(0, 1]` and a per-node ceiling never exceeds the cluster-wide quota.
    #[must_use]
    pub fn new(owned: u32, total: u32) -> Self {
        let total = total.max(1);
        let owned = owned.clamp(1, total);
        Self { owned, total }
    }

    /// The whole cluster on one node (own-all): fraction 1, so a per-node ceiling
    /// equals the full cluster-wide quota. This is the single-node default and the
    /// byte-identical path.
    #[must_use]
    pub fn own_all() -> Self {
        Self { owned: 1, total: 1 }
    }

    /// `ceil(quota × owned / total)` — this node's proportional slice of the
    /// cluster-wide `quota`, rounded UP so the per-node ceilings sum to ≥ quota
    /// (over-admit slightly under shard skew rather than starve — the right failure
    /// direction with generous defaults, CP-Phase-2 §3.6).
    #[must_use]
    pub fn per_node_ceiling(self, quota: u32) -> u32 {
        // Ceiling division in u64 to avoid overflow: (quota*owned + total-1) / total.
        let numerator = u64::from(quota) * u64::from(self.owned) + u64::from(self.total) - 1;
        let ceiling = numerator / u64::from(self.total);
        u32::try_from(ceiling).unwrap_or(u32::MAX)
    }
}

/// One namespace's claim plan for a single sweep: its CLAIMED-only headroom and the
/// routes (`task_queue`/node pools) that carry its pending work.
///
/// The headroom is the hard per-tenant backstop; the routes are how a namespace's
/// per-sweep allocation is spread round-robin across its several task queues so no
/// single route hoards the namespace's own slice.
#[derive(Clone, Debug)]
struct NamespacePlan {
    /// `per_node_ceiling − claimed`, clamped at zero. The hard backstop: a tenant
    /// can never exceed this many NEW claims this sweep no matter the round-robin.
    headroom: u32,
    /// This namespace's routes (distinct `(task_queue, node)` pools), the units the
    /// per-namespace allocation is round-robined over.
    routes: Vec<ClaimScope>,
}

/// Keyed backpressure over the outbox claim: resolves per-namespace ceilings and
/// plans a round-robin, headroom-capped, fair-shared claim per sweep.
///
/// Holds only read-side state (the quota cache + this node's shard fraction); the
/// claim itself goes through the unchanged [`OutboxStore`] the dispatcher already
/// owns. Cheap to clone (the cache shares its inner handle).
#[derive(Clone)]
pub struct Backpressure {
    quota: QuotaCache,
    fraction: OwnedShardFraction,
}

impl std::fmt::Debug for Backpressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backpressure")
            .field("fraction", &self.fraction)
            .finish_non_exhaustive()
    }
}

impl Backpressure {
    /// Build keyed backpressure from a quota cache and this node's owned-shard
    /// fraction.
    #[must_use]
    pub fn new(quota: QuotaCache, fraction: OwnedShardFraction) -> Self {
        Self { quota, fraction }
    }

    /// Claim up to `batch_size` rows across all namespaces-with-pending-work, with
    /// the batch budget allocated PER NAMESPACE first (a guaranteed fair slice each),
    /// then distributed round-robin across each namespace's routes. Returns every
    /// claimed row, in claim order.
    ///
    /// Rows not claimed (a tenant at its ceiling, or the batch budget exhausted)
    /// stay durably `Pending` and are reconsidered next sweep — the keyed
    /// backpressure: nothing is dropped, no `RESOURCE_EXHAUSTED` is surfaced.
    ///
    /// # Errors
    ///
    /// Propagates a store error from the route probe or any scoped claim; the
    /// dispatcher logs it and retries next tick (a transient backend failure must
    /// not tear the loop down).
    pub async fn claim_round_robin(
        &self,
        store: &Arc<dyn OutboxStore>,
        batch_size: u32,
        held: &std::collections::HashSet<aion_core::WorkflowId>,
    ) -> Result<Vec<OutboxRow>, aion_store::StoreError> {
        let routes = store.pending_outbox_routes().await?;
        if routes.is_empty() {
            return Ok(Vec::new());
        }
        let plan = self.plan_sweep(store, &routes, batch_size).await?;
        self.execute_plan(store, &plan, batch_size, held).await
    }

    /// Resolve each pending namespace's per-sweep headroom (CLAIMED-only,
    /// proportional ceiling) and group its routes, then compute the per-namespace
    /// slice so every active tenant is guaranteed an allocation of the batch before
    /// any single tenant can consume it.
    async fn plan_sweep(
        &self,
        store: &Arc<dyn OutboxStore>,
        routes: &[ClaimScope],
        batch_size: u32,
    ) -> Result<SweepPlan, aion_store::StoreError> {
        // Group routes by namespace, deterministically ordered, so each namespace's
        // slice is spread round-robin across ITS OWN task_queues/nodes. Routes are
        // gathered first with an empty headroom; the CLAIMED-only headroom is filled
        // in below from a SINGLE bucketed claimed-count scan (CP2-Q2 perf) rather than
        // one owned-shard scan per namespace (the N+1 the old per-namespace path
        // incurred over the same rows).
        let mut namespaces: BTreeMap<String, NamespacePlan> = BTreeMap::new();
        for route in routes {
            namespaces
                .entry(route.namespace.clone())
                .or_insert_with(|| NamespacePlan {
                    headroom: 0,
                    routes: Vec::new(),
                })
                .routes
                .push(route.clone());
        }
        // ONE scan over the owned shards, bucketed by namespace, instead of N scans
        // (one per active namespace). The result is byte-identical to counting each
        // namespace's Claimed rows separately: same owned-shard scope, same
        // Claimed-only predicate, one entry per requested namespace.
        let names: Vec<&str> = namespaces.keys().map(String::as_str).collect();
        let claimed_by_namespace = store.count_claimed_outbox_rows_by_namespace(&names).await?;
        for (namespace, plan) in &mut namespaces {
            // Ceiling is the proportional per-node slice of the namespace's cached
            // cluster-wide quota; the claimed count is this node's durable Claimed-row
            // count (NEVER Pending+Claimed — that would wedge a tenant against its own
            // backlog). Headroom = ceiling − claimed, clamped at zero.
            let ceiling = self
                .fraction
                .per_node_ceiling(self.quota.ceiling(namespace).await);
            let claimed = u32::try_from(claimed_by_namespace.get(namespace).copied().unwrap_or(0))
                .unwrap_or(u32::MAX);
            plan.headroom = ceiling.saturating_sub(claimed);
        }
        // Per-namespace slice: batch_size ÷ active (≥1) is the GUARANTEED allocation
        // each active tenant reserves before any tenant can consume the whole batch,
        // capped per tenant by its headroom. This is the fairness axis — per
        // NAMESPACE, never per route — so a tenant with many routes cannot drain the
        // budget on its own routes and starve a quiet single-route tenant.
        let active = u32::try_from(namespaces.len()).unwrap_or(u32::MAX).max(1);
        let per_namespace_slice = batch_size.div_ceil(active).max(1);
        Ok(SweepPlan {
            namespaces,
            per_namespace_slice,
        })
    }

    /// Execute the sweep in two passes: first the GUARANTEED per-namespace slice for
    /// every active tenant (fairness), then a second pass offering any leftover
    /// budget to tenants with more pending work (utilization). Rows over a tenant's
    /// headroom or beyond the batch budget stay durably `Pending`.
    async fn execute_plan(
        &self,
        store: &Arc<dyn OutboxStore>,
        plan: &SweepPlan,
        batch_size: u32,
        held: &std::collections::HashSet<aion_core::WorkflowId>,
    ) -> Result<Vec<OutboxRow>, aion_store::StoreError> {
        // Remaining headroom per namespace, decremented as its routes are claimed.
        let mut headroom: BTreeMap<&str, u32> = plan
            .namespaces
            .iter()
            .map(|(name, ns)| (name.as_str(), ns.headroom))
            .collect();
        let mut budget = batch_size;
        let mut claimed = Vec::new();
        // Pass 1 — fairness: every active namespace gets its guaranteed slice first,
        // capped by its own headroom, distributed round-robin across its routes.
        for (name, ns) in &plan.namespaces {
            let ns_headroom = headroom.entry(name.as_str()).or_default();
            let allocation = plan.per_namespace_slice.min(*ns_headroom).min(budget);
            let got =
                Self::claim_namespace_slice(store, &ns.routes, allocation, &mut claimed, held)
                    .await?;
            *ns_headroom = ns_headroom.saturating_sub(got);
            budget = budget.saturating_sub(got);
        }
        // Pass 2 — utilization: spread any leftover budget over the namespaces that
        // still have both headroom and unclaimed routes (fairness already honoured).
        for (name, ns) in &plan.namespaces {
            if budget == 0 {
                break;
            }
            let ns_headroom = headroom.entry(name.as_str()).or_default();
            let allocation = (*ns_headroom).min(budget);
            let got =
                Self::claim_namespace_slice(store, &ns.routes, allocation, &mut claimed, held)
                    .await?;
            *ns_headroom = ns_headroom.saturating_sub(got);
            budget = budget.saturating_sub(got);
        }
        if claimed.is_empty() && !plan.namespaces.is_empty() {
            // Every route was at its ceiling this sweep: rows stay Pending (held),
            // reconsidered next sweep when Claimed rows complete and headroom returns.
            warn!("outbox backpressure held all pending routes at ceiling this sweep");
        }
        Ok(claimed)
    }

    /// Claim up to `allocation` rows for one namespace, distributed round-robin
    /// across its `routes`, appending them to `claimed`. Returns how many were
    /// claimed so the caller can decrement the namespace headroom and batch budget.
    ///
    /// One pass over the routes suffices: each route is claimed at its running share
    /// of the remaining allocation, so a route with few pending rows yields the rest
    /// back to the later routes rather than the allocation being under-used.
    async fn claim_namespace_slice(
        store: &Arc<dyn OutboxStore>,
        routes: &[ClaimScope],
        allocation: u32,
        claimed: &mut Vec<OutboxRow>,
        held: &std::collections::HashSet<aion_core::WorkflowId>,
    ) -> Result<u32, aion_store::StoreError> {
        let mut remaining = allocation;
        let mut total: u32 = 0;
        let mut left = u32::try_from(routes.len()).unwrap_or(u32::MAX).max(1);
        for route in routes {
            if remaining == 0 {
                break;
            }
            // Even round-robin share of the remaining allocation across the remaining
            // routes, rounded up so the last route can mop up any residue. The pause
            // dispatch-hold (#204) is applied AT CLAIM TIME here too: a held run's row
            // is never selected, so it stays Pending under backpressure exactly as it
            // does on the unscoped claim.
            let share = remaining.div_ceil(left).max(1).min(remaining);
            let rows = store
                .claim_outbox_rows_scoped_excluding(route, share, held)
                .await?;
            let got = u32::try_from(rows.len()).unwrap_or(u32::MAX);
            remaining = remaining.saturating_sub(got);
            total = total.saturating_add(got);
            claimed.extend(rows);
            left = left.saturating_sub(1).max(1);
        }
        Ok(total)
    }
}

/// One sweep's resolved plan: each active namespace's headroom + routes, and the
/// guaranteed per-namespace slice of the batch.
struct SweepPlan {
    namespaces: BTreeMap<String, NamespacePlan>,
    per_namespace_slice: u32,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use aion_store::{ClaimScope, OutboxRow, OutboxStatus, OutboxStore, StoreError};
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};

    use super::{Backpressure, OwnedShardFraction};
    use crate::worker::QuotaCache;

    /// Mock outbox that holds a fixed set of rows and tallies claimed counts against them, recording
    /// how many times the scalar per-namespace count and the collapsed bucketed count are invoked so
    /// the N+1 collapse is observable. Only the read-side methods the planner touches are meaningful;
    /// the rest are inert stubs (the plan-sweep path never reaches them).
    struct CountingStore {
        rows: Vec<OutboxRow>,
        scalar_count_calls: AtomicUsize,
        bucketed_count_calls: AtomicUsize,
    }

    impl CountingStore {
        fn new(rows: Vec<OutboxRow>) -> Self {
            Self {
                rows,
                scalar_count_calls: AtomicUsize::new(0),
                bucketed_count_calls: AtomicUsize::new(0),
            }
        }

        fn claimed_in(&self, namespace: &str) -> u64 {
            let count = self
                .rows
                .iter()
                .filter(|row| {
                    row.namespace == namespace && matches!(row.status, OutboxStatus::Claimed)
                })
                .count();
            u64::try_from(count).unwrap_or(u64::MAX)
        }
    }

    #[async_trait]
    impl OutboxStore for CountingStore {
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
            self.scalar_count_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.claimed_in(namespace))
        }
        async fn count_claimed_outbox_rows_by_namespace(
            &self,
            namespaces: &[&str],
        ) -> Result<std::collections::BTreeMap<String, u64>, StoreError> {
            self.bucketed_count_calls.fetch_add(1, Ordering::SeqCst);
            Ok(namespaces
                .iter()
                .map(|ns| ((*ns).to_owned(), self.claimed_in(ns)))
                .collect())
        }
        async fn pending_outbox_routes(&self) -> Result<Vec<ClaimScope>, StoreError> {
            Ok(Vec::new())
        }
    }

    fn quota_cache() -> QuotaCache {
        // Empty namespace store: every ceiling resolves to the generous platform default, so the
        // headroom is entirely a function of the claimed count the collapse must preserve.
        let store: Arc<dyn aion_store::NamespaceStore> =
            Arc::new(aion_store::InMemoryStore::default());
        QuotaCache::new(store, 100, Duration::from_secs(60))
    }

    /// A Claimed row in `namespace` on `task_queue` (the count input the headroom subtracts).
    fn claimed_row(namespace: &str, task_queue: &str) -> OutboxRow {
        let now = Utc::now();
        let mut row = OutboxRow::pending(
            aion_core::WorkflowId::new_v4(),
            0,
            "act".to_owned(),
            aion_core::Payload::new(aion_core::ContentType::Json, Vec::new()),
            now,
        )
        .with_namespace(namespace)
        .with_task_queue(task_queue);
        row.status = OutboxStatus::Claimed;
        row
    }

    #[tokio::test]
    async fn collapsed_scan_yields_identical_namespace_buckets_and_one_scan() {
        // Three namespaces with differing claimed counts spread over several routes each: the
        // scenario the old path would scan the owned-shard set THREE times for (once per namespace).
        let rows = vec![
            claimed_row("alpha", "q1"),
            claimed_row("alpha", "q2"),
            claimed_row("alpha", "q1"),
            claimed_row("beta", "q1"),
            claimed_row("gamma", "q1"),
            claimed_row("gamma", "q2"),
        ];
        let counting = Arc::new(CountingStore::new(rows));
        let store: Arc<dyn OutboxStore> = Arc::clone(&counting) as Arc<dyn OutboxStore>;
        let bp = Backpressure::new(quota_cache(), OwnedShardFraction::own_all());

        // The routes the planner groups by namespace (a bursty tenant spread over many task queues).
        let routes = vec![
            ClaimScope::new("alpha", "q1"),
            ClaimScope::new("alpha", "q2"),
            ClaimScope::new("beta", "q1"),
            ClaimScope::new("gamma", "q1"),
            ClaimScope::new("gamma", "q2"),
        ];

        // OLD path baseline: each namespace's headroom = ceiling(100) − scalar_claimed(namespace).
        let expected: std::collections::BTreeMap<&str, u32> =
            [("alpha", 100 - 3), ("beta", 100 - 1), ("gamma", 100 - 2)]
                .into_iter()
                .collect();

        let plan = bp
            .plan_sweep(&store, &routes, 64)
            .await
            .expect("plan resolves");

        // Identical per-namespace headroom buckets to the old per-namespace scans.
        for (name, ns) in &plan.namespaces {
            assert_eq!(
                ns.headroom,
                expected[name.as_str()],
                "namespace {name} headroom must match the per-namespace-scan result"
            );
        }
        assert_eq!(plan.namespaces.len(), 3, "one bucket per active namespace");

        // Exactly ONE bucketed scan, and the scalar per-namespace scan is never used on this path.
        assert_eq!(
            counting.bucketed_count_calls.load(Ordering::SeqCst),
            1,
            "the owned-shard set is scanned exactly once for all namespaces"
        );
        assert_eq!(
            counting.scalar_count_calls.load(Ordering::SeqCst),
            0,
            "the collapsed path never falls back to the N per-namespace scans"
        );
    }

    #[test]
    fn own_all_ceiling_equals_full_quota() {
        // Single-node / own-all: fraction 1, so a per-node ceiling equals the
        // cluster-wide quota (the byte-identical path).
        let fraction = OwnedShardFraction::own_all();
        assert_eq!(fraction.per_node_ceiling(256), 256);
        assert_eq!(fraction.per_node_ceiling(0), 0);
        assert_eq!(fraction.per_node_ceiling(1), 1);
    }

    #[test]
    fn proportional_ceiling_is_owned_fraction_of_quota_rounded_up() {
        // A node owning 2 of 8 shards enforces ceil(quota × 2/8) = ceil(quota/4).
        let quarter = OwnedShardFraction::new(2, 8);
        assert_eq!(quarter.per_node_ceiling(256), 64, "256 × 2/8 = 64");
        assert_eq!(quarter.per_node_ceiling(100), 25, "100 × 2/8 = 25");
        // Rounding is UP so per-node ceilings sum to >= quota (over-admit, not starve).
        assert_eq!(
            quarter.per_node_ceiling(10),
            3,
            "ceil(10 × 2/8) = ceil(2.5) = 3"
        );
    }

    #[test]
    fn per_node_ceilings_sum_to_at_least_the_cluster_quota() {
        // Four nodes each owning 2 of 8 shards: each enforces ceil(quota/4); the
        // four ceilings sum to >= quota, never under (the right failure direction).
        let quota = 100;
        let node = OwnedShardFraction::new(2, 8);
        let per_node = node.per_node_ceiling(quota);
        assert!(
            u64::from(per_node) * 4 >= u64::from(quota),
            "4 × 25 = 100 >= 100"
        );
    }

    #[test]
    fn fraction_clamps_degenerate_inputs() {
        // Zero total is clamped to 1 (the keyspace always has >= 1 shard), and owned
        // is clamped to total, so the fraction is always in (0, 1].
        assert_eq!(OwnedShardFraction::new(0, 0).per_node_ceiling(64), 64);
        assert_eq!(
            OwnedShardFraction::new(9, 4).per_node_ceiling(64),
            64,
            "owned > total clamps to 1"
        );
    }
}
