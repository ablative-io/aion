//! Deploy audit-log proof (brief test plan item 11, log half): one
//! structured tracing line per mutation with who/what/version/outcome, and a
//! warn per denial.
//!
//! Lives in its own test binary so the thread-local capture subscriber never
//! races other tests' tracing callsite state.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, Manifest, ManifestVersion, PackageBuilder,
};
use aion_server::api::http::http_router;
use aion_server::config::{
    AuthConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig, MetricsConfig,
    NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use aion_server::{NamespaceResolver, ServerState};
use axum::{body, http::Request, http::StatusCode};
use serde_json::json;
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const RELOAD_MODULE: &str = "aion_reload_fixture";
const NAMESPACE: &str = "default";

/// Compiles the reload fixture returning `version` from both entrypoints.
fn compile_reload_beam(version: u32) -> Result<Vec<u8>, TestError> {
    let temp_dir = std::env::temp_dir().join(format!("aion-deploy-audit-{}", uuid::Uuid::new_v4()));
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
        timeout: Duration::from_secs(30),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    Ok(PackageBuilder::new(manifest, beams).write_to_bytes()?)
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
        deploy,
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: true },
    }
}

fn enabled_deploy() -> DeployConfig {
    DeployConfig {
        enabled: true,
        max_archive_bytes: Some(1_048_576),
    }
}

/// Real engine the test holds directly, wrapped in a served state.
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

fn post_archive(archive: Vec<u8>) -> Result<Request<body::Body>, TestError> {
    Ok(Request::builder()
        .uri("/deploy/packages")
        .method("POST")
        .header("content-type", "application/octet-stream")
        .header("x-aion-subject", "ci")
        .header("x-aion-namespaces", NAMESPACE)
        .header("x-aion-deploy", "true")
        .body(body::Body::from(archive))?)
}

/// Audit: one structured line per mutation carrying who/what/version/outcome,
/// and a warn per denial (test plan item 11's log half).
#[tokio::test]
async fn deploy_mutations_emit_structured_audit_lines() -> Result<(), TestError> {
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let Ok(mut inner) = self.0.lock() else {
                return Err(std::io::Error::other("capture lock poisoned"));
            };
            inner.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let (engine, state) = engine_state(enabled_deploy()).await?;
    let router = http_router(state)?;
    let beam = compile_reload_beam(1)?;
    let archive = archive_bytes(&beam, "run")?;
    let response = router.clone().oneshot(post_archive(archive)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let denied = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/deploy/versions")
                .method("GET")
                .header("x-aion-subject", "mallory")
                .body(body::Body::empty())?,
        )
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    engine.shutdown()?;
    drop(guard);

    let logged = String::from_utf8(
        capture
            .0
            .lock()
            .map_err(|_| "capture lock poisoned")?
            .clone(),
    )?;
    let audit_line = logged
        .lines()
        .find(|line| line.contains("deploy mutation applied"))
        .ok_or("expected an audit line per mutation")?;
    for needle in [
        "operation=\"deploy.load\"",
        "subject=\"ci\"",
        "grant_source=\"header\"",
        "transport=\"http\"",
        "outcome=\"loaded\"",
        "freshly_loaded=true",
        "route_changed=true",
        RELOAD_MODULE,
    ] {
        assert!(
            audit_line.contains(needle),
            "audit line must carry {needle}: {audit_line}"
        );
    }
    let denial_line = logged
        .lines()
        .find(|line| line.contains("deploy operation denied"))
        .ok_or("expected a warn line per denial")?;
    assert!(
        denial_line.contains("subject=\"mallory\"") && denial_line.contains("WARN"),
        "denial must warn with the subject: {denial_line}"
    );
    Ok(())
}
