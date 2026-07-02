//! Engine-seam BRIDGE dispatch to a liminal-connected worker (end-to-end).
//!
//! Gated on `liminal-transport`, so a default build never compiles it. This
//! reproduces the operator's live failure exactly: a worker registered over the
//! liminal transport (the REAL `aion_worker::serve_with_redial` production serve
//! entrypoint, connecting to a REAL `liminal-server` over loopback TCP hosted by
//! a `ServerState` booted with `outbox.enabled = true`) self-registers into the
//! shared connected-worker registry — and a workflow's PLAIN activity dispatch
//! through the engine-seam bridge dispatcher (`WorkerActivityDispatcher`, the
//! same `dispatch_blocking` path every `run_activity` NIF takes) selects that
//! worker. Before the fix, the bridge only implemented the gRPC delivery arm and
//! failed Terminal with "worker `WorkerId(_)` has no gRPC stream sender (non-gRPC
//! transport)". With the bridge transport-agnostic at the delivery seam, the
//! dispatch must ride the SAME liminal wire frames the outbox push path uses and
//! resolve exactly like a gRPC completion.
//!
//! The proofs:
//!
//! - `bridge_dispatch_reaches_liminal_worker_and_resolves` — a plain activity
//!   dispatch through the REAL `aion::ActivityDispatcher` seam executes on the
//!   remote worker, its correlated reply resolves the dispatch with the
//!   handler's result, a SECOND dispatch also round-trips (no one-shot luck),
//!   and the bridge's in-flight liveness bookkeeping is cleared afterwards.
//! - `bridge_dispatch_failure_surfaces_retryable_classification` — a handler
//!   returning a retryable `ActivityFailure` surfaces through the bridge as a
//!   `retryable:`-prefixed error string, the exact vocabulary the engine seam
//!   parses — classification fidelity identical to a gRPC completion.
#![cfg(feature = "liminal-transport")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aion::{ActivityDispatch, ActivityDispatcher};
use aion_core::{ActivityId, WorkflowId};
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, OutboxConfig, RuntimeConfig,
    WebSocketConfig, WorkerConfig as ServerWorkerConfig,
};
use aion_server::worker::{LiminalConnectionNotifier, WorkerActivityDispatcher};
use aion_server::{
    NamespaceResolver, ServerState, StaticScheduleNamespaces, StaticWorkflowNamespaces,
};
use aion_worker::{
    ActivityFailure, ActivityRegistry, RedialTiming, WorkerConfig, serve_with_redial,
};
use liminal_server::config::{ChannelDef, ServerConfig};
use liminal_server::server::connection::ConnectionSupervisor;
use liminal_server::server::listener::ServerListener;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type TestError = Box<dyn std::error::Error + Send + Sync>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
/// The operator's failing plain activity type (`agent_dev`'s "provision").
const PROVISION: &str = "provision";
/// A plain activity type whose handler always fails retryably.
const FLAKY: &str = "flaky-provision";
const FLAKY_REASON: &str = "provision backend briefly unavailable";

fn test_error(message: impl std::fmt::Display) -> TestError {
    message.to_string().into()
}

/// Typed input/output the provision handler round-trips, proving the worker
/// genuinely executed the dispatched activity (not an echo).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProvisionInput {
    resource: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProvisionOutput {
    provisioned: bool,
    resource: String,
}

// --- Server harness: a ServerState (outbox enabled, mirroring the operator's
//     demo-config.toml) hosting a REAL liminal listener whose notifier registers
//     connecting workers into the state's shared connected-worker registry. -----

struct RunningServer {
    listener: Option<ServerListener>,
    state: ServerState,
    address: SocketAddr,
}

impl RunningServer {
    fn start() -> Result<Self, TestError> {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config());

        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(test_error)?,
            health_listen_address: reserve_loopback_port()?,
            channels: Vec::<ChannelDef>::new(),
            routing_rules: Vec::new(),
            persistence_path: None,
            cluster: None,
            drain_timeout_ms: 30_000,
        };
        // The notifier registers in-band worker registrations into the SAME
        // registry the bridge dispatcher selects from — the exact production
        // wiring that let a liminal worker be selected by a plain dispatch.
        let notifier = Arc::new(LiminalConnectionNotifier::new(
            state.worker_registry().clone(),
        ));
        let supervisor = build_supervisor_with_notifier(&config, notifier.clone())?;
        if !notifier.bind_supervisor(supervisor.clone()) {
            return Err(test_error("notifier supervisor was already bound"));
        }
        let listener = ServerListener::bind(&config, supervisor).map_err(test_error)?;
        let address = listener.local_addr();
        Ok(Self {
            listener: Some(listener),
            state,
            address,
        })
    }

    /// Builds the ENGINE-SEAM bridge dispatcher over the state's shared parts —
    /// registry, pending map, heartbeat tracker, drain gate — exactly as the
    /// production `ServerState::new` composes the dispatcher the engine's
    /// `run_activity` NIFs call through.
    fn bridge_dispatcher(&self) -> WorkerActivityDispatcher {
        WorkerActivityDispatcher::new(
            self.state.worker_registry().clone(),
            NAMESPACE,
            self.state.heartbeat_tracker().clone(),
        )
        .with_pending(self.state.pending_activities().clone())
        .with_drain_state(self.state.drain_state().clone())
        .with_tokio_handle(tokio::runtime::Handle::current())
    }

    fn wait_for_registered_worker(&self) -> Result<(), TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if self
                .state
                .worker_registry()
                .select_worker(NAMESPACE, TASK_QUEUE, PROVISION, None)
                .map_err(test_error)?
                .is_some()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error("server never registered the in-band worker"))
    }

    fn shutdown(mut self) -> Result<(), TestError> {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().map_err(test_error)?;
        }
        Ok(())
    }
}

fn build_supervisor_with_notifier(
    config: &ServerConfig,
    notifier: Arc<LiminalConnectionNotifier>,
) -> Result<ConnectionSupervisor, TestError> {
    use liminal_server::server::connection::LiminalConnectionServices;
    let services = Arc::new(LiminalConnectionServices::from_config(config).map_err(test_error)?);
    ConnectionSupervisor::with_services_and_notifier(services, notifier).map_err(test_error)
}

fn reserve_loopback_port() -> Result<SocketAddr, TestError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(test_error)?;
    let address = listener.local_addr().map_err(test_error)?;
    drop(listener);
    Ok(address)
}

fn runtime_config() -> RuntimeConfig {
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
        worker: ServerWorkerConfig {
            heartbeat_window: Duration::from_millis(30_000),
        },
        websocket: WebSocketConfig {
            outbound_buffer_bound: 32,
            event_broadcast_capacity: Some(64),
            cluster_broadcast_capacity: Some(64),
        },
        workflow_packages: Vec::new(),
        deploy: DeployConfig::default(),
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        // The operator's failing boot ran with the durable outbox ON
        // (demo-config.toml: outbox.enabled = true); the bug fired on the plain
        // bridge path regardless, so the state mirrors that config.
        outbox: OutboxConfig {
            enabled: true,
            ..OutboxConfig::default()
        },
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

fn worker_config() -> Result<WorkerConfig, TestError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(NAMESPACE)
        .task_queue(TASK_QUEUE)
        .node("")
        .identity("bridge-liminal-worker")
        .max_concurrency(1)
        .reconnect_initial_backoff(Duration::from_millis(5))
        .reconnect_max_backoff(Duration::from_millis(20))
        .reconnect_max_attempts(3)
        .build()
        .map_err(test_error)
}

/// The worker's typed activity registry: the operator's plain "provision"
/// activity (succeeds, echoes its input) plus a retryably-failing sibling.
fn worker_activity_registry(
    executions: Arc<AtomicUsize>,
) -> Result<Arc<ActivityRegistry>, TestError> {
    let registry = ActivityRegistry::new()
        .register_activity(PROVISION, move |input: ProvisionInput, _context| {
            let executions = Arc::clone(&executions);
            Box::pin(async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ProvisionOutput {
                    provisioned: true,
                    resource: input.resource,
                })
            })
        })
        .map_err(test_error)?
        .register_activity(FLAKY, |_input: serde_json::Value, _context| {
            Box::pin(async move {
                Err::<serde_json::Value, _>(ActivityFailure::retryable(FLAKY_REASON))
            })
        })
        .map_err(test_error)?;
    Ok(Arc::new(registry))
}

/// Runs the REAL production serve entrypoint (`serve_with_redial`) on a
/// dedicated OS thread — the exact library seam the operator's
/// `examples/agent-dev/worker` binary drives — with a stop flag for teardown.
struct ServedWorker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<(), TestError>>>,
}

impl ServedWorker {
    fn spawn(address: String, registry: Arc<ActivityRegistry>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || -> Result<(), TestError> {
            let config = worker_config()?;
            serve_with_redial(
                vec![address],
                &config,
                &registry,
                RedialTiming::new(Duration::from_millis(5), Duration::from_millis(20)),
                &worker_stop,
                None,
                || {},
            )
            .map_err(test_error)
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn stop(mut self) -> Result<(), TestError> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| test_error("worker serve thread panicked"))??;
        }
        Ok(())
    }
}

/// One plain engine-seam dispatch request, exactly the shape the engine's
/// activity NIF hands `WorkerActivityDispatcher::dispatch`.
fn dispatch_request(
    workflow_id: &WorkflowId,
    ordinal: u64,
    activity_type: &str,
    input: &serde_json::Value,
) -> ActivityDispatch {
    ActivityDispatch {
        namespace: NAMESPACE.to_owned(),
        task_queue: TASK_QUEUE.to_owned(),
        node: None,
        workflow_id: workflow_id.clone(),
        activity_id: ActivityId::from_sequence_position(ordinal),
        name: activity_type.to_owned(),
        input: input.to_string(),
        config: "{}".to_owned(),
        attempt: 1,
        labels: BTreeMap::new(),
    }
}

/// Drives one dispatch through the REAL `aion::ActivityDispatcher` seam from a
/// spawned runtime task — the same calling context the engine's completion task
/// uses (`dispatch` detects the runtime and moves the blocking wait into
/// `block_in_place`, exactly as in production).
async fn dispatch_via_seam(
    dispatcher: &Arc<WorkerActivityDispatcher>,
    request: ActivityDispatch,
) -> Result<Result<String, String>, TestError> {
    let dispatcher = Arc::clone(dispatcher);
    tokio::time::timeout(
        Duration::from_secs(20),
        tokio::spawn(futures::future::lazy(move |_| dispatcher.dispatch(request))),
    )
    .await
    .map_err(|_| test_error("bridge dispatch did not resolve within the test deadline"))?
    .map_err(test_error)
}

/// THE OPERATOR'S SCENARIO, FIXED: a plain activity dispatch through the
/// engine-seam bridge reaches the liminal-registered worker, executes there, and
/// the correlated reply resolves the dispatch — twice (no one-shot luck) — with
/// the bridge's in-flight liveness bookkeeping cleared afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridge_dispatch_reaches_liminal_worker_and_resolves() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(Arc::clone(&executions))?,
    );
    server.wait_for_registered_worker()?;

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());

    // FIRST dispatch: before the fix this failed Terminal with "worker
    // WorkerId(_) has no gRPC stream sender (non-gRPC transport)".
    let first = dispatch_via_seam(
        &dispatcher,
        dispatch_request(
            &workflow_id,
            0,
            PROVISION,
            &serde_json::json!({ "resource": "database" }),
        ),
    )
    .await?;
    let first =
        first.map_err(|reason| test_error(format!("first bridge dispatch failed: {reason}")))?;
    let output: ProvisionOutput = serde_json::from_str(&first).map_err(test_error)?;
    assert!(output.provisioned, "the worker's handler genuinely ran");
    assert_eq!(
        output.resource, "database",
        "the handler saw the dispatched input"
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the remote worker executed the first dispatch exactly once"
    );

    // SECOND dispatch on the same connection: the delivery is repeatable, not
    // one-shot luck (a fresh correlated push + reply per dispatch).
    let second = dispatch_via_seam(
        &dispatcher,
        dispatch_request(
            &workflow_id,
            1,
            PROVISION,
            &serde_json::json!({ "resource": "cache" }),
        ),
    )
    .await?;
    let second =
        second.map_err(|reason| test_error(format!("second bridge dispatch failed: {reason}")))?;
    let output: ProvisionOutput = serde_json::from_str(&second).map_err(test_error)?;
    assert_eq!(output.resource, "cache");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        2,
        "the remote worker executed the second dispatch too"
    );

    // The bridge's liveness bookkeeping matches gRPC: both completions cleared
    // their in-flight entries (nothing left for a sweep to fail).
    assert_eq!(
        server
            .state
            .heartbeat_tracker()
            .in_flight_count()
            .map_err(test_error)?,
        0,
        "completed liminal dispatches must clear their in-flight tracking"
    );

    worker.stop()?;
    server.shutdown()?;
    Ok(())
}

/// THE FAILURE SIDE: a worker handler returning a retryable `ActivityFailure`
/// surfaces through the bridge as a `retryable:`-prefixed error string — the
/// exact vocabulary the engine seam parses — so retryability classification over
/// the liminal wire is identical to a gRPC completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridge_dispatch_failure_surfaces_retryable_classification() -> Result<(), TestError> {
    let server = RunningServer::start()?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(executions)?,
    );
    server.wait_for_registered_worker()?;

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());

    let result = dispatch_via_seam(
        &dispatcher,
        dispatch_request(&workflow_id, 0, FLAKY, &serde_json::json!({})),
    )
    .await?;
    let reason = result
        .err()
        .ok_or_else(|| test_error("a retryably-failing handler must fail the dispatch"))?;
    assert!(
        reason.starts_with("retryable:"),
        "the failure must carry the engine seam's retryable classification, got: {reason}"
    );
    assert!(
        reason.contains(FLAKY_REASON),
        "the handler's failure message must survive the wire, got: {reason}"
    );

    // The failed completion cleared its in-flight entry exactly like a success.
    assert_eq!(
        server
            .state
            .heartbeat_tracker()
            .in_flight_count()
            .map_err(test_error)?,
        0,
        "a failed liminal dispatch must clear its in-flight tracking"
    );

    worker.stop()?;
    server.shutdown()?;
    Ok(())
}
