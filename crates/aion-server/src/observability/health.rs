//! Kubernetes-style liveness and readiness probes.

use std::sync::Arc;
use std::time::Duration;

use aion_core::WorkflowId;
use aion_store::ReadableEventStore;
use axum::extract::State;
use axum::http::StatusCode;
use tokio::time::timeout;

const LIVENESS_TIMEOUT: Duration = Duration::from_millis(50);
const READINESS_TIMEOUT: Duration = Duration::from_millis(100);

/// Cloneable state used by health probe handlers.
#[derive(Clone)]
pub struct HealthState {
    store: Arc<dyn ReadableEventStore>,
    runtime_initialized: bool,
}

impl HealthState {
    /// Build health state from the store and runtime initialization flag.
    #[must_use]
    pub fn new(store: Arc<dyn ReadableEventStore>, runtime_initialized: bool) -> Self {
        Self {
            store,
            runtime_initialized,
        }
    }
}

/// Liveness probe: validates that the async scheduler can run a trivial task promptly.
pub async fn live() -> StatusCode {
    let check = async {
        let handle = tokio::spawn(async {});
        handle.await.is_ok()
    };

    match timeout(LIVENESS_TIMEOUT, check).await {
        Ok(true) => StatusCode::OK,
        Ok(false) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Readiness probe: validates runtime initialization and store reachability with a no-op read.
pub async fn ready(State(state): State<HealthState>) -> StatusCode {
    if !state.runtime_initialized {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    let workflow_id = WorkflowId::new_v4();
    match timeout(READINESS_TIMEOUT, state.store.read_history(&workflow_id)).await {
        Ok(Ok(_)) => StatusCode::OK,
        Ok(Err(_)) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
