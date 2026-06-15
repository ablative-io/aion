//! Cross-stack end-to-end proof of the worker-protocol ack contract (brief
//! test 30): the real Rust `aion-worker` SDK against the real `aion-server`
//! worker service over TCP loopback.
//!
//! register → `RegisterAck` → dispatch (attempt stamped) → execute → report →
//! `ResultAck`; then `broadcast_drain` → the worker finishes, redials after its
//! initial backoff without consuming drop budget, re-registers (fresh
//! `WorkerId` in the registry), and serves again.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aion::{ActivityDispatch, ActivityDispatcher as _};
use aion_core::{ActivityId, WorkflowId};
use aion_server::ServerState;
use aion_server::api::worker_grpc::worker_service;
use aion_server::config::{
    AuthConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig, MetricsConfig,
    NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
};
use aion_server::worker::{ConnectedWorkerRegistry, WorkerActivityDispatcher};
use aion_server::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
use aion_worker::{ReconnectConfig, Worker};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;

type TestError = Box<dyn std::error::Error>;

const NAMESPACE: &str = "default";
const ACTIVITY_TYPE: &str = "greet";

/// A `greet` dispatch request carrying real (test-synthesized) ids, the
/// engine-seam shape `WorkerActivityDispatcher::dispatch` consumes.
fn greet_request(input: &str, attempt: u32) -> ActivityDispatch {
    ActivityDispatch {
        namespace: NAMESPACE.to_owned(),
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(0),
        name: ACTIVITY_TYPE.to_owned(),
        input: input.to_owned(),
        config: "{}".to_owned(),
        attempt,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GreetInput {
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GreetOutput {
    greeting: String,
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
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: NAMESPACE.to_owned(),
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
    }
}

async fn wait_for_worker(
    registry: &ConnectedWorkerRegistry,
    not_id: Option<u64>,
) -> Result<u64, TestError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let workers = registry.workers_for(NAMESPACE, ACTIVITY_TYPE)?;
        if let Some(handle) = workers
            .iter()
            .find(|handle| Some(handle.id().value()) != not_id)
        {
            return Ok(handle.id().value());
        }
        if Instant::now() >= deadline {
            return Err("worker did not register with the server in time".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_worker_sdk_handshakes_serves_and_rides_through_drain() -> Result<(), TestError> {
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
            .add_service(worker_service(state.clone()))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );

    // Real SDK worker: one `greet` activity that records the attempt its
    // context exposes (the wire-stamped value, never a consumer default).
    let observed_attempts: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let worker_config = aion_worker::WorkerConfig::new(
        format!("http://{address}"),
        NAMESPACE,
        "rust-e2e-worker",
        2,
        ReconnectConfig::new(Duration::from_millis(50), Duration::from_secs(2), 2),
        None,
    );
    let worker = Worker::builder(worker_config)
        .register_activity("greet", {
            let observed_attempts = Arc::clone(&observed_attempts);
            move |input: GreetInput, context: &aion_worker::ActivityContext| {
                let observed_attempts = Arc::clone(&observed_attempts);
                let attempt = context.attempt();
                Box::pin(async move {
                    if let Ok(mut attempts) = observed_attempts.lock() {
                        attempts.push(attempt);
                    }
                    Ok(GreetOutput {
                        greeting: format!("hello {}", input.name),
                    })
                })
            }
        })?
        .build()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let worker_run = tokio::spawn(worker.run_until(async move {
        let _ = shutdown_rx.await;
    }));

    // Registration is complete (the SDK consumed the RegisterAck) once the
    // worker is dispatch-eligible in the registry.
    let first_worker_id = wait_for_worker(&registry, None).await?;

    let dispatcher = Arc::new(
        WorkerActivityDispatcher::new(
            registry.clone(),
            NAMESPACE,
            state.heartbeat_tracker().clone(),
        )
        .with_pending(state.pending_activities().clone())
        .with_drain_state(state.drain_state().clone()),
    );

    // Dispatch with a non-default attempt: the worker's handler context must
    // observe exactly the stamped value, proving the wire field end to end.
    let dispatch = Arc::clone(&dispatcher);
    let result = tokio::task::spawn_blocking(move || {
        dispatch.dispatch(greet_request(r#"{"name":"world"}"#, 3))
    })
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(result, Ok(r#"{"greeting":"hello world"}"#.to_owned()));

    // Drain broadcast: the worker must classify it as an unbudgeted drain,
    // redial after its initial backoff, and re-register with a fresh id.
    let delivered = registry.broadcast_drain()?;
    assert_eq!(delivered, 1, "the drain frame must reach the live worker");
    let second_worker_id = wait_for_worker(&registry, Some(first_worker_id)).await?;
    assert_ne!(
        second_worker_id, first_worker_id,
        "the post-drain session must be a fresh registration"
    );

    // The redialled session serves again: the run survived the drain (with a
    // budget of 2, a budgeted classification plus establishment would not
    // leave the worker this healthy this quickly).
    let dispatch = Arc::clone(&dispatcher);
    let result = tokio::task::spawn_blocking(move || {
        dispatch.dispatch(greet_request(r#"{"name":"again"}"#, 1))
    })
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(result, Ok(r#"{"greeting":"hello again"}"#.to_owned()));

    let attempts = observed_attempts
        .lock()
        .map_err(|_| "attempt log poisoned")?
        .clone();
    assert_eq!(
        attempts,
        vec![3, 1],
        "handler contexts must expose the wire-stamped attempts"
    );

    shutdown_tx.send(()).map_err(|()| "shutdown send failed")?;
    let run_result = tokio::time::timeout(Duration::from_secs(10), worker_run)
        .await
        .map_err(|_| "worker did not shut down promptly")?
        .map_err(|error| error.to_string())?;
    run_result?;

    server.abort();
    Ok(())
}
