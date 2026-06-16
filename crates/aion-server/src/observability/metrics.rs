//! Prometheus metrics registry and recording helpers.

use std::sync::Arc;
use std::time::Duration;

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
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
    workflows_reopened: IntCounterVec,
    activities_dispatched: IntCounterVec,
    activities_completed: IntCounterVec,
    activity_duration: HistogramVec,
    store_operation_duration: HistogramVec,
    connected_workers: IntGaugeVec,
    inflight_activities: IntGaugeVec,
    signals_delivered: IntCounterVec,
    schedules_fired: IntCounterVec,
    deploy_operations: IntCounterVec,
    deploy_denied: IntCounterVec,
    loaded_workflow_versions: IntGaugeVec,
}

impl MetricsInner {
    fn register_collectors(&self) -> Result<(), prometheus::Error> {
        self.registry
            .register(Box::new(self.workflows_started.clone()))?;
        self.registry
            .register(Box::new(self.workflows_completed.clone()))?;
        self.registry
            .register(Box::new(self.workflows_reopened.clone()))?;
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
        self.registry
            .register(Box::new(self.deploy_operations.clone()))?;
        self.registry
            .register(Box::new(self.deploy_denied.clone()))?;
        self.registry
            .register(Box::new(self.loaded_workflow_versions.clone()))?;
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
        let inner = build_metrics_inner()?;
        inner.register_collectors()?;
        initialize_default_label_sets(&inner);
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

    /// Increment the workflow-reopen counter when a failed run is reopened.
    pub fn workflow_reopened(&self, namespace: &str) {
        self.inner
            .workflows_reopened
            .with_label_values(&[namespace])
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

    /// Increment the deploy-operation counter for one mutation outcome.
    pub fn deploy_operation(&self, operation: &str, outcome: &str) {
        self.inner
            .deploy_operations
            .with_label_values(&[operation, outcome])
            .inc();
    }

    /// Increment the deploy-denied counter for a transport.
    pub fn deploy_denied(&self, transport: &str) {
        self.inner
            .deploy_denied
            .with_label_values(&[transport])
            .inc();
    }

    /// Set the loaded-version gauge for one workflow type from the
    /// post-operation listing.
    pub fn set_loaded_workflow_versions(&self, workflow_type: &str, count: i64) {
        self.inner
            .loaded_workflow_versions
            .with_label_values(&[workflow_type])
            .set(count);
    }
}

fn build_workflow_metrics() -> Result<(IntCounterVec, IntCounterVec, IntCounterVec), MetricsError> {
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
    let workflows_reopened = IntCounterVec::new(
        Opts::new(
            "aion_workflows_reopened_total",
            "Total failed workflow runs reopened, by namespace.",
        ),
        &["namespace"],
    )?;
    Ok((workflows_started, workflows_completed, workflows_reopened))
}

fn build_metrics_inner() -> Result<MetricsInner, MetricsError> {
    let registry = Registry::new();
    let (workflows_started, workflows_completed, workflows_reopened) = build_workflow_metrics()?;
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
    let (deploy_operations, deploy_denied, loaded_workflow_versions) = build_deploy_metrics()?;

    Ok(MetricsInner {
        registry,
        workflows_started,
        workflows_completed,
        workflows_reopened,
        activities_dispatched,
        activities_completed,
        activity_duration,
        store_operation_duration,
        connected_workers,
        inflight_activities,
        signals_delivered,
        schedules_fired,
        deploy_operations,
        deploy_denied,
        loaded_workflow_versions,
    })
}

/// Deploy API collectors: mutation counter, denial counter, and the
/// loaded-version gauge fed from the post-operation listing.
fn build_deploy_metrics() -> Result<(IntCounterVec, IntCounterVec, IntGaugeVec), MetricsError> {
    let deploy_operations = IntCounterVec::new(
        Opts::new(
            "aion_deploy_operations_total",
            "Total deploy API mutations by operation and outcome class.",
        ),
        &["operation", "outcome"],
    )?;
    let deploy_denied = IntCounterVec::new(
        Opts::new(
            "aion_deploy_denied_total",
            "Total deploy API authorization denials by transport.",
        ),
        &["transport"],
    )?;
    let loaded_workflow_versions = IntGaugeVec::new(
        Opts::new(
            "aion_loaded_workflow_versions",
            "Currently loaded package versions per workflow type.",
        ),
        &["workflow_type"],
    )?;
    Ok((deploy_operations, deploy_denied, loaded_workflow_versions))
}

/// Pre-initialize known label sets so all metric families appear in the
/// prometheus text output before any workflow or activity traffic occurs.
fn initialize_default_label_sets(inner: &MetricsInner) {
    for operation in ["append", "read_history", "list_active", "list_workflow_ids"] {
        inner
            .store_operation_duration
            .with_label_values(&[operation]);
    }
    inner
        .activity_duration
        .with_label_values(&["default", "default"]);
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
