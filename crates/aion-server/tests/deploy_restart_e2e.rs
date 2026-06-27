//! Deploy-durability restart e2e over the public HTTP transport: a package
//! deployed at runtime through `POST /deploy/packages` must survive a full
//! server restart (fresh store handle, engine, resolver, router over the
//! same libSQL file) WITHOUT `--workflow-package`, so a mid-flight run
//! recovers, its remaining signal lands over the API, and it completes.
//!
//! This is the server-level proof of the P0 fix: before package persistence,
//! the restarted server's recovery skipped the run ("pinned to package
//! version ... which is not loaded") and the post-restart signal failed with
//! `WorkflowNotFound`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ExtractionLimits, Manifest, ManifestVersion,
    Package, PackageBuilder,
};
use aion_proto::{
    ProtoListVersionsResponse, ProtoLoadPackageResponse, ProtoSignalRequest, WireErrorCode,
};
use aion_server::api::http::http_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
    MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use aion_server::{NamespaceResolver, ServerState};
use aion_store::EventStore;
use aion_store_libsql::LibSqlStore;
use axum::{body, http::Request, http::StatusCode, response::Response};
use serde_json::json;
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const RELOAD_MODULE: &str = "aion_reload_fixture";
const NAMESPACE: &str = "default";
const MAX_ARCHIVE_BYTES: u64 = 1_048_576;
const MAX_INFLATED_BYTES: u64 = 2_097_152;

/// Compiles the reload fixture whose `gated/1` entry completes with
/// `version` only after the durable signals `step` then `release` — the
/// mid-flight suspension shape of the restart scenario.
fn compile_gated_beam(version: u32) -> Result<Vec<u8>, TestError> {
    let temp_dir =
        std::env::temp_dir().join(format!("aion-deploy-restart-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{RELOAD_MODULE}.erl"));
    let beam_path = temp_dir.join(format!("{RELOAD_MODULE}.beam"));
    std::fs::write(
        &source_path,
        format!(
            "-module({RELOAD_MODULE}).\n\
             -export([gated/1]).\n\
             gated(_Input) ->\n\
             {{ok, _Step}} = aion_flow_ffi:receive_signal(<<\"step\">>, <<\"{{}}\">>),\n\
             {{ok, _Release}} = aion_flow_ffi:receive_signal(<<\"release\">>, <<\"{{}}\">>),\n\
             {version}.\n"
        ),
    )?;
    let status = Command::new("erlc")
        .arg("-o")
        .arg(&temp_dir)
        .arg(&source_path)
        .status()?;
    if !status.success() {
        let cleanup = std::fs::remove_dir_all(&temp_dir);
        drop(cleanup);
        return Err(format!("erlc failed with status {status}").into());
    }
    let bytes = std::fs::read(beam_path)?;
    std::fs::remove_dir_all(temp_dir)?;
    Ok(bytes)
}

fn archive_bytes(beam: &[u8]) -> Result<Vec<u8>, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(RELOAD_MODULE, beam.to_vec())])?;
    let manifest = Manifest {
        entry_module: RELOAD_MODULE.to_owned(),
        entry_function: "gated".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Duration::from_secs(30),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    Ok(PackageBuilder::new(manifest, beams).write_to_bytes()?)
}

fn runtime_config() -> RuntimeConfig {
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
        deploy: DeployConfig {
            enabled: true,
            max_archive_bytes: Some(MAX_ARCHIVE_BYTES),
            max_inflated_bytes: Some(MAX_INFLATED_BYTES),
        },
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
        owned_shards: Vec::new(),
    }
}

/// One in-process "server" epoch: a real engine over the shared durable
/// store (NO workflow packages — the deploy API is the only package source)
/// plus the production resolver wiring and the public HTTP router.
async fn server_over(store: Arc<dyn EventStore>) -> Result<(Arc<Engine>, axum::Router), TestError> {
    let mut search_attribute_schema = aion_core::SearchAttributeSchema::new();
    search_attribute_schema.register(
        aion_server::NAMESPACE_ATTRIBUTE,
        aion_core::SearchAttributeType::String,
    )?;
    let engine = Arc::new(
        EngineBuilder::new()
            .store_arc(store)
            .in_memory_visibility()
            .search_attribute_schema(search_attribute_schema)
            .scheduler_threads(1)
            .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
                Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
            })
            .build()
            .await?,
    );
    let resolver = NamespaceResolver::from_config(
        NamespaceConfig {
            mode: NamespaceMode::SharedEngine,
        },
        Arc::clone(&engine),
    );
    let state = ServerState::from_parts(resolver, runtime_config());
    Ok((engine, http_router(state)?))
}

fn granted_headers(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder
        .header("x-aion-subject", "ci")
        .header("x-aion-namespaces", NAMESPACE)
        .header("x-aion-deploy", "true")
}

fn post_archive(archive: Vec<u8>) -> Result<Request<body::Body>, TestError> {
    Ok(granted_headers(
        Request::builder()
            .uri("/deploy/packages")
            .method("POST")
            .header("content-type", "application/octet-stream"),
    )
    .body(body::Body::from(archive))?)
}

async fn read_json<T>(response: Response) -> Result<T, TestError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn start_over_http(router: &axum::Router) -> Result<(WorkflowId, RunId), TestError> {
    let request = granted_headers(
        Request::builder()
            .uri("/workflows/start")
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(&json!({
        "namespace": NAMESPACE,
        "workflow_type": RELOAD_MODULE,
        "input": { "restart": true },
    }))?))?;
    let response = router.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK, "start must succeed");
    let body: serde_json::Value = read_json(response).await?;
    let workflow_id = body["workflow_id"]["uuid"]
        .as_str()
        .ok_or("start response missing workflow id")?
        .parse::<uuid::Uuid>()?;
    let run_id = body["run_id"]["uuid"]
        .as_str()
        .ok_or("start response missing run id")?
        .parse::<uuid::Uuid>()?;
    Ok((WorkflowId::new(workflow_id), RunId::new(run_id)))
}

async fn signal_over_http(
    router: &axum::Router,
    workflow_id: &WorkflowId,
    run_id: &RunId,
    signal_name: &str,
) -> Result<Response, TestError> {
    let request_body = ProtoSignalRequest {
        namespace: NAMESPACE.to_owned(),
        workflow_id: Some(workflow_id.clone().into()),
        run_id: Some(run_id.clone().into()),
        signal_name: signal_name.to_owned(),
        payload: Some(Payload::from_json(&json!({}))?.into()),
    };
    let request = granted_headers(
        Request::builder()
            .uri("/workflows/signal")
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(&request_body)?))?;
    Ok(router.clone().oneshot(request).await?)
}

async fn result_int(engine: &Engine, id: &WorkflowId, run: &RunId) -> Result<i64, TestError> {
    let payload = engine
        .result(id, run)
        .await?
        .map_err(|error| format!("workflow failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    value
        .as_i64()
        .ok_or_else(|| format!("expected integer result, got {value}").into())
}

fn unique_db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "aion-deploy-restart-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// The P0 scenario end-to-end on the durable backend: deploy over the API,
/// run to mid-flight suspension, tear the whole server down, rebuild over
/// the same database with NO `--workflow-package` — the deploy is listed
/// route-active, the run recovered, the remaining signal lands over the API,
/// and the run completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_deploy_survives_server_restart_and_run_completes() -> Result<(), TestError> {
    let db_path = unique_db_path();
    let archive = archive_bytes(&compile_gated_beam(7)?)?;
    let expected_hash = Package::load_from_bytes(&archive, ExtractionLimits::unbounded())?
        .content_hash()
        .to_string();

    // Epoch 1: empty server; the deploy API is the only package source.
    let store: Arc<dyn EventStore> = Arc::new(LibSqlStore::open(db_path.clone()).await?);
    let (engine, router) = server_over(store).await?;
    let response = router
        .clone()
        .oneshot(post_archive(archive.clone())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let loaded: ProtoLoadPackageResponse = read_json(response).await?;
    assert!(loaded.freshly_loaded);
    assert_eq!(loaded.content_hash, expected_hash);

    let (workflow_id, run_id) = start_over_http(&router).await?;
    let response = signal_over_http(&router, &workflow_id, &run_id, "step").await?;
    assert_eq!(response.status(), StatusCode::OK, "step signal must land");
    engine.shutdown()?;
    drop(router);

    // Epoch 2: fresh store handle, engine, resolver, router over the same
    // database. No workflow packages are supplied.
    let store: Arc<dyn EventStore> = Arc::new(LibSqlStore::open(db_path).await?);
    let (engine, router) = server_over(store).await?;

    // The deployed version reloaded from the store and is route-active.
    let response = router
        .clone()
        .oneshot(
            granted_headers(Request::builder().uri("/deploy/versions").method("GET"))
                .body(body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let listing: ProtoListVersionsResponse = read_json(response).await?;
    assert_eq!(listing.versions.len(), 1, "{listing:?}");
    assert_eq!(listing.versions[0].content_hash, expected_hash);
    assert!(listing.versions[0].route_active);

    // The mid-flight run recovered: the remaining signal lands over the API
    // (this returned `WorkflowNotFound` before package persistence) and the
    // run completes with the deployed behavior.
    let response = signal_over_http(&router, &workflow_id, &run_id, "release").await?;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "release signal must land on the recovered run"
    );
    assert_eq!(result_int(&engine, &workflow_id, &run_id).await?, 7);

    // And the recovered deploy serves new starts end-to-end.
    let (new_id, new_run) = start_over_http(&router).await?;
    signal_over_http(&router, &new_id, &new_run, "step").await?;
    signal_over_http(&router, &new_id, &new_run, "release").await?;
    assert_eq!(result_int(&engine, &new_id, &new_run).await?, 7);

    engine.shutdown()?;
    Ok(())
}

/// Restarting with an empty database and no packages must keep the deploy
/// surface honest: nothing reloads, and signalling an unknown run is the
/// anti-leak `NotFound` — proving the recovered-run success above is the
/// persistence path, not permissive routing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_store_restart_reloads_nothing() -> Result<(), TestError> {
    let store: Arc<dyn EventStore> = Arc::new(LibSqlStore::open(unique_db_path()).await?);
    let (engine, router) = server_over(store).await?;

    let response = router
        .clone()
        .oneshot(
            granted_headers(Request::builder().uri("/deploy/versions").method("GET"))
                .body(body::Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let listing: ProtoListVersionsResponse = read_json(response).await?;
    assert!(listing.versions.is_empty(), "{listing:?}");

    let response =
        signal_over_http(&router, &WorkflowId::new_v4(), &RunId::new_v4(), "release").await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: aion_proto::WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::NotFound);

    engine.shutdown()?;
    Ok(())
}
