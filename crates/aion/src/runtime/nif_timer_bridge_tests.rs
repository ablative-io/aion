//! Wheel-level tests for the deadline fire re-drive (`fire_wheel_timer`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aion_core::{
    Event, PackageVersion, Payload, RunId, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary,
};
use aion_store::{
    EventStore, InMemoryStore, PackageRecord, PackageRouteRecord, ReadableEventStore, RunSummary,
    StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use chrono::{DateTime, Utc};

use super::{fire_wheel_timer, install_timer_nif_bridge, register_deadline_handler};
use crate::durability::{Recorder, WorkflowStartRecord};
use crate::registry::Registry;
use crate::runtime::{RuntimeConfig, RuntimeHandle};
use crate::time::{DeadlineHandler, DeadlineHandlerError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// An [`EventStore`] wrapping an in-memory store whose `read_history` fails for a
/// configured number of the next calls (a bounded, deterministic "store outage"),
/// then delegates. Every other operation delegates unconditionally.
struct FlakyReadStore {
    inner: Arc<InMemoryStore>,
    fail_reads: AtomicUsize,
}

impl FlakyReadStore {
    fn new(inner: Arc<InMemoryStore>) -> Self {
        Self {
            inner,
            fail_reads: AtomicUsize::new(0),
        }
    }

    /// Arms the next `count` `read_history` calls to fail.
    fn fail_next_reads(&self, count: usize) {
        self.fail_reads.store(count, Ordering::SeqCst);
    }

    /// Consumes one armed read failure, returning whether this call should fail.
    fn take_read_failure(&self) -> bool {
        let mut current = self.fail_reads.load(Ordering::SeqCst);
        loop {
            if current == 0 {
                return false;
            }
            match self.fail_reads.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }
}

#[async_trait::async_trait]
impl ReadableEventStore for FlakyReadStore {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        if self.take_read_failure() {
            return Err(StoreError::Backend(
                "simulated store outage during read_history".to_owned(),
            ));
        }
        self.inner.read_history(workflow_id).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history_from(workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_workflow_ids().await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_active().await
    }

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_paused().await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        self.inner.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.inner.expired_timers(as_of).await
    }
}

#[async_trait::async_trait]
impl WritableEventStore for FlakyReadStore {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await
    }
}

#[async_trait::async_trait]
impl aion_store::PackageStore for FlakyReadStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.inner.put_package(record).await
    }

    async fn put_package_with_routes(
        &self,
        record: PackageRecord,
        route_workflow_types: &[String],
    ) -> Result<(), StoreError> {
        self.inner
            .put_package_with_routes(record, route_workflow_types)
            .await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.inner.list_packages().await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner.delete_package(workflow_type, content_hash).await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner
            .put_package_route(workflow_type, content_hash)
            .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.inner.list_package_routes().await
    }
}

/// A deadline handler that counts the fires routed to it.
#[derive(Default)]
struct RecordingDeadlineHandler {
    calls: AtomicUsize,
}

impl RecordingDeadlineHandler {
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl DeadlineHandler for RecordingDeadlineHandler {
    async fn on_deadline_elapsed(
        &self,
        _workflow_id: WorkflowId,
        _run_id: RunId,
    ) -> Result<(), DeadlineHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// F1 re-drive: a store outage that fails BOTH the fire-path liveness read AND
/// the immediately-following `deadline_remains_live` read on the first attempt
/// must NOT kill the wheel task. Once the outage clears (before the retry), a
/// later attempt drives the deadline fire to the handler. Mutation-sensitive: the
/// pre-fix code returned on the liveness-read error, so the handler would never
/// be called.
#[tokio::test(flavor = "multi_thread")]
async fn deadline_fire_retries_through_a_store_outage_spanning_both_reads() -> TestResult {
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let inner = Arc::new(InMemoryStore::default());
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let deadline_id = crate::time::deadline_timer_id(&run_id)?;
    // Seed a live deadline in history BEFORE arming the outage.
    let event_store: Arc<dyn EventStore> = Arc::clone(&inner) as Arc<dyn EventStore>;
    let mut recorder = Recorder::new(workflow_id.clone(), event_store);
    recorder
        .record_workflow_started(
            Utc::now(),
            WorkflowStartRecord {
                workflow_type: "sleeper".to_owned(),
                input: Payload::from_json(&serde_json::json!({}))?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: PackageVersion::new("a".repeat(64)),
            },
        )
        .await?;
    recorder
        .record_timer_started(Utc::now(), deadline_id.clone(), Utc::now())
        .await?;

    let flaky = Arc::new(FlakyReadStore::new(inner));
    let registry = Arc::new(Registry::default());
    install_timer_nif_bridge(
        runtime.nif_state(),
        Arc::clone(&registry),
        Arc::clone(&flaky) as Arc<dyn EventStore>,
        tokio::runtime::Handle::current(),
        runtime.signal_delivery(),
    );
    let handler = Arc::new(RecordingDeadlineHandler::default());
    register_deadline_handler(
        runtime.nif_state(),
        Arc::clone(&handler) as Arc<dyn DeadlineHandler>,
    )
    .map_err(|error| format!("failed to register deadline handler: {error}"))?;

    // Fail the next two reads: the first attempt's fire-path liveness read and its
    // immediate deadline_remains_live read. The third read (attempt two) succeeds.
    flaky.fail_next_reads(2);

    fire_wheel_timer(
        &Arc::downgrade(runtime.nif_state()),
        &workflow_id,
        &deadline_id,
        Utc::now(),
    )
    .await;

    assert!(
        handler.call_count() >= 1,
        "a later retry attempt drove the deadline fire to the handler after the double-read outage"
    );
    runtime.shutdown()?;
    Ok(())
}
