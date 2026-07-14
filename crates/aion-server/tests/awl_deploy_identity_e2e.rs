//! Regression coverage for document-owned AWL deploy identity and routing.
//!
//! These tests drive the public `POST /awl/deploy` path against a project whose
//! frozen `workflow.toml` names `awl_hello`, matching the live failure shape.
//! They require the external `gleam` binary and runtime-gate only when that
//! binary is unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter, WorkflowVersionInfo};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder,
};
use aion_server::api::http::http_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::{NamespaceResolver, ServerState};
use aion_store::{EventStore, InMemoryStore};
use axum::{body, http::Request, http::StatusCode, response::Response};
use serde_json::{Value, json};
use tower::ServiceExt;

const NAMESPACE: &str = "default";
const FROZEN_ENTRY: &str = "awl_hello";

type TestError = Box<dyn std::error::Error>;

fn gleam_binary() -> Option<PathBuf> {
    let candidate = PathBuf::from("gleam");
    match Command::new(&candidate).arg("--version").output() {
        Ok(output) if output.status.success() => Some(candidate),
        _ => None,
    }
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn provision_project() -> Result<tempfile::TempDir, TestError> {
    let dir = tempfile::Builder::new()
        .prefix("aion-awl-deploy-identity-")
        .tempdir_in(examples_dir())?;
    let root = dir.path();
    std::fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"{FROZEN_ENTRY}\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"../../gleam/aion_flow\" }}\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n"
        ),
    )?;
    std::fs::write(
        root.join("workflow.toml"),
        format!(
            "[[workflow]]\nentry_module = \"{FROZEN_ENTRY}\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = [\"greet\", \"shout\"]\noutput = \"awl-hello.aion\"\n"
        ),
    )?;
    std::fs::create_dir_all(root.join("schemas"))?;
    std::fs::write(root.join("schemas/input.json"), br#"{ "type": "object" }"#)?;
    std::fs::write(root.join("schemas/output.json"), br#"{ "type": "object" }"#)?;
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join(format!("src/{FROZEN_ENTRY}.gleam")),
        b"pub fn run(_raw: a) -> Result(String, Nil) {\n  Ok(\"placeholder\")\n}\n",
    )?;
    Ok(dir)
}

fn document_source(workflow_type: &str, task_queue: &str) -> String {
    format!(
        "//! Test workflow for document-owned deploy identity.\nworkflow {workflow_type}\n  timeout 1m\n  input name: String\n  outcome shouted: type Shouted, route success\n\ntype Greeting {{ greeting: String }}\ntype Shouted {{ text: String }}\n\nworker {task_queue}\n  action greet(name: String) -> Greeting\n  action shout(text: String) -> Shouted\n\nstep greet_and_shout\n  name |> greet |> .greeting |> shout |> route shouted\n"
    )
}

fn runtime_config(authoring: AuthoringConfig) -> RuntimeConfig {
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
        authoring,
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

async fn server_with(
    gleam: PathBuf,
) -> Result<
    (
        Arc<Engine>,
        axum::Router,
        tempfile::TempDir,
        tempfile::TempDir,
    ),
    TestError,
> {
    let project = provision_project()?;
    aion_toolchain::build_project(project.path(), &gleam)?;
    let workspace = tempfile::tempdir()?;
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
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
    let authoring = AuthoringConfig {
        gleam_path: Some(gleam),
        project_root: Some(project.path().to_path_buf()),
        workspace_dir: Some(workspace.path().to_path_buf()),
    };
    let state = ServerState::from_parts(resolver, runtime_config(authoring));
    Ok((engine, http_router(state)?, project, workspace))
}

fn granted_headers(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder
        .header("x-aion-subject", "ci")
        .header("x-aion-namespaces", NAMESPACE)
        .header("x-aion-deploy", "true")
}

async fn read_json(response: Response) -> Result<Value, TestError> {
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn put_document(
    router: &axum::Router,
    path: &str,
    source: &str,
) -> Result<String, TestError> {
    let request = granted_headers(
        Request::builder()
            .uri(format!("/awl/documents/{path}"))
            .method("PUT")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(&json!({
        "source": source,
    }))?))?;
    let response = router.clone().oneshot(request).await?;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "document save must succeed"
    );
    let saved = read_json(response).await?;
    Ok(saved["content_hash"]
        .as_str()
        .ok_or("document response missing content_hash")?
        .to_owned())
}

async fn deploy_document(
    router: &axum::Router,
    path: &str,
    source: &str,
) -> Result<Value, TestError> {
    let content_hash = put_document(router, path, source).await?;
    let request = granted_headers(
        Request::builder()
            .uri("/awl/deploy")
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(&json!({
        "path": path,
        "content_hash": content_hash,
    }))?))?;
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let body = read_json(response).await?;
    assert_eq!(status, StatusCode::OK, "AWL deploy failed: {body}");
    Ok(body)
}

fn version_for<'a>(
    versions: &'a [WorkflowVersionInfo],
    workflow_type: &str,
) -> Result<&'a WorkflowVersionInfo, TestError> {
    versions
        .iter()
        .find(|version| version.workflow_type == workflow_type)
        .ok_or_else(|| format!("missing loaded version for `{workflow_type}`").into())
}

async fn load_preexisting(engine: &Engine) -> Result<String, TestError> {
    let source = document_source("pre_existing", "pre_existing_queue");
    let compiled = aion_awl::compile(&source, Path::new("."))?;
    let activities = compiled
        .actions
        .into_iter()
        .map(|action| DeclaredActivity {
            activity_type: action.action,
        })
        .collect();
    let beams = BeamSet::new(vec![BeamModule::new(
        compiled.workflow_name.clone(),
        compiled.beam_bytes,
    )])?;
    let manifest = Manifest {
        entry_module: compiled.workflow_name,
        entry_function: "run".to_owned(),
        input_schema: compiled.input_schema,
        output_schema: compiled.output_schema,
        timeout: compiled
            .timeout
            .ok_or("pre-existing AWL fixture did not carry its declared timeout")?,
        activities,
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let bytes = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;
    let outcome = engine.load_package(package).await?;
    Ok(outcome.record.version().to_string())
}

#[tokio::test]
async fn awl_deploy_uses_document_workflow_type_in_manifest_versions_and_response()
-> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP awl_deploy_uses_document_workflow_type_in_manifest_versions_and_response: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let (engine, router, _project, _workspace) = server_with(gleam).await?;
    let body = deploy_document(
        &router,
        "review_round.awl",
        &document_source("review_round", "review_queue"),
    )
    .await?;

    assert_eq!(body["deployment"]["workflow_type"], json!("review_round"));
    assert_eq!(body["deployment"]["task_queue"], json!("review_queue"));
    let package_id = body["deployment"]["package_id"]
        .as_str()
        .ok_or("deploy response missing package_id")?;
    let versions = engine.list_workflow_versions()?;
    assert_eq!(versions.len(), 1, "one document deploy loads one version");
    let loaded = version_for(&versions, "review_round")?;
    assert_eq!(loaded.content_hash.to_string(), package_id);
    assert!(
        loaded.route_active,
        "the document's version must own its route"
    );
    assert!(
        loaded.deployed_entry_module.starts_with("review_round$"),
        "the manifest entry module must be the document module: {}",
        loaded.deployed_entry_module
    );
    assert!(
        versions
            .iter()
            .all(|version| version.workflow_type != FROZEN_ENTRY),
        "the frozen template type must never be loaded"
    );
    Ok(())
}

#[tokio::test]
async fn sequential_awl_deploys_preserve_each_document_route_and_preexisting_type()
-> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP sequential_awl_deploys_preserve_each_document_route_and_preexisting_type: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let (engine, router, _project, _workspace) = server_with(gleam).await?;
    let preexisting_hash = load_preexisting(&engine).await?;
    let first = deploy_document(
        &router,
        "review_round.awl",
        &document_source("review_round", "review_queue"),
    )
    .await?;
    let first_hash = first["deployment"]["package_id"]
        .as_str()
        .ok_or("first deploy response missing package_id")?
        .to_owned();
    let second = deploy_document(
        &router,
        "dev_brief.awl",
        &document_source("dev_brief", "dev_brief"),
    )
    .await?;
    let second_hash = second["deployment"]["package_id"]
        .as_str()
        .ok_or("second deploy response missing package_id")?;

    assert_eq!(first["deployment"]["workflow_type"], json!("review_round"));
    assert_eq!(second["deployment"]["workflow_type"], json!("dev_brief"));
    let versions = engine.list_workflow_versions()?;
    assert_eq!(
        versions.len(),
        3,
        "each workflow type keeps one loaded version"
    );
    let first_version = version_for(&versions, "review_round")?;
    let second_version = version_for(&versions, "dev_brief")?;
    let third_version = version_for(&versions, "pre_existing")?;
    assert_eq!(first_version.content_hash.to_string(), first_hash);
    assert_eq!(second_version.content_hash.to_string(), second_hash);
    assert_eq!(third_version.content_hash.to_string(), preexisting_hash);
    assert!(
        first_version.route_active,
        "the first document route survives the second deploy"
    );
    assert!(
        second_version.route_active,
        "the second document owns only its route"
    );
    assert!(
        third_version.route_active,
        "the pre-existing third route is untouched"
    );
    assert!(
        versions
            .iter()
            .all(|version| version.workflow_type != FROZEN_ENTRY),
        "neither document may route-activate the frozen template type"
    );
    Ok(())
}
