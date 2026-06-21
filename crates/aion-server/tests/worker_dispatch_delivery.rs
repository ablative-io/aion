//! End-to-end regression coverage for remote activity dispatch latency.
//!
//! Reproduces the hello-world quickstart wiring with no synthetic stand-ins:
//! a real tonic `WorkerProtocol` bidirectional stream over TCP loopback, the
//! real connected-worker registry, the real per-stream forwarder task spawned
//! inside `stream_worker`, the real pending-activities completion sink, and a
//! dispatch invoked synchronously from inside a spawned tokio task — the
//! worst-case calling context `dispatch` defends against with
//! `block_in_place` (the engine itself now routes through
//! `dispatch_async_from_process`, which runs the sync dispatch on the
//! blocking pool).
//!
//! Guards against the production defect where the queued `ActivityTask` was
//! only flushed to the worker's gRPC stream when the dispatch timeout fired:
//! the `try_send` wake landed the forwarder task in the blocked dispatch
//! thread's non-stealable LIFO slot, so every remote activity failed with
//! `ActivityTimeout` even though the worker was healthy and idle.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::{ActivityDispatch, ActivityDispatcher as _};
use aion_core::{ActivityId, WorkflowId};
use aion_proto::generated::worker_protocol_client::WorkerProtocolClient;
use aion_proto::generated::{self, server_to_worker, worker_to_server};
use aion_server::ServerState;
use aion_server::api::worker_grpc::worker_service;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
    MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use aion_server::worker::{ConnectedWorkerRegistry, WorkerActivityDispatcher};
use aion_server::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const ACTIVITY_TYPE: &str = "greet";

/// A `greet` dispatch request carrying real (test-synthesized) ids, the
/// engine-seam shape `WorkerActivityDispatcher::dispatch` consumes. The
/// worker echoes these ids back on the wire, so completion keys correctly.
fn greet_request(input: &str, attempt: u32) -> ActivityDispatch {
    ActivityDispatch {
        namespace: NAMESPACE.to_owned(),
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(0),
        name: ACTIVITY_TYPE.to_owned(),
        input: input.to_owned(),
        config: "{}".to_owned(),
        attempt,
        labels: std::collections::BTreeMap::new(),
    }
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
        authoring: AuthoringConfig::default(),
        dev: aion_server::config::DevConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
    }
}

/// A live in-process server plus one registered remote worker stream.
struct Harness {
    state: ServerState,
    registry: ConnectedWorkerRegistry,
    /// Keeps the worker's outbound request stream open for the test duration.
    worker_tx: tokio::sync::mpsc::Sender<generated::WorkerToServer>,
    inbound: tonic::Streaming<generated::ServerToWorker>,
    /// The `RegisterAck` consumed as the guaranteed first response frame.
    register_ack: generated::RegisterAck,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Harness {
    /// Boot the real worker gRPC service on a loopback port and connect one
    /// worker stream registered for [`ACTIVITY_TYPE`] in [`NAMESPACE`].
    ///
    /// Consumes — and thereby pins — the `RegisterAck` as the first frame on
    /// the response stream: any other first frame fails the harness.
    async fn start() -> Result<Self, TestError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let registry = ConnectedWorkerRegistry::default();
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        );
        let state =
            ServerState::from_parts_with_registry(resolver, runtime_config(), registry.clone());
        let server = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(worker_service(state.clone()))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );

        let mut client = WorkerProtocolClient::connect(format!("http://{address}")).await?;
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<generated::WorkerToServer>(8);
        worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Register(
                    generated::RegisterWorker {
                        namespace: NAMESPACE.to_owned(),
                        activity_types: vec![ACTIVITY_TYPE.to_owned()],
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
        // is dispatch-eligible in the registry — no polling needed.
        let first = inbound
            .message()
            .await?
            .and_then(|frame| frame.message)
            .ok_or("response stream ended before the RegisterAck")?;
        let server_to_worker::Message::RegisterAck(register_ack) = first else {
            return Err(format!("first response frame must be RegisterAck, got {first:?}").into());
        };
        if registry.workers_for(NAMESPACE, ACTIVITY_TYPE)?.is_empty() {
            return Err("RegisterAck arrived before the registry registration".into());
        }

        Ok(Self {
            state,
            registry,
            worker_tx,
            inbound,
            register_ack,
            server,
        })
    }

    /// Build the production dispatcher over the harness's shared state, the
    /// same wiring `ServerState::build_with_store_arc` hands to the engine.
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

    /// Wait for the next `ResultAck` pushed down the worker stream.
    async fn next_result_ack(&mut self) -> Result<generated::ResultAck, TestError> {
        while let Some(message) = self.inbound.message().await? {
            if let Some(server_to_worker::Message::ResultAck(ack)) = message.message {
                return Ok(ack);
            }
        }
        Err("worker stream closed before a result ack was delivered".into())
    }

    /// Wait for the next `DrainRequest` pushed down the worker stream.
    async fn next_drain(&mut self) -> Result<(), TestError> {
        while let Some(message) = self.inbound.message().await? {
            if let Some(server_to_worker::Message::Drain(_)) = message.message {
                return Ok(());
            }
        }
        Err("worker stream closed before a drain request was delivered".into())
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

/// Schedules an activity through the real dispatcher/bridge against a real
/// gRPC worker session and asserts the task is delivered promptly and the
/// result round-trips.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_dispatch_delivers_task_promptly_and_round_trips() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    let dispatcher = Arc::new(harness.dispatcher());

    let started = Instant::now();
    // Invoke the sync dispatch inside the first poll of a spawned tokio
    // task: the worst-case calling context the `block_in_place` guard in
    // `dispatch` defends against.
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request(r#"{"name":"world"}"#, 3))
    }));

    let task = harness.next_task().await?;
    let delivery_elapsed = started.elapsed();
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
    assert_eq!(
        task.attempt, 3,
        "the engine-seam attempt must be stamped onto the wire task"
    );
    assert!(
        delivery_elapsed < Duration::from_secs(5),
        "task took {delivery_elapsed:?} to reach the worker stream; delivery \
         must not be coupled to the dispatch timeout"
    );

    harness
        .complete(task, br#"{"greeting":"hello world"}"#)
        .await?;
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    let round_trip_elapsed = started.elapsed();

    assert_eq!(result, Ok(r#"{"greeting":"hello world"}"#.to_owned()));
    assert!(
        round_trip_elapsed < Duration::from_secs(5),
        "dispatch round trip took {round_trip_elapsed:?}"
    );

    harness.server.abort();
    Ok(())
}

/// The engine imposes no activity timeout of its own: the dispatch wait is
/// unbounded by construction (there is no `recv_timeout` against any
/// constant left in the dispatch path — the former hardcoded 30s deadline
/// killed every activity longer than 30s, and agent activities legitimately
/// run for over an hour). This test holds the worker's reply back for
/// longer than a bounded wait would tolerate and proves the dispatch still
/// returns the genuine result.
///
/// Honesty note: the default 2s delay keeps CI sane; it proves a delayed
/// completion round-trips, while the absence of any deadline constant is
/// structural (the field, builder, and timeout arm were deleted). Set
/// `AION_PROVE_LONG_ACTIVITY=1` to run the literal proof that an activity
/// outlives the deleted 30s deadline (35s wall-clock).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activity_completing_after_long_delay_round_trips() -> Result<(), TestError> {
    let delay = if std::env::var_os("AION_PROVE_LONG_ACTIVITY").is_some() {
        Duration::from_secs(35)
    } else {
        Duration::from_secs(2)
    };
    let mut harness = Harness::start().await?;
    let dispatcher = Arc::new(harness.dispatcher());

    let started = Instant::now();
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request("{}", 1))
    }));

    // The worker receives the task promptly, then "works" for the delay.
    let task = harness.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
    tokio::time::sleep(delay).await;
    assert!(
        !dispatch_task.is_finished(),
        "dispatch terminated during the {delay:?} work window; nothing but \
         completion, worker loss, or drain may end the wait"
    );

    harness.complete(task, br#"{"slow":true}"#).await?;
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    let elapsed = started.elapsed();

    assert_eq!(result, Ok(r#"{"slow":true}"#.to_owned()));
    assert!(
        elapsed >= delay,
        "result arrived in {elapsed:?}, before the worker finished?"
    );

    harness.server.abort();
    Ok(())
}

/// Worker death mid-activity: the worker receives the task, then its stream
/// ends without ever reporting a result. Because the dispatch wait is
/// unbounded, the stream teardown sweep is what must unblock it — promptly,
/// with the retryable lost-worker classification the engine's retry policy
/// acts on — rather than the workflow hanging forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_death_mid_activity_fails_dispatch_with_retryable_lost_worker()
-> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    let dispatcher = Arc::new(harness.dispatcher());

    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request("{}", 1))
    }));
    let task = harness.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);

    // Kill the worker mid-activity: dropping its request stream ends the
    // gRPC session exactly as a process crash or network cut does.
    let died_at = Instant::now();
    drop(harness.worker_tx);
    drop(harness.inbound);

    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    let failure_latency = died_at.elapsed();
    let error = result.err().ok_or("expected a lost-worker failure")?;
    assert!(
        error.starts_with("retryable:"),
        "worker loss must be retryable so the engine can re-dispatch: {error}"
    );
    assert!(
        error.contains("lost before reporting activity result"),
        "failure must name worker loss: {error}"
    );
    assert!(
        failure_latency < Duration::from_secs(10),
        "lost-worker failure took {failure_latency:?}; the unbounded dispatch \
         wait must terminate on worker loss, not hang"
    );
    assert!(
        harness
            .state
            .worker_registry()
            .workers_for(NAMESPACE, ACTIVITY_TYPE)?
            .is_empty(),
        "the dead worker must be deregistered"
    );
    assert_eq!(
        harness.state.heartbeat_tracker().in_flight_count()?,
        0,
        "the swept activity must leave in-flight accounting"
    );

    harness.server.abort();
    Ok(())
}

/// Brief test 3: `RegisterAck` is the first response frame, carries the
/// configured heartbeat window and the authorized namespace, and only then
/// does a dispatched task arrive — pinning the ack-before-task ordering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_ack_is_first_frame_then_task() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;

    // Harness::start already failed if the first frame was not the ack.
    assert_eq!(harness.register_ack.namespace, NAMESPACE);
    assert_eq!(
        harness.register_ack.heartbeat_window_ms, 30_000,
        "the ack must carry the operator-configured heartbeat window"
    );
    assert!(
        harness.register_ack.worker_id > 0,
        "the ack must carry the server-assigned worker id"
    );

    let dispatcher = Arc::new(harness.dispatcher());
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request("{}", 1))
    }));
    let task = harness.next_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
    harness.complete(task, br#"{"greeting":"hi"}"#).await?;
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    assert_eq!(result, Ok(r#"{"greeting":"hi"}"#.to_owned()));

    harness.server.abort();
    Ok(())
}

/// Brief test 4: a denied registration still fails the RPC with
/// `PermissionDenied` and delivers no frames — the denial taxonomy is
/// byte-for-byte unchanged; there is no nack frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_registration_fails_rpc_without_frames() -> Result<(), TestError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let registry = ConnectedWorkerRegistry::default();
    let resolver = NamespaceResolver::authorization_only(
        NamespaceMode::SharedEngine,
        StaticWorkflowNamespaces::default(),
        StaticScheduleNamespaces::default(),
    );
    let state = ServerState::from_parts_with_registry(resolver, runtime_config(), registry.clone());
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(worker_service(state))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );

    let mut client = WorkerProtocolClient::connect(format!("http://{address}")).await?;
    let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<generated::WorkerToServer>(8);
    worker_tx
        .send(generated::WorkerToServer {
            message: Some(worker_to_server::Message::Register(
                generated::RegisterWorker {
                    // Registers a namespace the metadata grant does not cover.
                    namespace: "ungranted".to_owned(),
                    activity_types: vec![ACTIVITY_TYPE.to_owned()],
                },
            )),
        })
        .await?;
    let mut request = tonic::Request::new(ReceiverStream::new(worker_rx));
    request
        .metadata_mut()
        .insert("x-aion-namespaces", NAMESPACE.parse()?);

    let denial = match client.stream_worker(request).await {
        Ok(mut response) => {
            // Some transports surface the denial on the first stream read
            // rather than the RPC call itself; either way no frame arrives.
            match response.get_mut().message().await {
                Ok(Some(frame)) => {
                    return Err(format!("denied registration delivered a frame: {frame:?}").into());
                }
                Ok(None) => {
                    return Err("denied registration ended the stream without a status".into());
                }
                Err(status) => status,
            }
        }
        Err(status) => status,
    };
    assert_eq!(denial.code(), tonic::Code::PermissionDenied);
    assert!(registry.workers_for("ungranted", ACTIVITY_TYPE)?.is_empty());

    server.abort();
    Ok(())
}

/// Brief tests 5 + 6: every well-formed result frame is answered with a
/// `ResultAck` carrying its ids — including a duplicate re-report whose
/// pending waiter is already gone (its obligation is equally discharged).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn result_frames_are_acked_including_duplicates() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    let dispatcher = Arc::new(harness.dispatcher());

    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request("{}", 1))
    }));
    let task = harness.next_task().await?;
    let workflow_id = task.workflow_id.clone();
    let activity_id = task.activity_id;

    harness.complete(task.clone(), br#"{"ok":true}"#).await?;
    let ack = harness.next_result_ack().await?;
    assert_eq!(ack.workflow_id, workflow_id);
    assert_eq!(ack.activity_id, activity_id);
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    assert_eq!(result, Ok(r#"{"ok":true}"#.to_owned()));

    // Duplicate re-report: the pending waiter is gone, the engine cannot
    // apply it again — but the ack must still come back so the worker stops
    // re-reporting forever.
    harness.complete(task, br#"{"ok":true}"#).await?;
    let duplicate_ack = harness.next_result_ack().await?;
    assert_eq!(duplicate_ack.workflow_id, workflow_id);
    assert_eq!(duplicate_ack.activity_id, activity_id);

    harness.server.abort();
    Ok(())
}

/// Brief test 7: a malformed result (missing activity id) produces no ack —
/// there is no key to ack with — and the stream stays healthy: a subsequent
/// well-formed exchange still round-trips and acks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_result_gets_no_ack_and_stream_stays_healthy() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;

    harness
        .worker_tx
        .send(generated::WorkerToServer {
            message: Some(worker_to_server::Message::Result(
                generated::ActivityResult {
                    workflow_id: Some(generated::WorkflowId {
                        uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
                    }),
                    activity_id: None,
                    outcome: Some(generated::activity_result::Outcome::Result(
                        generated::Payload {
                            content_type: "application/json".to_owned(),
                            bytes: b"{}".to_vec(),
                        },
                    )),
                },
            )),
        })
        .await?;

    // A well-formed exchange afterwards: its task and ack are the next
    // frames on the stream, proving the malformed frame produced neither an
    // ack nor a stream teardown.
    let dispatcher = Arc::new(harness.dispatcher());
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(greet_request("{}", 1))
    }));
    let task = harness.next_task().await?;
    let workflow_id = task.workflow_id.clone();
    harness.complete(task, br#"{"ok":true}"#).await?;
    let ack = harness.next_result_ack().await?;
    assert_eq!(
        ack.workflow_id, workflow_id,
        "the only ack on the stream must belong to the well-formed result"
    );
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    assert_eq!(result, Ok(r#"{"ok":true}"#.to_owned()));

    harness.server.abort();
    Ok(())
}

/// Brief test 10: the drain broadcast reaches the worker stream as a
/// `DrainRequest` frame, and post-drain dispatch is rejected by the gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_broadcast_reaches_worker_and_gates_dispatch() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;

    assert!(harness.state.drain_state().begin());
    let delivered = harness.state.worker_registry().broadcast_drain()?;
    assert_eq!(delivered, 1);
    harness.next_drain().await?;

    let dispatcher = harness.dispatcher();
    let dispatch_task =
        tokio::task::spawn_blocking(move || dispatcher.dispatch(greet_request("{}", 1)));
    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    let error = result.err().ok_or("post-drain dispatch must be rejected")?;
    assert!(
        error.contains("draining"),
        "rejection must name the drain gate: {error}"
    );

    harness.server.abort();
    Ok(())
}
