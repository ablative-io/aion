//! End-to-end proof that the worker heartbeat expiry sweeper (#176) runs in
//! production wiring.
//!
//! `HeartbeatTracker::fail_expired_workers` existed but had NO production
//! caller: the stream-teardown sweep only fires when a worker's stream ENDS,
//! so a worker whose stream stayed open while its process wedged (stopped
//! heartbeating without disconnecting) was never expired — its in-flight
//! dispatches waited forever. These tests drive the REAL seam the server boot
//! path commissions — [`ServerState::spawn_heartbeat_sweeper`] — over a real
//! tonic `WorkerProtocol` stream, the real connected-worker registry, the real
//! heartbeat tracker, and the real pending-activities completion sink. No
//! stand-ins for any of them.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::{ActivityDispatch, ActivityDispatcher as _};
use aion_core::{ClusterEvent, WorkerDeathReason};
use aion_proto::generated::worker_protocol_client::WorkerProtocolClient;
use aion_proto::generated::{self, server_to_worker, worker_to_server};
use aion_server::ServerState;
use aion_server::api::worker_grpc::worker_service;
use aion_server::cluster_publisher::ClusterEventPublisher;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::worker::{ConnectedWorkerRegistry, WorkerActivityDispatcher};
use aion_server::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const TASK_QUEUE: &str = "default";
const ACTIVITY_TYPE: &str = "greet";

/// Short heartbeat window so expiry is observable in test time. The derived
/// sweep cadence for a sub-second window is the window itself, so worst-case
/// detection is roughly two windows (~1s here).
const HEARTBEAT_WINDOW: Duration = Duration::from_millis(500);

/// Generous wall-clock bound on sweeper-driven expiry: expected ~1s with the
/// 500ms window; anything near this bound means the sweep is not running.
const EXPIRY_DEADLINE: Duration = Duration::from_secs(10);

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
        worker: WorkerConfig { heartbeat_window },
        websocket: WebSocketConfig {
            outbound_buffer_bound: 32,
            event_broadcast_capacity: Some(64),
            cluster_broadcast_capacity: Some(64),
        },
        workflow_packages: Vec::new(),
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
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

/// A `greet` dispatch request in the engine-seam shape
/// `WorkerActivityDispatcher::dispatch` consumes.
fn greet_request() -> ActivityDispatch {
    ActivityDispatch {
        namespace: NAMESPACE.to_owned(),
        task_queue: TASK_QUEUE.to_owned(),
        node: None,
        workflow_id: aion_core::WorkflowId::new_v4(),
        activity_id: aion_core::ActivityId::from_sequence_position(0),
        name: ACTIVITY_TYPE.to_owned(),
        input: "{}".to_owned(),
        config: "{}".to_owned(),
        attempt: 1,
        labels: std::collections::BTreeMap::new(),
    }
}

/// A live in-process worker gRPC service plus one registered worker stream,
/// with the WS3 cluster publisher attached to the registry so the tests can
/// observe WHICH path deregistered the worker (the sweep's provable `Timeout`
/// vs teardown's `Disconnect`).
struct Harness {
    state: ServerState,
    registry: ConnectedWorkerRegistry,
    publisher: ClusterEventPublisher,
    /// Keeps the worker's outbound request stream open for the test duration —
    /// crucially, holding this open is what proves the SWEEPER (not the
    /// stream-teardown path) expired the silent worker.
    worker_tx: tokio::sync::mpsc::Sender<generated::WorkerToServer>,
    inbound: tonic::Streaming<generated::ServerToWorker>,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

/// A live in-process worker gRPC service with no worker connected yet — the
/// substrate for BOTH the raw-stream harness and the real-runtime test.
struct ServerHarness {
    state: ServerState,
    registry: ConnectedWorkerRegistry,
    publisher: ClusterEventPublisher,
    address: SocketAddr,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl ServerHarness {
    /// Boot the real worker gRPC service on a loopback port.
    async fn start() -> Result<Self, TestError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let publisher =
            ClusterEventPublisher::new(std::num::NonZeroUsize::new(64).ok_or("nonzero capacity")?);
        let registry = ConnectedWorkerRegistry::default().with_cluster_publisher(publisher.clone());
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        );
        let state = ServerState::from_parts_with_registry(
            resolver,
            runtime_config(HEARTBEAT_WINDOW),
            registry.clone(),
        );
        let server = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(worker_service(state.clone()))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        Ok(Self {
            state,
            registry,
            publisher,
            address,
            server,
        })
    }

    /// Build the production dispatcher over the harness's shared state — the
    /// same wiring `ServerState` hands to the engine.
    fn dispatcher(&self) -> WorkerActivityDispatcher {
        WorkerActivityDispatcher::new(
            self.registry.clone(),
            NAMESPACE,
            self.state.heartbeat_tracker().clone(),
        )
        .with_pending(self.state.pending_activities().clone())
        .with_drain_state(self.state.drain_state().clone())
    }
}

impl Harness {
    /// Boot the real worker gRPC service on a loopback port and connect one
    /// worker stream registered for [`ACTIVITY_TYPE`] in [`NAMESPACE`].
    async fn start() -> Result<Self, TestError> {
        let ServerHarness {
            state,
            registry,
            publisher,
            address,
            server,
        } = ServerHarness::start().await?;

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

        // The ack is the registration-success signal: once read, the worker
        // is dispatch-eligible in the registry.
        let first = inbound
            .message()
            .await?
            .and_then(|frame| frame.message)
            .ok_or("response stream ended before the RegisterAck")?;
        let server_to_worker::Message::RegisterAck(_) = first else {
            return Err(format!("first response frame must be RegisterAck, got {first:?}").into());
        };

        Ok(Self {
            state,
            registry,
            publisher,
            worker_tx,
            inbound,
            server,
        })
    }

    /// Build the production dispatcher over the harness's shared state — the
    /// same wiring `ServerState` hands to the engine.
    fn dispatcher(&self) -> WorkerActivityDispatcher {
        WorkerActivityDispatcher::new(
            self.registry.clone(),
            NAMESPACE,
            self.state.heartbeat_tracker().clone(),
        )
        .with_pending(self.state.pending_activities().clone())
        .with_drain_state(self.state.drain_state().clone())
    }

    /// Wait for the next `ActivityTask` pushed down the worker stream.
    async fn next_task(&mut self) -> Result<generated::ActivityTask, TestError> {
        while let Some(message) = self.inbound.message().await? {
            if let Some(server_to_worker::Message::Task(task)) = message.message {
                return Ok(task);
            }
        }
        Err("worker stream closed before a task was delivered".into())
    }

    /// Send one heartbeat frame for the given in-flight task.
    async fn heartbeat(&self, task: &generated::ActivityTask) -> Result<(), TestError> {
        self.worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Heartbeat(generated::Heartbeat {
                    workflow_id: task.workflow_id.clone(),
                    activity_id: task.activity_id,
                    progress: None,
                })),
            })
            .await?;
        Ok(())
    }

    /// Report a successful activity result back over the worker stream.
    async fn complete(
        &self,
        task: generated::ActivityTask,
        result_json: &[u8],
    ) -> Result<(), TestError> {
        self.worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Result(
                    generated::ActivityResult {
                        workflow_id: task.workflow_id,
                        activity_id: task.activity_id,
                        run_id: task.run_id,
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

/// THE #176 regression proof: a worker that receives a task and then goes
/// silent — stream OPEN, no heartbeats, no result — is expired by the sweeper:
/// its blocked dispatch fails with the retryable lost-worker error, it is
/// deregistered from the routing registry, and the WS3 delta carries the
/// provable `Timeout` reason (only the sweep asserts it; the teardown path
/// would say `Disconnect`, and the stream never ended here anyway).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_worker_with_open_stream_is_expired_and_deregistered_by_the_sweeper()
-> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    // Subscribe BEFORE anything can deregister, so the delta cannot be missed.
    let mut cluster_events = harness.publisher.subscribe(0);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // The REAL production seam `run_server` commissions on every boot.
    let sweeper = harness.state.spawn_heartbeat_sweeper(shutdown_rx);

    let dispatcher = Arc::new(harness.dispatcher());
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request())
    }));
    let task = harness.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);

    // The worker now wedges: no heartbeat, no result, stream held OPEN. The
    // dispatch wait is unbounded by design, so ONLY the sweeper can end it.
    let wedged_at = Instant::now();
    let result = tokio::time::timeout(EXPIRY_DEADLINE, dispatch_task)
        .await
        .map_err(|_| {
            "dispatch still blocked after the expiry deadline; the heartbeat \
             sweeper is not expiring silent workers"
        })?
        .map_err(|join_error| join_error.to_string())?;
    let expiry_latency = wedged_at.elapsed();

    let error = result.err().ok_or("expected a lost-worker failure")?;
    assert!(
        error.starts_with("retryable:"),
        "heartbeat expiry must be retryable so the engine can re-dispatch: {error}"
    );
    assert!(
        error.contains("lost before reporting activity result"),
        "failure must name worker loss: {error}"
    );
    assert!(
        expiry_latency > HEARTBEAT_WINDOW,
        "expired in {expiry_latency:?}, before the {HEARTBEAT_WINDOW:?} window \
         elapsed — the sweep must never fail a worker still inside its window"
    );
    assert!(
        harness
            .registry
            .workers_for(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, None)?
            .is_empty(),
        "the expired worker must be deregistered from routing"
    );
    assert_eq!(
        harness.state.heartbeat_tracker().in_flight_count()?,
        0,
        "the expired worker's tasks must be removed from liveness tracking"
    );

    // The WS3 delta proves WHICH path deregistered: the sweep's provable
    // Timeout, not a stream-teardown Disconnect.
    let delta = tokio::time::timeout(Duration::from_secs(5), cluster_events.next())
        .await
        .map_err(|_| "no cluster delta after expiry")?
        .ok_or("cluster event stream closed")?
        .map_err(|lagged| format!("cluster stream lagged: skipped {}", lagged.skipped))?;
    let ClusterEvent::WorkerDisconnected { reason, .. } = delta else {
        return Err(format!("expected a WorkerDisconnected delta, got {delta:?}").into());
    };
    assert_eq!(
        reason,
        WorkerDeathReason::Timeout,
        "the sweep must attribute the death to the provable Timeout reason"
    );

    // #176 major-finding fix (no zombie sessions): the server must TERMINATE
    // the expired worker's stream, not silently deregister it — otherwise the
    // worker sits connected-but-unroutable, its heartbeats rejected, never
    // re-registering. The client's next read must observe the terminal status
    // the write forwarder emits when the registry drops its delivery sender.
    let terminal = tokio::time::timeout(Duration::from_secs(5), harness.inbound.message())
        .await
        .map_err(|_| {
            "expired worker's stream was never terminated by the server; \
             a live-but-deregistered worker would zombie"
        })?;
    let status = match terminal {
        Err(status) => status,
        Ok(frame) => {
            return Err(format!(
                "expected the expired worker's stream to end with a terminal status, got {frame:?}"
            )
            .into());
        }
    };
    assert_eq!(
        status.code(),
        tonic::Code::Unavailable,
        "termination must be retryable-unavailable so the worker's reconnect \
         machinery re-registers: {status}"
    );
    assert!(
        status.message().contains("deregistered"),
        "termination must name the deregistration: {status}"
    );

    // Double-fail safety: the worker's stream has now ended, firing the
    // teardown sweep on the already-expired worker. The idempotent core must
    // make it a no-op — no second WorkerDisconnected delta, no
    // double-completion.
    drop(harness.worker_tx);
    drop(harness.inbound);
    let second = tokio::time::timeout(Duration::from_secs(1), cluster_events.next()).await;
    assert!(
        second.is_err(),
        "teardown after the sweep already expired the worker must be a no-op, \
         got a second delta: {second:?}"
    );

    // Shutdown cleanliness: the sweeper exits promptly on the shared watch.
    shutdown_tx.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), sweeper)
        .await
        .map_err(|_| "sweeper did not stop after shutdown")??;
    harness.server.abort();
    Ok(())
}

/// False-positive guard: a worker that keeps heartbeating stays registered and
/// its dispatch stays pending across several heartbeat windows, then completes
/// normally — the sweeper must only expire the genuinely silent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeating_worker_survives_the_sweeper_and_completes() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let sweeper = harness.state.spawn_heartbeat_sweeper(shutdown_rx);

    let dispatcher = Arc::new(harness.dispatcher());
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request())
    }));
    let task = harness.next_task().await?;

    // "Work" for three full heartbeat windows, heartbeating well inside each.
    let work_deadline = Instant::now() + HEARTBEAT_WINDOW * 3;
    while Instant::now() < work_deadline {
        harness.heartbeat(&task).await?;
        tokio::time::sleep(HEARTBEAT_WINDOW / 5).await;
    }
    assert!(
        !dispatch_task.is_finished(),
        "a heartbeating worker's dispatch must never be failed by the sweeper"
    );
    assert!(
        !harness
            .registry
            .workers_for(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, None)?
            .is_empty(),
        "a heartbeating worker must stay registered across windows"
    );

    harness.complete(task, br#"{"greeting":"hello"}"#).await?;
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    assert_eq!(result, Ok(r#"{"greeting":"hello"}"#.to_owned()));

    shutdown_tx.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), sweeper)
        .await
        .map_err(|_| "sweeper did not stop after shutdown")??;
    harness.server.abort();
    Ok(())
}

/// #176 critical-finding regression proof, full stack: a REAL
/// `aion_worker::Worker` (the production Rust runtime every shipped worker —
/// norn-fan-worker, stacked-dev — is built on) serving an activity that runs
/// THREE TIMES the heartbeat window, whose handler never calls
/// `ActivityContext::heartbeat`, with the production sweeper running the
/// whole time. The runtime's automatic liveness pump must keep the task
/// beating: the dispatch completes successfully, and the worker is never
/// expired with the `Timeout` reason. Before the pump existed this exact
/// setup looped every long activity into retry-exhaustion.
#[derive(serde::Serialize, serde::Deserialize)]
struct GreetInput {}

#[derive(serde::Serialize)]
struct GreetOutput {
    greeting: String,
}

/// Real `greet` handler for the full-stack pump proof: three full heartbeat
/// windows of genuine handler work, with NO explicit heartbeat call anywhere.
fn slow_greet(
    _input: GreetInput,
    _context: &aion_worker::ActivityContext,
) -> aion_worker::HandlerFuture<'_, GreetOutput> {
    Box::pin(async move {
        tokio::time::sleep(HEARTBEAT_WINDOW * 3).await;
        Ok(GreetOutput {
            greeting: String::from("hello"),
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_worker_runtime_survives_activity_three_windows_long() -> Result<(), TestError> {
    let harness = ServerHarness::start().await?;
    // Subscribe BEFORE the worker connects so no delta can be missed.
    let mut cluster_events = harness.publisher.subscribe(0);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let sweeper = harness.state.spawn_heartbeat_sweeper(shutdown_rx);

    let worker_config = aion_worker::WorkerConfig::builder()
        .endpoint(format!("http://{}", harness.address))
        .task_queue(TASK_QUEUE)
        .identity("slow-real-worker")
        .max_concurrency(2)
        .reconnect_initial_backoff(Duration::from_millis(50))
        .reconnect_max_backoff(Duration::from_millis(500))
        .reconnect_max_attempts(3)
        .namespace(NAMESPACE)
        .subject("slow-real-worker")
        .build()
        .map_err(|error| error.to_string())?;
    let worker = aion_worker::Worker::builder(worker_config)
        .register_activity(ACTIVITY_TYPE, slow_greet)
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    let (worker_shutdown_tx, worker_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let worker_task = tokio::spawn(worker.run_until(async move {
        let _ = worker_shutdown_rx.await;
    }));

    // The production dispatcher blocks until the worker registers, then the
    // dispatch itself must survive all three windows and complete.
    let dispatcher = Arc::new(harness.dispatcher());
    let dispatched_at = Instant::now();
    let result = tokio::time::timeout(
        EXPIRY_DEADLINE,
        tokio::spawn(futures::future::lazy(move |_| {
            dispatcher.dispatch(greet_request())
        })),
    )
    .await
    .map_err(|_| "dispatch did not complete within the deadline")?
    .map_err(|join_error| join_error.to_string())?;
    let served_in = dispatched_at.elapsed();

    assert_eq!(
        result,
        Ok(r#"{"greeting":"hello"}"#.to_owned()),
        "a long activity on the real runtime must complete, not be expired"
    );
    assert!(
        served_in >= HEARTBEAT_WINDOW * 3,
        "the handler genuinely ran three windows ({served_in:?}), so success \
         proves the runtime pump — not a fast path"
    );
    assert!(
        !harness
            .registry
            .workers_for(NAMESPACE, TASK_QUEUE, ACTIVITY_TYPE, None)?
            .is_empty(),
        "the worker must still be registered after serving the long activity"
    );

    // No delta on the stream may be a Timeout death: the sweeper must never
    // have expired the healthy worker.
    while let Ok(Some(Ok(delta))) =
        tokio::time::timeout(Duration::from_millis(200), cluster_events.next()).await
    {
        if let ClusterEvent::WorkerDisconnected { reason, .. } = delta {
            assert_ne!(
                reason,
                WorkerDeathReason::Timeout,
                "the sweeper expired a worker whose runtime was healthy and pumping"
            );
        }
    }

    let _ = worker_shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), worker_task)
        .await
        .map_err(|_| "worker did not shut down")?
        .map_err(|join_error| join_error.to_string())?
        .map_err(|error| error.to_string())?;
    shutdown_tx.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), sweeper)
        .await
        .map_err(|_| "sweeper did not stop after shutdown")??;
    harness.server.abort();
    Ok(())
}

/// Shutdown cleanliness in isolation: with the DEFAULT 30s window (7.5s sweep
/// cadence), the sweeper must exit on the watch flip well before its next tick
/// — proving the shutdown arm of the select, not tick-aligned luck.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweeper_exits_promptly_on_shutdown_between_ticks() -> Result<(), TestError> {
    let resolver = NamespaceResolver::authorization_only(
        NamespaceMode::SharedEngine,
        StaticWorkflowNamespaces::default(),
        StaticScheduleNamespaces::default(),
    );
    let state = ServerState::from_parts(resolver, runtime_config(Duration::from_secs(30)));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let sweeper = state.spawn_heartbeat_sweeper(shutdown_rx);

    // Let the task start and take its immediate first tick.
    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown_tx.send(true)?;
    let stopped_in = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), sweeper)
        .await
        .map_err(|_| "sweeper did not stop after shutdown")??;
    assert!(
        stopped_in.elapsed() < Duration::from_secs(2),
        "sweeper must exit on the watch, not wait out its 7.5s tick"
    );
    Ok(())
}
