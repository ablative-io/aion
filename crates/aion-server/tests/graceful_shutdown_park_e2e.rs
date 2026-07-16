//! Graceful-shutdown parking end-to-end (#207): SIGTERM converges on kill -9.
//!
//! The incident: a graceful drain SYNTHESIZED a retryable lost-worker
//! `ActivityFailed` for every in-flight activity; without a retry policy the
//! failure was delivered to the workflow as terminal → `WorkflowFailed` →
//! excluded from `list_active` respawn forever. A kill -9 — which records
//! nothing — recovered cleanly. These tests drive the REAL drain coordinator
//! (`shutdown::drain_after_first_signal`, the exact function the SIGTERM
//! listener calls) over a full `ServerState` with an engine, a real libSQL
//! store, a real workflow whose remote activity a real gRPC worker stream
//! holds in flight, and prove BOTH drain paths park instead of failing:
//!
//! - the stream-teardown path (the worker obeys the drain request and ends its
//!   stream) — `teardown_worker_stream`'s drain branch;
//! - the drain-timeout backstop (the worker ignores the drain and outlives the
//!   window) — `park_all_in_flight_workers` → `ShutdownOutcome::Parked` →
//!   `ExitCode::SUCCESS`.
//!
//! The convergence proof: the durable history AFTER the drain is byte-for-byte
//! the history captured BEFORE it (the dangling scheduled/started trail a
//! kill -9 leaves — no `ActivityFailed`, no `WorkflowFailed`), and a fresh
//! server epoch over the same store recovers the run, re-dispatches the
//! activity to a reconnected worker at the CONTINUED attempt, and COMPLETES
//! the workflow.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use aion_core::Event;
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder,
};
use aion_proto::generated::worker_protocol_client::WorkerProtocolClient;
use aion_proto::generated::{self, server_to_worker, worker_to_server};
use aion_server::ServerState;
use aion_server::api::http::http_router;
use aion_server::api::worker_grpc::worker_service;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::shutdown::{self, ShutdownOutcome};
use aion_store::ReadableEventStore;
use aion_store_libsql::LibSqlStore;
use axum::body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
const PARK_MODULE: &str = "aion_park_fixture";
const ACTIVITY_TYPE: &str = "greet";
const POLL_DEADLINE: Duration = Duration::from_secs(20);

/// Compiles the park fixture: one workflow that dispatches a single remote
/// `greet` activity and completes with `7` once its result arrives — the
/// exact in-flight-suspension shape the incident's `dev_brief` round had.
fn compile_park_beam() -> Result<Vec<u8>, TestError> {
    let temp_dir = std::env::temp_dir().join(format!("aion-park-fixture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{PARK_MODULE}.erl"));
    let beam_path = temp_dir.join(format!("{PARK_MODULE}.beam"));
    std::fs::write(
        &source_path,
        format!(
            "-module({PARK_MODULE}).\n\
             -export([single_activity/1]).\n\
             single_activity(_Input) ->\n\
             {{ok, Correlation}} = aion_flow_ffi:dispatch_activity(\n\
                 <<\"{ACTIVITY_TYPE}\">>, <<\"{{}}\">>, <<\"{{}}\">>),\n\
             {{ok, _Result}} = aion_flow_ffi:await_activity_result(Correlation),\n\
             7.\n"
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

/// Write the compiled fixture package to disk so `ServerState::build_with_store`
/// loads it exactly as production loads operator-supplied `workflow_packages`.
fn write_package_archive(dir: &std::path::Path, beam: &[u8]) -> Result<PathBuf, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(PARK_MODULE, beam.to_vec())])?;
    let manifest = Manifest {
        entry_module: PARK_MODULE.to_owned(),
        entry_function: "single_activity".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Duration::from_secs(60),
        activities: vec![DeclaredActivity {
            activity_type: ACTIVITY_TYPE.to_owned(),
        }],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    let path = dir.join("park_fixture.aion");
    std::fs::write(&path, archive)?;
    Ok(path)
}

fn runtime_config(package_path: PathBuf, drain_timeout: Duration) -> RuntimeConfig {
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
        workflow_packages: vec![package_path],
        deploy: DeployConfig::default(),
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        outbox: aion_server::config::OutboxConfig::default(),
        observability: aion_server::config::ObservabilityConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout,
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

/// One raw worker gRPC stream registered for [`ACTIVITY_TYPE`], with full
/// control over its lifetime — the tests decide whether it obeys the drain
/// request (ends its stream) or ignores it (holds the stream open).
struct WorkerSession {
    worker_tx: tokio::sync::mpsc::Sender<generated::WorkerToServer>,
    inbound: tonic::Streaming<generated::ServerToWorker>,
}

impl WorkerSession {
    async fn connect(address: SocketAddr) -> Result<Self, TestError> {
        let mut client = WorkerProtocolClient::connect(format!("http://{address}")).await?;
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<generated::WorkerToServer>(8);
        worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Register(
                    generated::RegisterWorker {
                        namespaces: vec![NAMESPACE.to_owned()],
                        activity_types: vec![ACTIVITY_TYPE.to_owned()],
                        task_queue: TASK_QUEUE.to_owned(),
                        node: String::new(),
                    },
                )),
            })
            .await?;
        let mut request = tonic::Request::new(ReceiverStream::new(worker_rx));
        request
            .metadata_mut()
            .insert("x-aion-namespaces", NAMESPACE.parse()?);
        let mut inbound = client.stream_worker(request).await?.into_inner();
        let first = inbound
            .message()
            .await?
            .and_then(|frame| frame.message)
            .ok_or("response stream ended before the RegisterAck")?;
        let server_to_worker::Message::RegisterAck(_) = first else {
            return Err(format!("first response frame must be RegisterAck, got {first:?}").into());
        };
        Ok(Self { worker_tx, inbound })
    }

    /// Wait for the next `ActivityTask` pushed down the worker stream.
    async fn next_task(&mut self) -> Result<generated::ActivityTask, TestError> {
        loop {
            let frame = tokio::time::timeout(POLL_DEADLINE, self.inbound.message())
                .await
                .map_err(|_| "timed out waiting for an activity task on the worker stream")??;
            match frame.and_then(|message| message.message) {
                Some(server_to_worker::Message::Task(task)) => return Ok(task),
                Some(_) => {}
                None => return Err("worker stream closed before a task was delivered".into()),
            }
        }
    }

    /// Wait for the server's drain request, then END the stream (drop both
    /// halves) — the obedient-worker response that fires the stream-teardown
    /// park path. `String`-typed errors so the whole future is `Send`able onto
    /// a spawned task.
    async fn obey_drain_and_disconnect(mut self) -> Result<(), String> {
        loop {
            let frame = tokio::time::timeout(POLL_DEADLINE, self.inbound.message())
                .await
                .map_err(|_| "timed out waiting for the drain request".to_owned())?
                .map_err(|status| status.to_string())?;
            match frame.and_then(|message| message.message) {
                Some(server_to_worker::Message::Drain(_)) => {
                    drop(self.worker_tx);
                    drop(self.inbound);
                    return Ok(());
                }
                Some(_) => {}
                None => return Err("worker stream closed before the drain request".to_owned()),
            }
        }
    }

    /// Report a successful activity result back over the worker stream.
    async fn complete(
        &self,
        task: &generated::ActivityTask,
        result_json: &[u8],
    ) -> Result<(), TestError> {
        self.worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Result(
                    generated::ActivityResult {
                        workflow_id: task.workflow_id.clone(),
                        activity_id: task.activity_id,
                        run_id: task.run_id.clone(),
                        outcome: Some(generated::activity_result::Outcome::Result(
                            generated::Payload {
                                content_type: "application/json".to_owned(),
                                bytes: result_json.to_vec(),
                            },
                        )),
                    },
                )),
            })
            .await?;
        Ok(())
    }
}

/// One in-process server epoch: full `ServerState` (engine + recovery) over
/// the shared libSQL file, the real worker gRPC service on loopback, and the
/// real HTTP router.
struct ServerEpoch {
    state: ServerState,
    router: axum::Router,
    grpc_address: SocketAddr,
    grpc_server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl ServerEpoch {
    async fn start(
        db_path: &std::path::Path,
        package_path: PathBuf,
        drain_timeout: Duration,
    ) -> Result<Self, TestError> {
        let store = LibSqlStore::open(db_path.to_path_buf()).await?;
        let state =
            ServerState::build_with_store(store, runtime_config(package_path, drain_timeout))
                .await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let grpc_address = listener.local_addr()?;
        let grpc_server = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(worker_service(state.clone()))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let router = http_router(state.clone())?;
        Ok(Self {
            state,
            router,
            grpc_address,
            grpc_server,
        })
    }
}

async fn start_over_http(router: &axum::Router) -> Result<aion_core::WorkflowId, TestError> {
    let request = Request::builder()
        .uri("/workflows/start")
        .method("POST")
        .header("content-type", "application/json")
        .header("x-aion-subject", "ci")
        .header("x-aion-namespaces", NAMESPACE)
        .body(body::Body::from(serde_json::to_vec(&json!({
            "namespace": NAMESPACE,
            "workflow_type": PARK_MODULE,
            "input": {},
        }))?))?;
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "workflow start over HTTP must succeed: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: serde_json::Value = serde_json::from_slice(&bytes)?;
    let workflow_id = body["workflow_id"]
        .as_str()
        .ok_or("start response missing workflow id")?
        .parse::<uuid::Uuid>()?;
    Ok(aion_core::WorkflowId::new(workflow_id))
}

async fn wait_for_history<F>(
    reader: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, TestError>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let history = reader.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn event_kinds(history: &[Event]) -> Vec<&'static str> {
    history
        .iter()
        .map(|event| match event {
            Event::WorkflowStarted { .. } => "WorkflowStarted",
            Event::ActivityScheduled { .. } => "ActivityScheduled",
            Event::ActivityStarted { .. } => "ActivityStarted",
            Event::ActivityCompleted { .. } => "ActivityCompleted",
            Event::ActivityFailed { .. } => "ActivityFailed",
            Event::WorkflowCompleted { .. } => "WorkflowCompleted",
            Event::WorkflowFailed { .. } => "WorkflowFailed",
            _ => "Other",
        })
        .collect()
}

fn assert_no_synthesized_terminals(history: &[Event]) {
    assert!(
        !history
            .iter()
            .any(|event| matches!(event, Event::ActivityFailed { .. })),
        "a graceful drain must record NO ActivityFailed for an in-flight dispatch: {:?}",
        event_kinds(history)
    );
    assert!(
        !history
            .iter()
            .any(|event| matches!(event, Event::WorkflowFailed { .. })),
        "a graceful drain must record NO WorkflowFailed: {:?}",
        event_kinds(history)
    );
}

/// Restart over the same store and drive the recovered run to completion: the
/// re-dispatched activity reaches a reconnected worker at the CONTINUED
/// attempt, completes, and the workflow completes — the recovery half of the
/// kill -9 convergence proof.
async fn restart_and_complete(
    db_path: &std::path::Path,
    package_path: PathBuf,
    workflow_id: &aion_core::WorkflowId,
) -> Result<(), TestError> {
    let epoch = ServerEpoch::start(db_path, package_path, Duration::from_secs(30)).await?;
    let mut worker = WorkerSession::connect(epoch.grpc_address).await?;
    let task = worker.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
    assert_eq!(
        task.attempt, 2,
        "the post-restart re-dispatch must CONTINUE the recorded attempt trail"
    );
    worker.complete(&task, br#""recovered""#).await?;

    let reader = LibSqlStore::open(db_path.to_path_buf()).await?;
    let settled = wait_for_history(&reader, workflow_id, "post-restart completion", |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::WorkflowCompleted { .. }))
    })
    .await?;
    assert_no_synthesized_terminals(&settled);
    assert_eq!(
        settled
            .iter()
            .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
            .count(),
        1,
        "exactly one activity terminal: {:?}",
        event_kinds(&settled)
    );

    drop(worker);
    epoch.grpc_server.abort();
    epoch.state.shutdown()?;
    Ok(())
}

/// #207 stream-teardown park: the worker OBEYS the drain request and ends its
/// stream; the teardown parks the in-flight dispatch (no `ActivityFailed`, no
/// `WorkflowFailed`), the drain completes Clean → SUCCESS, the post-drain
/// durable history is BYTE-IDENTICAL to the pre-drain snapshot (the kill -9
/// shape), and a restart over the same store completes the workflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_drain_parks_in_flight_activity_and_restart_completes() -> Result<(), TestError> {
    let dir = tempfile::Builder::new()
        .prefix("aion-graceful-park-")
        .tempdir()?;
    let db_path = dir.path().join("aion.db");
    let package_path = write_package_archive(dir.path(), &compile_park_beam()?)?;

    let epoch = ServerEpoch::start(&db_path, package_path.clone(), Duration::from_secs(30)).await?;
    let mut worker = WorkerSession::connect(epoch.grpc_address).await?;
    let workflow_id = start_over_http(&epoch.router).await?;
    let task = worker.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);

    // The kill -9 reference snapshot: the durable log at the moment the worker
    // holds the dispatch in flight (dangling scheduled/started, no outcome).
    let reader = LibSqlStore::open(db_path.clone()).await?;
    let snapshot = wait_for_history(&reader, &workflow_id, "in-flight trail", |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::ActivityStarted { .. }))
    })
    .await?;

    // The worker obeys the drain request the coordinator broadcasts.
    let obedient_worker = tokio::spawn(worker.obey_drain_and_disconnect());

    // THE code path SIGTERM triggers (run.rs routes the first signal here).
    let outcome = tokio::time::timeout(
        POLL_DEADLINE,
        shutdown::drain_after_first_signal(epoch.state.clone(), std::future::pending()),
    )
    .await
    .map_err(|_| "graceful drain did not complete")??;
    assert_eq!(
        outcome,
        ShutdownOutcome::Clean,
        "an obeyed drain completes cleanly (tasks parked as the stream tore down)"
    );
    obedient_worker.await.map_err(|error| error.to_string())??;

    // Convergence: the drain recorded NOTHING — the post-drain history is the
    // pre-drain snapshot, byte for byte (exactly what a kill -9 leaves).
    let post_drain = reader.read_history(&workflow_id).await?;
    assert_eq!(
        post_drain, snapshot,
        "a graceful drain must leave the durable log byte-identical to a kill -9"
    );
    assert_no_synthesized_terminals(&post_drain);

    epoch.grpc_server.abort();
    drop(epoch);

    restart_and_complete(&db_path, package_path, &workflow_id).await
}

/// #207 drain-timeout backstop: the worker IGNORES the drain and outlives the
/// window; the backstop PARKS every in-flight task — `ShutdownOutcome::Parked`
/// → `ExitCode::SUCCESS` (parked state is recoverable by design, not a
/// failure) — records nothing, and a restart over the same store completes
/// the workflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_timeout_parks_and_exits_success_and_restart_completes() -> Result<(), TestError> {
    let dir = tempfile::Builder::new()
        .prefix("aion-timeout-park-")
        .tempdir()?;
    let db_path = dir.path().join("aion.db");
    let package_path = write_package_archive(dir.path(), &compile_park_beam()?)?;

    // A short drain window the held-open worker stream is guaranteed to outlive.
    let epoch =
        ServerEpoch::start(&db_path, package_path.clone(), Duration::from_millis(500)).await?;
    let mut worker = WorkerSession::connect(epoch.grpc_address).await?;
    let workflow_id = start_over_http(&epoch.router).await?;
    let task = worker.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);

    let reader = LibSqlStore::open(db_path.clone()).await?;
    let snapshot = wait_for_history(&reader, &workflow_id, "in-flight trail", |events| {
        events
            .iter()
            .any(|event| matches!(event, Event::ActivityStarted { .. }))
    })
    .await?;

    // The worker deliberately ignores the drain: its stream stays open, so
    // ONLY the timeout backstop can end the drain.
    let outcome = tokio::time::timeout(
        POLL_DEADLINE,
        shutdown::drain_after_first_signal(epoch.state.clone(), std::future::pending()),
    )
    .await
    .map_err(|_| "drain-timeout backstop did not fire")??;
    assert_eq!(
        outcome,
        ShutdownOutcome::Parked,
        "an outlived drain window parks the remaining work — the recoverable-by-design outcome"
    );
    assert_eq!(
        format!("{:?}", outcome.exit_code()),
        format!("{:?}", std::process::ExitCode::SUCCESS),
        "a fully-parked drain is a routine deploy, not a failure"
    );

    let post_drain = reader.read_history(&workflow_id).await?;
    assert_eq!(
        post_drain, snapshot,
        "the timeout backstop must leave the durable log byte-identical to a kill -9"
    );
    assert_no_synthesized_terminals(&post_drain);

    drop(worker);
    epoch.grpc_server.abort();
    drop(epoch);

    restart_and_complete(&db_path, package_path, &workflow_id).await
}
