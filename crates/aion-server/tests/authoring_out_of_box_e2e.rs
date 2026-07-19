//! Out-of-box regressions for the stock AWL studio configuration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aion::EngineBuilder;
use aion_server::api::http::http_router;
use aion_server::config::{DEFAULT_AUTHORING_WORKSPACE_DIR, NamespaceConfig, ServerConfig};
use aion_server::{NamespaceResolver, ServerState};
use aion_store::{EventStore, InMemoryStore};
use axum::{
    body,
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const SOURCE: &str = "//! Stock studio round trip.\nworkflow stock_studio\n  outcome done: type Done, route success\n\ntype Done { value: String }\n\nstep finish\n  route done(value: \"ok\")\n";

struct Harness {
    scratch: tempfile::TempDir,
    workspace: PathBuf,
    router: axum::Router,
}

impl Harness {
    async fn from_toml(toml: &[u8]) -> Result<Self, TestError> {
        let scratch = private_tempdir()?;
        let config = ServerConfig::from_slice_with_home(toml, scratch.path())?;
        let (_, mut runtime) = config.into_parts();
        let configured = runtime
            .authoring
            .workspace_dir
            .as_deref()
            .ok_or("stock config omitted the AWL workspace")?;
        let workspace = resolve_from_working_dir(scratch.path(), configured);
        runtime.authoring.workspace_dir = Some(workspace.clone());

        let store = Arc::new(InMemoryStore::default());
        let event_store: Arc<dyn EventStore> = store;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(event_store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver =
            NamespaceResolver::from_config(NamespaceConfig::default(), Arc::clone(&engine));
        let state = ServerState::from_parts(resolver, runtime);
        let router = http_router(state)?;
        Ok(Self {
            scratch,
            workspace,
            router,
        })
    }

    fn scratch(&self) -> &Path {
        self.scratch.path()
    }
}

fn resolve_from_working_dir(working_dir: &Path, configured: &Path) -> PathBuf {
    if configured.is_relative() {
        working_dir.join(configured)
    } else {
        configured.to_owned()
    }
}

fn authorized(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder
        .header("x-aion-subject", "stock-studio-test")
        .header("x-aion-namespaces", "default")
        .header("x-aion-deploy", "true")
}

async fn request(
    router: &axum::Router,
    method: &str,
    uri: &str,
    value: Option<&Value>,
) -> Result<Response, TestError> {
    let mut builder = authorized(Request::builder().method(method).uri(uri));
    let payload = match value {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            body::Body::from(serde_json::to_vec(value)?)
        }
        None => body::Body::empty(),
    };
    Ok(router.clone().oneshot(builder.body(payload)?).await?)
}

async fn response_json(response: Response) -> Result<Value, TestError> {
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn request_json(
    router: &axum::Router,
    method: &str,
    uri: &str,
    value: Option<&Value>,
    expected: StatusCode,
) -> Result<Value, TestError> {
    let response = request(router, method, uri, value).await?;
    assert_eq!(response.status(), expected, "{method} {uri} failed");
    response_json(response).await
}

#[tokio::test]
async fn empty_toml_mounts_empty_awl_workspace_while_gleam_routes_stay_dark()
-> Result<(), TestError> {
    let harness = Harness::from_toml(b"").await?;
    assert_eq!(
        harness.workspace,
        harness.scratch().join(DEFAULT_AUTHORING_WORKSPACE_DIR)
    );
    assert!(!harness.workspace.exists());

    let documents = request_json(
        &harness.router,
        "GET",
        "/awl/documents",
        None,
        StatusCode::OK,
    )
    .await?;
    assert_eq!(documents, json!([]));
    assert!(!harness.workspace.exists());

    let gleam = request(
        &harness.router,
        "POST",
        "/authoring/compile",
        Some(&json!({ "source": "pub fn main() { Nil }" })),
    )
    .await?;
    assert_eq!(gleam.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn stock_config_studio_save_check_deploy_revision_and_status_round_trip()
-> Result<(), TestError> {
    let harness = Harness::from_toml(b"").await?;
    let saved = request_json(
        &harness.router,
        "PUT",
        "/awl/documents/stock.awl",
        Some(&json!({ "source": SOURCE })),
        StatusCode::OK,
    )
    .await?;
    let content_hash = saved["content_hash"]
        .as_str()
        .ok_or("document save omitted content_hash")?;

    let checked = request_json(
        &harness.router,
        "POST",
        "/awl/check",
        Some(&json!({ "source": SOURCE, "path": "stock.awl" })),
        StatusCode::OK,
    )
    .await?;
    assert_eq!(checked["ok"], true);
    assert_eq!(checked["deploys_green"], true);

    let deployed = request_json(
        &harness.router,
        "POST",
        "/awl/deploy",
        Some(&json!({ "path": "stock.awl", "content_hash": content_hash })),
        StatusCode::OK,
    )
    .await?;
    let deployment_id = deployed["deployment"]["deployment_id"]
        .as_str()
        .ok_or("deploy response omitted deployment_id")?;
    assert_eq!(deployed["deployment"]["workflow_type"], "stock_studio");

    let revision = request_json(
        &harness.router,
        "GET",
        &format!("/awl/revisions/{content_hash}"),
        None,
        StatusCode::OK,
    )
    .await?;
    assert_eq!(revision["content_hash"], content_hash);
    assert_eq!(revision["source"], SOURCE);

    let status = request_json(
        &harness.router,
        "GET",
        &format!("/awl/runs/{deployment_id}"),
        None,
        StatusCode::OK,
    )
    .await?;
    assert_eq!(status["deployment"]["deployment_id"], deployment_id);
    assert_eq!(status["deployed_source"], SOURCE);
    assert_eq!(status["drifted"], false);

    assert_eq!(
        std::fs::read_to_string(harness.workspace.join("stock.awl"))?,
        SOURCE
    );
    assert!(
        harness
            .workspace
            .join(".aion-authoring/revisions")
            .join(content_hash)
            .is_file()
    );
    assert!(
        harness
            .workspace
            .join(".aion-authoring/deployments")
            .join(format!("{deployment_id}.json"))
            .is_file()
    );
    Ok(())
}

#[tokio::test]
async fn explicit_workspace_is_honored_and_traversal_is_typed_and_confined() -> Result<(), TestError>
{
    let harness = Harness::from_toml(
        br#"
            [authoring]
            workspace_dir = "custom-studio"
        "#,
    )
    .await?;
    assert_eq!(harness.workspace, harness.scratch().join("custom-studio"));

    request_json(
        &harness.router,
        "PUT",
        "/awl/documents/explicit.awl",
        Some(&json!({ "source": SOURCE })),
        StatusCode::OK,
    )
    .await?;
    assert!(harness.workspace.join("explicit.awl").is_file());
    assert!(
        !harness
            .scratch()
            .join(DEFAULT_AUTHORING_WORKSPACE_DIR)
            .exists()
    );

    let refusal = request_json(
        &harness.router,
        "PUT",
        "/awl/documents/nested/%2E%2E/escape.awl",
        Some(&json!({ "source": SOURCE })),
        StatusCode::BAD_REQUEST,
    )
    .await?;
    assert_eq!(refusal["error_type"], "InvalidDocumentPath");
    assert!(!harness.scratch().join("escape.awl").exists());
    Ok(())
}

/// Umask-independent private temporary directory: the server's private-root
/// validation requires sensitive roots to be `0700`, while
/// `tempfile::tempdir` inherits the process umask.
fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}
