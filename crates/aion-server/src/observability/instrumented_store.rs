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
    /// Advisory outbox wake (LSUB-2): pulsed when an `append_with_outbox` commits
    /// a non-empty outbox-row batch, so the in-process [`OutboxDispatcher`] sweeps
    /// promptly instead of waiting for its next poll tick. Body-less and
    /// best-effort: the dispatcher's interval poll remains the correctness
    /// backstop, so a lost wake only costs poll latency.
    ///
    /// [`OutboxDispatcher`]: crate::worker::OutboxDispatcher
    outbox_wake: Arc<tokio::sync::Notify>,
}

impl InstrumentedEventStore {
    /// Wrap an event store with server-side metrics.
    ///
    /// The store is given a private, never-pulsed outbox wake; callers that share
    /// the engine's stage seam with the dispatcher install the shared handle with
    /// [`Self::with_outbox_wake`].
    #[must_use]
    pub fn new(inner: Arc<dyn EventStore>, metrics: Metrics, namespace: impl Into<String>) -> Self {
        Self {
            inner,
            metrics,
            namespace: namespace.into(),
            outbox_wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Install the shared advisory outbox wake (LSUB-2).
    ///
    /// The supplied `Notify` is the same handle the [`OutboxDispatcher`] awaits,
    /// so a committed outbox-row batch wakes the dispatcher's run loop directly.
    ///
    /// [`OutboxDispatcher`]: crate::worker::OutboxDispatcher
    #[must_use]
    pub fn with_outbox_wake(mut self, outbox_wake: Arc<tokio::sync::Notify>) -> Self {
        self.outbox_wake = outbox_wake;
        self
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
            // LSUB-2 advisory wake: a successful commit that staged at least one
            // outbox row pulses the dispatcher so it sweeps in ~RTT rather than on
            // its next poll tick. Body-less and best-effort — `notify_one`
            // coalesces, which is the desired advisory semantics: the dispatcher's
            // poll is the correctness backstop, so a lost or merged wake only costs
            // poll latency, never a dropped dispatch. Skip the wake when nothing was
            // staged (no fan-out to dispatch) or the append failed (nothing
            // committed).
            if !outbox_rows.is_empty() {
                self.outbox_wake.notify_one();
            }
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

    /// Forward the SS-5 failover scope-widening to the inner store; this
    /// decorator adds only metrics, never ownership policy.
    fn extend_owned_shards(&self, shards: &[usize]) {
        self.inner.extend_owned_shards(shards);
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use aion_core::{
        ContentType, Event, EventEnvelope, PackageVersion, Payload, RunId, WorkflowId,
    };
    use aion_store::{OutboxRow, WritableEventStore, WriteToken};
    use aion_store_libsql::LibSqlStore;
    use chrono::Utc;

    use super::InstrumentedEventStore;
    use crate::observability::Metrics;

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-server-instrumented-store-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }

    fn workflow_started(workflow_id: &WorkflowId) -> Event {
        Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("checkout"),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            run_id: RunId::new_v4(),
            parent_run_id: None,
            package_version: PackageVersion::new("a".repeat(64)),
        }
    }

    /// `notified()` resolves only if the wake has a stored permit (or one arrives);
    /// returns whether it fired inside a short deadline.
    async fn wake_fired(wake: &tokio::sync::Notify) -> bool {
        tokio::time::timeout(Duration::from_millis(200), wake.notified())
            .await
            .is_ok()
    }

    /// LSUB-2 seam: a successful `append_with_outbox` carrying a non-empty outbox
    /// slice pulses the shared advisory wake exactly once.
    #[tokio::test]
    async fn append_with_outbox_fires_wake_on_successful_non_empty_stage()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(LibSqlStore::open(unique_temp_path("fires")).await?);
        let metrics = Metrics::new()?;
        let wake = Arc::new(tokio::sync::Notify::new());
        let instrumented = InstrumentedEventStore::new(store, metrics, "default")
            .with_outbox_wake(Arc::clone(&wake));

        let workflow_id = WorkflowId::new_v4();
        let event = workflow_started(&workflow_id);
        let row = OutboxRow::pending(
            workflow_id.clone(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        );
        instrumented
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                0,
                std::slice::from_ref(&row),
            )
            .await?;

        assert!(
            wake_fired(&wake).await,
            "a successful non-empty outbox stage must pulse the advisory wake"
        );
        Ok(())
    }

    /// LSUB-2 seam: a successful append with an EMPTY outbox slice does NOT pulse
    /// the wake — there is nothing for the dispatcher to sweep.
    #[tokio::test]
    async fn append_with_outbox_does_not_fire_wake_on_empty_slice()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(LibSqlStore::open(unique_temp_path("empty")).await?);
        let metrics = Metrics::new()?;
        let wake = Arc::new(tokio::sync::Notify::new());
        let instrumented = InstrumentedEventStore::new(store, metrics, "default")
            .with_outbox_wake(Arc::clone(&wake));

        let workflow_id = WorkflowId::new_v4();
        let event = workflow_started(&workflow_id);
        // Empty outbox slice: the override delegates to a plain append; no wake.
        instrumented
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                0,
                &[],
            )
            .await?;

        assert!(
            !wake_fired(&wake).await,
            "an empty outbox slice must not pulse the wake (nothing to dispatch)"
        );
        Ok(())
    }

    /// LSUB-2 seam: a FAILED append (here a sequence conflict — wrong expected
    /// head, so nothing commits) does NOT pulse the wake. Without a committed row
    /// there is nothing to dispatch, so a wake would be a spurious sweep at best
    /// and misleading at worst.
    #[tokio::test]
    async fn append_with_outbox_does_not_fire_wake_on_failed_append()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(LibSqlStore::open(unique_temp_path("failed")).await?);
        let metrics = Metrics::new()?;
        let wake = Arc::new(tokio::sync::Notify::new());
        let instrumented = InstrumentedEventStore::new(store, metrics, "default")
            .with_outbox_wake(Arc::clone(&wake));

        let workflow_id = WorkflowId::new_v4();
        let event = workflow_started(&workflow_id);
        let row = OutboxRow::pending(
            workflow_id.clone(),
            0,
            String::from("charge"),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            Utc::now(),
        );
        // expected_seq = 9 against an empty history is a sequence conflict: the
        // append fails and nothing commits, so the wake must stay silent.
        let result = instrumented
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                9,
                std::slice::from_ref(&row),
            )
            .await;
        assert!(result.is_err(), "the seq-conflict append must fail");

        assert!(
            !wake_fired(&wake).await,
            "a failed append commits nothing, so it must not pulse the wake"
        );
        Ok(())
    }
}
