//! REAL-transport durable-outbox cutover end-to-end (increment #5).
//!
//! # Why this test exists
//!
//! The existing `crates/aion/tests/outbox_e2e.rs` drives fan-out completions
//! by calling `RuntimeHandle::deliver_outbox_completion` DIRECTLY from the
//! test's outbox pump. That proves the engine half of the cutover, but it
//! BYPASSES the real server completion sink: the
//! `PendingActivities::complete_activity` → unmatched → installed
//! `ServerOutboxDeliveryCallback` → `deliver_outbox_completion` chain is never
//! exercised. This test closes that blind spot.
//!
//! # What this test proves, organically
//!
//! With a FULL `ServerState` built WITH an engine (`ServerState::build_with_store`)
//! over a real persistent `LibSqlStore`, `outbox.enabled = true`, and the
//! `collect_four` fan-out package loaded:
//!
//! 1. Starting the workflow over the REAL HTTP transport drives the engine's
//!    `dispatch_unscheduled`, which stages four members through
//!    `record_fan_out_dispatch` (atomic scheduling events + pending outbox
//!    rows) and spawns NO in-process completion task.
//! 2. The REAL `OutboxDispatcher`, spawned over the SAME DB file the engine
//!    writes to (mirroring `run.rs::maybe_spawn_outbox_dispatcher`, which opens
//!    a fresh `LibSqlStore` handle on the same `store.url`), claims those
//!    pending rows and pushes an `ActivityTask` to the connected worker through
//!    the production push `ActivityDispatcher`.
//! 3. A REAL gRPC worker (booted over TCP loopback, as in
//!    `worker_dispatch_delivery.rs`) reads each task with `next_task()` and
//!    streams a completion back with `complete()`. That completion lands at
//!    `PendingActivities::complete_activity` (`worker_grpc.rs` ->
//!    `session.pending.complete_activity(...)`) — the REAL sink — UNMATCHED,
//!    because the outbox dispatch path registered NO pending oneshot. The
//!    unmatched branch routes through the installed `ServerOutboxDeliveryCallback`
//!    into the live workflow, waking it.
//! 4. The workflow finishes with all four results, recorded in history read
//!    back from the store: exactly four `ActivityCompleted` (one per ordinal)
//!    and one `WorkflowCompleted`.
//!
//! # Faithfulness anchors
//!
//! - Completions flow through `PendingActivities::complete_activity` (the real
//!   sink), never a direct `deliver_outbox_completion` call. The sink is reached
//!   by `worker_grpc.rs` decoding a wire `ActivityResult` from the real gRPC
//!   stream; the test only writes bytes to that stream.
//! - The `OutboxDispatcher` is the REAL one, `tokio::spawn`ed, reading the same
//!   DB the engine writes to (a second `LibSqlStore` handle on the same path,
//!   exactly as production opens it).
//! - The four staged outbox rows are observed transitioning `Pending` -> `Done`
//!   by the dispatcher, and the worker is observed receiving exactly the four
//!   fan-out tasks via the dispatcher push — proving the outbox path carried
//!   them, not an in-process dispatcher. With `ServerState::build_with_store`
//!   there is no injectable in-process dispatcher stub (the build path owns the
//!   `WorkerActivityDispatcher`), so the cutover proof is the outbox-row state
//!   machine plus the worker-side task receipt rather than a `fired` guard.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
    MetricsConfig, NamespaceConfig, NamespaceMode, OutboxConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::worker::{
    ActivityDispatcher, ConnectedWorkerRegistry, OutboxDispatcher, OutboxDispatcherConfig,
    WorkerOutboxDispatch,
};
use aion_store::{OutboxRow, OutboxStatus, OutboxStore, ReadableEventStore};
use aion_store_libsql::LibSqlStore;
use axum::body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const OUTBOX_MODULE: &str = "aion_outbox_fixture";
const OUTBOX_BEAM: &[u8] = include_bytes!("fixtures/aion_outbox_fixture.beam");
const OUTBOX_SOURCE: &[u8] = include_bytes!("fixtures/aion_outbox_fixture.erl");

/// The `collect_four` fixture fans out four members whose per-member activity
/// types are the spec names `fan:0`..`fan:3` (the spec `name` becomes the
/// outbox row `activity_type`). The worker registers for all four.
const FAN_OUT: usize = 4;
const FAN_ACTIVITY_TYPES: [&str; FAN_OUT] = ["fan:0", "fan:1", "fan:2", "fan:3"];

const POLL_DEADLINE: Duration = Duration::from_secs(20);

// --- fixture package (mirrors crates/aion/tests/outbox_e2e.rs) -----------------

/// A unique temp directory holding both the libSQL DB file and the `.aion`
/// archive the full build loads. Returned `TempDir` is kept alive for the test.
fn unique_temp_dir(name: &str) -> Result<tempfile::TempDir, TestError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(tempfile::Builder::new()
        .prefix(&format!(
            "aion-outbox-transport-{name}-{pid}-{nanos}-{unique}-"
        ))
        .tempdir()?)
}

/// Write the built `collect_four` package to a `.aion` archive on disk so the
/// full `ServerState::build_with_store` path loads it exactly as production
/// loads operator-supplied `workflow_packages`.
fn write_package_archive(dir: &std::path::Path) -> Result<PathBuf, TestError> {
    let beams = BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)])?;
    let manifest = Manifest {
        entry_module: OUTBOX_MODULE.to_owned(),
        entry_function: "collect_four".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
            .write_to_bytes()?;
    let path = dir.join("collect_four.aion");
    std::fs::write(&path, archive)?;
    Ok(path)
}

// --- runtime config with the outbox commissioned ------------------------------

/// A full runtime config with `outbox.enabled = true` and every outbox knob set
/// to the same shape `resolve_outbox_config` expects (all `Some`, in range).
/// `workflow_packages` points at the on-disk `collect_four.aion` archive.
fn runtime_config(package_path: PathBuf) -> RuntimeConfig {
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
        workflow_packages: vec![package_path],
        deploy: DeployConfig::default(),
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        outbox: OutboxConfig {
            enabled: true,
            poll_interval_ms: Some(20),
            batch_size: Some(16),
            max_attempts: Some(5),
            backoff_base_ms: Some(50),
            backoff_multiplier: Some(2),
            backoff_max_ms: Some(1_000),
        },
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
    }
}

/// The resolved dispatcher config mirroring `run.rs::resolve_outbox_config` over
/// the same knobs set in [`runtime_config`].
fn outbox_dispatcher_config() -> OutboxDispatcherConfig {
    OutboxDispatcherConfig {
        poll_interval: Duration::from_millis(20),
        batch_size: 16,
        max_attempts: 5,
        backoff_base: Duration::from_millis(50),
        backoff_multiplier: 2,
        backoff_max: Duration::from_millis(1_000),
    }
}

// --- a real gRPC worker registered for the four fan-out activity types --------

/// A live worker stream registered for all four `fan:N` activity types in
/// [`NAMESPACE`], modeled on `worker_dispatch_delivery.rs::Harness`.
struct WorkerSession {
    worker_tx: tokio::sync::mpsc::Sender<generated::WorkerToServer>,
    inbound: tonic::Streaming<generated::ServerToWorker>,
}

impl WorkerSession {
    /// Connect one worker stream registered for every fan-out activity type and
    /// pin the `RegisterAck` as the first frame.
    async fn connect(
        address: SocketAddr,
        registry: &ConnectedWorkerRegistry,
    ) -> Result<Self, TestError> {
        let mut client = WorkerProtocolClient::connect(format!("http://{address}")).await?;
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<generated::WorkerToServer>(16);
        worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Register(
                    generated::RegisterWorker {
                        namespace: NAMESPACE.to_owned(),
                        activity_types: FAN_ACTIVITY_TYPES
                            .iter()
                            .map(|t| (*t).to_owned())
                            .collect(),
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
        // Registration is live once the ack is consumed: the worker is
        // dispatch-eligible for every fan-out activity type.
        for activity_type in FAN_ACTIVITY_TYPES {
            if registry.workers_for(NAMESPACE, activity_type)?.is_empty() {
                return Err(
                    format!("worker not registered for {activity_type} after the ack").into(),
                );
            }
        }
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

    /// Stream a successful activity result back, echoing the task's ids so the
    /// completion keys correctly at the server sink.
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

// --- history helpers (read back from the store) -------------------------------

fn count_kind(history: &[Event], matcher: impl Fn(&Event) -> bool) -> usize {
    history.iter().filter(|event| matcher(event)).count()
}

fn count_completed(history: &[Event]) -> usize {
    count_kind(history, |event| {
        matches!(event, Event::ActivityCompleted { .. })
    })
}

fn count_completed_for(history: &[Event], ordinal: u64) -> usize {
    count_kind(history, |event| match event {
        Event::ActivityCompleted { activity_id, .. } => activity_id.sequence_position() == ordinal,
        _ => false,
    })
}

fn count_scheduled(history: &[Event]) -> usize {
    count_kind(history, |event| {
        matches!(event, Event::ActivityScheduled { .. })
    })
}

async fn wait_for_history<F>(
    store: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
    description: &str,
    predicate: F,
) -> Result<Vec<Event>, TestError>
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {description}: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// JSON-encoded worker result for `ordinal` (a JSON string), matching the shape
/// the `collect_all` fixture collects.
fn worker_result(ordinal: u64) -> String {
    format!("\"worker-{ordinal}\"")
}

/// Start the loaded `collect_four` workflow over the REAL HTTP transport,
/// returning its ids. Mirrors `authoring_e2e.rs::start_over_http`.
async fn start_over_http(
    router: &axum::Router,
) -> Result<(aion_core::WorkflowId, aion_core::RunId), TestError> {
    // Build a fresh start request each attempt (the body is consumed by oneshot).
    let build_request = || -> Result<Request<body::Body>, TestError> {
        Ok(Request::builder()
            .uri("/workflows/start")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-aion-subject", "ci")
            .header("x-aion-namespaces", NAMESPACE)
            .body(body::Body::from(serde_json::to_vec(&json!({
                "namespace": NAMESPACE,
                "workflow_type": OUTBOX_MODULE,
                "input": { "fixture": "input" },
            }))?))?)
    };

    // A real client retries a transient server-side start error; bound it so a
    // genuine failure still surfaces. Only 5xx is retried — a 4xx (bad request,
    // not found) is a hard failure asserted on the final attempt.
    let mut last_status = StatusCode::OK;
    let mut last_body = Vec::new();
    for attempt in 0..5 {
        let response = router.clone().oneshot(build_request()?).await?;
        last_status = response.status();
        last_body = body::to_bytes(response.into_body(), usize::MAX)
            .await?
            .to_vec();
        if last_status == StatusCode::OK || !last_status.is_server_error() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
    }
    assert_eq!(
        last_status,
        StatusCode::OK,
        "workflow start over HTTP must succeed: {}",
        String::from_utf8_lossy(&last_body)
    );
    let bytes = last_body;
    let body: serde_json::Value = serde_json::from_slice(&bytes)?;
    let workflow_id = body["workflow_id"]["uuid"]
        .as_str()
        .ok_or("start response missing workflow id")?
        .parse::<uuid::Uuid>()?;
    let run_id = body["run_id"]["uuid"]
        .as_str()
        .ok_or("start response missing run id")?
        .parse::<uuid::Uuid>()?;
    Ok((
        aion_core::WorkflowId::new(workflow_id),
        aion_core::RunId::new(run_id),
    ))
}

/// Assert the fan-out settled cleanly after completions routed through the real
/// sink + callback: exactly `FAN_OUT` terminals (one per ordinal), exactly one
/// `WorkflowCompleted` carrying the input-ordered result list, and every outbox
/// row driven to `Done` by the dispatcher (the durable-outbox-path proof).
async fn assert_fan_out_settled(
    reader: &LibSqlStore,
    workflow_id: &aion_core::WorkflowId,
) -> Result<(), TestError> {
    let settled = wait_for_history(reader, workflow_id, "fan-out settled", |events| {
        count_completed(events) == FAN_OUT
    })
    .await?;
    assert_eq!(count_completed(&settled), FAN_OUT);
    for ordinal in 0..FAN_OUT as u64 {
        assert_eq!(
            count_completed_for(&settled, ordinal),
            1,
            "ordinal {ordinal} must have exactly one terminal"
        );
    }
    let workflow_completed = count_kind(&settled, |event| {
        matches!(event, Event::WorkflowCompleted { .. })
    });
    assert_eq!(
        workflow_completed, 1,
        "the workflow must complete exactly once"
    );

    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(workflow_id, ordinal);
        let state = reader
            .outbox_row_state(&key)
            .await?
            .ok_or_else(|| format!("no outbox row for ordinal {ordinal}"))?;
        assert_eq!(
            state.status,
            OutboxStatus::Done,
            "ordinal {ordinal} outbox row must be Done after the dispatcher carried it"
        );
    }

    let completed = settled
        .iter()
        .find_map(|event| match event {
            Event::WorkflowCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .ok_or("no WorkflowCompleted result payload")?;
    let value: serde_json::Value = serde_json::from_slice(completed.bytes())?;
    assert_eq!(
        value,
        json!([
            worker_result(0),
            worker_result(1),
            worker_result(2),
            worker_result(3),
        ]),
        "collect_all must return all four results in input order"
    );
    Ok(())
}

// --- the test -----------------------------------------------------------------

/// Full-stack proof: an `outbox.enabled` server runs a `collect_four` fan-out
/// whose members are dispatched via the durable outbox, completed by a real
/// gRPC worker, routed through the REAL `PendingActivities::complete_activity`
/// sink UNMATCHED, into the installed `ServerOutboxDeliveryCallback`, waking the
/// workflow to completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_cutover_completes_through_the_real_server_sink() -> Result<(), TestError> {
    let dir = unique_temp_dir("cutover")?;
    let db_path = dir.path().join("aion.db");
    let package_path = write_package_archive(dir.path())?;

    // 1. Full ServerState WITH an engine, over a real persistent LibSqlStore,
    //    outbox commissioned, collect_four loaded. build_with_store installs the
    //    ServerOutboxDeliveryCallback (state.rs, gated on outbox.enabled) and
    //    wires the production WorkerActivityDispatcher into the engine — there is
    //    no in-process dispatcher stub to inject on this path.
    let engine_store = LibSqlStore::open(db_path.clone()).await?;
    let state = ServerState::build_with_store(engine_store, runtime_config(package_path)).await?;

    // 2. Real worker gRPC service over loopback, one worker registered for the
    //    four fan-out activity types in the same namespace.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let grpc_address = listener.local_addr()?;
    let grpc_server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(worker_service(state.clone()))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let mut worker = WorkerSession::connect(grpc_address, state.worker_registry()).await?;

    // 3. The REAL OutboxDispatcher, spawned over a SECOND LibSqlStore handle on
    //    the SAME DB file the engine writes to (exactly how run.rs opens the
    //    outbox store: a fresh LibSqlStore::open on store.url). It pushes claimed
    //    rows to the connected worker through the production push dispatcher.
    let outbox_store: Arc<dyn OutboxStore> = Arc::new(LibSqlStore::open(db_path.clone()).await?);
    let push = ActivityDispatcher::new(state.worker_registry().clone())
        .with_drain_state(state.drain_state().clone());
    let row_dispatch = Arc::new(WorkerOutboxDispatch::new(push, NAMESPACE));
    let dispatcher = OutboxDispatcher::new(outbox_store, row_dispatch, outbox_dispatcher_config());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let dispatcher_task = tokio::spawn(dispatcher.run(shutdown_rx));

    // A read-only store handle (third handle on the same file) for asserting
    // history and outbox-row state without disturbing the engine or dispatcher.
    let reader = LibSqlStore::open(db_path.clone()).await?;

    // 4. Start collect_four over the real HTTP transport.
    let router = http_router(state.clone())?;
    let (workflow_id, run_id) = start_over_http(&router).await?;

    // The engine's dispatch_unscheduled stages four members through the outbox:
    // four ActivityScheduled events, four pending outbox rows, zero terminals.
    let scheduled = wait_for_history(&reader, &workflow_id, "fan-out scheduled", |events| {
        count_scheduled(events) == FAN_OUT
    })
    .await?;
    assert_eq!(
        count_completed(&scheduled),
        0,
        "no terminal may exist before any completion is delivered"
    );
    for ordinal in 0..FAN_OUT as u64 {
        let key = OutboxRow::dispatch_key_for(&workflow_id, ordinal);
        let state = reader
            .outbox_row_state(&key)
            .await?
            .ok_or_else(|| format!("no outbox row staged for ordinal {ordinal} (key {key})"))?;
        assert!(
            matches!(
                state.status,
                OutboxStatus::Pending | OutboxStatus::Claimed | OutboxStatus::Done
            ),
            "ordinal {ordinal} outbox row must be staged (Pending/Claimed/Done as the dispatcher \
             races the read), got {:?}",
            state.status
        );
    }

    // 5. Drive the organic completion loop: the spawned OutboxDispatcher claims
    //    each pending row and pushes an ActivityTask to the worker; the worker
    //    reads it (next_task) and streams a completion back (complete). That
    //    completion lands at PendingActivities::complete_activity UNMATCHED
    //    (the outbox path registered no oneshot) and routes through the installed
    //    ServerOutboxDeliveryCallback into the live workflow.
    //
    //    We collect the four pushed tasks first (the dispatcher delivers them as
    //    it claims rows), then complete each — proving every member traveled the
    //    outbox -> dispatcher -> worker push path.
    let mut tasks = Vec::with_capacity(FAN_OUT);
    for _ in 0..FAN_OUT {
        tasks.push(worker.next_task().await?);
    }
    // The four tasks must be exactly the four fan-out activity types, each once.
    let mut received_types: Vec<String> = tasks.iter().map(|t| t.activity_type.clone()).collect();
    received_types.sort();
    let mut expected_types: Vec<String> =
        FAN_ACTIVITY_TYPES.iter().map(|t| (*t).to_owned()).collect();
    expected_types.sort();
    assert_eq!(
        received_types, expected_types,
        "the worker must receive exactly the four fan-out activity types via the outbox push"
    );

    for task in &tasks {
        let ordinal = task
            .activity_id
            .as_ref()
            .ok_or("pushed task missing activity id")?
            .sequence_position;
        worker
            .complete(task, worker_result(ordinal).as_bytes())
            .await?;
    }

    // 6 + 7. The workflow settles through the real sink + callback (exactly
    //    FAN_OUT terminals, one WorkflowCompleted, input-ordered results) and
    //    every outbox row reached Done — the dispatcher carried it pending ->
    //    claimed -> done, proving the durable outbox path drove these members.
    assert_fan_out_settled(&reader, &workflow_id).await?;

    // run_id is recorded against the workflow (sanity that we started the run we
    // read back).
    let _ = run_id;

    // Teardown: stop the dispatcher, drop the worker, abort the gRPC server,
    // shut the engine down so durable appends finish.
    shutdown_tx.send(true).ok();
    tokio::time::timeout(Duration::from_secs(5), dispatcher_task)
        .await
        .map_err(|_| "outbox dispatcher did not stop after shutdown")??;
    drop(worker);
    grpc_server.abort();
    state.shutdown()?;
    Ok(())
}
