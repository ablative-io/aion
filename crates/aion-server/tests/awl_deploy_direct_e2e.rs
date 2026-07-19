//! Direct bytecode deployment regressions for the studio AWL HTTP surface.

use std::sync::Arc;
use std::time::Duration;

use aion::{Engine, EngineBuilder};
use aion_awl_package::compile_and_assemble_awl;
use aion_core::DEFAULT_TASK_QUEUE;
use aion_package::{ExtractionLimits, Package};
use aion_server::api::http::http_router;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::{NamespaceResolver, ServerState};
use aion_store::{EventStore, InMemoryStore, PackageStore};
use axum::{body, http::Request, http::StatusCode, response::Response};
use serde_json::{Value, json};
use tower::ServiceExt;

#[cfg(feature = "auth")]
use base64::Engine as _;
#[cfg(feature = "auth")]
use jsonwebtoken::{Algorithm, EncodingKey, Header};

const NAMESPACE: &str = "default";
const FROZEN_ENTRY: &str = "awl_hello";

type TestError = Box<dyn std::error::Error>;

struct Harness {
    workspace: tempfile::TempDir,
    engine: Arc<Engine>,
    store: Arc<InMemoryStore>,
    state: ServerState,
    router: axum::Router,
}

impl Harness {
    async fn new() -> Result<Self, TestError> {
        Self::new_with_auth(false).await
    }

    async fn new_with_auth(auth_enabled: bool) -> Result<Self, TestError> {
        let workspace = tempfile::tempdir()?;
        // `tempfile::tempdir` inherits the umask; the server's private-root
        // validation requires the workspace root itself to be `0700`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(workspace.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let template_root = workspace.path().join("frozen-template");
        std::fs::create_dir_all(&template_root)?;
        std::fs::write(
            template_root.join("workflow.toml"),
            format!("[[workflow]]\nentry_module = \"{FROZEN_ENTRY}\"\nentry_function = \"run\"\n"),
        )?;
        let store = Arc::new(InMemoryStore::default());
        let event_store: Arc<dyn EventStore> = store.clone();
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(event_store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_config(
            NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            Arc::clone(&engine),
        );
        let config = runtime_config(workspace.path().to_path_buf(), template_root, auth_enabled);
        #[cfg(feature = "auth")]
        let state = if auth_enabled {
            let mut config = config;
            let jwks_url = serve_jwks()?;
            config.auth.jwks_url = Some(jwks_url.clone());
            let cache = aion_server::auth::JwksCache::new(
                jwks_url,
                Duration::from_secs(config.auth.jwks_refresh_seconds),
            )
            .await?;
            ServerState::from_parts_with_jwks(resolver, config, cache)
        } else {
            ServerState::from_parts(resolver, config)
        };
        #[cfg(not(feature = "auth"))]
        let state = ServerState::from_parts(resolver, config);
        let router = http_router(state.clone())?;
        Ok(Self {
            workspace,
            engine,
            store,
            state,
            router,
        })
    }

    fn workspace(&self) -> &std::path::Path {
        self.workspace.path()
    }
}

fn runtime_config(
    workspace_dir: std::path::PathBuf,
    project_root: std::path::PathBuf,
    auth_enabled: bool,
) -> RuntimeConfig {
    RuntimeConfig {
        listen: ListenConfig {
            grpc: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            http: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        },
        tls: None,
        auth: AuthConfig {
            enabled: auth_enabled,
            jwks_url: Some("direct-test-token".to_owned()),
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
        authoring: AuthoringConfig {
            gleam_path: None,
            project_root: Some(project_root),
            workspace_dir: Some(workspace_dir),
        },
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

#[cfg(feature = "auth")]
const JWT_KEY_ID: &str = "aion-test-key";
#[cfg(feature = "auth")]
const JWT_SECRET: &[u8] = b"aion-test-jwt-shared-secret";

#[cfg(feature = "auth")]
fn serve_jwks() -> Result<String, std::io::Error> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    tokio::spawn(async move {
        let router = axum::Router::new().route(
            "/jwks.json",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "keys": [{
                        "kty": "oct",
                        "kid": JWT_KEY_ID,
                        "alg": "HS256",
                        "k": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(JWT_SECRET),
                    }]
                }))
            }),
        );
        if let Err(error) = axum::serve(listener, router).await {
            eprintln!("fixture JWKS server exited: {error}");
        }
    });
    Ok(format!("http://{address}/jwks.json"))
}

#[cfg(feature = "auth")]
fn mint_token(deploy: bool) -> Result<String, jsonwebtoken::errors::Error> {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(JWT_KEY_ID.to_owned());
    jsonwebtoken::encode(
        &header,
        &json!({
            "sub": "direct-deploy-test",
            "namespace": NAMESPACE,
            "exp": jsonwebtoken::get_current_timestamp() + 3600,
            "deploy": deploy,
        }),
        &EncodingKey::from_secret(JWT_SECRET),
    )
}

fn caller_headers(
    builder: axum::http::request::Builder,
) -> Result<axum::http::request::Builder, TestError> {
    #[cfg(feature = "auth")]
    let bearer = mint_token(false)?;
    #[cfg(not(feature = "auth"))]
    let bearer = "direct-test-token".to_owned();
    let authorization = format!("Bearer {bearer}").parse::<axum::http::HeaderValue>()?;
    Ok(builder
        .header("x-aion-subject", "direct-deploy-test")
        .header("x-aion-namespaces", NAMESPACE)
        .header("authorization", authorization))
}

fn granted(
    builder: axum::http::request::Builder,
) -> Result<axum::http::request::Builder, TestError> {
    #[cfg(feature = "auth")]
    let builder = builder.header("authorization", format!("Bearer {}", mint_token(true)?));
    #[cfg(not(feature = "auth"))]
    let builder = caller_headers(builder)?.header("x-aion-deploy", "true");
    Ok(builder)
}

async fn json_request(
    router: &axum::Router,
    method: &str,
    uri: &str,
    value: Value,
) -> Result<Response, TestError> {
    let request = granted(
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json"),
    )?
    .body(body::Body::from(serde_json::to_vec(&value)?))?;
    Ok(router.clone().oneshot(request).await?)
}

async fn read_json(response: Response) -> Result<Value, TestError> {
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn get_request(router: &axum::Router, uri: &str) -> Result<Response, TestError> {
    let request = granted(Request::builder().method("GET").uri(uri))?.body(body::Body::empty())?;
    Ok(router.clone().oneshot(request).await?)
}

async fn save_document(
    router: &axum::Router,
    path: &str,
    source: &str,
) -> Result<String, TestError> {
    let response = json_request(
        router,
        "PUT",
        &format!("/awl/documents/{path}"),
        json!({ "source": source }),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK, "document save failed");
    let value = read_json(response).await?;
    Ok(value["content_hash"]
        .as_str()
        .ok_or("save response omitted content_hash")?
        .to_owned())
}

async fn deploy_request_as(
    router: &axum::Router,
    path: &str,
    content_hash: &str,
    deploy_granted: bool,
) -> Result<Response, TestError> {
    let builder = Request::builder()
        .method("POST")
        .uri("/awl/deploy")
        .header("content-type", "application/json");
    let builder = if deploy_granted {
        granted(builder)?
    } else {
        caller_headers(builder)?
    };
    let request = builder.body(body::Body::from(serde_json::to_vec(&json!({
        "path": path,
        "content_hash": content_hash,
    }))?))?;
    Ok(router.clone().oneshot(request).await?)
}

async fn deploy_request(
    router: &axum::Router,
    path: &str,
    content_hash: &str,
) -> Result<Response, TestError> {
    deploy_request_as(router, path, content_hash, true).await
}

async fn deploy_document(
    router: &axum::Router,
    path: &str,
    source: &str,
) -> Result<Value, TestError> {
    let content_hash = save_document(router, path, source).await?;
    let response = deploy_request(router, path, &content_hash).await?;
    assert_eq!(response.status(), StatusCode::OK, "AWL deploy failed");
    read_json(response).await
}

fn workflow_source(name: &str, worker: &str, timeout: Option<&str>) -> String {
    let timeout = timeout.map_or_else(String::new, |value| format!("  timeout {value}\n"));
    format!(
        "//! Direct studio deployment fixture.\nworkflow {name}\n{timeout}  input url: String\n  outcome summarized: type Summary, route success\n\ntype Document {{ body: String }}\ntype Summary  {{ text: String }}\n\nworker {worker}\n  action fetch(url: String) -> Document\n  action summarize(body: String) -> Summary\n\nstep fetch_doc\n  fetch(url: url) -> doc\n\nstep summarize_doc after fetch_doc\n  doc |> .body |> summarize |> route summarized\n"
    )
}

fn workerless_source(name: &str) -> String {
    format!(
        "//! Workerless direct deployment fixture.\nworkflow {name}\n  outcome done: type Done, route success\n\ntype Done {{ value: String }}\n\nstep finish\n  route done(value: \"ok\")\n"
    )
}

const LOWER_REFUSED: &str = "//! Focused on-failure lowering refusal (checker-valid; the direct path\n//! refuses `on failure` blocks — one of the three remaining refusal classes\n//! alongside parallel regions and substeps).\nworkflow studio_on_failure_refused\n  input title: String\n  outcome done: type Done, route success\n  outcome failed: type Failed, route failure\n\ntype Done   { title: String }\ntype Failed { reason: String }\n\nworker review\n  action check_doc(title: String) -> Done\n  action undo_check(title: String) -> Nil\n\nstep check_one\n  check_doc(title: title) -> checked\n\n  on failure\n    undo_check(title: title)\n    route failed(reason: \"check failed after compensation\")\n\n  route done(title: checked.title)\n";
const CHECK_REFUSED: &str = "//! Focused checker refusal.\nworkflow studio_check_refused\n  outcome done: type Done, route success\n\ntype Done { value: String }\n\nstep finish\n  route done(value: missing)\n";

async fn seed_package(
    engine: &Engine,
    source: &str,
    root: &std::path::Path,
) -> Result<String, TestError> {
    let prepared = compile_and_assemble_awl(source, root)?;
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    Ok(engine
        .load_package(package)
        .await?
        .record
        .version()
        .to_string())
}

fn revision_files(root: &std::path::Path) -> Result<Vec<(String, Vec<u8>)>, TestError> {
    let directory = root.join(".aion-authoring/revisions");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.push((
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path())?,
            ));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

async fn admission_probe(
    harness: &Harness,
    deploy_granted: bool,
) -> Result<(StatusCode, Value), TestError> {
    let hash = save_document(&harness.router, "admission-refused.awl", LOWER_REFUSED).await?;
    std::fs::remove_file(
        harness
            .workspace()
            .join(".aion-authoring/revisions")
            .join(&hash),
    )?;
    let revisions_before = revision_files(harness.workspace())?;
    let versions_before = harness.engine.list_workflow_versions()?;
    let packages_before = harness.store.list_packages().await?;
    let routes_before = harness.store.list_package_routes().await?;
    let response = deploy_request_as(
        &harness.router,
        "admission-refused.awl",
        &hash,
        deploy_granted,
    )
    .await?;
    let status = response.status();
    let body = read_json(response).await?;
    assert_eq!(revision_files(harness.workspace())?, revisions_before);
    assert_eq!(harness.engine.list_workflow_versions()?, versions_before);
    assert_eq!(harness.store.list_packages().await?, packages_before);
    assert_eq!(harness.store.list_package_routes().await?, routes_before);
    Ok((status, body))
}

#[tokio::test]
async fn awl_unauthorized_lower_refusal_is_denied_before_revision_or_compile()
-> Result<(), TestError> {
    let harness = Harness::new_with_auth(true).await?;
    let (status, body) = admission_probe(&harness, false).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "deploy_denied");
    assert_ne!(body["error_type"], "DirectCompile");
    Ok(())
}

#[tokio::test]
async fn awl_draining_lower_refusal_is_unavailable_before_revision_or_compile()
-> Result<(), TestError> {
    let harness = Harness::new().await?;
    assert!(harness.state.drain_state().begin());
    let (status, body) = admission_probe(&harness, true).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "backend");
    assert_eq!(
        body["message"],
        "server is draining and not accepting authoring submissions"
    );
    assert_ne!(body["error_type"], "DirectCompile");
    Ok(())
}

#[tokio::test]
async fn awl_deploy_identity_e2e_keeps_each_route_and_preserves_a_third() -> Result<(), TestError> {
    let harness = Harness::new().await?;
    let existing = workflow_source("studio_existing", "existing_queue", None);
    let existing_hash = seed_package(&harness.engine, &existing, harness.workspace()).await?;
    let alpha = workflow_source("studio_alpha", "alpha_queue", None);
    let beta = workflow_source("studio_beta", "beta_queue", None);
    let alpha_deploy = deploy_document(&harness.router, "alpha.awl", &alpha).await?;
    let beta_deploy = deploy_document(&harness.router, "beta.awl", &beta).await?;
    assert_eq!(alpha_deploy["deployment"]["workflow_type"], "studio_alpha");
    assert_eq!(alpha_deploy["deployment"]["task_queue"], "alpha_queue");
    assert_eq!(beta_deploy["deployment"]["workflow_type"], "studio_beta");
    assert_eq!(beta_deploy["deployment"]["task_queue"], "beta_queue");
    let alpha_hash = alpha_deploy["deployment"]["package_id"]
        .as_str()
        .ok_or("alpha deploy response omitted package_id")?;
    let beta_hash = beta_deploy["deployment"]["package_id"]
        .as_str()
        .ok_or("beta deploy response omitted package_id")?;
    let versions = harness.engine.list_workflow_versions()?;
    assert_eq!(versions.len(), 3, "each workflow type keeps one version");
    for (workflow_type, package_id) in [
        ("studio_alpha", alpha_hash),
        ("studio_beta", beta_hash),
        ("studio_existing", existing_hash.as_str()),
    ] {
        let version = versions
            .iter()
            .find(|version| version.workflow_type == workflow_type)
            .ok_or("document-owned workflow version was not loaded")?;
        assert_eq!(version.content_hash.to_string(), package_id);
        assert!(version.route_active, "`{workflow_type}` route is inactive");
        assert!(
            version
                .deployed_entry_module
                .starts_with(&format!("{workflow_type}$")),
            "deployed module did not derive from `{workflow_type}`: {}",
            version.deployed_entry_module
        );
    }
    assert!(
        versions
            .iter()
            .all(|version| version.workflow_type != FROZEN_ENTRY),
        "the configured frozen template type must never be loaded"
    );

    let catalog = harness.engine.workflow_catalog();
    assert!(catalog.routed("studio_alpha")?.is_some());
    assert!(catalog.routed("studio_beta")?.is_some());
    assert_eq!(
        catalog
            .routed_version("studio_existing")?
            .map(|version| version.to_string()),
        Some(existing_hash)
    );

    let packages = harness.store.list_packages().await?;
    for workflow_type in ["studio_alpha", "studio_beta"] {
        let record = packages
            .iter()
            .find(|record| record.workflow_type == workflow_type)
            .ok_or("deployed package was not persisted")?;
        let package = Package::load_from_bytes(&record.archive, ExtractionLimits::unbounded())?;
        assert_eq!(package.manifest().entry_module, workflow_type);
    }
    Ok(())
}

/// Bug #21 regression: identity comes from the document's `workflow <name>`
/// declaration alone — never the file name, and never the configured
/// authoring template's frozen entry. A document saved under an existing
/// routed type's file name deploys under its own declared type, the
/// colliding route keeps its original version, and the persisted deployment
/// record read back through the status surface carries the same identity.
#[tokio::test]
async fn awl_deploy_identity_comes_from_the_declaration_not_the_file_name() -> Result<(), TestError>
{
    let harness = Harness::new().await?;
    let existing = workflow_source("studio_existing", "existing_queue", None);
    let existing_hash = seed_package(&harness.engine, &existing, harness.workspace()).await?;
    let gamma = workflow_source("studio_gamma", "gamma_queue", None);
    let deploy = deploy_document(&harness.router, "studio_existing.awl", &gamma).await?;
    let deployment_id = deploy["deployment"]["deployment_id"]
        .as_str()
        .ok_or("deploy response omitted deployment_id")?;
    let status = get_request(&harness.router, &format!("/awl/runs/{deployment_id}")).await?;
    assert_eq!(status.status(), StatusCode::OK);
    let record = read_json(status).await?;
    assert_eq!(record["deployment"]["workflow_type"], "studio_gamma");
    assert_eq!(record["deployment"]["document_path"], "studio_existing.awl");
    let catalog = harness.engine.workflow_catalog();
    let existing_route = catalog
        .routed_version("studio_existing")?
        .map(|version| version.to_string());
    assert_eq!(
        existing_route,
        Some(existing_hash),
        "deploying a colliding file name must not re-route the existing type"
    );
    assert!(catalog.routed("studio_gamma")?.is_some());
    assert!(
        catalog.routed(FROZEN_ENTRY)?.is_none(),
        "the frozen template entry must never gain a route from a studio deploy"
    );
    Ok(())
}

#[tokio::test]
async fn awl_direct_deploy_carries_document_timeout_into_manifest() -> Result<(), TestError> {
    let harness = Harness::new().await?;
    let source = workflow_source("studio_timeout", "timeout_queue", Some("6h"));
    deploy_document(&harness.router, "timeout.awl", &source).await?;

    let record = harness
        .store
        .list_packages()
        .await?
        .into_iter()
        .find(|record| record.workflow_type == "studio_timeout")
        .ok_or("timeout package was not persisted")?;
    let package = Package::load_from_bytes(record.archive, ExtractionLimits::unbounded())?;
    assert_eq!(package.manifest().timeout, Duration::from_secs(21_600));
    Ok(())
}

#[tokio::test]
async fn awl_lower_refusal_is_verbatim_and_mutates_no_catalog_or_route() -> Result<(), TestError> {
    let harness = Harness::new().await?;
    let baseline = workflow_source("studio_baseline", "baseline_queue", None);
    seed_package(&harness.engine, &baseline, harness.workspace()).await?;
    let check = json_request(
        &harness.router,
        "POST",
        "/awl/check",
        json!({ "source": LOWER_REFUSED, "path": "refused.awl" }),
    )
    .await?;
    let check = read_json(check).await?;
    assert_eq!(check["ok"], true);
    assert_eq!(check["deploys_green"], true);

    let content_hash = save_document(&harness.router, "refused.awl", LOWER_REFUSED).await?;
    let versions_before = harness.engine.list_workflow_versions()?;
    let packages_before = harness.store.list_packages().await?;
    let routes_before = harness.store.list_package_routes().await?;
    let expected = match aion_awl::compile(LOWER_REFUSED, harness.workspace()) {
        Err(error) => error.to_string(),
        Ok(_) => return Err("refusal fixture unexpectedly direct-compiled".into()),
    };
    let response = deploy_request(&harness.router, "refused.awl", &content_hash).await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refusal = read_json(response).await?;
    assert_eq!(refusal["message"], expected);
    assert_eq!(refusal["error_type"], "DirectCompile");
    assert_eq!(harness.engine.list_workflow_versions()?, versions_before);
    assert_eq!(harness.store.list_packages().await?, packages_before);
    assert_eq!(harness.store.list_package_routes().await?, routes_before);
    Ok(())
}

#[tokio::test]
async fn awl_checker_refusal_is_verbatim_and_loads_nothing() -> Result<(), TestError> {
    let harness = Harness::new().await?;
    let content_hash = save_document(&harness.router, "check-refused.awl", CHECK_REFUSED).await?;
    let expected = match aion_awl::compile(CHECK_REFUSED, harness.workspace()) {
        Err(error) => error.to_string(),
        Ok(_) => return Err("checker refusal fixture unexpectedly direct-compiled".into()),
    };

    let response = deploy_request(&harness.router, "check-refused.awl", &content_hash).await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refusal = read_json(response).await?;
    assert_eq!(refusal["message"], expected);
    assert_eq!(refusal["error_type"], "DirectCompile");
    assert!(harness.engine.list_workflow_versions()?.is_empty());
    assert!(harness.store.list_packages().await?.is_empty());
    assert!(harness.store.list_package_routes().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn awl_direct_redeploy_is_deterministic_and_workerless_uses_default_queue()
-> Result<(), TestError> {
    let harness = Harness::new().await?;
    let source = workerless_source("studio_deterministic");
    let content_hash = save_document(&harness.router, "deterministic.awl", &source).await?;
    let first =
        read_json(deploy_request(&harness.router, "deterministic.awl", &content_hash).await?)
            .await?;
    let second =
        read_json(deploy_request(&harness.router, "deterministic.awl", &content_hash).await?)
            .await?;

    assert_eq!(
        first["deployment"]["package_id"],
        second["deployment"]["package_id"]
    );
    assert_eq!(first["deployment"]["task_queue"], DEFAULT_TASK_QUEUE);
    assert_eq!(second["deployment"]["task_queue"], DEFAULT_TASK_QUEUE);
    let versions = harness.engine.list_workflow_versions()?;
    assert_eq!(
        versions
            .iter()
            .filter(|version| version.workflow_type == "studio_deterministic")
            .count(),
        1
    );
    Ok(())
}
