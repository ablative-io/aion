//! [`InstrumentedEventStore`]: event-store decorator recording server metrics.

use std::sync::Arc;
use std::time::Instant;

use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_store::{
    EventStore, OutboxRow, PackageRecord, PackageRouteRecord, PackageStore, ReadableEventStore,
    RunSummary, StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::metrics::Metrics;

/// Event-store wrapper that observes operation latency and lifecycle events without changing engine crates.
pub struct InstrumentedEventStore {
    inner: Arc<dyn EventStore>,
    metrics: Metrics,
    namespace: String,
}

impl InstrumentedEventStore {
    /// Wrap an event store with server-side metrics.
    #[must_use]
    pub fn new(inner: Arc<dyn EventStore>, metrics: Metrics, namespace: impl Into<String>) -> Self {
        Self {
            inner,
            metrics,
            namespace: namespace.into(),
        }
    }

    fn record_events(&self, events: &[Event]) {
        for event in events {
            match event {
                Event::WorkflowStarted { workflow_type, .. } => {
                    self.metrics
                        .workflow_started(&self.namespace, workflow_type.as_str());
                }
                Event::WorkflowCompleted { .. } => {
                    self.metrics
                        .workflow_completed(&self.namespace, "completed");
                }
                Event::WorkflowFailed { .. } => {
                    self.metrics.workflow_completed(&self.namespace, "failed");
                }
                Event::WorkflowCancelled { .. } => {
                    self.metrics
                        .workflow_completed(&self.namespace, "cancelled");
                }
                Event::WorkflowTimedOut { .. } => {
                    self.metrics
                        .workflow_completed(&self.namespace, "timed_out");
                }
                Event::WorkflowContinuedAsNew { .. } => {
                    self.metrics
                        .workflow_completed(&self.namespace, "continued_as_new");
                }
                Event::WorkflowReopened { .. } => {
                    self.metrics.workflow_reopened(&self.namespace);
                }
                Event::SignalReceived { .. } => {
                    self.metrics.signal_delivered(&self.namespace, "resident");
                }
                Event::ScheduleTriggered { .. } => {
                    self.metrics.schedule_fired(&self.namespace);
                }
                _ => {}
            }
        }
    }

    fn observe_since(&self, operation: &str, started: Instant) {
        self.metrics.store_operation(operation, started.elapsed());
    }
}

impl std::fmt::Debug for InstrumentedEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstrumentedEventStore")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl WritableEventStore for InstrumentedEventStore {
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self
            .inner
            .append(token, workflow_id, events, expected_seq)
            .await;
        self.observe_since("append", started);
        if result.is_ok() {
            self.record_events(events);
        }
        result
    }

    /// Forward the atomic durable-outbox append to the inner store.
    ///
    /// The default trait method REFUSES a non-empty `outbox_rows` slice (to stop
    /// an outbox-unaware backend silently dropping fan-out rows). Without this
    /// override the engine — which writes through this decorator — would never
    /// reach the inner libSQL store's outbox-capable append, so a commissioned
    /// (`outbox.enabled`) server could not stage a single fan-out member. We
    /// delegate to the inner store so its atomicity guarantee (events + rows
    /// commit together) holds, and observe the same `append` latency bucket and
    /// lifecycle metrics as a plain append.
    async fn append_with_outbox(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self
            .inner
            .append_with_outbox(token, workflow_id, events, expected_seq, outbox_rows)
            .await;
        self.observe_since("append", started);
        if result.is_ok() {
            self.record_events(events);
        }
        result
    }

    /// Forward the crash-recovery outbox re-arm to the inner store.
    ///
    /// As with [`Self::append_with_outbox`], the refusing default would strand a
    /// recovered fan-out member because the engine re-arms through this decorator.
    async fn rearm_outbox_pending(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self.inner.rearm_outbox_pending(rows).await;
        self.observe_since("append", started);
        result
    }
}

#[async_trait]
impl ReadableEventStore for InstrumentedEventStore {
    /// Forward owned-shard scoping to the inner store; this decorator adds only
    /// metrics, never shard policy, so the inner backend remains the sole
    /// authority on enumeration scope.
    fn set_owned_shards(&self, shards: Option<&[usize]>) {
        self.inner.set_owned_shards(shards);
    }

    /// Forward the SS-2 shard election to the inner store; this decorator adds
    /// only metrics, never ownership policy, so the inner backend runs the
    /// election (or no-ops in single-node mode).
    fn acquire_owned_shards(&self, shards: &[usize]) -> Result<(), StoreError> {
        self.inner.acquire_owned_shards(shards)
    }

    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        let started = Instant::now();
        let result = self.inner.read_history(workflow_id).await;
        self.observe_since("read_history", started);
        result
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        let started = Instant::now();
        let result = self.inner.read_history_from(workflow_id, from_seq).await;
        self.observe_since("read_history_from", started);
        result
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let started = Instant::now();
        let result = self.inner.list_workflow_ids().await;
        self.observe_since("list_workflow_ids", started);
        result
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let started = Instant::now();
        let result = self.inner.list_active().await;
        self.observe_since("list_active", started);
        result
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

#[async_trait]
impl PackageStore for InstrumentedEventStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self.inner.put_package(record).await;
        self.observe_since("put_package", started);
        result
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        let started = Instant::now();
        let result = self.inner.list_packages().await;
        self.observe_since("list_packages", started);
        result
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self.inner.delete_package(workflow_type, content_hash).await;
        self.observe_since("delete_package", started);
        result
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self
            .inner
            .put_package_route(workflow_type, content_hash)
            .await;
        self.observe_since("put_package_route", started);
        result
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        let started = Instant::now();
        let result = self.inner.list_package_routes().await;
        self.observe_since("list_package_routes", started);
        result
    }
}
