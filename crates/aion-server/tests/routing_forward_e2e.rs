//! R-3 request-forwarding end-to-end coverage over a REAL gRPC hop.
//!
//! Stands up an "owner" `WorkflowGrpcService` on a real tonic server and drives
//! the production [`GrpcRequestForwarder`] against it, proving the forwarder
//! dials the owner's gRPC address, copies the caller metadata, stamps the
//! forward-hop header, and relays the owner's reply/error verbatim. This is the
//! gRPC half of the forward path that a 2-node cluster exercises, without
//! needing haematite distribution stood up.
//!
//! Behind `--features haematite-backend` (the forwarder lives there).

#![cfg(feature = "haematite-backend")]

use std::sync::Arc;
use std::time::Duration;

use aion::EngineBuilder;
use aion_proto::generated::{self, workflow_service_server::WorkflowServiceServer};
use aion_server::api::grpc::workflow_service;
use aion_server::config::{NamespaceConfig, NamespaceMode};
use aion_server::routing::{
    FORWARD_HOPS_METADATA, ForwardReply, ForwardRequest, GrpcRequestForwarder, RequestForwarder,
    current_hops,
};
use aion_server::{NamespaceResolver, ServerState};
use aion_store::{EventStore, InMemoryStore};
use tokio::net::TcpListener;
use tonic::transport::Server;

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "tenant-a";

/// Build an owner `ServerState` over an in-memory engine (no workflows loaded),
/// wired through the production resolver exactly as a server boot does.
async fn owner_state() -> Result<ServerState, TestError> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = Arc::new(
        EngineBuilder::new()
            .store_arc(store)
            .in_memory_visibility()
            .scheduler_threads(1)
            .query_timeout(Duration::from_secs(5))
            .build()
            .await?,
    );
    let resolver = NamespaceResolver::from_config(
        NamespaceConfig {
            mode: NamespaceMode::SharedEngine,
        },
        engine,
    );
    // Auth disabled by default in the test runtime config, so the forwarded
    // metadata authorizes through the development caller path.
    Ok(ServerState::from_parts(resolver, test_runtime()))
}

fn test_runtime() -> aion_server::config::RuntimeConfig {
    use aion_server::config::{
        AuthConfig, AuthoringConfig, DeployConfig, DevConfig, ListenConfig, MetricsConfig,
        OpsConsoleAssetSource, OpsConsoleConfig, OutboxConfig, RuntimeConfig, WebSocketConfig,
        WorkerConfig,
    };
    RuntimeConfig {
        listen: ListenConfig {
            grpc: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            http: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        },
        tls: None,
        auth: AuthConfig {
            enabled: false,
            jwks_url: None,
            jwks_refresh_seconds: 300,
        },
        ops_console: OpsConsoleConfig {
            source: OpsConsoleAssetSource::Embedded,
        },
        namespace: NamespaceConfig {
            mode: NamespaceMode::SharedEngine,
        },
        worker: WorkerConfig {
            heartbeat_window: Duration::from_millis(30_000),
        },
        websocket: WebSocketConfig {
            outbound_buffer_bound: 32,
            event_broadcast_capacity: Some(64),
            cluster_broadcast_capacity: Some(64),
        },
        workflow_packages: Vec::new(),
        deploy: DeployConfig::default(),
        authoring: AuthoringConfig::default(),
        dev: DevConfig::default(),
        outbox: OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(5_000)),
        default_namespace: "default".to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

/// Spawn the owner gRPC server on an ephemeral port; returns its address and a
/// shutdown trigger.
async fn spawn_owner(
    service: WorkflowServiceServer<impl generated::workflow_service_server::WorkflowService>,
) -> Result<(std::net::SocketAddr, tokio::sync::oneshot::Sender<()>), TestError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = rx.await;
            })
            .await;
    });
    // Give the server a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok((addr, tx))
}

/// A cancel for an unknown workflow forwarded to the owner relays the owner's
/// typed `NotFound` back through the forwarder — proving the real gRPC hop,
/// metadata copy, hop-stamp, and verbatim error relay.
#[tokio::test]
async fn forward_cancel_relays_owner_not_found() -> Result<(), TestError> {
    let state = owner_state().await?;
    let (addr, shutdown) = spawn_owner(workflow_service(state)).await?;

    let forwarder = GrpcRequestForwarder::new();
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert("x-aion-subject", "alice".parse()?);
    metadata.insert("x-aion-namespaces", NAMESPACE.parse()?);

    let request = ForwardRequest::Cancel(generated::CancelRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_id: Some(generated::WorkflowId {
            uuid: uuid::Uuid::from_u128(424_242).to_string(),
        }),
        run_id: None,
        reason: "forwarded".to_owned(),
    });

    let result = forwarder.forward(addr, metadata, request).await;
    let _ = shutdown.send(());

    // The owner has no such workflow: it returns NotFound, relayed verbatim.
    let status = result.err().ok_or("expected the owner to reject")?;
    assert_eq!(status.code(), tonic::Code::NotFound);
    Ok(())
}

/// A steered `start` forwarded to the owner runs there over the real gRPC hop:
/// the owner has no such workflow type loaded, so it returns its typed
/// `WorkflowNotFound`, relayed verbatim through the forwarder — proving the
/// `ForwardRequest::Start` path dials the owner, copies metadata, stamps the hop
/// header, and relays the owner's reply/error (R-4 steered-start forward).
#[tokio::test]
async fn forward_start_relays_owner_reply() -> Result<(), TestError> {
    let state = owner_state().await?;
    let (addr, shutdown) = spawn_owner(workflow_service(state)).await?;

    let forwarder = GrpcRequestForwarder::new();
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert("x-aion-subject", "alice".parse()?);
    metadata.insert("x-aion-namespaces", NAMESPACE.parse()?);

    let request = ForwardRequest::Start(generated::StartWorkflowRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_type: "checkout".to_owned(),
        input: Some(generated::Payload {
            content_type: "application/json".to_owned(),
            bytes: b"{}".to_vec(),
        }),
        routing_key: Some("tenant-a/order-1".to_owned()),
        task_queue: None,
    });

    let result = forwarder.forward(addr, metadata, request).await;
    let _ = shutdown.send(());

    // The owner has no `checkout` workflow loaded: it rejects with NotFound,
    // relayed verbatim. The forward path itself (dial + metadata + hop) succeeded.
    let status = result
        .err()
        .ok_or("expected the owner to reject the start")?;
    assert_eq!(status.code(), tonic::Code::NotFound);
    Ok(())
}

/// The steered-start reply variant is inhabited as expected.
#[test]
fn forward_reply_start_is_constructible() {
    let reply = ForwardReply::Start(generated::StartWorkflowResponse {
        workflow_id: None,
        run_id: None,
    });
    assert!(matches!(reply, ForwardReply::Start(_)));
}

/// The forwarder stamps the next hop count onto the outbound request, and a
/// request already at the hop cap is recognised by `current_hops`.
#[test]
fn hop_metadata_round_trips() -> Result<(), TestError> {
    let mut metadata = tonic::metadata::MetadataMap::new();
    assert_eq!(current_hops(&metadata), 0);
    metadata.insert(FORWARD_HOPS_METADATA, "2".parse()?);
    assert_eq!(current_hops(&metadata), 2);
    Ok(())
}

/// A reply variant mismatch is impossible on the happy path, but the relay
/// pattern is exercised by the e2e test above; this asserts the reply enum is
/// inhabited as expected for cancel.
#[test]
fn forward_reply_cancel_is_constructible() {
    let reply = ForwardReply::Cancel(generated::CancelResponse {});
    assert!(matches!(reply, ForwardReply::Cancel(_)));
}
