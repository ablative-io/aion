//! Deploy subcommand smoke tests against a live in-process `aion-server`
//! gRPC listener (brief §5 test 12): exit codes, JSON output shapes,
//! `--token` / `AION_TOKEN` sourcing and precedence, and actionable
//! rendering of `deploy_denied` and `version_pinned`.
//!
//! The server runs in development-token mode (`auth.enabled = true` without
//! the `auth` feature): the configured `jwks_url` value acts as the shared
//! secret, so token sourcing is exercised for real.

use std::process::{Command, Output};
use std::time::Duration;

use aion_server::ServerState;
use aion_server::api::deploy_grpc::deploy_service;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
    MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use serde_json::{Value, json};

type TestError = Box<dyn std::error::Error>;

const RELOAD_MODULE: &str = "aion_cli_deploy_fixture";
const SECRET: &str = "cli-deploy-secret";

/// Compiles a trivially completing fixture workflow with `erlc` (the engine
/// reload-suite precedent).
fn compile_fixture_beam() -> Result<Vec<u8>, TestError> {
    let temp_dir = std::env::temp_dir().join(format!("aion-cli-deploy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{RELOAD_MODULE}.erl"));
    let beam_path = temp_dir.join(format!("{RELOAD_MODULE}.beam"));
    std::fs::write(
        &source_path,
        format!("-module({RELOAD_MODULE}).\n-export([run/1]).\nrun(_Input) -> 1.\n"),
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

fn write_archive(directory: &std::path::Path) -> Result<std::path::PathBuf, TestError> {
    use aion_package::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, Manifest, ManifestVersion, PackageBuilder,
    };
    let beam = compile_fixture_beam()?;
    let beams = BeamSet::new(vec![BeamModule::new(RELOAD_MODULE, beam)])?;
    let manifest = Manifest {
        entry_module: RELOAD_MODULE.to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Duration::from_secs(30),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    let path = directory.join(format!("{RELOAD_MODULE}.aion"));
    std::fs::write(&path, archive)?;
    Ok(path)
}

fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        listen: ListenConfig {
            grpc: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            http: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        },
        tls: None,
        cors_allowed_origins: Vec::new(),
        auth: AuthConfig {
            enabled: true,
            jwks_url: Some(SECRET.to_owned()),
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
            cluster_broadcast_capacity: Some(64),
        },
        workflow_packages: Vec::new(),
        deploy: DeployConfig {
            enabled: true,
            max_archive_bytes: Some(1_048_576),
            max_inflated_bytes: Some(2_097_152),
        },
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: "default".to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
    }
}

async fn serve_deploy_grpc()
-> Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>), TestError> {
    use tokio_stream::wrappers::TcpListenerStream;

    let state =
        ServerState::build_with_store(aion_store::InMemoryStore::default(), runtime_config())
            .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let service = deploy_service(state)?;
    let handle = tokio::spawn(async move {
        let result = tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;
        if let Err(error) = result {
            eprintln!("fixture deploy server exited: {error}");
        }
    });
    Ok((address, handle))
}

struct CliRun {
    output: Output,
}

impl CliRun {
    fn stdout_json(&self) -> Result<Value, TestError> {
        assert_eq!(
            self.output.status.code(),
            Some(0),
            "expected success, stderr: {}",
            String::from_utf8_lossy(&self.output.stderr)
        );
        Ok(serde_json::from_slice(&self.output.stdout)?)
    }

    fn failure_stderr(&self) -> String {
        assert_eq!(
            self.output.status.code(),
            Some(1),
            "expected failure, stdout: {}",
            String::from_utf8_lossy(&self.output.stdout)
        );
        assert!(
            self.output.stdout.is_empty(),
            "errors must never reach stdout"
        );
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }
}

fn run_cli(
    endpoint: &std::net::SocketAddr,
    env_token: Option<&str>,
    args: &[&str],
) -> Result<CliRun, TestError> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aion"));
    command
        .args(["--endpoint", &endpoint.to_string()])
        .args(args)
        .env_remove("AION_TOKEN");
    if let Some(token) = env_token {
        command.env("AION_TOKEN", token);
    }
    Ok(CliRun {
        output: command.output()?,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deploy_subcommands_drive_a_live_server() -> Result<(), TestError> {
    let temp_dir = tempfile::tempdir()?;
    let archive_path = write_archive(temp_dir.path())?;
    let archive = archive_path.to_string_lossy().into_owned();
    let (address, server) = serve_deploy_grpc().await?;

    // `--token` sourcing: deploy succeeds with the shared secret.
    let deployed = run_cli(&address, None, &["--token", SECRET, "deploy", &archive])?;
    let body = deployed.stdout_json()?;
    assert_eq!(body["workflow_type"], RELOAD_MODULE);
    assert_eq!(body["freshly_loaded"], true);
    assert_eq!(body["route_changed"], true);
    let content_hash = body["content_hash"]
        .as_str()
        .ok_or("deploy output missing content_hash")?
        .to_owned();

    // Idempotent re-deploy reports both flags false.
    let again = run_cli(&address, None, &["--token", SECRET, "deploy", &archive])?;
    let body = again.stdout_json()?;
    assert_eq!(body["freshly_loaded"], false);
    assert_eq!(body["route_changed"], false);

    // `AION_TOKEN` env sourcing: versions succeeds without --token.
    let versions = run_cli(&address, Some(SECRET), &["versions"])?;
    let body = versions.stdout_json()?;
    let rows = body.as_array().ok_or("versions output must be an array")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["content_hash"], content_hash.as_str());
    assert_eq!(rows[0]["route_active"], true);

    // Client-side type filter.
    let filtered = run_cli(
        &address,
        Some(SECRET),
        &["versions", "--workflow-type", "no-such-type"],
    )?;
    let body = filtered.stdout_json()?;
    assert_eq!(body.as_array().map(Vec::len), Some(0));

    // `--token` overrides a wrong AION_TOKEN.
    let precedence = run_cli(
        &address,
        Some("wrong-env-token"),
        &["--token", SECRET, "versions"],
    )?;
    precedence.stdout_json()?;

    // Wrong token: deploy_denied rendered actionably.
    let denied = run_cli(&address, None, &["--token", "wrong", "versions"])?;
    let stderr = denied.failure_stderr();
    assert!(
        stderr.starts_with("error[deploy_denied]: "),
        "denial must render the deploy_denied class: {stderr}"
    );
    assert!(
        stderr.contains("hint:"),
        "denial must carry an actionable hint: {stderr}"
    );

    // Route to the loaded hash succeeds (idempotent re-point).
    let routed = run_cli(
        &address,
        None,
        &["--token", SECRET, "route", RELOAD_MODULE, &content_hash],
    )?;
    let body = routed.stdout_json()?;
    assert_eq!(body["route_active"], true);

    // Unloading the route-active version renders version_pinned actionably.
    let pinned = run_cli(
        &address,
        None,
        &["--token", SECRET, "unload", RELOAD_MODULE, &content_hash],
    )?;
    let stderr = pinned.failure_stderr();
    assert!(
        stderr.starts_with("error[version_pinned]: "),
        "pin refusal must render the version_pinned class: {stderr}"
    );
    assert!(
        stderr.contains("server error type: RouteActive"),
        "pin refusal must carry the typed variant: {stderr}"
    );
    assert!(
        stderr.contains("hint:"),
        "pin refusal must carry an actionable hint: {stderr}"
    );

    // Route to an unknown hash renders not_found with the listing hint.
    let unknown = run_cli(
        &address,
        None,
        &["--token", SECRET, "route", RELOAD_MODULE, &"b".repeat(64)],
    )?;
    let stderr = unknown.failure_stderr();
    assert!(
        stderr.starts_with("error[not_found]: failed to route workflow version: "),
        "unknown version must render not_found with context: {stderr}"
    );

    server.abort();
    Ok(())
}
