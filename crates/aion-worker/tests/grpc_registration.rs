//! Pins the worker registration sequence against a scripted server that
//! models the real `aion-server` `StreamWorker` contract.
//!
//! The real server (`crates/aion-server/src/api/worker_grpc.rs`) reads the
//! `RegisterWorker` frame from the inbound stream *before* it returns its
//! response stream, so tonic withholds response headers until registration is
//! processed, and then answers with `RegisterAck` as the guaranteed first
//! response frame. A client that awaits response headers before sending
//! `RegisterWorker` deadlocks forever; these tests prove the SDK queues the
//! registration as the first frame, completes `register` only on the ack,
//! times out retryably against a server that never acks, rejects a non-ack
//! first frame as a protocol violation, and maps denials unchanged.

use std::collections::BTreeSet;
use std::time::Duration;

use aion_proto::generated::worker_protocol_server::{WorkerProtocol, WorkerProtocolServer};
use aion_proto::generated::{self, ServerToWorker, WorkerToServer};
use aion_worker::{
    GrpcWorkerSession, ReconnectConfig, WorkerConfig, WorkerError, register_connected_session,
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Request, Response, Status, Streaming};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct TestFailure {
    message: String,
}

fn failure(message: impl Into<String>) -> WorkerError {
    WorkerError::decode(TestFailure {
        message: message.into(),
    })
}

/// What the scripted server does after accepting the `RegisterWorker` frame.
#[derive(Clone)]
enum RegistrationScript {
    /// Send `RegisterAck` as the first response frame (the real contract).
    Ack,
    /// Hold the stream open without ever acking (the pre-ack wire shape; the
    /// SDK must time out retryably, not hang).
    NeverAck,
    /// Send a task frame before the ack (a server ordering bug; the SDK must
    /// surface a typed protocol violation).
    TaskBeforeAck,
    /// Fail the RPC with this status (denial taxonomy unchanged).
    Deny(Status),
}

/// Scripted `WorkerProtocol` service mirroring the real server's stream
/// lifecycle: demand `RegisterWorker` first, then run the configured script.
struct ScriptedRealServer {
    registrations: mpsc::UnboundedSender<generated::RegisterWorker>,
    script: RegistrationScript,
}

fn register_ack_frame() -> ServerToWorker {
    ServerToWorker {
        message: Some(generated::server_to_worker::Message::RegisterAck(
            generated::RegisterAck {
                worker_id: 7,
                namespace: String::from("default-queue"),
                heartbeat_window_ms: 30_000,
            },
        )),
    }
}

fn task_frame() -> ServerToWorker {
    ServerToWorker {
        message: Some(generated::server_to_worker::Message::Task(
            generated::ActivityTask {
                workflow_id: None,
                activity_id: None,
                activity_type: String::from("greet"),
                input: None,
                attempt: 1,
                labels: std::collections::HashMap::new(),
                run_id: None,
            },
        )),
    }
}

#[tonic::async_trait]
impl WorkerProtocol for ScriptedRealServer {
    type StreamWorkerStream = ReceiverStream<Result<ServerToWorker, Status>>;

    async fn stream_worker(
        &self,
        request: Request<Streaming<WorkerToServer>>,
    ) -> Result<Response<Self::StreamWorkerStream>, Status> {
        let mut inbound = request.into_inner();
        // Real server behaviour: block on the first inbound frame and demand
        // RegisterWorker before sending response headers.
        let first = inbound
            .message()
            .await?
            .and_then(|message| message.message)
            .ok_or_else(|| Status::invalid_argument("first message must be RegisterWorker"))?;
        let generated::worker_to_server::Message::Register(register) = first else {
            return Err(Status::invalid_argument(
                "first message must be RegisterWorker",
            ));
        };
        self.registrations
            .send(register)
            .map_err(|_| Status::internal("registration capture closed"))?;
        let (task_sender, task_receiver) = mpsc::channel::<Result<ServerToWorker, Status>>(4);
        match &self.script {
            RegistrationScript::Deny(denial) => return Err(denial.clone()),
            RegistrationScript::Ack => {
                task_sender
                    .try_send(Ok(register_ack_frame()))
                    .map_err(|_| Status::internal("ack frame could not be queued"))?;
            }
            RegistrationScript::TaskBeforeAck => {
                task_sender
                    .try_send(Ok(task_frame()))
                    .map_err(|_| Status::internal("task frame could not be queued"))?;
            }
            RegistrationScript::NeverAck => {}
        }
        tokio::spawn(async move {
            // Hold the response stream open while draining inbound frames,
            // matching the real server's post-registration read loop.
            while let Ok(Some(_)) = inbound.message().await {}
            drop(task_sender);
        });
        Ok(Response::new(ReceiverStream::new(task_receiver)))
    }
}

async fn spawn_scripted_server(
    script: RegistrationScript,
) -> Result<
    (
        std::net::SocketAddr,
        mpsc::UnboundedReceiver<generated::RegisterWorker>,
        tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    ),
    WorkerError,
> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(WorkerError::decode)?;
    let address = listener.local_addr().map_err(WorkerError::decode)?;
    let (registration_sender, registrations) = mpsc::unbounded_channel();
    let service = WorkerProtocolServer::new(ScriptedRealServer {
        registrations: registration_sender,
        script,
    });
    let server = tonic::transport::Server::builder()
        .add_service(service)
        .serve_with_incoming(TcpListenerStream::new(listener));
    Ok((address, registrations, tokio::spawn(server)))
}

fn test_config(address: std::net::SocketAddr) -> WorkerConfig {
    WorkerConfig::new(
        format!("http://{address}"),
        "default-queue",
        "rust-worker-1",
        2,
        ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(200), 3),
        None,
    )
}

#[tokio::test]
async fn register_completes_on_register_ack_and_captures_ack_payload() -> Result<(), WorkerError> {
    let (address, mut registrations, server_handle) =
        spawn_scripted_server(RegistrationScript::Ack).await?;
    let config = test_config(address);
    let activity_types = vec![String::from("greet")];
    let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();

    let session = GrpcWorkerSession::connect(config.clone()).await?;
    let registered = tokio::time::timeout(
        Duration::from_secs(5),
        register_connected_session(session, &config, activity_types.clone(), &handlers),
    )
    .await
    .map_err(|_| failure("register did not complete within five seconds"))??;

    let info = registered
        .registered_info()
        .ok_or_else(|| failure("registered session must expose the RegisterAck payload"))?;
    assert_eq!(info.worker_id, 7);
    assert_eq!(info.namespace, "default-queue");
    assert_eq!(info.heartbeat_window, Duration::from_millis(30_000));

    let captured = registrations
        .recv()
        .await
        .ok_or_else(|| failure("server never received the RegisterWorker frame"))?;
    assert_eq!(captured.namespace, "default-queue");
    assert_eq!(captured.activity_types, activity_types);
    drop(registered);
    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn register_times_out_retryably_when_server_never_acks() -> Result<(), WorkerError> {
    let (address, mut registrations, server_handle) =
        spawn_scripted_server(RegistrationScript::NeverAck).await?;
    let config = test_config(address);
    let activity_types = vec![String::from("greet")];
    let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();

    let session = GrpcWorkerSession::connect(config.clone()).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        register_connected_session(session, &config, activity_types, &handlers),
    )
    .await
    .map_err(|_| {
        failure("a never-acking server must time the ack wait out at max_backoff, not hang")
    })?;

    let Err(error) = result else {
        return Err(failure(
            "registration must not succeed without a RegisterAck frame",
        ));
    };
    assert!(
        error.is_retryable(),
        "ack-wait timeout must be a retryable registration failure: {error}"
    );
    assert!(
        matches!(error, WorkerError::Registration { .. }),
        "ack-wait timeout must classify as a registration failure: {error}"
    );
    assert!(registrations.recv().await.is_some());
    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn non_ack_first_frame_is_a_typed_protocol_violation() -> Result<(), WorkerError> {
    let (address, mut registrations, server_handle) =
        spawn_scripted_server(RegistrationScript::TaskBeforeAck).await?;
    let config = test_config(address);
    let activity_types = vec![String::from("greet")];
    let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();

    let session = GrpcWorkerSession::connect(config.clone()).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        register_connected_session(session, &config, activity_types, &handlers),
    )
    .await
    .map_err(|_| failure("a task-before-ack server must fail registration promptly"))?;

    let Err(error) = result else {
        return Err(failure(
            "a task frame before RegisterAck must fail registration",
        ));
    };
    assert!(
        matches!(error, WorkerError::Decode { .. }),
        "task-before-ack must be a typed protocol-violation decode error: {error}"
    );
    assert!(
        error.is_retryable(),
        "the protocol violation must remain a retryable, budgeted drop: {error}"
    );
    assert!(registrations.recv().await.is_some());
    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn register_surfaces_server_permission_denial_as_non_retryable() -> Result<(), WorkerError> {
    let denial = Status::permission_denied(
        "namespace `default-queue` is not granted to subject `rust-worker-1`",
    );
    let (address, mut registrations, server_handle) =
        spawn_scripted_server(RegistrationScript::Deny(denial)).await?;
    let config = test_config(address);
    let activity_types = vec![String::from("greet")];
    let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();

    let session = GrpcWorkerSession::connect(config.clone()).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        register_connected_session(session, &config, activity_types, &handlers),
    )
    .await
    .map_err(|_| failure("denied registration must fail promptly, not hang"))?;

    let Err(error) = result else {
        return Err(failure("registration must surface the server denial"));
    };
    assert!(!error.is_retryable());
    assert!(matches!(
        error.grpc_status().map(tonic::Status::code),
        Some(tonic::Code::PermissionDenied)
    ));
    assert_eq!(
        error.grpc_status().map(tonic::Status::message),
        Some("namespace `default-queue` is not granted to subject `rust-worker-1`")
    );
    assert!(registrations.recv().await.is_some());
    server_handle.abort();
    Ok(())
}
