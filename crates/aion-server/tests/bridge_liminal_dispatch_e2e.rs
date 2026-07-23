//! Engine-seam BRIDGE dispatch to a liminal-connected worker (end-to-end).
//!
//! Gated on `liminal-transport`, so a default build never compiles it. This
//! exercises the operator's failing SEAM end-to-end: a worker registered over
//! the liminal transport (the REAL `aion_worker::serve_with_redial` production
//! serve entrypoint, connecting to a REAL `liminal-server` over loopback TCP
//! hosted by a `ServerState` booted with `outbox.enabled = true`) self-registers
//! into the shared connected-worker registry — and a plain activity dispatch
//! through the engine-seam bridge dispatcher (`WorkerActivityDispatcher`, the
//! same `dispatch_blocking` path every `run_activity` NIF takes) selects that
//! worker. The dispatcher is driven directly at the `aion::ActivityDispatcher`
//! seam from a spawned runtime task (the engine's calling context); no
//! engine-hosted workflow drives it, so engine scheduling/replay is
//! deliberately out of scope here. Before the fix, the bridge only implemented
//! the gRPC delivery arm and failed Terminal with "worker `WorkerId(_)` has no
//! gRPC stream sender (non-gRPC transport)". With the bridge
//! transport-agnostic at the delivery seam, the dispatch must ride the SAME
//! liminal wire frames the outbox push path uses and resolve exactly like a
//! gRPC completion.
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
//! - `dispatch_outliving_heartbeat_window_survives_the_sweeper` — with the
//!   PRODUCTION heartbeat sweeper running and a deliberately short window, an
//!   activity that runs several windows long still completes: the worker's
//!   automatic liveness beats over the liminal wire keep the tracked dispatch
//!   alive, the worker is NOT deregistered, and the activity executes exactly
//!   once (no duplicate from a false lost-worker retry).
//! - `worker_lost_mid_dispatch_fails_retryable_lost_worker` — a worker whose
//!   connection dies while its dispatch is in flight resolves the dispatch
//!   promptly with the SAME retryable lost-worker failure the gRPC teardown
//!   sweep reports (the reply router's Disconnected arm), and its in-flight
//!   tracking is cleared.
//! - `concurrent_dispatches_correlate_replies_to_their_ordinals` — two
//!   dispatches in flight against one worker each resolve with THEIR handler
//!   result (correlation, not delivery order).
//! - `dispatch_attempt_reaches_the_handler` — the engine-provided `attempt`
//!   rides the liminal wire and reaches the handler's `ActivityContext`
//!   exactly as it does over gRPC (a retry is not re-stamped as attempt 1).
//! - `bridge_dispatch_is_enumerable_for_intervention_while_in_flight` — the
//!   dispatch is bound into the NOI-6 attempt→owner back-index for exactly its
//!   in-flight window, so the ops console's live-attempts enumeration sees it
//!   while it runs (transcript target + intervention routing) and releases it
//!   once it resolves.
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
/// A plain activity type whose handler deliberately outlives the heartbeat
/// window (the agent-shaped long-runner, compressed for the test).
const SLOW: &str = "slow-provision";
/// A plain activity type whose handler echoes its `ActivityContext` attempt.
const ATTEMPT_ECHO: &str = "attempt-echo";
/// The activity type served by the fake worker that dies mid-dispatch.
const DOOMED: &str = "doomed-provision";
/// The default production heartbeat window, used by tests that don't exercise
/// expiry.
const DEFAULT_WINDOW: Duration = Duration::from_secs(30);
/// A deliberately short window so the over-window test spans several sweep
/// ticks (and several missed windows) in about two seconds.
const SHORT_WINDOW: Duration = Duration::from_millis(500);
/// How long the SLOW handler runs: four heartbeat windows, so an unbeaten
/// dispatch would be expired several times over before it completes.
const SLOW_RUNTIME: Duration = Duration::from_secs(2);

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
    /// Stops the production heartbeat sweeper on shutdown.
    sweeper_shutdown: tokio::sync::watch::Sender<bool>,
}

impl RunningServer {
    fn start(heartbeat_window: Duration) -> Result<Self, TestError> {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config(heartbeat_window));

        let config = ServerConfig {
            listen_address: "127.0.0.1:0".parse().map_err(test_error)?,
            health_listen_address: reserve_loopback_port()?,
            channels: Vec::<ChannelDef>::new(),
            routing_rules: Vec::new(),
            persistence_path: None,
            cluster: None,
            // Open at the liminal layer (no Connect token), matching the embedded
            // production listener; aion-level registration metadata is the auth story.
            auth: None,
            drain_timeout_ms: 30_000,
            // liminal 0.2.4 defaults = the 0.2.3 behaviour (full profile, signed caps).
            services: liminal_server::config::ServicesConfig::default(),
            limits: liminal_server::config::LimitsConfig::default(),
            // liminal 0.3.0: no WebSocket listener, participant capability
            // disabled — byte-identical to the pre-0.3.0 build, matching run.rs.
            websocket: None,
            participant: None,
        };
        // The notifier registers in-band worker registrations into the SAME
        // registry the bridge dispatcher selects from, and carries the SAME
        // liveness tracker the bridge tracks into so worker liveness beats
        // refresh it — the exact production wiring (`build_liminal_row_dispatch`).
        let notifier = Arc::new(
            LiminalConnectionNotifier::new(state.worker_registry().clone())
                .with_heartbeat_tracker(state.heartbeat_tracker().clone()),
        );
        let supervisor = build_supervisor_with_notifier(&config, notifier.clone())?;
        if !notifier.bind_supervisor(supervisor.clone()) {
            return Err(test_error("notifier supervisor was already bound"));
        }
        let listener = ServerListener::bind(&config, supervisor).map_err(test_error)?;
        let address = listener.local_addr();
        // The PRODUCTION #176 expiry sweeper, exactly as the boot path spawns
        // it (always on): the over-window test is only honest with the sweeper
        // genuinely ticking against the configured window.
        let (sweeper_shutdown, sweeper_rx) = tokio::sync::watch::channel(false);
        drop(state.spawn_heartbeat_sweeper(sweeper_rx));
        Ok(Self {
            listener: Some(listener),
            state,
            address,
            sweeper_shutdown,
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
        .with_attempt_owners(self.state.attempt_owners().clone())
    }

    fn wait_for_registered_worker(&self, activity_type: &str) -> Result<(), TestError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if self.worker_is_registered(activity_type)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(test_error("server never registered the in-band worker"))
    }

    fn worker_is_registered(&self, activity_type: &str) -> Result<bool, TestError> {
        Ok(self
            .state
            .worker_registry()
            .select_worker(NAMESPACE, TASK_QUEUE, activity_type, None)
            .map_err(test_error)?
            .is_some())
    }

    fn shutdown(mut self) -> Result<(), TestError> {
        let _ = self.sweeper_shutdown.send(true);
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

fn runtime_config(heartbeat_window: Duration) -> RuntimeConfig {
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
        worker: ServerWorkerConfig { heartbeat_window },
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
        observability: aion_server::config::ObservabilityConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_secs(10)),
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
/// activity (succeeds, echoes its input), a retryably-failing sibling, a
/// deliberately window-outliving long-runner, and an attempt echo.
fn worker_activity_registry(
    executions: Arc<AtomicUsize>,
) -> Result<Arc<ActivityRegistry>, TestError> {
    let slow_executions = Arc::clone(&executions);
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
        .map_err(test_error)?
        .register_activity(SLOW, move |input: ProvisionInput, _context| {
            let executions = Arc::clone(&slow_executions);
            Box::pin(async move {
                // Genuinely runs past several heartbeat windows — the
                // compressed shape of an agent activity that runs for over an
                // hour under the default 30s window.
                tokio::time::sleep(SLOW_RUNTIME).await;
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ProvisionOutput {
                    provisioned: true,
                    resource: input.resource,
                })
            })
        })
        .map_err(test_error)?
        .register_activity(ATTEMPT_ECHO, |_input: serde_json::Value, context| {
            Box::pin(async move { Ok(serde_json::json!({ "attempt": context.attempt() })) })
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
    let server = RunningServer::start(DEFAULT_WINDOW)?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(Arc::clone(&executions))?,
    );
    server.wait_for_registered_worker(PROVISION)?;

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

/// THE SWEEPER SIDE (the false-lost-worker regression): with the PRODUCTION
/// heartbeat sweeper ticking against a deliberately short window, an activity
/// that genuinely runs FOUR windows long still completes — the worker's
/// automatic liveness beats over the liminal wire keep the tracked dispatch
/// alive, exactly as the gRPC runtime's quarter-window liveness pump does.
/// Without the liminal liveness path this fails in under a second: the sweeper
/// declares the healthy worker lost, the dispatch resolves retryable
/// lost-worker while the handler keeps running, and the worker is deregistered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_outliving_heartbeat_window_survives_the_sweeper() -> Result<(), TestError> {
    let server = RunningServer::start(SHORT_WINDOW)?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(Arc::clone(&executions))?,
    );
    server.wait_for_registered_worker(SLOW)?;

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());

    let result = dispatch_via_seam(
        &dispatcher,
        dispatch_request(
            &workflow_id,
            0,
            SLOW,
            &serde_json::json!({ "resource": "long-haul" }),
        ),
    )
    .await?;
    let result = result.map_err(|reason| {
        test_error(format!(
            "an over-window dispatch must not be failed by the sweeper: {reason}"
        ))
    })?;
    let output: ProvisionOutput = serde_json::from_str(&result).map_err(test_error)?;
    assert_eq!(output.resource, "long-haul");
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "the activity executed exactly once — no duplicate from a false lost-worker retry"
    );
    // The healthy worker was NOT deregistered by the sweeper.
    assert!(
        server.worker_is_registered(SLOW)?,
        "a healthy worker running a long activity must stay registered"
    );
    // The completion cleared its in-flight entry (nothing left to sweep).
    assert_eq!(
        server
            .state
            .heartbeat_tracker()
            .in_flight_count()
            .map_err(test_error)?,
        0
    );

    worker.stop()?;
    server.shutdown()?;
    Ok(())
}

/// THE LOST-WORKER SIDE (the reply router's Disconnected arm): a worker whose
/// connection dies while its dispatch is in flight resolves the dispatch
/// promptly with the SAME retryable lost-worker failure the gRPC teardown
/// sweep reports — never a hang. The fake worker is a REAL liminal push client
/// that registers in-band, receives the pushed dispatch frame, and then drops
/// its connection without replying (a mid-activity `kill -9` at the wire).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_lost_mid_dispatch_fails_retryable_lost_worker() -> Result<(), TestError> {
    let server = RunningServer::start(DEFAULT_WINDOW)?;

    let registration = liminal::protocol::WorkerRegistration {
        namespaces: vec![NAMESPACE.to_owned()],
        task_queue: TASK_QUEUE.to_owned(),
        node: None,
        activity_types: vec![DOOMED.to_owned()],
        identity: "doomed-worker".to_owned(),
    };
    let client = liminal_sdk::PushClient::connect_with_registration(
        &server.address.to_string(),
        registration,
    )
    .map_err(test_error)?;
    server.wait_for_registered_worker(DOOMED)?;

    // Receive the pushed dispatch, then die without replying.
    let doomed = std::thread::spawn(move || -> Result<(), TestError> {
        let _frame = client
            .recv_timeout(Duration::from_secs(10))
            .map_err(test_error)?;
        // Dropping the client joins its reader and closes the connection.
        drop(client);
        Ok(())
    });

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());
    let result = dispatch_via_seam(
        &dispatcher,
        dispatch_request(&workflow_id, 0, DOOMED, &serde_json::json!({})),
    )
    .await?;

    let reason = result.err().ok_or_else(|| {
        test_error("a dispatch whose worker died mid-flight must fail, not succeed")
    })?;
    assert!(
        reason.starts_with("retryable:"),
        "worker loss must surface as a retryable failure, got: {reason}"
    );
    assert!(
        reason.contains("lost"),
        "the failure must be the lost-worker vocabulary, got: {reason}"
    );
    // The failed dispatch cleared its in-flight tracking (the router's
    // complete_task gate ran), leaving nothing for a later sweep.
    assert_eq!(
        server
            .state
            .heartbeat_tracker()
            .in_flight_count()
            .map_err(test_error)?,
        0
    );
    doomed
        .join()
        .map_err(|_| test_error("doomed worker thread panicked"))??;

    server.shutdown()?;
    Ok(())
}

/// CORRELATION: two dispatches in flight against ONE worker each resolve with
/// THEIR handler's result — the pending map plus the correlated push replies
/// never cross two outstanding ordinals.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_dispatches_correlate_replies_to_their_ordinals() -> Result<(), TestError> {
    let server = RunningServer::start(DEFAULT_WINDOW)?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(Arc::clone(&executions))?,
    );
    server.wait_for_registered_worker(PROVISION)?;

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());

    let first = dispatch_via_seam(
        &dispatcher,
        dispatch_request(
            &workflow_id,
            0,
            PROVISION,
            &serde_json::json!({ "resource": "alpha" }),
        ),
    );
    let second = dispatch_via_seam(
        &dispatcher,
        dispatch_request(
            &workflow_id,
            1,
            PROVISION,
            &serde_json::json!({ "resource": "beta" }),
        ),
    );
    let (first, second) = tokio::join!(first, second);

    let first: ProvisionOutput =
        serde_json::from_str(&first?.map_err(test_error)?).map_err(test_error)?;
    let second: ProvisionOutput =
        serde_json::from_str(&second?.map_err(test_error)?).map_err(test_error)?;
    assert_eq!(
        first.resource, "alpha",
        "ordinal 0 must resolve with ITS handler result"
    );
    assert_eq!(
        second.resource, "beta",
        "ordinal 1 must resolve with ITS handler result"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(
        server
            .state
            .heartbeat_tracker()
            .in_flight_count()
            .map_err(test_error)?,
        0
    );

    worker.stop()?;
    server.shutdown()?;
    Ok(())
}

/// ATTEMPT PARITY: the engine-provided `attempt` rides the liminal wire and
/// reaches the handler's `ActivityContext` exactly as over gRPC — a retry
/// dispatched over liminal executes as attempt N, never a re-stamped first
/// delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_attempt_reaches_the_handler() -> Result<(), TestError> {
    let server = RunningServer::start(DEFAULT_WINDOW)?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(executions)?,
    );
    server.wait_for_registered_worker(ATTEMPT_ECHO)?;

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());

    // A RETRY-shaped dispatch: the engine hands attempt 4 to the seam.
    let request = ActivityDispatch {
        attempt: 4,
        ..dispatch_request(&workflow_id, 0, ATTEMPT_ECHO, &serde_json::json!({}))
    };
    let result = dispatch_via_seam(&dispatcher, request).await?;
    let output: serde_json::Value =
        serde_json::from_str(&result.map_err(test_error)?).map_err(test_error)?;
    assert_eq!(
        output["attempt"], 4,
        "the handler must observe the engine's attempt, not a re-stamped 1"
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
    let server = RunningServer::start(DEFAULT_WINDOW)?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(executions)?,
    );
    server.wait_for_registered_worker(FLAKY)?;

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

/// THE CONSOLE'S VIEW (NOI-6/NOI-7): a bridge-dispatched liminal activity is
/// bound into the attempt→owner back-index for exactly its in-flight window, so
/// the ops console's live-attempts enumeration (`intervenable_attempts`, the
/// read behind `POST /workflows/attempts`) sees it while it runs and stops
/// seeing it once it resolves. This is the operator-reported regression: the
/// bind existed only on the outbox row arm, so every bridge-dispatched agent
/// step left the console's attempt list empty — no transcript target, no
/// intervention controls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridge_dispatch_is_enumerable_for_intervention_while_in_flight() -> Result<(), TestError> {
    let server = RunningServer::start(DEFAULT_WINDOW)?;
    let executions = Arc::new(AtomicUsize::new(0));
    let worker = ServedWorker::spawn(
        server.address.to_string(),
        worker_activity_registry(executions)?,
    );
    server.wait_for_registered_worker(SLOW)?;

    let dispatcher = Arc::new(server.bridge_dispatcher());
    let workflow_id = WorkflowId::new(Uuid::new_v4());

    // A RETRY-shaped dispatch: the enumeration must expose the engine's real
    // attempt (the key the worker stamps its intervention session with).
    let request = ActivityDispatch {
        attempt: 2,
        ..dispatch_request(
            &workflow_id,
            0,
            SLOW,
            &serde_json::json!({ "resource": "intervenable" }),
        )
    };
    let dispatch = {
        let dispatcher = Arc::clone(&dispatcher);
        tokio::spawn(futures::future::lazy(move |_| dispatcher.dispatch(request)))
    };

    // While the SLOW handler runs, the console's enumeration sees the attempt.
    let router = server.state.intervention_router();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let attempts = loop {
        let attempts = router
            .intervenable_attempts(&workflow_id)
            .map_err(test_error)?;
        if !attempts.is_empty() {
            break attempts;
        }
        if Instant::now() > deadline {
            return Err(test_error(
                "the in-flight bridge dispatch never appeared in the live-attempts enumeration",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let (key, _capabilities) = &attempts[0];
    assert_eq!(key.workflow_id, workflow_id);
    assert_eq!(key.activity_id, ActivityId::from_sequence_position(0));
    assert_eq!(
        key.attempt, 2,
        "the enumeration must carry the engine's real attempt, not a re-stamp"
    );

    // Once the dispatch resolves, the binding is released: a finished attempt
    // is never offered as an intervention target.
    let result = tokio::time::timeout(Duration::from_secs(20), dispatch)
        .await
        .map_err(|_| test_error("bridge dispatch did not resolve within the test deadline"))?
        .map_err(test_error)?;
    result.map_err(test_error)?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if router
            .intervenable_attempts(&workflow_id)
            .map_err(test_error)?
            .is_empty()
        {
            break;
        }
        if Instant::now() > deadline {
            return Err(test_error(
                "the resolved dispatch was never released from the live-attempts enumeration",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    worker.stop()?;
    server.shutdown()?;
    Ok(())
}
