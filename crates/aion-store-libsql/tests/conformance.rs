//! `LibSQL` `EventStore` conformance coverage.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_store::{
    Event, EventStore, ReadableEventStore, RunSummary, StoreError, TimerEntry, TimerId,
    WorkflowFilter, WorkflowId, WorkflowSummary, WritableEventStore, WriteToken,
    conformance::run_event_store_suite,
};
use aion_store_libsql::LibSqlStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn libsql_store_satisfies_event_store_conformance_suite() -> Result<(), StoreError> {
    run_event_store_suite(|| async {
        let store = LibSqlStore::open(unique_temp_path("conformance")).await;
        Arc::new(StoreOpenResult::new(store)) as Arc<dyn EventStore>
    })
    .await
}

struct StoreOpenResult {
    store: Result<LibSqlStore, StoreError>,
}

impl StoreOpenResult {
    fn new(store: Result<LibSqlStore, StoreError>) -> Self {
        Self { store }
    }

    fn store(&self) -> Result<&LibSqlStore, StoreError> {
        self.store.as_ref().map_err(Clone::clone)
    }
}

#[async_trait]
impl WritableEventStore for StoreOpenResult {
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
impl ReadableEventStore for StoreOpenResult {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.store()?.read_history(workflow_id).await
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

fn unique_temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!(
        "aion-store-libsql-{name}-{}-{nanos}-{counter}.db",
        std::process::id()
    ))
}
