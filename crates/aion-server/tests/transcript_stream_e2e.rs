//! NOI-5b end-to-end: the agent-observability transcript transport over the real
//! HTTP router.
//!
//! Proves the full transport seam the transcript subscription mounts on
//! `/events/stream`:
//!
//! - an `ActivityEvent` arriving at the server bridge (the same seam the
//!   worker->liminal->server ingestion path delivers to) is SEQUENCED by the
//!   `ActivityEventPublisher` (commit-allocated `store_seq`), PERSISTED to the
//!   observability store, and delivered LIVE to a connected transcript WS
//!   subscriber;
//! - a LATE subscriber resuming from a `store_seq` receives the durable tail with
//!   NO gap and NO duplicate at the splice seam, then tails new live events;
//! - the namespace gate denies a caller without a grant for the workflow's
//!   namespace with one terminal error frame (anti-leak, byte-identical to the
//!   workflow event path);
//! - ephemeral token deltas are forwarded live but never carry a `store_seq` and
//!   are never persisted (they do not appear in a fresh subscriber's durable
//!   replay).
//!
//! The durable O-vs-E replay-invisibility guarantee (an `O` record is never seen
//! by `read_history`) is proven at the store layer in
//! `aion-store-haematite/tests/observability.rs`; this test proves the *transport*
//! that carries those records to and from the socket.

use std::net::SocketAddr;
use std::time::Duration;

use aion_core::{ActivityEvent, ActivityEventKind, ActivityId, MessageRole, WorkflowId};
use aion_server::api::http::workflow_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::{
    NamespaceResolver, ServerState, StaticScheduleNamespaces, StaticWorkflowNamespaces,
};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use uuid::Uuid;

type TestError = Box<dyn std::error::Error>;
type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(2);

fn workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x5B))
}

const ACTIVITY_SEQ: u64 = 3;
const ATTEMPT: u32 = 0;

/// Build a non-ephemeral assistant message for the fixed `(workflow, activity,
/// attempt)` stream under test. `store_seq` is `None` on the producer side; the
/// server sequencer stamps it at commit.
fn message_event(worker_seq: u64, text: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: workflow_id(),
        activity_id: ActivityId::from_sequence_position(ACTIVITY_SEQ),
        attempt: ATTEMPT,
        agent_id: Uuid::from_u128(9),
        agent_role: "orchestrator".to_owned(),
        emitted_at: Utc::now(),
        worker_seq,
        store_seq: None,
        ephemeral: false,
        kind: ActivityEventKind::Message {
            role: MessageRole::Assistant,
            text: text.to_owned(),
        },
    }
}

/// Build an ephemeral token delta (WS-forward-only, never persisted).
fn delta_event(worker_seq: u64, fragment: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: workflow_id(),
        activity_id: ActivityId::from_sequence_position(ACTIVITY_SEQ),
        attempt: ATTEMPT,
        agent_id: Uuid::from_u128(9),
        agent_role: "orchestrator".to_owned(),
        emitted_at: Utc::now(),
        worker_seq,
        store_seq: None,
        ephemeral: true,
        kind: ActivityEventKind::Delta {
            message_id: "m1".to_owned(),
            text_fragment: fragment.to_owned(),
        },
    }
}

struct TranscriptServer {
    address: SocketAddr,
    state: ServerState,
    server: tokio::task::JoinHandle<()>,
}

impl TranscriptServer {
    async fn start() -> Result<Self, TestError> {
        // Attribute the workflow under test to TENANT_A so the per-workflow
        // namespace gate the transcript socket reuses can authorize it; a caller
        // for TENANT_B is denied the same way the workflow event stream denies.
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), TENANT_A)?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config());
        let router = workflow_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "transcript test server exited with error");
            }
        });
        Ok(Self {
            address,
            state,
            server,
        })
    }

    /// Emit an event at the server bridge exactly as the worker->liminal->server
    /// ingestion seam does: publish through the SAME `ActivityEventPublisher` the
    /// transcript socket subscribes to. Returns the commit-allocated `store_seq`
    /// (or `None` for an ephemeral event).
    async fn emit(&self, event: &ActivityEvent) -> Result<Option<u64>, TestError> {
        Ok(self.state.transcript_publisher().publish(event).await?)
    }

    fn stop(self) {
        self.server.abort();
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
            heartbeat_window: Duration::from_secs(30),
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
        observability: aion_server::config::ObservabilityConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_secs(10)),
        default_namespace: TENANT_A.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
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

/// Send the transcript subscription frame for the fixed stream, optionally with a
/// resume cursor.
async fn subscribe_transcript(
    socket: &mut ClientSocket,
    namespace: &str,
    after_seq: Option<u64>,
) -> Result<(), TestError> {
    let mut transcript = json!({
        "namespace": namespace,
        "workflow_id": workflow_id().to_string(),
        "activity_id": ACTIVITY_SEQ,
        "attempt": ATTEMPT,
    });
    if let Some(cursor) = after_seq {
        transcript["after_seq"] = json!(cursor);
    }
    socket
        .send(Message::Text(
            json!({ "transcript": transcript }).to_string().into(),
        ))
        .await?;
    Ok(())
}

/// Await the next transcript event frame, decoding the `activity_event` envelope.
async fn next_activity_event(socket: &mut ClientSocket) -> Result<ActivityEvent, TestError> {
    let frame = tokio::time::timeout(RECEIVE_TIMEOUT, socket.next())
        .await
        .map_err(|_| "timed out waiting for a transcript frame")?
        .ok_or("socket closed before a transcript frame")??;
    let Message::Text(text) = frame else {
        return Err(format!("expected a text transcript frame, got {frame:?}").into());
    };
    let body: serde_json::Value = serde_json::from_str(&text)?;
    if body.get("error").is_some() {
        return Err(format!("expected a transcript event, got an error frame: {body}").into());
    }
    assert_eq!(
        body["kind"], "activity_event",
        "transcript frames must be tagged activity_event: {body}"
    );
    Ok(serde_json::from_value(body["event"].clone())?)
}

/// A live subscriber sees a persisted event delivered with its commit-allocated
/// `store_seq`, AND an ephemeral delta forwarded live with no `store_seq`.
#[tokio::test]
async fn live_subscriber_receives_sequenced_events_and_ephemeral_deltas() -> Result<(), TestError> {
    let server = TranscriptServer::start().await?;
    let mut socket = connect(server.address, TENANT_A).await?;
    subscribe_transcript(&mut socket, TENANT_A, None).await?;
    // Give the socket time to attach its live subscription before emitting.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Ephemeral delta first (forwarded live, store_seq None), then a persisted
    // message (store_seq Some(0)).
    assert_eq!(server.emit(&delta_event(1, "wor")).await?, None);
    assert_eq!(server.emit(&message_event(2, "word")).await?, Some(0));

    let first = next_activity_event(&mut socket).await?;
    assert!(first.ephemeral, "the ephemeral delta must arrive live");
    assert_eq!(
        first.store_seq, None,
        "an ephemeral event carries no store_seq"
    );

    let second = next_activity_event(&mut socket).await?;
    assert!(!second.ephemeral);
    assert_eq!(
        second.store_seq,
        Some(0),
        "the persisted message carries the commit-allocated store_seq"
    );
    assert!(matches!(
        second.kind,
        ActivityEventKind::Message { text, .. } if text == "word"
    ));

    server.stop();
    Ok(())
}

/// The load-bearing NOI-5b proof: an event emitted at the bridge is sequenced +
/// persisted, a LATE subscriber resuming from a `store_seq` gets the durable tail
/// with NO gap and NO duplicate, then splices onto new live events. The ephemeral
/// delta emitted earlier is NOT in the durable replay (never persisted).
#[tokio::test]
async fn late_subscriber_resumes_by_store_seq_with_no_gap_or_duplicate() -> Result<(), TestError> {
    let server = TranscriptServer::start().await?;

    // The activity emitted an ephemeral delta then three persisted messages while
    // no one (or an earlier, now-dropped client) was watching.
    assert_eq!(server.emit(&delta_event(1, "frag")).await?, None);
    assert_eq!(server.emit(&message_event(2, "m-0")).await?, Some(0));
    assert_eq!(server.emit(&message_event(3, "m-1")).await?, Some(1));
    assert_eq!(server.emit(&message_event(4, "m-2")).await?, Some(2));

    // A client reconnects having last applied store_seq 1: it must receive ONLY
    // the durable tail strictly after the cursor (store_seq 2), with no gap and no
    // re-delivery of <= 1, and NOT the ephemeral delta (never persisted).
    let mut socket = connect(server.address, TENANT_A).await?;
    subscribe_transcript(&mut socket, TENANT_A, Some(1)).await?;

    let resumed = next_activity_event(&mut socket).await?;
    assert_eq!(
        resumed.store_seq,
        Some(2),
        "resume from store_seq 1 replays exactly the missed durable record (store_seq 2)"
    );
    assert!(matches!(
        resumed.kind,
        ActivityEventKind::Message { text, .. } if text == "m-2"
    ));

    // Now the activity keeps emitting: the splice yields store_seq 3 then 4 live,
    // contiguous with the replayed tail — no gap, no duplicate at the seam.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.emit(&message_event(5, "m-3")).await?, Some(3));
    assert_eq!(server.emit(&message_event(6, "m-4")).await?, Some(4));

    let live_3 = next_activity_event(&mut socket).await?;
    assert_eq!(live_3.store_seq, Some(3), "first live event after the seam");
    let live_4 = next_activity_event(&mut socket).await?;
    assert_eq!(
        live_4.store_seq,
        Some(4),
        "contiguous, no gap, no duplicate"
    );

    server.stop();
    Ok(())
}

/// A FRESH subscriber (no resume cursor) replays the full durable transcript from
/// `store_seq` 0 — and never the ephemeral deltas that were emitted between the
/// persisted messages.
#[tokio::test]
async fn fresh_subscriber_replays_full_durable_transcript_without_ephemerals()
-> Result<(), TestError> {
    let server = TranscriptServer::start().await?;
    server.emit(&message_event(1, "a")).await?;
    server.emit(&delta_event(2, "eph")).await?;
    server.emit(&message_event(3, "b")).await?;

    let mut socket = connect(server.address, TENANT_A).await?;
    subscribe_transcript(&mut socket, TENANT_A, None).await?;

    let first = next_activity_event(&mut socket).await?;
    assert_eq!(first.store_seq, Some(0));
    assert!(matches!(
        first.kind,
        ActivityEventKind::Message { text, .. } if text == "a"
    ));
    let second = next_activity_event(&mut socket).await?;
    assert_eq!(
        second.store_seq,
        Some(1),
        "the ephemeral delta is not persisted, so the durable replay is contiguous 0,1"
    );
    assert!(matches!(
        second.kind,
        ActivityEventKind::Message { text, .. } if text == "b"
    ));

    server.stop();
    Ok(())
}

/// Anti-leak: a caller WITHOUT a grant for the workflow's namespace is denied with
/// one terminal error frame then close — never a transcript, never existence. The
/// gate is byte-identical to the workflow event stream's per-workflow gate.
#[tokio::test]
async fn transcript_denies_caller_without_workflow_namespace_grant() -> Result<(), TestError> {
    let server = TranscriptServer::start().await?;
    // Caller holds TENANT_B; the workflow is attributed to TENANT_A.
    let mut socket = connect(server.address, TENANT_B).await?;
    subscribe_transcript(&mut socket, TENANT_B, None).await?;

    let frame = tokio::time::timeout(RECEIVE_TIMEOUT, socket.next())
        .await
        .map_err(|_| "timed out waiting for the denial frame")?
        .ok_or("socket closed without a terminal error frame")??;
    let Message::Text(text) = frame else {
        return Err(format!("expected a terminal error frame, got {frame:?}").into());
    };
    let body: serde_json::Value = serde_json::from_str(&text)?;
    assert!(
        body["error"].is_object(),
        "an unauthorized transcript subscription must receive a terminal error frame: {body}"
    );

    server.stop();
    Ok(())
}
