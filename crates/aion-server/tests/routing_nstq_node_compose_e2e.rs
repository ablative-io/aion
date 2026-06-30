//! Composition proof for the NSTQ + NODE dispatch rethread.
//!
//! The namespace / `task_queue` / node routing slices are each unit-green; this
//! test proves they *compose* end to end through the production push-dispatch
//! path. It stands up the real `aion-server` worker gRPC service on a loopback
//! port, connects several real worker streams over `WorkerProtocolClient` (each
//! registered through the same `accept_registration` -> `register_namespaces`
//! seam a live worker uses, differing across namespace, `task_queue`, and node),
//! and then drives the production [`ActivityDispatcher`] (the NODE-aware push
//! dispatcher wired at `run.rs` and fed by the outbox dispatcher) against the
//! shared registry. Routing is asserted by observing which worker's *real gRPC
//! stream* actually receives the `ActivityTask` frame, and — just as load
//! bearing — that every mismatched worker receives nothing.
//!
//! Why this harness level: the rethreaded routing core is
//! `ActivityDispatcher::dispatch` (`crates/aion-server/src/worker/dispatch.rs`),
//! which reads `(namespace, task_queue, node)` straight off the `ScheduledActivity`
//! and calls `registry.workers_for(ns, tq, type, node)`. Registration is the real
//! gRPC `stream_worker` path. This exercises the production wiring of all three
//! dimensions at once over a real transport, rather than the older
//! engine-seam bridge (`WorkerActivityDispatcher`) which still hard-codes
//! `node: None` and is therefore not the rethreaded path under test.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use aion_core::{ActivityId, ContentType, Payload, WorkflowId};
use aion_proto::generated::worker_protocol_client::WorkerProtocolClient;
use aion_proto::generated::{self, server_to_worker, worker_to_server};
use aion_server::ServerState;
use aion_server::api::worker_grpc::worker_service;
use aion_server::config::{
    AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig, NamespaceConfig,
    NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig, WebSocketConfig,
    WorkerConfig,
};
use aion_server::worker::{ActivityDispatcher, ConnectedWorkerRegistry, ScheduledActivity};
use aion_server::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

type TestError = Box<dyn std::error::Error>;

const ACTIVITY_TYPE: &str = "charge";

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
        worker: WorkerConfig {
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
        outbox: aion_server::config::OutboxConfig::default(),
        scheduler_threads: 1,
        query_timeout: Some(Duration::from_millis(10_000)),
        default_namespace: "default".to_owned(),
        auto_create: aion_server::config::AutoCreate::Open,
        max_in_flight_activities: aion_server::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
        drain_timeout: Duration::from_secs(30),
        metrics: MetricsConfig { enabled: false },
        owned_shards: Vec::new(),
        cors_allowed_origins: Vec::new(),
    }
}

/// A live in-process worker gRPC service over loopback, plus the shared registry
/// and the production NODE-aware push dispatcher built over it.
struct Cluster {
    address: SocketAddr,
    dispatcher: ActivityDispatcher,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Cluster {
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
        // The production push dispatcher (dispatch.rs): the rethreaded routing
        // core that consumes (namespace, task_queue, node) off the activity.
        let dispatcher =
            ActivityDispatcher::new(registry.clone()).with_drain_state(state.drain_state().clone());
        Ok(Self {
            address,
            dispatcher,
            server,
        })
    }

    /// Connect and register one real worker stream serving `namespaces` under
    /// `task_queue` on `node` (empty `node` = unpinned). The worker is
    /// dispatch-eligible (its `RegisterAck` consumed) before this returns.
    async fn connect_worker(
        &self,
        label: &'static str,
        namespaces: &[&str],
        task_queue: &str,
        node: &str,
    ) -> Result<TestWorker, TestError> {
        let mut client = WorkerProtocolClient::connect(format!("http://{}", self.address)).await?;
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<generated::WorkerToServer>(8);
        worker_tx
            .send(generated::WorkerToServer {
                message: Some(worker_to_server::Message::Register(
                    generated::RegisterWorker {
                        namespaces: namespaces.iter().map(|ns| (*ns).to_owned()).collect(),
                        activity_types: vec![ACTIVITY_TYPE.to_owned()],
                        task_queue: task_queue.to_owned(),
                        node: node.to_owned(),
                    },
                )),
            })
            .await?;
        let mut request = tonic::Request::new(ReceiverStream::new(worker_rx));
        // Dev caller grants exactly the namespaces this worker serves; a worker
        // serving a SET {A,B} must be granted both for registration to scope.
        request
            .metadata_mut()
            .insert("x-aion-subject", "tester".parse()?);
        request
            .metadata_mut()
            .insert("x-aion-namespaces", namespaces.join(",").parse()?);
        let mut inbound = client.stream_worker(request).await?.into_inner();

        // RegisterAck is the registration-success signal: once read, the worker
        // is dispatch-eligible in the registry — no polling for membership.
        let first = inbound
            .message()
            .await?
            .and_then(|frame| frame.message)
            .ok_or("response stream ended before the RegisterAck")?;
        let server_to_worker::Message::RegisterAck(ack) = first else {
            return Err(format!("first response frame must be RegisterAck, got {first:?}").into());
        };

        Ok(TestWorker {
            label,
            worker_id: ack.worker_id,
            // Hold the request stream open for the worker's lifetime.
            _worker_tx: worker_tx,
            inbound,
        })
    }

    fn scheduled(namespace: &str, task_queue: &str, node: Option<&str>) -> ScheduledActivity {
        ScheduledActivity {
            namespace: namespace.to_owned(),
            task_queue: task_queue.to_owned(),
            activity_type: ACTIVITY_TYPE.to_owned(),
            node: node.map(str::to_owned),
            workflow_id: WorkflowId::new(uuid::Uuid::new_v4()),
            activity_id: ActivityId::from_sequence_position(0),
            run_id: None,
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            attempt: 1,
            labels: BTreeMap::new(),
        }
    }

    fn server(&self) {
        self.server.abort();
    }
}

/// One connected worker stream and the id the registry assigned it.
struct TestWorker {
    label: &'static str,
    worker_id: u64,
    _worker_tx: tokio::sync::mpsc::Sender<generated::WorkerToServer>,
    inbound: tonic::Streaming<generated::ServerToWorker>,
}

impl TestWorker {
    /// Block (bounded) until an `ActivityTask` frame arrives on this worker's
    /// real gRPC stream.
    async fn expect_task(&mut self) -> Result<generated::ActivityTask, TestError> {
        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(message) = self.inbound.message().await? {
                if let Some(server_to_worker::Message::Task(task)) = message.message {
                    return Ok::<_, TestError>(Some(task));
                }
            }
            Ok(None)
        })
        .await
        .map_err(|_| format!("worker {} received no task within the deadline", self.label))??;
        frame.ok_or_else(|| {
            format!("worker {} stream closed before a task arrived", self.label).into()
        })
    }

    /// Assert NO `ActivityTask` arrives on this worker's stream within a bounded
    /// settle window. The negative half of routing: a mismatched worker must be
    /// untouched. The window is generous relative to loopback dispatch latency
    /// (sub-millisecond in practice) so a routing leak would be caught reliably.
    async fn expect_no_task(&mut self) -> Result<(), TestError> {
        let outcome = tokio::time::timeout(Duration::from_millis(300), async {
            while let Some(message) = self.inbound.message().await? {
                if let Some(server_to_worker::Message::Task(task)) = message.message {
                    return Ok::<_, TestError>(Some(task));
                }
            }
            Ok(None)
        })
        .await;
        match outcome {
            // Timed out waiting (Err) or the stream ended with no task
            // (Ok(Ok(None))): both mean nothing was routed here — correct.
            Err(_) | Ok(Ok(None)) => Ok(()),
            Ok(Ok(Some(task))) => Err(format!(
                "worker {} wrongly received a task ({}); routing leaked across a \
                 dimension it should have isolated",
                self.label, task.activity_type
            )
            .into()),
            Ok(Err(error)) => Err(error),
        }
    }
}

/// NAMESPACE isolation: a worker in {a} and a worker in {b}; a dispatch in
/// namespace `a` reaches the `a` worker and NEVER the `b` worker. A third
/// worker serving the SET {a,b} is reachable from BOTH namespaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn namespace_routing_isolates_and_a_set_worker_serves_both() -> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    let mut worker_a = cluster
        .connect_worker("ns-a", &["a"], "default", "")
        .await?;
    let mut worker_b = cluster
        .connect_worker("ns-b", &["b"], "default", "")
        .await?;
    let mut worker_both = cluster
        .connect_worker("ns-ab", &["a", "b"], "queue-ab", "")
        .await?;

    // Dispatch in namespace `a` (its own task queue, so only worker_a is in
    // pool): reaches worker_a, never worker_b, never the {a,b} worker (different
    // task queue keeps the pool unambiguous for this leg).
    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("a", "default", None))
        .await?;
    worker_a.expect_task().await?;
    worker_b.expect_no_task().await?;
    worker_both.expect_no_task().await?;

    // The {a,b} worker is reachable from namespace `a` via its own queue.
    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("a", "queue-ab", None))
        .await?;
    worker_both.expect_task().await?;
    worker_a.expect_no_task().await?;
    worker_b.expect_no_task().await?;

    // ...and from namespace `b` via the same queue — one worker, two namespaces.
    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("b", "queue-ab", None))
        .await?;
    worker_both.expect_task().await?;
    worker_a.expect_no_task().await?;
    worker_b.expect_no_task().await?;

    cluster.server();
    Ok(())
}

/// Task-queue selection: two workers in the same namespace and same activity
/// type but different task queues ("gpu" vs "cpu"); a dispatch targeting "gpu"
/// reaches only the gpu worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_queue_routing_selects_the_named_pool() -> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    let mut gpu = cluster
        .connect_worker("gpu", &["tenant"], "gpu", "")
        .await?;
    let mut cpu = cluster
        .connect_worker("cpu", &["tenant"], "cpu", "")
        .await?;

    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "gpu", None))
        .await?;
    gpu.expect_task().await?;
    cpu.expect_no_task().await?;

    // The symmetric leg: targeting "cpu" reaches only the cpu worker.
    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "cpu", None))
        .await?;
    cpu.expect_task().await?;
    gpu.expect_no_task().await?;

    cluster.server();
    Ok(())
}

/// NODE affinity: two workers in the same (namespace, `task_queue`) pool on
/// different node ids. A dispatch pinned to node `n1` reaches only the `n1`
/// worker; pinned to `n2` reaches only the `n2` worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_pinned_dispatch_reaches_only_the_pinned_node() -> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    let mut n1 = cluster
        .connect_worker("n1", &["tenant"], "pool", "n1")
        .await?;
    let mut n2 = cluster
        .connect_worker("n2", &["tenant"], "pool", "n2")
        .await?;

    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "pool", Some("n1")))
        .await?;
    n1.expect_task().await?;
    n2.expect_no_task().await?;

    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "pool", Some("n2")))
        .await?;
    n2.expect_task().await?;
    n1.expect_no_task().await?;

    cluster.server();
    Ok(())
}

/// NODE affinity, unpinned: an UNPINNED dispatch into a pool of two
/// node-distinct workers may reach EITHER (round-robin), so assert
/// set-membership — exactly one worker is served and it is one of the pool.
/// Two unpinned dispatches in a row cover both round-robin positions, proving
/// the pool is genuinely shared rather than pinned to one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpinned_dispatch_round_robins_across_the_pool() -> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    let mut n1 = cluster
        .connect_worker("n1", &["tenant"], "pool", "n1")
        .await?;
    let mut n2 = cluster
        .connect_worker("n2", &["tenant"], "pool", "n2")
        .await?;

    // First unpinned dispatch: exactly one of the two is served.
    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "pool", None))
        .await?;
    let first_served = served_exactly_one(&mut n1, &mut n2).await?;

    // Second unpinned dispatch: the rotation advances to the other worker, so
    // across the two dispatches BOTH are eligible and each is served once.
    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "pool", None))
        .await?;
    let second_served = served_exactly_one(&mut n1, &mut n2).await?;

    assert_ne!(
        first_served, second_served,
        "two unpinned dispatches must round-robin across both pool members, \
         proving the pool is shared and not pinned to one worker"
    );
    cluster.server();
    Ok(())
}

/// NODE affinity, shared node: two workers SHARING a node id are BOTH eligible
/// when a dispatch is pinned to that node. Two pinned dispatches in a row are
/// served by different members, proving both passed the node filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_sharing_a_node_are_both_eligible_when_pinned() -> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    let mut a = cluster
        .connect_worker("shared-a", &["tenant"], "pool", "shared")
        .await?;
    let mut b = cluster
        .connect_worker("shared-b", &["tenant"], "pool", "shared")
        .await?;

    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "pool", Some("shared")))
        .await?;
    let first = served_exactly_one(&mut a, &mut b).await?;

    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("tenant", "pool", Some("shared")))
        .await?;
    let second = served_exactly_one(&mut a, &mut b).await?;

    assert_ne!(
        first, second,
        "both workers sharing the pinned node must be eligible; the rotation \
         must serve each once across two pinned dispatches"
    );
    cluster.server();
    Ok(())
}

/// COMBINED: a dispatch addressed to a specific (namespace, `task_queue`, node)
/// lands on EXACTLY the matching worker, and is refused by every worker that
/// mismatches on ANY single dimension. Five workers: the exact match plus one
/// that differs on namespace only, one on `task_queue` only, one on node only,
/// and one that matches namespace + `task_queue` but on a different node.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn combined_address_lands_on_exact_match_and_is_refused_by_every_mismatch()
-> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    // The exact target: namespace `ns`, task_queue `tq`, node `node`.
    let mut exact = cluster
        .connect_worker("exact", &["ns"], "tq", "node")
        .await?;
    // Mismatch on namespace only.
    let mut wrong_ns = cluster
        .connect_worker("wrong-ns", &["other"], "tq", "node")
        .await?;
    // Mismatch on task_queue only.
    let mut wrong_tq = cluster
        .connect_worker("wrong-tq", &["ns"], "other-tq", "node")
        .await?;
    // Mismatch on node only (same namespace + task_queue pool).
    let mut wrong_node = cluster
        .connect_worker("wrong-node", &["ns"], "tq", "other-node")
        .await?;
    // An unpinned worker in the right pool: still wrong, because a pinned
    // dispatch requires an advertised node equal to the pin, and this worker
    // advertises none.
    let mut no_node = cluster.connect_worker("no-node", &["ns"], "tq", "").await?;

    cluster
        .dispatcher
        .dispatch(&Cluster::scheduled("ns", "tq", Some("node")))
        .await?;

    let task = exact.expect_task().await?;
    assert_eq!(task.activity_type, ACTIVITY_TYPE);
    wrong_ns.expect_no_task().await?;
    wrong_tq.expect_no_task().await?;
    wrong_node.expect_no_task().await?;
    no_node.expect_no_task().await?;

    cluster.server();
    Ok(())
}

/// Drive the two candidate workers and assert exactly one received a task;
/// return the label of the served worker. The unserved worker must be clean.
async fn served_exactly_one(
    first: &mut TestWorker,
    second: &mut TestWorker,
) -> Result<&'static str, TestError> {
    // Poll both concurrently with a bounded deadline; whichever yields a task
    // first identifies the served worker, then the other must be empty.
    tokio::select! {
        result = first.expect_task() => {
            result?;
            second.expect_no_task().await?;
            Ok(first.label)
        }
        result = second.expect_task() => {
            result?;
            first.expect_no_task().await?;
            Ok(second.label)
        }
    }
}

/// Sanity: the worker ids the registry assigned are distinct per connection,
/// so the "exactly one served" assertions above are over genuinely distinct
/// streams rather than an accidental alias.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_workers_get_distinct_ids() -> Result<(), TestError> {
    let cluster = Cluster::start().await?;
    let one = cluster
        .connect_worker("one", &["tenant"], "pool", "n1")
        .await?;
    let two = cluster
        .connect_worker("two", &["tenant"], "pool", "n2")
        .await?;
    assert_ne!(
        one.worker_id, two.worker_id,
        "each registered stream must receive a distinct worker id"
    );
    cluster.server();
    Ok(())
}
