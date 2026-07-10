//! Single-node `HaematiteStore` `EventStore` conformance coverage.
//!
//! Gates the haematite-backed store against the exact same behavioural suite the
//! in-memory and libSQL stores run (`aion_store::conformance`). Each scenario
//! gets a fresh temp-directory database.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_store::{
    Event, EventStore, PackageRecord, PackageRouteRecord, PackageStore, ReadableEventStore,
    RunSummary, StoreError, TimerEntry, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary,
    WritableEventStore, WriteToken,
    conformance::{run_event_store_suite, run_outbox_settlement_suite},
};
use aion_store_haematite::HaematiteStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[tokio::test(flavor = "multi_thread")]
async fn haematite_store_satisfies_event_store_conformance_suite() -> Result<(), StoreError> {
    run_event_store_suite(|| async {
        let store = HaematiteStore::create(unique_temp_dir("conformance"));
        Arc::new(StoreCreateResult::new(store)) as Arc<dyn EventStore>
    })
    .await
}

/// #253: the workflow-terminal outbox settlement contract (settle twin,
/// unsettled enumeration, read-only stale probe, Cancelled-untouchable re-arm
/// pins, reopen-re-arm resurrection, writer-seam settle) over the real
/// haematite outbox keyspace.
#[tokio::test(flavor = "multi_thread")]
async fn haematite_store_satisfies_outbox_settlement_conformance_suite() -> Result<(), StoreError> {
    run_outbox_settlement_suite(|| async {
        HaematiteStore::create(unique_temp_dir("outbox-settlement"))
    })
    .await
}

/// Defers a fallible `HaematiteStore::create` into the `EventStore` surface: the
/// conformance suite's `make_store` closure must hand back an infallible
/// `Arc<dyn EventStore>`, so a creation failure is carried as the stored `Result`
/// and surfaces (with its original context) on the first store method call.
struct StoreCreateResult {
    store: Result<HaematiteStore, StoreError>,
}

impl StoreCreateResult {
    fn new(store: Result<HaematiteStore, StoreError>) -> Self {
        Self { store }
    }

    fn store(&self) -> Result<&HaematiteStore, StoreError> {
        self.store.as_ref().map_err(Clone::clone)
    }
}

#[async_trait]
impl WritableEventStore for StoreCreateResult {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        self.store()?
            .append(token, workflow_id, events, expected_seq)
            .await
    }
}

#[async_trait]
impl ReadableEventStore for StoreCreateResult {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.store()?.read_history(workflow_id).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        self.store()?.read_history_from(workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.store()?.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.store()?.list_workflow_ids().await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.store()?.list_active().await
    }

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.store()?.list_paused().await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        self.store()?.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.store()?
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.store()?.expired_timers(as_of).await
    }
}

#[async_trait]
impl PackageStore for StoreCreateResult {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.store()?.put_package(record).await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.store()?.list_packages().await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.store()?
            .delete_package(workflow_type, content_hash)
            .await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.store()?
            .put_package_route(workflow_type, content_hash)
            .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.store()?.list_package_routes().await
    }
}

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-haematite-{name}-{}-{nanos}-{counter}",
        std::process::id()
    ))
}
