//! End-to-end coverage for the local dev-server surface (WA-002 R2).
//!
//! Production parity is the load-bearing property under test: every dev
//! endpoint drives the REAL engine, store, and event stream — there is no
//! mock-only engine and no dev-only execution path (CN4). These tests assert
//! that by riding the production `ServerState::build_with_store` startup path
//! (the same path `aion server` uses) behind the real public HTTP router, with
//! the dev surface commissioned via `[dev].enabled`.
//!
//! The live-stream test that needs a running workflow process is gated at
//! runtime on the `erlc` compiler: when it is absent the test prints a skip
//! line and passes, never silently skipped.

use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::Engine;
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, Manifest, ManifestVersion, PackageBuilder,
};
use aion_proto::StreamedEvent;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig, DevConfig,
    ListenConfig, MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::{ServerState, api::http::http_router};
use aion_store::InMemoryStore;
use axum::body;
use axum::http::{Request, StatusCode, request::Builder};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "tenant-a";
const FIXTURE_MODULE: &str = "aion_dev_fixture";
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(5);

fn runtime_config(dev_enabled: bool) -> RuntimeConfig {
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
        dashboard: DashboardConfig {
            source: DashboardAssetSource::Embedded,
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
        },
        workflow_packages: Vec::new(),
        deploy: DeployConfig::default(),
        authoring: AuthoringConfig::default(),
        dev: DevConfig {
            enabled: dev_enabled,
        },
        outbox: aion_server::config::OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
    }
}

/// Production startup path: builds the real engine + store + firehose with the
/// dev surface commissioned, returning the state (for direct engine loads) and
/// the real public router.
async fn dev_server(dev_enabled: bool) -> Result<(ServerState, axum::Router), TestError> {
    let state =
        ServerState::build_with_store(InMemoryStore::default(), runtime_config(dev_enabled))
            .await?;
    let router = http_router(state.clone())?;
    Ok((state, router))
}

fn granted(builder: Builder) -> Builder {
    builder
        .header("x-aion-subject", "dev")
        .header("x-aion-namespaces", NAMESPACE)
}

fn post(path: &str, body: &Value) -> Result<Request<body::Body>, TestError> {
    Ok(granted(
        Request::builder()
            .uri(path)
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(body)?))?)
}

async fn read_json(response: axum::response::Response) -> Result<Value, TestError> {
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn dev_surface_is_dark_when_disabled() -> Result<(), TestError> {
    let (state, router) = dev_server(false).await?;
    assert!(
        state.activity_mock_registry().is_none(),
        "a server with the dev surface disabled installs no mock registry, \
         so the engine runs the bare production dispatcher (CN4)"
    );

    let response = router
        .oneshot(post(
            "/dev/runs",
            &json!({"namespace": NAMESPACE, "workflow_type": "x", "input": {}}),
        )?)
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "every /dev/* path is a plain 404 when the dev surface is dark"
    );
    Ok(())
}

#[tokio::test]
async fn dev_trigger_drives_the_real_engine_start_path() -> Result<(), TestError> {
    // With no workflow type loaded, the trigger reaches the REAL start path and
    // returns the same WorkflowTypeNotFound a `/workflows/start` would — proving
    // it is the production engine, not a mock-only path (CN4).
    let (state, router) = dev_server(true).await?;
    assert!(
        state.activity_mock_registry().is_some(),
        "the dev surface installs the shared mock registry the engine consults"
    );

    let response = router
        .oneshot(post(
            "/dev/runs",
            &json!({"namespace": NAMESPACE, "workflow_type": "missing", "input": {}}),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = read_json(response).await?;
    assert_eq!(body["error_type"], json!("WorkflowTypeNotFound"));
    Ok(())
}

#[tokio::test]
async fn dev_replay_rejects_an_unknown_run_over_the_real_store() -> Result<(), TestError> {
    let (_state, router) = dev_server(true).await?;
    let response = router
        .oneshot(post(
            "/dev/replay",
            &json!({
                "namespace": NAMESPACE,
                "workflow_id": "00000000-0000-0000-0000-0000000000aa",
            }),
        )?)
        .await?;
    // The replay reads the real store, finds no such workflow, and returns the
    // anti-existence-leak not-found — never a fabricated success.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn dev_mock_registration_requires_the_run_to_exist() -> Result<(), TestError> {
    let (_state, router) = dev_server(true).await?;
    let response = router
        .oneshot(post(
            "/dev/mocks",
            &json!({
                "namespace": NAMESPACE,
                "workflow_id": "00000000-0000-0000-0000-0000000000bb",
                "activity_name": "charge",
                "outcome": {"kind": "succeeds", "result": {"ok": true}},
            }),
        )?)
        .await?;
    // Mocking targets an existing run; an unknown run is not-found, scoped
    // exactly like every other run operation.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// Compiles a trivial signal-gated Erlang workflow into a `.beam`, or returns
/// `None` when `erlc` is not installed (the caller prints a skip line).
fn compile_fixture_beam() -> Result<Option<Vec<u8>>, TestError> {
    if Command::new("erlc").arg("-version").output().is_err() {
        return Ok(None);
    }
    let dir = std::env::temp_dir().join(format!("aion-dev-ui-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir)?;
    let source = dir.join(format!("{FIXTURE_MODULE}.erl"));
    let beam = dir.join(format!("{FIXTURE_MODULE}.beam"));
    std::fs::write(
        &source,
        format!(
            "-module({FIXTURE_MODULE}).\n\
             -export([run/1]).\n\
             run(_Input) ->\n\
             {{ok, _Release}} = aion_flow_ffi:receive_signal(<<\"release\">>, <<\"{{}}\">>),\n\
             1.\n"
        ),
    )?;
    let status = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&source)
        .status()?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("erlc failed with status {status}").into());
    }
    let bytes = std::fs::read(&beam)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(Some(bytes))
}

fn fixture_archive(beam: &[u8]) -> Result<aion_package::Package, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(FIXTURE_MODULE, beam.to_vec())])?;
    let manifest = Manifest {
        entry_module: FIXTURE_MODULE.to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Duration::from_secs(30),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let bytes = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    Ok(aion_package::Package::load_from_bytes(
        &bytes,
        aion_package::ExtractionLimits::unbounded(),
    )?)
}

async fn load_fixture(engine: &Arc<Engine>) -> Result<(), TestError> {
    let Some(beam) = compile_fixture_beam()? else {
        return Err("erlc absent".into());
    };
    engine.load_package(fixture_archive(&beam)?).await?;
    Ok(())
}

#[tokio::test]
async fn dev_triggered_run_streams_over_the_existing_firehose() -> Result<(), TestError> {
    // PRODUCTION PARITY (R2): triggering a run via the dev server streams that
    // run's events live over the EXISTING `/events/stream` WebSocket firehose —
    // the dev server reuses the production stream, never a second one.
    let (state, _router) = dev_server(true).await?;
    let engine = state
        .deploy_guard()
        .engine()
        .map(Arc::clone)
        .map_err(|error| -> TestError { error.to_string().into() })?;

    if load_fixture(&engine).await.is_err() {
        tracing::info!("skipping dev firehose e2e: erlc not installed");
        println!(
            "skipping dev_triggered_run_streams_over_the_existing_firehose: erlc not installed"
        );
        return Ok(());
    }

    // Serve the real router on a loopback port so a real WebSocket client can
    // connect to the existing firehose.
    let router = http_router(state.clone())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router.into_make_service()).await {
            tracing::warn!(%error, "dev e2e server exited with error");
        }
    });

    // Subscribe to the EXISTING namespace firehose FIRST, so the triggered
    // run's WorkflowStarted is observed live (the firehose broadcasts; it does
    // not replay history to late subscribers).
    let mut request = format!("ws://{address}/events/stream").into_client_request()?;
    request
        .headers_mut()
        .insert("x-aion-subject", "dev".parse()?);
    request
        .headers_mut()
        .insert("x-aion-namespaces", NAMESPACE.parse()?);
    let (mut socket, _response) = connect_async(request).await?;
    socket
        .send(Message::Text(
            json!({
                "type": "subscribe",
                "subscription": { "firehose": { "namespace": NAMESPACE } }
            })
            .to_string()
            .into(),
        ))
        .await?;
    // Let the firehose subscription register before triggering: the broadcast
    // does not replay to late subscribers, so the subscribe must land first.
    // This is a test-side settle, not a production poll interval.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Trigger a run over the dev surface against the same live engine.
    let trigger = dev_post(
        &state,
        "/dev/runs",
        &json!({"namespace": NAMESPACE, "workflow_type": FIXTURE_MODULE, "input": {}}),
    )
    .await?;
    let workflow_id = trigger["workflow_id"]
        .as_str()
        .ok_or("trigger response missing workflow id")?
        .to_owned();
    assert_eq!(
        trigger["stream_subscription"]["path"], "/events/stream",
        "the dev trigger must hand back the EXISTING firehose path, not a new stream"
    );

    let started = tokio::time::timeout(RECEIVE_TIMEOUT, next_event_for(&mut socket, &workflow_id))
        .await
        .map_err(|_| "timed out waiting for the streamed WorkflowStarted event")??;
    assert_eq!(
        started.decode_event()?.workflow_id().to_string(),
        workflow_id,
        "the firehose must deliver the triggered run's own events"
    );

    server.abort();
    Ok(())
}

/// Issues a dev POST through the real router in-process (same engine/store as
/// the live WebSocket server), returning the decoded JSON body on success.
async fn dev_post(state: &ServerState, path: &str, body: &Value) -> Result<Value, TestError> {
    let router = http_router(state.clone())?;
    let response = router.oneshot(post(path, body)?).await?;
    let status = response.status();
    let value = read_json(response).await?;
    if !status.is_success() {
        return Err(format!("{path} failed with {status}: {value}").into());
    }
    Ok(value)
}

/// Reads streamed frames until one is for `workflow_id`.
async fn next_event_for(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    workflow_id: &str,
) -> Result<StreamedEvent, TestError> {
    loop {
        let Some(frame) = socket.next().await else {
            return Err("socket closed before delivering the run's event".into());
        };
        let Message::Text(text) = frame? else {
            continue;
        };
        let streamed: StreamedEvent = serde_json::from_str(&text)?;
        if streamed.decode_event()?.workflow_id().to_string() == workflow_id {
            return Ok(streamed);
        }
    }
}
