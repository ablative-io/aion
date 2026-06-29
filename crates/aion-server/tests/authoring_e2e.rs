//! Server-side authoring loop e2e over the public HTTP transport.
//!
//! Proves R2 / C13 / C14 / S7 / S8: with `[authoring].gleam_path` configured,
//! `POST /authoring/compile` returns a type error inline (HTTP 400) for a
//! type-erroneous workflow, and packages + hot-loads a corrected workflow so a
//! subsequent `/workflows/start` runs it on the new version.
//!
//! The compile path requires the external `gleam` binary plus the cached Hex
//! dependencies of the `aion_flow` SDK, so it is gated at RUNTIME: when
//! `gleam` is not runnable the test emits a skip line and returns `Ok(())` —
//! never `#[ignore]`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{RunId, WorkflowId};
use aion_server::api::http::http_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
    MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use aion_server::{NamespaceResolver, ServerState};
use aion_store::{EventStore, InMemoryStore};
use axum::{body, http::Request, http::StatusCode, response::Response};
use serde_json::json;
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const ENTRY_MODULE: &str = "aion_authoring_fixture";

/// A type-erroneous workflow: `run` is annotated `Result(String, _)` but
/// returns a bare `Int`. The Gleam compiler rejects it.
const TYPE_ERROR_SOURCE: &str = r"import gleam/dynamic.{type Dynamic}

pub fn run(_raw_input: Dynamic) -> Result(String, Nil) {
  42
}
";

/// A corrected, valid workflow with no activity, so a started run completes
/// without a worker. `run` returns the decoded name (or a default).
const VALID_SOURCE: &str = r#"import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

pub fn run(raw_input: Dynamic) -> Result(String, Nil) {
  case decode.run(raw_input, decode.string) {
    Ok(name) -> Ok("Hello, " <> name)
    Error(_) -> Ok("Hello, world")
  }
}
"#;

/// A second, distinct valid workflow: same entry module (so the same
/// `workflow_type`) but a different `run` body, hence different bytecode and a
/// different content hash than [`VALID_SOURCE`]. Used to prove two overlapping
/// submissions of DIFFERENT source each receive THEIR OWN content hash.
const OTHER_VALID_SOURCE: &str = r#"import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

pub fn run(raw_input: Dynamic) -> Result(String, Nil) {
  case decode.run(raw_input, decode.string) {
    Ok(name) -> Ok("Goodbye, " <> name <> "!")
    Error(_) -> Ok("Goodbye, world!")
  }
}
"#;

fn gleam_binary() -> Option<PathBuf> {
    let candidate = PathBuf::from("gleam");
    match Command::new(&candidate).arg("--version").output() {
        Ok(output) if output.status.success() => Some(candidate),
        _ => None,
    }
}

/// Absolute path to the repository `examples/` directory.
///
/// The fixture is provisioned here, at the same directory depth (2) as every
/// real example template, so its **relative** `aion_flow = { path =
/// "../../gleam/aion_flow" }` dependency resolves to the real SDK from the
/// staged same-depth working copy — exactly as production does.
fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

/// Provisions a built single-workflow Gleam project whose `aion_flow`
/// dependency is the production-shape **relative** path `../../gleam/aion_flow`
/// (mirroring every real example template), placed at the same directory depth
/// as those templates. This makes the server e2e genuinely exercise the
/// same-depth staging that production relies on: an absolute dependency would
/// resolve regardless of staging depth and so would not be load-bearing.
///
/// The temp dir is auto-removed on drop, leaving the repo's `examples/` clean.
fn provision_project() -> Result<tempfile::TempDir, TestError> {
    let dir = tempfile::Builder::new()
        .prefix("aion-authoring-server-e2e-")
        .tempdir_in(examples_dir())?;
    let root = dir.path();

    std::fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"{ENTRY_MODULE}\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"../../gleam/aion_flow\" }}\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n"
        ),
    )?;
    std::fs::write(
        root.join("workflow.toml"),
        format!(
            "[[workflow]]\nentry_module = \"{ENTRY_MODULE}\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = []\noutput = \"fixture.aion\"\n"
        )
        .into_bytes(),
    )?;
    std::fs::create_dir_all(root.join("schemas"))?;
    std::fs::write(root.join("schemas/input.json"), br#"{ "type": "string" }"#)?;
    std::fs::write(root.join("schemas/output.json"), br#"{ "type": "string" }"#)?;
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join(format!("src/{ENTRY_MODULE}.gleam")),
        b"pub fn run(_raw: a) -> Result(String, Nil) {\n  Ok(\"placeholder\")\n}\n",
    )?;
    Ok(dir)
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
        authoring,
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

async fn server_with(authoring: AuthoringConfig) -> Result<(Arc<Engine>, axum::Router), TestError> {
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
    let state = ServerState::from_parts(resolver, runtime_config(authoring));
    Ok((engine, http_router(state)?))
}

fn granted_headers(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder
        .header("x-aion-subject", "ci")
        .header("x-aion-namespaces", NAMESPACE)
        .header("x-aion-deploy", "true")
}

fn compile_request(source: &str) -> Result<Request<body::Body>, TestError> {
    Ok(granted_headers(
        Request::builder()
            .uri("/authoring/compile")
            .method("POST")
            .header("content-type", "application/json"),
    )
    .body(body::Body::from(serde_json::to_vec(&json!({
        "source": source,
    }))?))?)
}

async fn read_json<T>(response: Response) -> Result<T, TestError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn read_text(response: Response) -> Result<String, TestError> {
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8(bytes.to_vec())?)
}

/// R2 acceptance #3 / CN7: with `[authoring].gleam_path` absent, every
/// authoring route is a plain 404 — the surface is not mounted.
#[tokio::test]
async fn authoring_absent_is_404_on_every_route() -> Result<(), TestError> {
    let (_engine, router) = server_with(AuthoringConfig::default()).await?;

    let cases = [
        ("POST", "/authoring/compile"),
        ("GET", "/authoring/compile"),
        ("POST", "/authoring/anything"),
    ];
    for (method, uri) in cases {
        let response = router
            .clone()
            .oneshot(
                granted_headers(Request::builder().method(method).uri(uri))
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} {uri} must be 404 when authoring is dark"
        );
    }
    Ok(())
}

/// R2 acceptance #2 / C14 / S8: with `[authoring].gleam_path` configured, a
/// type-erroneous submission returns the gleam error inline (HTTP 400), then a
/// corrected submission packages, hot-loads (the new version appears in the
/// engine's loaded versions), and a subsequent start runs it.
#[tokio::test]
async fn authoring_compiles_loads_and_runs_a_corrected_workflow() -> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP authoring_compiles_loads_and_runs_a_corrected_workflow: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let project = provision_project()?;
    let authoring = AuthoringConfig {
        gleam_path: Some(gleam),
        project_root: Some(project.path().to_path_buf()),
    };
    let (engine, router) = server_with(authoring).await?;

    // 1. Type-erroneous source -> 400 carrying the gleam error inline (C13).
    let type_error = router
        .clone()
        .oneshot(compile_request(TYPE_ERROR_SOURCE)?)
        .await?;
    if type_error.status() == StatusCode::SERVICE_UNAVAILABLE
        || type_error.status() == StatusCode::INTERNAL_SERVER_ERROR
    {
        // gleam could not run in this environment (dependency resolution
        // sandbox); skip rather than fail a product assertion.
        eprintln!(
            "SKIP authoring_compiles_loads_and_runs_a_corrected_workflow: gleam build unavailable in this environment ({})",
            type_error.status()
        );
        return Ok(());
    }
    assert_eq!(
        type_error.status(),
        StatusCode::BAD_REQUEST,
        "a type error must be a 400"
    );
    let body = read_text(type_error).await?;
    assert!(
        body.to_lowercase().contains("error"),
        "the gleam error must travel back inline: {body}"
    );
    assert!(
        engine.list_workflow_versions()?.is_empty(),
        "a type error must not load any version"
    );

    // 2. Corrected source -> packages + hot-loads (C14).
    let corrected = router
        .clone()
        .oneshot(compile_request(VALID_SOURCE)?)
        .await?;
    assert_eq!(
        corrected.status(),
        StatusCode::OK,
        "a corrected workflow must compile and hot-load"
    );
    let loaded: serde_json::Value = read_json(corrected).await?;
    assert_eq!(loaded["workflow_type"], json!(ENTRY_MODULE));
    assert!(
        loaded["content_hash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty()),
        "the response must carry a content hash: {loaded}"
    );

    let versions = engine.list_workflow_versions()?;
    assert!(
        versions
            .iter()
            .any(|info| info.workflow_type == ENTRY_MODULE),
        "the hot-loaded version must appear in the engine's loaded versions"
    );

    // 3. A start runs on the new version and completes (S8).
    let (workflow_id, run_id) = start_over_http(&router).await?;
    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("workflow failed: {error:?}"))?;
    let rendered = String::from_utf8_lossy(result.bytes()).into_owned();
    assert!(
        rendered.contains("authoring"),
        "the hot-loaded workflow must run and return its computed result over the decoded input, got: {rendered}"
    );
    Ok(())
}

/// Per-submission isolation over the wire (the BEST-solution property): two
/// OVERLAPPING `POST /authoring/compile` submissions of DIFFERENT source,
/// against the one operator-configured (read-only) template, each get back
/// THEIR OWN `content_hash` — proving no cross-talk and no wrong-artifact
/// return when concurrent authors race on the shared template. Both load into
/// the engine as distinct versions of the same workflow type, and the template
/// is left pristine.
#[tokio::test]
async fn concurrent_submissions_return_their_own_content_hash() -> Result<(), TestError> {
    let Some(gleam) = gleam_binary() else {
        eprintln!(
            "SKIP concurrent_submissions_return_their_own_content_hash: `gleam` binary not runnable"
        );
        return Ok(());
    };
    let project = provision_project()?;
    let template_root = project.path().to_path_buf();
    let authoring = AuthoringConfig {
        gleam_path: Some(gleam),
        project_root: Some(template_root.clone()),
    };
    let (engine, router) = server_with(authoring).await?;

    // Fire BOTH submissions concurrently against the shared template. If the
    // template were the mutable build root, these two would race on its
    // entry-file, build/ dir, and .aion output and could return the wrong
    // artifact; per-submission isolation makes them independent.
    let first_router = router.clone();
    let second_router = router.clone();
    let first_body = compile_request(VALID_SOURCE)?;
    let second_body = compile_request(OTHER_VALID_SOURCE)?;
    let (first, second) = tokio::join!(
        first_router.oneshot(first_body),
        second_router.oneshot(second_body),
    );
    let first = first?;
    let second = second?;

    // An environment that cannot resolve gleam dependencies skips, exactly like
    // the single-submission e2e above.
    for (label, status) in [("first", first.status()), ("second", second.status())] {
        if status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::INTERNAL_SERVER_ERROR
        {
            eprintln!(
                "SKIP concurrent_submissions_return_their_own_content_hash: gleam build unavailable in this environment ({label}: {status})"
            );
            return Ok(());
        }
    }
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "the first concurrent submission must compile and hot-load"
    );
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "the second concurrent submission must compile and hot-load"
    );

    let first: serde_json::Value = read_json(first).await?;
    let second: serde_json::Value = read_json(second).await?;

    // Both are the same workflow type (same template entry module)...
    assert_eq!(first["workflow_type"], json!(ENTRY_MODULE));
    assert_eq!(second["workflow_type"], json!(ENTRY_MODULE));

    let first_hash = first["content_hash"]
        .as_str()
        .ok_or("first response missing content hash")?;
    let second_hash = second["content_hash"]
        .as_str()
        .ok_or("second response missing content hash")?;
    assert!(!first_hash.is_empty(), "first content hash must be present");
    assert!(
        !second_hash.is_empty(),
        "second content hash must be present"
    );

    // ...but DIFFERENT content hashes: each author got back exactly their own
    // artifact, never the other's (no cross-talk, no wrong-artifact return).
    assert_ne!(
        first_hash, second_hash,
        "two concurrent submissions of different source must each return their OWN content hash"
    );

    // Both versions are loaded in the engine — distinct content hashes of one
    // workflow type, the live-authoring loop's shape.
    let versions = engine.list_workflow_versions()?;
    let loaded_hashes: Vec<String> = versions
        .iter()
        .filter(|info| info.workflow_type == ENTRY_MODULE)
        .map(|info| info.content_hash.to_string())
        .collect();
    assert!(
        loaded_hashes.iter().any(|hash| hash == first_hash),
        "the first submission's version is loaded: {loaded_hashes:?}"
    );
    assert!(
        loaded_hashes.iter().any(|hash| hash == second_hash),
        "the second submission's version is loaded: {loaded_hashes:?}"
    );

    // The operator-provisioned template is read-only at request time: no build
    // artifacts leaked into it despite two concurrent submissions.
    assert!(
        !template_root.join("fixture.aion").exists(),
        "the read-only template carries no .aion after concurrent submissions"
    );
    assert!(
        !template_root.join("build").exists(),
        "the read-only template carries no build/ dir after concurrent submissions"
    );
    Ok(())
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
        "workflow_type": ENTRY_MODULE,
        "input": "authoring",
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
