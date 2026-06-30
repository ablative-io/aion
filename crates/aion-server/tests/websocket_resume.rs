//! End-to-end WebSocket subscription resumption over the real HTTP router.
//!
//! Proves the full splice path with the real engine broadcast publisher
//! (`EngineBuilder::event_streaming`) behind the real axum router:
//!
//! - connect, receive events, drop the socket, reconnect with
//!   `resume_from_seq = last_seq + 1`, and observe contiguous duplicate-free
//!   delivery spanning the disconnect;
//! - a cursor beyond the recorded head is one terminal `invalid_input` frame
//!   (`error_type` = `ResumeCursorAheadOfHistory`) followed by close;
//! - a firehose authorized for one namespace never observes another tenant's
//!   events even though the broadcast channel is engine-global.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use aion::{Engine, EngineBuilder};
use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
use aion_proto::StreamedEvent;
use aion_server::api::http::workflow_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::{
    NamespaceResolver, ServerState, StaticScheduleNamespaces, StaticWorkflowNamespaces,
};
use aion_store::{EventStore, InMemoryStore, WriteToken};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type TestError = Box<dyn std::error::Error>;
type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(2);

struct StreamServer {
    address: SocketAddr,
    /// Engine-owned store: appends flow through the publishing wrapper so
    /// every durable commit is broadcast, exactly as in production.
    store: Arc<dyn EventStore>,
    server: tokio::task::JoinHandle<()>,
    engine: Arc<Engine>,
}

impl StreamServer {
    async fn start(ownership: StaticWorkflowNamespaces) -> Result<Self, TestError> {
        let backing: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(backing)
                .event_streaming(
                    NonZeroUsize::new(64).ok_or("broadcast capacity must be non-zero")?,
                )
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let store = engine.store();
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(Arc::clone(&engine)),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(ServerState::from_parts(resolver, runtime_config()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "resume test server exited with error");
            }
        });
        Ok(Self {
            address,
            store,
            server,
            engine,
        })
    }

    async fn append(&self, workflow_id: &WorkflowId, events: &[Event]) -> Result<(), TestError> {
        let expected_seq = events
            .first()
            .map(Event::seq)
            .ok_or("append requires at least one event")?
            - 1;
        self.store
            .append(WriteToken::recorder(), workflow_id, events, expected_seq)
            .await?;
        Ok(())
    }

    fn stop(self) -> Result<(), TestError> {
        self.server.abort();
        self.engine.shutdown()?;
        Ok(())
    }
}

fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        listen: ListenConfig {
            grpc: SocketAddr::from(([127, 0, 0, 1], 0)),
            http: SocketAddr::from(([127, 0, 0, 1], 0)),
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
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: TENANT_A.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

async fn connect(address: SocketAddr, namespaces: &str) -> Result<ClientSocket, TestError> {
    let mut request = format!("ws://{address}/events/stream").into_client_request()?;
    request
        .headers_mut()
        .insert("x-aion-subject", "alice".parse()?);
    request
        .headers_mut()
        .insert("x-aion-namespaces", namespaces.parse()?);
    let (socket, _response) = connect_async(request).await?;
    Ok(socket)
}

async fn subscribe_per_workflow(
    socket: &mut ClientSocket,
    namespace: &str,
    workflow_id: &WorkflowId,
    resume_from_seq: Option<u64>,
) -> Result<(), TestError> {
    let mut per_workflow = json!({
        "namespace": namespace,
        "workflow_id": workflow_id.to_string(),
    });
    if let Some(cursor) = resume_from_seq {
        per_workflow["resume_from_seq"] = json!(cursor);
    }
    socket
        .send(Message::Text(
            json!({ "per_workflow": per_workflow }).to_string().into(),
        ))
        .await?;
    Ok(())
}

async fn next_text_frame(socket: &mut ClientSocket) -> Result<Option<String>, TestError> {
    loop {
        let frame = tokio::time::timeout(RECEIVE_TIMEOUT, socket.next()).await?;
        match frame {
            Some(Ok(Message::Text(text))) => return Ok(Some(text.to_string())),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(other)) => return Err(format!("unexpected frame {other:?}").into()),
            Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => {
                return Ok(None);
            }
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

async fn next_event_seq(socket: &mut ClientSocket) -> Result<u64, TestError> {
    let text = next_text_frame(socket)
        .await?
        .ok_or("stream closed before the expected event frame")?;
    let streamed: StreamedEvent = serde_json::from_str(&text)?;
    Ok(streamed.decode_event()?.seq())
}

fn started(seq: u64, workflow_id: &WorkflowId) -> Result<Event, TestError> {
    Ok(Event::WorkflowStarted {
        envelope: envelope(seq, workflow_id),
        workflow_type: "resume-fixture".to_owned(),
        input: Payload::from_json(&json!({ "seq": seq }))?,
        run_id: aion_core::RunId::new(uuid::Uuid::from_u128(u128::from(seq))),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    })
}

fn signal(seq: u64, workflow_id: &WorkflowId) -> Result<Event, TestError> {
    Ok(Event::SignalReceived {
        envelope: envelope(seq, workflow_id),
        name: format!("signal-{seq}"),
        payload: Payload::from_json(&json!({ "seq": seq }))?,
    })
}

fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: chrono::Utc::now(),
        workflow_id: workflow_id.clone(),
    }
}

#[tokio::test]
async fn resume_after_disconnect_delivers_contiguous_duplicate_free_events() -> Result<(), TestError>
{
    let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
    let ownership = StaticWorkflowNamespaces::default();
    ownership.record(workflow_id.clone(), TENANT_A)?;
    let server = StreamServer::start(ownership).await?;

    // Seed recorded history 1..=3 before the first connection.
    server
        .append(
            &workflow_id,
            &[
                started(1, &workflow_id)?,
                signal(2, &workflow_id)?,
                signal(3, &workflow_id)?,
            ],
        )
        .await?;

    // First connection: full replay from seq 1, then a live event.
    let mut first = connect(server.address, TENANT_A).await?;
    subscribe_per_workflow(&mut first, TENANT_A, &workflow_id, Some(1)).await?;
    let mut delivered = Vec::new();
    for _ in 0..3 {
        delivered.push(next_event_seq(&mut first).await?);
    }
    // The live tail was attached before the snapshot, so an append made after
    // the replay frames arrived is guaranteed to be delivered live.
    server
        .append(&workflow_id, &[signal(4, &workflow_id)?])
        .await?;
    delivered.push(next_event_seq(&mut first).await?);
    assert_eq!(delivered, vec![1, 2, 3, 4]);
    let last_seq = 4;
    drop(first);

    // Events recorded while disconnected.
    server
        .append(
            &workflow_id,
            &[signal(5, &workflow_id)?, signal(6, &workflow_id)?],
        )
        .await?;

    // Reconnect with resume_from_seq = last_seq + 1: replay 5..=6, then live 7.
    let mut second = connect(server.address, TENANT_A).await?;
    subscribe_per_workflow(&mut second, TENANT_A, &workflow_id, Some(last_seq + 1)).await?;
    for _ in 0..2 {
        delivered.push(next_event_seq(&mut second).await?);
    }
    server
        .append(&workflow_id, &[signal(7, &workflow_id)?])
        .await?;
    delivered.push(next_event_seq(&mut second).await?);

    assert_eq!(
        delivered,
        vec![1, 2, 3, 4, 5, 6, 7],
        "delivery spanning the disconnect must be contiguous with no duplicates"
    );
    drop(second);
    server.stop()?;
    Ok(())
}

#[tokio::test]
async fn cursor_beyond_history_head_is_terminal_invalid_input_then_close() -> Result<(), TestError>
{
    let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(2));
    let ownership = StaticWorkflowNamespaces::default();
    ownership.record(workflow_id.clone(), TENANT_A)?;
    let server = StreamServer::start(ownership).await?;
    server
        .append(
            &workflow_id,
            &[started(1, &workflow_id)?, signal(2, &workflow_id)?],
        )
        .await?;

    let mut socket = connect(server.address, TENANT_A).await?;
    // Head is 2; the largest valid cursor is 3.
    subscribe_per_workflow(&mut socket, TENANT_A, &workflow_id, Some(4)).await?;

    let text = next_text_frame(&mut socket)
        .await?
        .ok_or("expected a terminal error frame before close")?;
    let body: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(body["error"]["code"], json!("invalid_input"));
    assert_eq!(
        body["error"]["error_type"],
        json!("ResumeCursorAheadOfHistory")
    );

    // The error frame is followed by a close, never further data.
    assert_eq!(
        next_text_frame(&mut socket).await?,
        None,
        "socket must close after the terminal error frame"
    );
    server.stop()?;
    Ok(())
}

/// REVIEW RIDER 1 (end to end): the broadcast channel is engine-global and one
/// shared engine serves all tenants; a firehose authorized for tenant-a must
/// never receive — or be labeled with — tenant-b's events.
#[tokio::test]
async fn firehose_never_observes_foreign_namespace_events() -> Result<(), TestError> {
    let workflow_a = WorkflowId::new(uuid::Uuid::from_u128(3));
    let workflow_b = WorkflowId::new(uuid::Uuid::from_u128(4));
    let ownership = StaticWorkflowNamespaces::default();
    ownership.record(workflow_a.clone(), TENANT_A)?;
    ownership.record(workflow_b.clone(), TENANT_B)?;
    let server = StreamServer::start(ownership).await?;

    let mut socket = connect(server.address, TENANT_A).await?;
    socket
        .send(Message::Text(
            json!({ "firehose": { "namespace": TENANT_A } })
                .to_string()
                .into(),
        ))
        .await?;

    // Establish that the live subscription is attached: append tenant-a events
    // until one arrives (events appended before attach are live-only misses).
    let mut next_a_seq = 1_u64;
    let mut received = Vec::new();
    'attach: for _ in 0..100 {
        server
            .append(&workflow_a, &[event_for(next_a_seq, &workflow_a)?])
            .await?;
        next_a_seq += 1;
        loop {
            let frame = match tokio::time::timeout(Duration::from_millis(100), socket.next()).await
            {
                Ok(frame) => frame,
                Err(_elapsed) => break,
            };
            match frame {
                Some(Ok(Message::Text(text))) => {
                    received.push(text.to_string());
                    break 'attach;
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                other => return Err(format!("unexpected firehose frame: {other:?}").into()),
            }
        }
    }
    assert!(
        !received.is_empty(),
        "firehose subscription never delivered a tenant-a event"
    );

    // Interleave foreign events before a final own event. Broadcast order is
    // preserved, so receiving the final tenant-a event proves the tenant-b
    // events were filtered, not still in flight.
    server
        .append(
            &workflow_b,
            &[event_for(1, &workflow_b)?, event_for(2, &workflow_b)?],
        )
        .await?;
    server
        .append(&workflow_a, &[event_for(next_a_seq, &workflow_a)?])
        .await?;
    loop {
        let text = next_text_frame(&mut socket)
            .await?
            .ok_or("firehose closed before the final tenant-a event")?;
        let streamed: StreamedEvent = serde_json::from_str(&text)?;
        let event = streamed.decode_event()?;
        assert_eq!(
            event.workflow_id(),
            &workflow_a,
            "tenant-b events must never reach a tenant-a firehose"
        );
        assert_eq!(streamed.namespace, TENANT_A);
        received.push(text);
        if event.seq() == next_a_seq {
            break;
        }
    }

    // Every frame ever delivered on this socket belongs to tenant-a.
    for text in &received {
        let streamed: StreamedEvent = serde_json::from_str(text)?;
        assert_eq!(streamed.namespace, TENANT_A);
        assert_eq!(streamed.decode_event()?.workflow_id(), &workflow_a);
    }
    server.stop()?;
    Ok(())
}

fn event_for(seq: u64, workflow_id: &WorkflowId) -> Result<Event, TestError> {
    if seq == 1 {
        started(1, workflow_id)
    } else {
        signal(seq, workflow_id)
    }
}

fn typed_started(
    seq: u64,
    workflow_id: &WorkflowId,
    workflow_type: &str,
) -> Result<Event, TestError> {
    Ok(Event::WorkflowStarted {
        envelope: envelope(seq, workflow_id),
        workflow_type: workflow_type.to_owned(),
        input: Payload::from_json(&json!({ "seq": seq }))?,
        run_id: aion_core::RunId::new(uuid::Uuid::new_v4()),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    })
}

fn completed(seq: u64, workflow_id: &WorkflowId) -> Result<Event, TestError> {
    Ok(Event::WorkflowCompleted {
        envelope: envelope(seq, workflow_id),
        result: Payload::from_json(&json!({ "seq": seq }))?,
    })
}

/// Append `[started, completed]` batches to fresh checkout workflows until
/// the filtered subscriber delivers its first frame, proving the live
/// subscription is attached. Returns the frames received so far.
async fn attach_completed_checkouts(
    server: &StreamServer,
    socket: &mut ClientSocket,
    attach_workflows: &[WorkflowId],
) -> Result<Vec<String>, TestError> {
    let mut received = Vec::new();
    for workflow_id in attach_workflows {
        server
            .append(
                workflow_id,
                &[
                    typed_started(1, workflow_id, "checkout")?,
                    completed(2, workflow_id)?,
                ],
            )
            .await?;
        loop {
            let frame = match tokio::time::timeout(Duration::from_millis(100), socket.next()).await
            {
                Ok(frame) => frame,
                Err(_elapsed) => break,
            };
            match frame {
                Some(Ok(Message::Text(text))) => {
                    received.push(text.to_string());
                    return Ok(received);
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                other => return Err(format!("unexpected filtered frame: {other:?}").into()),
            }
        }
    }
    Ok(received)
}

/// FINDING M2 (end to end): `filtered` subscription selectors are enforced
/// server-side through the full wire path — a `workflow_type` + `status`
/// subscriber receives exactly the matching workflows' matching events, never
/// the whole namespace stream.
#[tokio::test]
async fn filtered_subscription_enforces_type_and_status_selectors() -> Result<(), TestError> {
    let fulfillment = WorkflowId::new(uuid::Uuid::from_u128(64));
    let ownership = StaticWorkflowNamespaces::default();
    ownership.record_with_type(fulfillment.clone(), TENANT_A, "fulfillment")?;
    // Fresh checkout-typed workflows minted on demand for attach + assertion.
    let mut next_checkout = 100_u128;
    let mut mint_checkout = || -> Result<WorkflowId, TestError> {
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(next_checkout));
        next_checkout += 1;
        ownership.record_with_type(workflow_id.clone(), TENANT_A, "checkout")?;
        Ok(workflow_id)
    };
    let mut attach_workflows = Vec::new();
    for _ in 0..100 {
        attach_workflows.push(mint_checkout()?);
    }
    let final_checkout = mint_checkout()?;
    let server = StreamServer::start(ownership).await?;

    let mut socket = connect(server.address, TENANT_A).await?;
    socket
        .send(Message::Text(
            json!({
                "filtered": {
                    "namespace": TENANT_A,
                    "workflow_type": "checkout",
                    "status": "Completed",
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    // Establish that the live subscription is attached: complete fresh
    // checkout workflows until one Completed frame arrives (events appended
    // before attach are live-only misses).
    let mut received = attach_completed_checkouts(&server, &mut socket, &attach_workflows).await?;
    assert!(
        !received.is_empty(),
        "filtered subscription never delivered a matching event"
    );

    // Interleave non-matching traffic before the matching terminal event:
    // a fulfillment workflow completing (wrong type) and the checkout
    // workflow's non-terminal events (wrong status). Broadcast order is
    // preserved, so receiving the final completed frame proves the
    // non-matching events were filtered, not still in flight.
    server
        .append(
            &fulfillment,
            &[
                typed_started(1, &fulfillment, "fulfillment")?,
                completed(2, &fulfillment)?,
            ],
        )
        .await?;
    server
        .append(
            &final_checkout,
            &[
                typed_started(1, &final_checkout, "checkout")?,
                signal(2, &final_checkout)?,
                completed(3, &final_checkout)?,
            ],
        )
        .await?;
    let text = next_text_frame(&mut socket)
        .await?
        .ok_or("filtered stream closed before the final matching event")?;
    let streamed: StreamedEvent = serde_json::from_str(&text)?;
    let event = streamed.decode_event()?;
    assert_eq!(event.workflow_id(), &final_checkout);
    assert_eq!(event.seq(), 3, "only the Completed event may be delivered");
    received.push(text);

    // Every frame ever delivered is a checkout workflow's Completed event.
    for text in &received {
        let streamed: StreamedEvent = serde_json::from_str(text)?;
        assert_eq!(streamed.namespace, TENANT_A);
        let event = streamed.decode_event()?;
        assert!(
            matches!(event, Event::WorkflowCompleted { .. }),
            "status selector must filter non-Completed events, got {event:?}"
        );
        assert_ne!(
            event.workflow_id(),
            &fulfillment,
            "type selector must filter foreign-typed workflows"
        );
    }
    server.stop()?;
    Ok(())
}
