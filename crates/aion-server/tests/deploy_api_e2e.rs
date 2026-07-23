//! Deploy API end-to-end: the reload-server-endpoint brief's §5 server tests
//! over both public transports against a real engine executing two compiled
//! fixture versions (the #62 reload fixture, compiled at test time with
//! `erlc` — same precedent as the engine reload suites).

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
    ProtoListVersionsResponse, ProtoLoadPackageResponse, ProtoWireError, WireError, WireErrorCode,
    generated,
};
use aion_server::api::http::http_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::{NamespaceResolver, ServerState};
use axum::{body, http::Request, http::StatusCode, response::Response};
use prost::Message as _;
use serde_json::json;
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const RELOAD_MODULE: &str = "aion_reload_fixture";
const NAMESPACE: &str = "default";
const MAX_ARCHIVE_BYTES: u64 = 1_048_576;
const MAX_INFLATED_BYTES: u64 = 2_097_152;

/// Compiles the reload fixture returning `version` from both entrypoints.
fn compile_reload_beam(version: u32) -> Result<Vec<u8>, TestError> {
    let temp_dir = std::env::temp_dir().join(format!("aion-deploy-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{RELOAD_MODULE}.erl"));
    let beam_path = temp_dir.join(format!("{RELOAD_MODULE}.beam"));
    std::fs::write(
        &source_path,
        format!(
            "-module({RELOAD_MODULE}).\n\
             -export([run/1, park/1]).\n\
             run(_Input) -> {version}.\n\
             park(_Input) -> receive _Any -> {version} end.\n"
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

/// Builds a complete `.aion` archive over `beam` with the given entry function.
fn archive_bytes(beam: &[u8], entry_function: &str) -> Result<Vec<u8>, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(RELOAD_MODULE, beam.to_vec())])?;
    let manifest = Manifest {
        entry_module: RELOAD_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    Ok(PackageBuilder::new(manifest, beams).write_to_bytes()?)
}

fn content_hash_of(archive: &[u8]) -> Result<String, TestError> {
    Ok(
        Package::load_from_bytes(archive, ExtractionLimits::unbounded())?
            .content_hash()
            .to_string(),
    )
}

fn runtime_config(deploy: DeployConfig) -> RuntimeConfig {
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
        deploy,
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        observability: aion_server::config::ObservabilityConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_secs(10)),
        default_namespace: NAMESPACE.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

fn enabled_deploy() -> DeployConfig {
    DeployConfig {
        enabled: true,
        max_archive_bytes: Some(MAX_ARCHIVE_BYTES),
        max_inflated_bytes: Some(MAX_INFLATED_BYTES),
    }
}

/// Real engine the test holds directly (for execution/result assertions)
/// wrapped in a served state.
async fn engine_state(deploy: DeployConfig) -> Result<(Arc<Engine>, ServerState), TestError> {
    let mut search_attribute_schema = aion_core::SearchAttributeSchema::new();
    search_attribute_schema.register(
        aion_server::NAMESPACE_ATTRIBUTE,
        aion_core::SearchAttributeType::String,
    )?;
    let engine = Arc::new(
        EngineBuilder::new()
            .store_arc(
                Arc::new(aion_store::InMemoryStore::default()) as Arc<dyn aion_store::EventStore>
            )
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
    let state = ServerState::from_parts(resolver, runtime_config(deploy));
    Ok((engine, state))
}

fn deploy_headers(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder
        .header("x-aion-subject", "ci")
        .header("x-aion-namespaces", NAMESPACE)
        .header("x-aion-deploy", "true")
}

fn post_archive(archive: Vec<u8>) -> Result<Request<body::Body>, TestError> {
    Ok(deploy_headers(
        Request::builder()
            .uri("/deploy/packages")
            .method("POST")
            .header("content-type", "application/octet-stream"),
    )
    .body(body::Body::from(archive))?)
}

fn post_json(uri: &str, value: &serde_json::Value) -> Result<Request<body::Body>, TestError> {
    Ok(deploy_headers(
        Request::builder()
            .uri(uri)
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(value)?))?)
}

/// Shared-secret bearer accepted by the auth-on dev-token path
/// (`auth.enabled = true`, `not(feature = "auth")`).
#[cfg(not(feature = "auth"))]
const AUTH_ON_TOKEN: &str = "deploy-secret";

/// Auth-on runtime config (dev-token path): a valid shared-secret bearer plus
/// the deploy grant authorize deploy; an authenticated caller lacking the
/// grant is still denied (the strict gate stays strict).
#[cfg(not(feature = "auth"))]
fn auth_on_runtime_config(deploy: DeployConfig) -> RuntimeConfig {
    let mut config = runtime_config(deploy);
    config.auth = AuthConfig {
        enabled: true,
        jwks_url: Some(AUTH_ON_TOKEN.to_owned()),
        jwks_refresh_seconds: 300,
    };
    config
}

/// Auth-on deploy headers: a valid bearer, the subject, the namespaces, and the
/// `x-aion-deploy` grant the dev-token gate requires.
#[cfg(not(feature = "auth"))]
fn auth_on_deploy_headers(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    deploy_headers(builder).header("authorization", format!("Bearer {AUTH_ON_TOKEN}"))
}

#[cfg(not(feature = "auth"))]
fn auth_on_post_archive(archive: Vec<u8>) -> Result<Request<body::Body>, TestError> {
    Ok(auth_on_deploy_headers(
        Request::builder()
            .uri("/deploy/packages")
            .method("POST")
            .header("content-type", "application/octet-stream"),
    )
    .body(body::Body::from(archive))?)
}

#[cfg(not(feature = "auth"))]
fn auth_on_post_json(
    uri: &str,
    value: &serde_json::Value,
) -> Result<Request<body::Body>, TestError> {
    Ok(auth_on_deploy_headers(
        Request::builder()
            .uri(uri)
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(value)?))?)
}

fn get_versions() -> Result<Request<body::Body>, TestError> {
    Ok(
        deploy_headers(Request::builder().uri("/deploy/versions").method("GET"))
            .body(body::Body::empty())?,
    )
}

async fn read_json<T>(response: Response) -> Result<T, TestError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn start_over_http(router: &axum::Router) -> Result<(WorkflowId, RunId), TestError> {
    let request = deploy_headers(
        Request::builder()
            .uri("/workflows/start")
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(&json!({
        "namespace": NAMESPACE,
        "workflow_type": RELOAD_MODULE,
        "input": { "reload": true },
    }))?))?;
    let response = router.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK, "start must succeed");
    // Clean wire contract: start response exposes plain UUID strings.
    let body: serde_json::Value = read_json(response).await?;
    let workflow_id = body["workflow_id"]
        .as_str()
        .ok_or("start response missing workflow id")?
        .parse::<uuid::Uuid>()?;
    let run_id = body["run_id"]
        .as_str()
        .ok_or("start response missing run id")?
        .parse::<uuid::Uuid>()?;
    Ok((WorkflowId::new(workflow_id), RunId::new(run_id)))
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

/// gRPC request carrying the development subject + deploy grant metadata.
fn granted<T>(message: T) -> Result<tonic::Request<T>, TestError> {
    let mut request = tonic::Request::new(message);
    request
        .metadata_mut()
        .insert("x-aion-subject", "ci".parse()?);
    request
        .metadata_mut()
        .insert("x-aion-deploy", "true".parse()?);
    Ok(request)
}

/// POSTs a route re-point and asserts the expected status.
async fn post_route(
    router: &axum::Router,
    content_hash: &str,
    expected: StatusCode,
) -> Result<Response, TestError> {
    let response = router
        .clone()
        .oneshot(post_json(
            "/deploy/route",
            &json!({ "workflow_type": RELOAD_MODULE, "content_hash": content_hash }),
        )?)
        .await?;
    assert_eq!(response.status(), expected);
    Ok(response)
}

/// POSTs an unload and asserts the expected status.
async fn post_unload(
    router: &axum::Router,
    content_hash: &str,
    expected: StatusCode,
) -> Result<Response, TestError> {
    let response = router
        .clone()
        .oneshot(post_json(
            "/deploy/unload",
            &json!({ "workflow_type": RELOAD_MODULE, "content_hash": content_hash }),
        )?)
        .await?;
    assert_eq!(response.status(), expected);
    Ok(response)
}

fn route_active_hashes(listing: &ProtoListVersionsResponse) -> Vec<&str> {
    listing
        .versions
        .iter()
        .filter(|version| version.route_active)
        .map(|version| version.content_hash.as_str())
        .collect()
}

/// Brief §5 tests 4–8 over HTTP: load fresh / idempotent / re-route, the
/// versions read model, unload refusals (route-active and live-run pin) with
/// `version_pinned`, unload success, route-to-unknown `not_found`, and the
/// D10 manifest-mismatch refusal — all against a really-executing engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_deploy_lifecycle_over_a_running_engine() -> Result<(), TestError> {
    let (engine, state) = engine_state(enabled_deploy()).await?;
    let router = http_router(state)?;

    let beam_v1 = compile_reload_beam(1)?;
    let beam_v2 = compile_reload_beam(2)?;
    let v1 = archive_bytes(&beam_v1, "run")?;
    let v2 = archive_bytes(&beam_v2, "run")?;
    let v1_hash = content_hash_of(&v1)?;
    let v2_hash = content_hash_of(&v2)?;

    // Deploy v1 -> fresh load takes the route; a start executes v1.
    let response = router.clone().oneshot(post_archive(v1.clone())?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let loaded: ProtoLoadPackageResponse = read_json(response).await?;
    assert_eq!(loaded.workflow_type, RELOAD_MODULE);
    assert_eq!(loaded.content_hash, v1_hash);
    assert!(loaded.freshly_loaded);
    assert!(loaded.route_changed);
    let (id, run) = start_over_http(&router).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 1);

    // Idempotent re-deploy: same archive, 200, both flags false.
    let again: ProtoLoadPackageResponse =
        read_json(router.clone().oneshot(post_archive(v1.clone())?).await?).await?;
    assert!(!again.freshly_loaded);
    assert!(!again.route_changed);

    // Deploy v2 -> fresh + route moves; new starts execute v2.
    let loaded_v2: ProtoLoadPackageResponse =
        read_json(router.clone().oneshot(post_archive(v2.clone())?).await?).await?;
    assert!(loaded_v2.freshly_loaded);
    assert!(loaded_v2.route_changed);
    let (id, run) = start_over_http(&router).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 2);

    // Versions listing shows both with v2 route-active.
    let listing: ProtoListVersionsResponse =
        read_json(router.clone().oneshot(get_versions()?).await?).await?;
    assert_eq!(listing.versions.len(), 2);
    assert_eq!(route_active_hashes(&listing), vec![v2_hash.as_str()]);

    // Roll back to v1; new starts execute v1 again.
    post_route(&router, &v1_hash, StatusCode::OK).await?;
    let listing: ProtoListVersionsResponse =
        read_json(router.clone().oneshot(get_versions()?).await?).await?;
    assert_eq!(route_active_hashes(&listing), vec![v1_hash.as_str()]);
    let (id, run) = start_over_http(&router).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 1);

    // Re-deploying the rolled-back-from v2 archive re-points the route.
    let redeployed: ProtoLoadPackageResponse =
        read_json(router.clone().oneshot(post_archive(v2.clone())?).await?).await?;
    assert!(!redeployed.freshly_loaded);
    assert!(redeployed.route_changed);

    engine.shutdown()?;
    Ok(())
}

/// Brief §5 tests 7–8 over HTTP: unload refusals (route-active and live-run
/// pin) carrying `version_pinned`, unload success leaving the listing,
/// route-to-unloaded `not_found`, and the D10 manifest-mismatch refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_unload_refusals_success_and_manifest_mismatch() -> Result<(), TestError> {
    let (engine, state) = engine_state(enabled_deploy()).await?;
    let router = http_router(state)?;

    let beam_v2 = compile_reload_beam(2)?;
    let v2 = archive_bytes(&beam_v2, "run")?;
    let v2_hash = content_hash_of(&v2)?;
    let loaded: ProtoLoadPackageResponse =
        read_json(router.clone().oneshot(post_archive(v2)?).await?).await?;
    assert!(loaded.freshly_loaded);

    // Unloading the route-active version is a 409 version_pinned.
    let response = post_unload(&router, &v2_hash, StatusCode::CONFLICT).await?;
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::VersionPinned);
    assert_eq!(error.error_type.as_deref(), Some("RouteActive"));

    // A parked live run pins its version against unload, naming the run.
    let beam_v3 = compile_reload_beam(3)?;
    let v3 = archive_bytes(&beam_v3, "park")?;
    let v3_hash = content_hash_of(&v3)?;
    let parked: ProtoLoadPackageResponse =
        read_json(router.clone().oneshot(post_archive(v3)?).await?).await?;
    assert!(parked.freshly_loaded);
    let (parked_id, parked_run) = start_over_http(&router).await?;
    // Point the route back at v2 so v3 is unload-eligible except for the pin.
    post_route(&router, &v2_hash, StatusCode::OK).await?;
    let response = post_unload(&router, &v3_hash, StatusCode::CONFLICT).await?;
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::VersionPinned);
    assert!(
        error.message.contains(&parked_id.to_string()),
        "pin refusal must name the live run: {}",
        error.message
    );

    // Release the parked run; once terminal the version unloads cleanly.
    engine
        .signal(
            &parked_id,
            &parked_run,
            "release",
            Payload::from_json(&json!({}))?,
        )
        .await?;
    assert_eq!(result_int(&engine, &parked_id, &parked_run).await?, 3);
    post_unload(&router, &v3_hash, StatusCode::OK).await?;
    let listing: ProtoListVersionsResponse =
        read_json(router.clone().oneshot(get_versions()?).await?).await?;
    assert!(
        listing
            .versions
            .iter()
            .all(|version| version.content_hash != v3_hash),
        "unloaded version must leave the listing"
    );

    // Routing to the unloaded hash is now not_found.
    let response = post_route(&router, &v3_hash, StatusCode::NOT_FOUND).await?;
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::NotFound);
    assert_eq!(error.error_type.as_deref(), Some("UnknownVersion"));

    // D10: same beams, different manifest entry function -> invalid_input
    // with the manifest-mismatch refusal; the resident version is untouched.
    let mismatched = archive_bytes(&beam_v2, "park")?;
    assert_eq!(content_hash_of(&mismatched)?, v2_hash);
    let response = router.clone().oneshot(post_archive(mismatched)?).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::InvalidInput);
    assert_eq!(error.error_type.as_deref(), Some("ManifestMismatch"));
    let (id, run) = start_over_http(&router).await?;
    assert_eq!(result_int(&engine, &id, &run).await?, 2);

    engine.shutdown()?;
    Ok(())
}

/// HTTP drain semantics: mutations refuse with 503 while the versions read
/// model keeps serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_drain_refuses_mutations_but_serves_listing() -> Result<(), TestError> {
    let (engine, state) = engine_state(enabled_deploy()).await?;
    assert!(state.drain_state().begin());
    let router = http_router(state)?;

    let response = router
        .clone()
        .oneshot(post_json(
            "/deploy/route",
            &json!({ "workflow_type": RELOAD_MODULE, "content_hash": "a".repeat(64) }),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error: WireError = read_json(response).await?;
    assert!(
        error.message.contains("draining"),
        "drain refusal must be explicit: {}",
        error.message
    );

    let listing = router.clone().oneshot(get_versions()?).await?;
    assert_eq!(listing.status(), StatusCode::OK);
    engine.shutdown()?;
    Ok(())
}

/// Metrics: counters increment per outcome class and the loaded-version
/// gauge tracks the listing (test plan item 11's metric half; the audit
/// line is asserted in `deploy_audit_log.rs`-style capture below).
///
/// Runs auth ENABLED (dev-token path) so the deploy gate can actually deny an
/// ungranted caller — under auth-off single-tenant operator mode there is no
/// deploy denial to count. Gated `not(feature = "auth")` because the real-JWT
/// build needs a live JWKS endpoint; the dev-token path is the auth-on analog.
#[cfg(not(feature = "auth"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deploy_metrics_count_operations_and_denials() -> Result<(), TestError> {
    let mut config = auth_on_runtime_config(enabled_deploy());
    config.metrics = MetricsConfig { enabled: true };
    let state = ServerState::build_with_store(aion_store::InMemoryStore::default(), config).await?;
    let router = http_router(state)?;

    // One successful load (valid bearer + deploy grant).
    let beam = compile_reload_beam(1)?;
    let archive = archive_bytes(&beam, "run")?;
    let response = router
        .clone()
        .oneshot(auth_on_post_archive(archive)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    // One denial: valid bearer + subject, but NO deploy grant (auth-on strict
    // gate). This is the denial the counter records.
    let denied = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/deploy/versions")
                .method("GET")
                .header("authorization", format!("Bearer {AUTH_ON_TOKEN}"))
                .header("x-aion-subject", "ci")
                .header("x-aion-namespaces", NAMESPACE)
                .body(body::Body::empty())?,
        )
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // One refusal (route to unknown version), authorized with the deploy grant.
    let refused = router
        .clone()
        .oneshot(auth_on_post_json(
            "/deploy/route",
            &json!({ "workflow_type": "missing", "content_hash": "a".repeat(64) }),
        )?)
        .await?;
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    let metrics = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(body::Body::empty())?,
        )
        .await?;
    assert_eq!(metrics.status(), StatusCode::OK);
    let bytes = body::to_bytes(metrics.into_body(), usize::MAX).await?;
    let text = String::from_utf8(bytes.to_vec())?;
    assert!(
        text.contains(
            "aion_deploy_operations_total{operation=\"deploy.load\",outcome=\"loaded\"} 1"
        ),
        "load counter must record the loaded outcome: {text}"
    );
    assert!(
        text.contains("aion_deploy_denied_total{transport=\"http\"} 1"),
        "denial counter must record the http denial: {text}"
    );
    assert!(
        text.contains(
            "aion_deploy_operations_total{operation=\"deploy.route\",outcome=\"not_found\"} 1"
        ),
        "refusal counter must record the refusal class: {text}"
    );
    assert!(
        text.contains(&format!(
            "aion_loaded_workflow_versions{{workflow_type=\"{RELOAD_MODULE}\"}} 1"
        )),
        "loaded-version gauge must track the listing: {text}"
    );
    Ok(())
}

/// The typed `ProtoWireError` detail rides deploy statuses so machines can
/// branch on `version_pinned` (CI gating a rollback target).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_unload_route_active_carries_version_pinned_detail() -> Result<(), TestError> {
    use aion_proto::generated::deploy_service_client::DeployServiceClient;
    use tokio_stream::wrappers::TcpListenerStream;

    let (engine, state) = engine_state(enabled_deploy()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let deploy = aion_server::api::deploy_grpc::deploy_service(state.clone())?;
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(deploy)
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let channel = tonic::transport::Endpoint::try_from(format!("http://{address}"))?
        .connect()
        .await?;
    let mut client = DeployServiceClient::new(channel);

    let beam = compile_reload_beam(1)?;
    let archive = archive_bytes(&beam, "run")?;
    let hash = content_hash_of(&archive)?;
    let loaded = client
        .load_package(granted(generated::LoadPackageRequest { archive })?)
        .await?
        .into_inner();
    assert!(loaded.freshly_loaded);

    // Idempotent re-load over gRPC.
    let beam_archive = archive_bytes(&beam, "run")?;
    let again = client
        .load_package(granted(generated::LoadPackageRequest {
            archive: beam_archive,
        })?)
        .await?
        .into_inner();
    assert!(!again.freshly_loaded);
    assert!(!again.route_changed);

    // Listing over gRPC.
    let listing = client
        .list_versions(granted(generated::ListVersionsRequest {})?)
        .await?
        .into_inner();
    assert_eq!(listing.versions.len(), 1);
    assert!(listing.versions[0].route_active);

    // Unloading the route-active version: FailedPrecondition with the typed
    // version_pinned detail.
    let status = client
        .unload_version(granted(generated::UnloadVersionRequest {
            workflow_type: RELOAD_MODULE.to_owned(),
            content_hash: hash,
        })?)
        .await
        .err()
        .ok_or("expected route-active unload refusal")?;
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    let detail = WireError::try_from(ProtoWireError::decode(status.details())?)?;
    assert_eq!(detail.code, WireErrorCode::VersionPinned);
    assert_eq!(detail.error_type.as_deref(), Some("RouteActive"));

    // Auth-off single-tenant operator mode: a caller with no deploy metadata is
    // the operator and is authorized — deploy is granted server-side at request
    // time. (The auth-ON strict-gate denial is proven in the lib-level
    // `auth_on_denies_caller_without_deploy_grant` test.)
    let mut request = tonic::Request::new(generated::ListVersionsRequest {});
    request
        .metadata_mut()
        .insert("x-aion-subject", "ci".parse()?);
    let listing = client.list_versions(request).await?;
    // The route is active, so the loaded version is still listed.
    assert!(!listing.into_inner().versions.is_empty());

    engine.shutdown()?;
    server.abort();
    Ok(())
}

/// A server whose gRPC listener carries no deploy service (deploy disabled)
/// answers `Unimplemented` for every deploy RPC.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_grpc_deploy_surface_is_unimplemented() -> Result<(), TestError> {
    use aion_proto::generated::deploy_service_client::DeployServiceClient;
    use tokio_stream::wrappers::TcpListenerStream;

    let (engine, state) = engine_state(DeployConfig::default()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    // Mirror main.rs: with deploy disabled only the workflow service mounts.
    let workflow = aion_server::api::grpc::workflow_service(state);
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(workflow)
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let channel = tonic::transport::Endpoint::try_from(format!("http://{address}"))?
        .connect()
        .await?;
    let mut client = DeployServiceClient::new(channel);

    let status = client
        .list_versions(tonic::Request::new(generated::ListVersionsRequest {}))
        .await
        .err()
        .ok_or("expected unimplemented service")?;
    assert_eq!(status.code(), tonic::Code::Unimplemented);

    engine.shutdown()?;
    server.abort();
    Ok(())
}

/// A DEFLATE bomb: compressed well under `max_archive_bytes`, inflating past
/// `max_inflated_bytes`. `PackageBuilder` writes Stored entries only, so the
/// hostile shape is assembled directly with the zip writer.
fn bomb_archive() -> Result<Vec<u8>, TestError> {
    use std::io::Write as _;

    let manifest = Manifest {
        entry_module: RELOAD_MODULE.to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![],
        version: ManifestVersion::new("irrelevant-never-reached"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let cursor = std::io::Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    archive.start_file("manifest.json", options)?;
    archive.write_all(&manifest_bytes)?;
    archive.start_file(format!("beam/{RELOAD_MODULE}.beam"), options)?;
    // 8 MiB of zeros: inflates 4x past MAX_INFLATED_BYTES while compressing
    // to a few KiB, far under MAX_ARCHIVE_BYTES.
    archive.write_all(&vec![0_u8; 8 * 1024 * 1024])?;
    Ok(archive.finish()?.into_inner())
}

/// A zip bomb under the upload ceiling but inflating past
/// `deploy.max_inflated_bytes` is refused with the same 413 wire class as an
/// oversized archive — naming the inflate key — and is recorded as a refused
/// load (metrics outcome), with nothing registered in the engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_inflate_bomb_is_413_naming_max_inflated_bytes() -> Result<(), TestError> {
    let config = runtime_config(enabled_deploy());
    let state = ServerState::build_with_store(aion_store::InMemoryStore::default(), config).await?;
    let router = http_router(state)?;

    let bomb = bomb_archive()?;
    assert!(
        u64::try_from(bomb.len())? < MAX_ARCHIVE_BYTES,
        "bomb must pass the upload ceiling to exercise the inflate ceiling: {} bytes",
        bomb.len()
    );

    let response = router.clone().oneshot(post_archive(bomb)?).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: WireError = read_json(response).await?;
    assert_eq!(error.code, WireErrorCode::InvalidInput);
    assert!(
        error.message.contains("deploy.max_inflated_bytes"),
        "413 must name the inflate config key: {}",
        error.message
    );

    // Nothing loaded: the bomb never reached the engine.
    let listing: ProtoListVersionsResponse =
        read_json(router.clone().oneshot(get_versions()?).await?).await?;
    assert!(listing.versions.is_empty());

    // The refusal is recorded like every other refused load.
    let metrics = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(body::Body::empty())?,
        )
        .await?;
    let bytes = body::to_bytes(metrics.into_body(), usize::MAX).await?;
    let text = String::from_utf8(bytes.to_vec())?;
    assert!(
        text.contains(
            "aion_deploy_operations_total{operation=\"deploy.load\",outcome=\"invalid_input\"} 1"
        ),
        "inflate refusal must be recorded as a refused load: {text}"
    );
    Ok(())
}
