//! End-to-end regression coverage for remote activity dispatch latency.
//!
//! Reproduces the hello-world quickstart wiring with no synthetic stand-ins:
//! a real tonic `WorkerProtocol` bidirectional stream over TCP loopback, the
//! real connected-worker registry, the real per-stream forwarder task spawned
//! inside `stream_worker`, the real pending-activities completion sink, and a
//! dispatch invoked from inside a spawned tokio task exactly the way the
//! engine's `spawn_completion_task` does it (`futures::future::lazy` polled
//! on a runtime worker thread).
//!
//! Guards against the production defect where the queued `ActivityTask` was
//! only flushed to the worker's gRPC stream when the dispatch timeout fired:
//! the `try_send` wake landed the forwarder task in the blocked dispatch
//! thread's non-stealable LIFO slot, so every remote activity failed with
//! `ActivityTimeout` even though the worker was healthy and idle.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::ActivityDispatcher as _;
use aion_proto::generated::worker_protocol_client::WorkerProtocolClient;
use aion_proto::generated::{self, server_to_worker, worker_to_server};
use aion_server::ServerState;
use aion_server::api::worker_grpc::worker_service;
use aion_server::config::{
    AuthConfig, DashboardAssetSource, DashboardConfig, ListenConfig, MetricsConfig,
    NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use aion_server::worker::{ConnectedWorkerRegistry, WorkerActivityDispatcher};
use aion_server::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const ACTIVITY_TYPE: &str = "greet";

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
        },
        workflow_packages: Vec::new(),
        scheduler_threads: 1,
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
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Harness {
    /// Boot the real worker gRPC service on a loopback port and connect one
    /// worker stream registered for [`ACTIVITY_TYPE`] in [`NAMESPACE`].
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
        let inbound = client.stream_worker(request).await?.into_inner();

        // The registration is processed asynchronously by the stream handler;
        // dispatch is only meaningful once the worker is in the registry.
        let deadline = Instant::now() + Duration::from_secs(10);
        while registry.workers_for(NAMESPACE, ACTIVITY_TYPE)?.is_empty() {
            if Instant::now() >= deadline {
                return Err("worker registration did not reach the registry".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Ok(Self {
            state,
            registry,
            worker_tx,
            inbound,
            server,
        })
    }

    /// Build the production dispatcher over the harness's shared state, the
    /// same wiring `ServerState::build_with_store_arc` hands to the engine.
    fn dispatcher(&self) -> WorkerActivityDispatcher {
        WorkerActivityDispatcher::new(self.registry.clone(), NAMESPACE)
            .with_pending(self.state.pending_activities().clone())
            .with_heartbeat_tracker(self.state.heartbeat_tracker().clone())
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
/// gRPC worker session and asserts the task is delivered promptly (bounded
/// well under the 30s dispatch timeout) and the result round-trips.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_dispatch_delivers_task_promptly_and_round_trips() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    let dispatcher = Arc::new(harness.dispatcher());

    let started = Instant::now();
    // Invoke exactly like the engine's spawn_completion_task: the sync
    // dispatch runs inside the first poll of a spawned tokio task.
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(ACTIVITY_TYPE, r#"{"name":"world"}"#, "{}")
    }));

    let task = harness.next_task().await?;
    let delivery_elapsed = started.elapsed();
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
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

/// The timeout must fire only when the worker genuinely stays silent — and
/// the task must still have been delivered promptly beforehand, proving the
/// timeout machinery is independent of task delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_times_out_only_when_worker_stays_silent() -> Result<(), TestError> {
    let mut harness = Harness::start().await?;
    let dispatcher = Arc::new(harness.dispatcher().with_timeout(Duration::from_secs(2)));

    let started = Instant::now();
    let dispatch_task = tokio::spawn(futures::future::lazy(move |_| {
        dispatcher.dispatch(ACTIVITY_TYPE, "{}", "{}")
    }));

    // The worker receives the task well before the timeout but never replies.
    let task = harness.next_task().await?;
    let delivery_elapsed = started.elapsed();
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
    assert!(
        delivery_elapsed < Duration::from_secs(1),
        "task took {delivery_elapsed:?} to reach the worker stream; with the \
         dispatch stall defect it would only arrive when the 2s timeout fired"
    );

    let result = dispatch_task.await.map_err(|error| error.to_string())?;
    let error = result.err().ok_or("expected dispatch to time out")?;
    assert!(
        error.contains("timed out after 2s"),
        "unexpected error: {error}"
    );

    harness.server.abort();
    Ok(())
}
