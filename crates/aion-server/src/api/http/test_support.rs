//! Shared fixtures and request helpers for the HTTP facade tests.

use std::net::SocketAddr;
use std::sync::Arc;

use aion::{Engine, EngineBuilder};
use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
use aion_proto::convert::ProtoPayload;
use aion_store::{EventStore, InMemoryStore, visibility::VisibilityStore};
use axum::{body, http::Request, response::Response};
use chrono::Utc;
use serde_json::json;

use crate::config::{
    AuthConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig, MetricsConfig,
    NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use crate::{NamespaceResolver, ServerState};

pub(crate) const NAMESPACE: &str = "tenant-a";

/// Development shared-secret token validated by the `cfg(not(feature = "auth"))`
/// path (it doubles as the configured `jwks_url` dev value).
pub(crate) const TOKEN: &str = "test-token";

/// Server state whose bearer validation matches the compiled auth path: under
/// `feature = "auth"` a real [`crate::auth::JwksCache`] is fetched from a live
/// fixture JWKS endpoint; otherwise the development token path needs no cache.
pub(crate) async fn server_state(
    resolver: NamespaceResolver,
    runtime: RuntimeConfig,
) -> Result<ServerState, Box<dyn std::error::Error>> {
    #[cfg(feature = "auth")]
    {
        let url = crate::auth::test_support::serve_jwks()?;
        let refresh = std::time::Duration::from_secs(runtime.auth.jwks_refresh_seconds);
        let cache = crate::auth::JwksCache::new(url, refresh).await?;
        Ok(ServerState::from_parts_with_jwks(resolver, runtime, cache))
    }
    #[cfg(not(feature = "auth"))]
    {
        // Yield to preserve the async signature required by the auth-feature branch.
        tokio::task::yield_now().await;
        Ok(ServerState::from_parts(resolver, runtime))
    }
}

/// Engine over one in-memory backing store shared by events and visibility.
pub(crate) async fn shared_engine()
-> Result<(Arc<Engine>, Arc<dyn EventStore>, Arc<dyn VisibilityStore>), aion::EngineError> {
    let backing = Arc::new(InMemoryStore::default());
    let store: Arc<dyn EventStore> = backing.clone();
    let visibility_store: Arc<dyn VisibilityStore> = backing;
    let engine = Arc::new(
        EngineBuilder::new()
            .store_arc(Arc::clone(&store))
            .visibility_store_arc(Arc::clone(&visibility_store))
            .scheduler_threads(1)
            .build()
            .await?,
    );
    Ok((engine, store, visibility_store))
}

pub(crate) fn json_request<T>(
    path: &str,
    value: &T,
) -> Result<Request<body::Body>, Box<dyn std::error::Error>>
where
    T: serde::Serialize,
{
    let body = serde_json::to_vec(value)?;
    // Bearer credential accepted by the compiled authentication path: a JWT
    // minted against the fixture JWKS under `feature = "auth"`, the
    // development shared-secret token otherwise.
    #[cfg(feature = "auth")]
    let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
    #[cfg(not(feature = "auth"))]
    let bearer = TOKEN.to_owned();
    Ok(authenticated_request(path, &bearer)
        .method("POST")
        .header("content-type", "application/json")
        .body(body::Body::from(body))?)
}

pub(crate) fn get_request(path: &str) -> Result<Request<body::Body>, Box<dyn std::error::Error>> {
    #[cfg(feature = "auth")]
    let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
    #[cfg(not(feature = "auth"))]
    let bearer = TOKEN.to_owned();
    Ok(authenticated_request(path, &bearer)
        .method("GET")
        .body(body::Body::empty())?)
}

fn authenticated_request(path: &str, bearer: &str) -> axum::http::request::Builder {
    Request::builder()
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
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

/// Test runtime settings with authentication enabled.
///
/// `auth.jwks_url` carries the development shared secret; the states built by
/// [`server_state`] under `feature = "auth"` validate against an injected
/// [`crate::auth::JwksCache`] (fed by a live fixture JWKS endpoint), and the
/// one test that exercises the production `build_with_store` startup path
/// overrides `jwks_url` with a live endpoint itself.
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
        deploy: DeployConfig::default(),
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
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
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
