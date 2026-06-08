//! Prometheus metrics registry and recording helpers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_store::{EventStore, RunSummary, StoreError, TimerEntry};
use async_trait::async_trait;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder,
};
use thiserror::Error;

const TEXT_FORMAT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Prometheus registry construction or exposition error.
#[derive(Debug, Error)]
pub enum MetricsError {
    /// A metric failed to register in the prometheus registry.
    #[error("failed to register prometheus metric: {0}")]
    Register(#[from] prometheus::Error),
    /// The prometheus text encoder failed to encode gathered metric families.
    #[error("failed to encode prometheus metrics: {0}")]
    Encode(String),
}

/// Cloneable server metrics handle backed by a prometheus registry.
#[derive(Clone, Debug)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

#[derive(Debug)]
struct MetricsInner {
    registry: Registry,
    workflows_started: IntCounterVec,
    workflows_completed: IntCounterVec,
    activities_dispatched: IntCounterVec,
    activities_completed: IntCounterVec,
    activity_duration: HistogramVec,
    store_operation_duration: HistogramVec,
    connected_workers: IntGaugeVec,
    inflight_activities: IntGaugeVec,
    signals_delivered: IntCounterVec,
    schedules_fired: IntCounterVec,
}

impl MetricsInner {
    fn register_collectors(&self) -> Result<(), prometheus::Error> {
        self.registry
            .register(Box::new(self.workflows_started.clone()))?;
        self.registry
            .register(Box::new(self.workflows_completed.clone()))?;
        self.registry
            .register(Box::new(self.activities_dispatched.clone()))?;
        self.registry
            .register(Box::new(self.activities_completed.clone()))?;
        self.registry
            .register(Box::new(self.activity_duration.clone()))?;
        self.registry
            .register(Box::new(self.store_operation_duration.clone()))?;
        self.registry
            .register(Box::new(self.connected_workers.clone()))?;
        self.registry
            .register(Box::new(self.inflight_activities.clone()))?;
        self.registry
            .register(Box::new(self.signals_delivered.clone()))?;
        self.registry
            .register(Box::new(self.schedules_fired.clone()))?;
        Ok(())
    }
}

impl Metrics {
    /// Construct the server metrics registry and register all exported metrics.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Register`] if prometheus rejects a metric descriptor.
    pub fn new() -> Result<Self, MetricsError> {
        let registry = Registry::new();

        let workflows_started = IntCounterVec::new(
            Opts::new(
                "aion_workflows_started_total",
                "Total workflow executions started by namespace and workflow type.",
            ),
            &["namespace", "workflow_type"],
        )?;
        let workflows_completed = IntCounterVec::new(
            Opts::new(
                "aion_workflows_completed_total",
                "Total workflow executions that reached a terminal status by namespace and status.",
            ),
            &["namespace", "status"],
        )?;
        let activities_dispatched = IntCounterVec::new(
            Opts::new(
                "aion_activities_dispatched_total",
                "Total activities dispatched to workers by namespace and activity type.",
            ),
            &["namespace", "activity_type"],
        )?;
        let activities_completed = IntCounterVec::new(
            Opts::new(
                "aion_activities_completed_total",
                "Total activity results received by namespace and outcome.",
            ),
            &["namespace", "outcome"],
        )?;
        let activity_duration = HistogramVec::new(
            HistogramOpts::new(
                "aion_activity_duration_seconds",
                "Wall-clock activity execution latency from dispatch to result by namespace and activity type.",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
            ]),
            &["namespace", "activity_type"],
        )?;
        let store_operation_duration = HistogramVec::new(
            HistogramOpts::new(
                "aion_store_operation_duration_seconds",
                "Store operation latency by operation.",
            )
            .buckets(vec![
                0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
            ]),
            &["operation"],
        )?;
        let connected_workers = IntGaugeVec::new(
            Opts::new(
                "aion_connected_workers",
                "Current connected worker streams by namespace.",
            ),
            &["namespace"],
        )?;
        let inflight_activities = IntGaugeVec::new(
            Opts::new(
                "aion_inflight_activities",
                "Current dispatched activities awaiting worker completion by namespace.",
            ),
            &["namespace"],
        )?;
        let signals_delivered = IntCounterVec::new(
            Opts::new(
                "aion_signals_delivered_total",
                "Total signals delivered by namespace and residency classification.",
            ),
            &["namespace", "residency"],
        )?;
        let schedules_fired = IntCounterVec::new(
            Opts::new(
                "aion_schedules_fired_total",
                "Total schedule timer evaluations that started a workflow by namespace.",
            ),
            &["namespace"],
        )?;

        let inner = MetricsInner {
            registry,
            workflows_started,
            workflows_completed,
            activities_dispatched,
            activities_completed,
            activity_duration,
            store_operation_duration,
            connected_workers,
            inflight_activities,
            signals_delivered,
            schedules_fired,
        };
        inner.register_collectors()?;
        for operation in ["append", "read_history", "list_active"] {
            inner
                .store_operation_duration
                .with_label_values(&[operation]);
        }

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Encode all currently gathered metrics in prometheus text exposition format.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Encode`] if prometheus cannot encode gathered metrics.
    pub fn encode(&self) -> Result<Vec<u8>, MetricsError> {
        let encoder = TextEncoder::new();
        let families = self.inner.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&families, &mut buffer)
            .map_err(|error| MetricsError::Encode(error.to_string()))?;
        Ok(buffer)
    }

    /// Increment the workflow-start counter.
    pub fn workflow_started(&self, namespace: &str, workflow_type: &str) {
        self.inner
            .workflows_started
            .with_label_values(&[namespace, workflow_type])
            .inc();
    }

    /// Increment the workflow-terminal counter.
    pub fn workflow_completed(&self, namespace: &str, status: &str) {
        self.inner
            .workflows_completed
            .with_label_values(&[namespace, status])
            .inc();
    }

    /// Increment the activity-dispatch counter and in-flight gauge.
    pub fn activity_dispatched(&self, namespace: &str, activity_type: &str) {
        self.inner
            .activities_dispatched
            .with_label_values(&[namespace, activity_type])
            .inc();
        self.inner
            .inflight_activities
            .with_label_values(&[namespace])
            .inc();
    }

    /// Increment the activity-completion counter, observe duration, and decrement in-flight gauge.
    pub fn activity_completed(
        &self,
        namespace: &str,
        activity_type: &str,
        outcome: &str,
        duration: Duration,
    ) {
        self.inner
            .activities_completed
            .with_label_values(&[namespace, outcome])
            .inc();
        self.inner
            .activity_duration
            .with_label_values(&[namespace, activity_type])
            .observe(duration.as_secs_f64());
        self.inner
            .inflight_activities
            .with_label_values(&[namespace])
            .dec();
    }

    /// Decrement in-flight activity gauge when dispatch fails before a result can arrive.
    pub fn activity_abandoned(&self, namespace: &str) {
        self.inner
            .inflight_activities
            .with_label_values(&[namespace])
            .dec();
    }

    /// Observe a store operation duration.
    pub fn store_operation(&self, operation: &str, duration: Duration) {
        self.inner
            .store_operation_duration
            .with_label_values(&[operation])
            .observe(duration.as_secs_f64());
    }

    /// Increment connected worker gauge for a namespace.
    pub fn worker_connected(&self, namespace: &str) {
        self.inner
            .connected_workers
            .with_label_values(&[namespace])
            .inc();
    }

    /// Decrement connected worker gauge for a namespace.
    pub fn worker_disconnected(&self, namespace: &str) {
        self.inner
            .connected_workers
            .with_label_values(&[namespace])
            .dec();
    }

    /// Increment signal delivery counter.
    pub fn signal_delivered(&self, namespace: &str, residency: &str) {
        self.inner
            .signals_delivered
            .with_label_values(&[namespace, residency])
            .inc();
    }

    /// Increment schedule-fired counter.
    pub fn schedule_fired(&self, namespace: &str) {
        self.inner
            .schedules_fired
            .with_label_values(&[namespace])
            .inc();
    }
}

/// Axum handler for `/metrics`.
pub async fn metrics_handler(
    axum::extract::State(metrics): axum::extract::State<Metrics>,
) -> Response {
    match metrics.encode() {
        Ok(body) => {
            let mut response = body.into_response();
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static(TEXT_FORMAT));
            response
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

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
impl EventStore for InstrumentedEventStore {
    async fn append(
        &self,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let result = self.inner.append(workflow_id, events, expected_seq).await;
        self.observe_since("append", started);
        if result.is_ok() {
            self.record_events(events);
        }
        result
    }

    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        let started = Instant::now();
        let result = self.inner.read_history(workflow_id).await;
        self.observe_since("read_history", started);
        result
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(workflow_id).await
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
