//! Shared fixtures and request helpers for the HTTP facade tests.

use std::net::SocketAddr;

use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
use aion_proto::convert::ProtoPayload;
use axum::{body, http::Request, response::Response};
use chrono::Utc;
use serde_json::json;

use crate::config::{
    AuthConfig, DashboardAssetSource, DashboardConfig, ListenConfig, MetricsConfig,
    NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};

pub(crate) const NAMESPACE: &str = "tenant-a";
pub(crate) const TOKEN: &str = "test-token";

pub(crate) fn json_request<T>(
    path: &str,
    value: &T,
) -> Result<Request<body::Body>, Box<dyn std::error::Error>>
where
    T: serde::Serialize,
{
    let body = serde_json::to_vec(value)?;
    Ok(authenticated_request(path)
        .method("POST")
        .header("content-type", "application/json")
        .body(body::Body::from(body))?)
}

pub(crate) fn get_request(path: &str) -> Result<Request<body::Body>, Box<dyn std::error::Error>> {
    Ok(authenticated_request(path)
        .method("GET")
        .body(body::Body::empty())?)
}

fn authenticated_request(path: &str) -> axum::http::request::Builder {
    Request::builder()
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("x-aion-subject", "alice")
        .header("x-aion-namespaces", NAMESPACE)
}

pub(crate) async fn read_json<T>(response: Response) -> Result<T, Box<dyn std::error::Error>>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) async fn read_text(response: Response) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8(bytes.to_vec())?)
}

pub(crate) fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        listen: ListenConfig {
            grpc: SocketAddr::from(([127, 0, 0, 1], 50051)),
            http: SocketAddr::from(([127, 0, 0, 1], 8080)),
        },
        tls: None,
        auth: AuthConfig {
            enabled: true,
            jwks_url: Some(TOKEN.to_owned()),
            jwks_refresh_seconds: 300,
        },
        dashboard: DashboardConfig {
            source: DashboardAssetSource::Embedded,
        },
        namespace: NamespaceConfig {
            mode: NamespaceMode::SharedEngine,
        },
        worker: WorkerConfig {
            heartbeat_window: std::time::Duration::from_millis(30_000),
        },
        websocket: WebSocketConfig {
            outbound_buffer_bound: 32,
            event_broadcast_capacity: Some(64),
        },
        workflow_packages: Vec::new(),
        scheduler_threads: 1,
        query_timeout: Some(std::time::Duration::from_millis(10_000)),
        default_namespace: "default".to_owned(),
        drain_timeout: std::time::Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
    }
}

pub(crate) fn started_event() -> Result<Event, aion_core::PayloadError> {
    Ok(Event::WorkflowStarted {
        envelope: EventEnvelope {
            seq: 1,
            recorded_at: Utc::now(),
            workflow_id: workflow_id(),
        },
        workflow_type: "fixture".to_owned(),
        input: payload()?,
        run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
        parent_run_id: None,
    })
}

pub(crate) fn proto_payload() -> Result<ProtoPayload, aion_core::PayloadError> {
    Ok(payload()?.into())
}

fn payload() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "fixture": "input" }))
}

pub(crate) fn workflow_id() -> WorkflowId {
    WorkflowId::new(uuid::Uuid::from_u128(1))
}

pub(crate) fn run_id() -> aion_core::RunId {
    aion_core::RunId::new(uuid::Uuid::from_u128(10))
}
