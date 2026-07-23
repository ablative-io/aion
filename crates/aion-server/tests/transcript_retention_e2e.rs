//! Lane #229 acceptance: durable transcript retention read back over the REST
//! pair, with every live subscriber gone.
//!
//! The defect this lane closes is "transcripts evaporate unwatched". The
//! acceptance case is therefore executed literally: events are published at
//! the worker->server ingress seam (the same `ActivityEventPublisher` the
//! liminal tap feeds), ALL live subscribers are dropped, and the full ordered
//! transcript is then fetched over plain HTTP (`POST /workflows/transcript`) —
//! no socket, no subscriber, store-backed and time-independent. Enumeration
//! (`POST /workflows/transcripts`), the empty honest answer for a run with no
//! retained transcript, the anti-leak namespace gate, the `[observability]`
//! bounds plumbed from `RuntimeConfig` to the wire, and mid-stream resume
//! (`from_seq`) complete the surface.

use std::time::Duration;

use aion_core::{ActivityEvent, ActivityEventKind, ActivityId, MessageRole, WorkflowId};
use aion_server::api::http::workflow_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, ObservabilityConfig, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig,
    WebSocketConfig, WorkerConfig,
};
use aion_server::{
    NamespaceResolver, ServerState, StaticScheduleNamespaces, StaticWorkflowNamespaces,
};
use aion_store::ActivityStreamKey;
use axum::body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

type TestError = Box<dyn std::error::Error>;

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";
const ACTIVITY_SEQ: u64 = 3;
const ATTEMPT: u32 = 0;

fn workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x229))
}

/// A second workflow, also tenant-a, that never records a transcript.
fn quiet_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x230))
}

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

fn delta_event(worker_seq: u64, fragment: &str) -> ActivityEvent {
    ActivityEvent {
        ephemeral: true,
        kind: ActivityEventKind::Delta {
            message_id: "m1".to_owned(),
            text_fragment: fragment.to_owned(),
        },
        ..message_event(worker_seq, "")
    }
}

fn stream_key() -> ActivityStreamKey {
    ActivityStreamKey::new(
        workflow_id(),
        ActivityId::from_sequence_position(ACTIVITY_SEQ),
        ATTEMPT,
    )
}

fn runtime_config(observability: ObservabilityConfig) -> RuntimeConfig {
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
        observability,
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

/// The state + real axum router under test. Requests are driven with
/// `tower::ServiceExt::oneshot` — the full HTTP stack, no TCP listener.
struct RetentionServer {
    state: ServerState,
    router: axum::Router,
}

impl RetentionServer {
    fn start(observability: ObservabilityConfig) -> Result<Self, TestError> {
        // Attribute both workflows to TENANT_A so the per-workflow gate can
        // authorize them; a TENANT_B request is denied anti-leak.
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), TENANT_A)?;
        ownership.record(quiet_workflow_id(), TENANT_A)?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config(observability));
        let router = workflow_router(state.clone());
        Ok(Self { state, router })
    }

    /// Publish at the exact worker->server ingress seam (the liminal tap
    /// publishes through this same `ActivityEventPublisher`).
    async fn emit(&self, event: &ActivityEvent) -> Result<Option<u64>, TestError> {
        Ok(self.state.transcript_publisher().publish(event).await?)
    }

    /// `POST` a JSON body with the development caller headers and return the
    /// status + decoded JSON body.
    async fn post(
        &self,
        path: &str,
        namespaces: &str,
        request_body: &serde_json::Value,
    ) -> Result<(StatusCode, serde_json::Value), TestError> {
        let request = Request::builder()
            .uri(path)
            .method("POST")
            .header("content-type", "application/json")
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", namespaces)
            .body(body::Body::from(serde_json::to_vec(request_body)?))?;
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
        let value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, value))
    }
}

fn fetch_body(namespace: &str, workflow: &WorkflowId, from_seq: Option<u64>) -> serde_json::Value {
    let mut request_body = json!({
        "namespace": namespace,
        "workflow_id": workflow,
        "activity_id": ACTIVITY_SEQ,
        "attempt": ATTEMPT,
    });
    if let Some(from_seq) = from_seq {
        request_body["from_seq"] = json!(from_seq);
    }
    request_body
}

fn streams_body(namespace: &str, workflow: &WorkflowId) -> serde_json::Value {
    json!({ "namespace": namespace, "workflow_id": workflow })
}

/// Extract the `(store_seq, text)` view of a fetched events array.
fn seq_and_text(events: &serde_json::Value) -> Result<Vec<(u64, String)>, TestError> {
    events
        .as_array()
        .ok_or("events must be an array")?
        .iter()
        .map(|event| {
            let seq = event["store_seq"]
                .as_u64()
                .ok_or("every fetched event carries its store_seq")?;
            let text = event["kind"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            Ok((seq, text))
        })
        .collect()
}

/// THE ACCEPTANCE CASE: write chunks, drop ALL subscribers, fetch later over
/// plain HTTP — the full ordered transcript comes back. Fetching with zero
/// subscribers IS the "open it an hour later" proof: retention is
/// store-backed, subscriber- and time-independent.
#[tokio::test]
async fn retained_transcript_is_fetchable_in_order_after_all_subscribers_drop()
-> Result<(), TestError> {
    let server = RetentionServer::start(ObservabilityConfig::default())?;

    // A live subscriber watches the start of the run...
    let live = server
        .state
        .transcript_publisher()
        .subscribe(stream_key(), None);
    assert_eq!(server.emit(&message_event(1, "m0")).await?, Some(0));
    assert_eq!(server.emit(&message_event(2, "m1")).await?, Some(1));
    // ...an ephemeral delta flows live-only, never retained...
    assert_eq!(server.emit(&delta_event(3, "frag")).await?, None);
    assert_eq!(server.emit(&message_event(4, "m2")).await?, Some(2));
    // ...then EVERY subscriber goes away (the console tab closes).
    drop(live);
    assert_eq!(server.emit(&message_event(5, "m3")).await?, Some(3));
    assert_eq!(server.emit(&message_event(6, "m4")).await?, Some(4));

    // Later, with no subscriber anywhere: fetch over plain HTTP.
    let (status, fetched) = server
        .post(
            "/workflows/transcript",
            TENANT_A,
            &fetch_body(TENANT_A, &workflow_id(), None),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let events = seq_and_text(&fetched["events"])?;
    assert_eq!(
        events,
        vec![
            (0, "m0".to_owned()),
            (1, "m1".to_owned()),
            (2, "m2".to_owned()),
            (3, "m3".to_owned()),
            (4, "m4".to_owned()),
        ],
        "the full ordered transcript, subscriber-independent"
    );
    // No ephemeral delta was retained.
    for event in fetched["events"].as_array().ok_or("events array")? {
        assert_ne!(event["kind"]["kind"], json!("Delta"));
        assert_eq!(event["ephemeral"], json!(false));
    }

    // Enumeration names the one retained stream with its head.
    let (status, streams) = server
        .post(
            "/workflows/transcripts",
            TENANT_A,
            &streams_body(TENANT_A, &workflow_id()),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        streams["streams"],
        json!([{ "activity_id": ACTIVITY_SEQ, "attempt": ATTEMPT, "head": 5 }])
    );
    Ok(())
}

/// A run with no retained transcript reads EMPTY on both endpoints — the
/// honest answer for a pre-retention run, never an error.
#[tokio::test]
async fn a_run_with_no_retained_transcript_reads_empty() -> Result<(), TestError> {
    let server = RetentionServer::start(ObservabilityConfig::default())?;

    let (status, streams) = server
        .post(
            "/workflows/transcripts",
            TENANT_A,
            &streams_body(TENANT_A, &quiet_workflow_id()),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(streams, json!({ "streams": [] }));

    let (status, fetched) = server
        .post(
            "/workflows/transcript",
            TENANT_A,
            &fetch_body(TENANT_A, &quiet_workflow_id(), None),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, json!({ "events": [] }));
    Ok(())
}

/// Anti-leak: a caller scoped to a foreign namespace is denied with a wire
/// error that leaks neither events nor stream existence — parity with the
/// transcript WS subscription's gate.
#[tokio::test]
async fn transcript_fetch_denies_a_foreign_namespace_caller() -> Result<(), TestError> {
    let server = RetentionServer::start(ObservabilityConfig::default())?;
    server.emit(&message_event(1, "secret")).await?;

    for (path, request_body) in [
        (
            "/workflows/transcript",
            fetch_body(TENANT_B, &workflow_id(), None),
        ),
        (
            "/workflows/transcripts",
            streams_body(TENANT_B, &workflow_id()),
        ),
    ] {
        let (status, denied) = server.post(path, TENANT_B, &request_body).await?;
        assert_ne!(
            status,
            StatusCode::OK,
            "{path}: a foreign-namespace caller must be denied"
        );
        assert!(
            denied.get("events").is_none() && denied.get("streams").is_none(),
            "{path}: the denial must leak nothing: {denied}"
        );
        assert!(
            !denied.to_string().contains("secret"),
            "{path}: the denial must not carry transcript content"
        );
    }
    Ok(())
}

/// Config plumbing end-to-end: `[observability]` bounds set on `RuntimeConfig`
/// reach the publisher — the oversized event is truncated, the stream caps
/// with one marker, and the enumeration head reflects exactly what was kept.
#[tokio::test]
async fn configured_bounds_flow_from_runtime_config_to_the_wire() -> Result<(), TestError> {
    let server = RetentionServer::start(ObservabilityConfig {
        max_event_bytes: 512,
        max_stream_events: 3,
    })?;

    let huge = "x".repeat(10_000);
    assert_eq!(server.emit(&message_event(1, &huge)).await?, Some(0));
    for worker_seq in 2..7u64 {
        server.emit(&message_event(worker_seq, "normal")).await?;
    }

    let (status, fetched) = server
        .post(
            "/workflows/transcript",
            TENANT_A,
            &fetch_body(TENANT_A, &workflow_id(), None),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let events = fetched["events"].as_array().ok_or("events array")?;
    assert_eq!(events.len(), 4, "3 capped records + the one marker");
    let truncated = events[0]["kind"]["text"]
        .as_str()
        .ok_or("seq 0 is a message")?;
    assert!(
        truncated.ends_with("bytes by observability.max_event_bytes]"),
        "seq 0 carries the truncation marker: {truncated}"
    );
    assert!(!truncated.contains(&huge), "the oversized text is bounded");
    assert_eq!(events[1]["kind"]["text"], json!("normal"));
    assert_eq!(events[2]["kind"]["text"], json!("normal"));
    let marker = events[3]["kind"]["detail"]["text"]
        .as_str()
        .ok_or("seq 3 is the cap marker note")?;
    assert!(
        marker.contains("retention cap"),
        "the marker names the cap: {marker}"
    );
    assert_eq!(events[3]["store_seq"], json!(3), "nothing persisted after");

    let (status, streams) = server
        .post(
            "/workflows/transcripts",
            TENANT_A,
            &streams_body(TENANT_A, &workflow_id()),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        streams["streams"][0]["head"],
        json!(4),
        "the head counts the capped records + marker"
    );
    Ok(())
}

/// The REST twin of resume-by-`store_seq`: `from_seq` reads exactly the tail.
#[tokio::test]
async fn fetch_from_seq_resumes_mid_stream() -> Result<(), TestError> {
    let server = RetentionServer::start(ObservabilityConfig::default())?;
    for worker_seq in 0..5u64 {
        server
            .emit(&message_event(worker_seq, &format!("m{worker_seq}")))
            .await?;
    }

    let (status, fetched) = server
        .post(
            "/workflows/transcript",
            TENANT_A,
            &fetch_body(TENANT_A, &workflow_id(), Some(3)),
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let events = seq_and_text(&fetched["events"])?;
    assert_eq!(
        events,
        vec![(3, "m3".to_owned()), (4, "m4".to_owned())],
        "from_seq reads exactly the tail — no gap, no duplicate"
    );
    Ok(())
}
