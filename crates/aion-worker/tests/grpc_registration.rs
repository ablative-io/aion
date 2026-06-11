//! Pins the worker registration sequence against a scripted server that
//! models the real `aion-server` `StreamWorker` contract.
//!
//! The real server (`crates/aion-server/src/api/worker_grpc.rs`) reads the
//! `RegisterWorker` frame from the inbound stream *before* it returns its
//! response stream, so tonic withholds response headers until registration is
//! processed, and the wire protocol carries no registration-ack frame. A
//! client that awaits response headers before sending `RegisterWorker`
//! deadlocks forever; these tests prove the SDK queues the registration as
//! the first frame and completes `register` promptly.

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

/// Scripted `WorkerProtocol` service mirroring the real server's stream
/// lifecycle: demand `RegisterWorker` first, optionally deny it, then hold
/// the response stream open without ever sending an ack frame.
struct ScriptedRealServer {
    registrations: mpsc::UnboundedSender<generated::RegisterWorker>,
    denial: Option<Status>,
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
        if let Some(denial) = self.denial.clone() {
            return Err(denial);
        }
        let (task_sender, task_receiver) = mpsc::channel::<Result<ServerToWorker, Status>>(4);
        tokio::spawn(async move {
            // Hold the response stream open while draining inbound frames,
            // matching the real server's post-registration read loop. No
            // registration-ack frame is ever pushed.
            while let Ok(Some(_)) = inbound.message().await {}
            drop(task_sender);
        });
        Ok(Response::new(ReceiverStream::new(task_receiver)))
    }
}

async fn spawn_scripted_server(
    denial: Option<Status>,
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
        denial,
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
        ReconnectConfig::new(Duration::from_millis(5), Duration::from_millis(20), 3),
        None,
    )
}

#[tokio::test]
async fn register_completes_promptly_against_real_protocol_without_ack_frame()
-> Result<(), WorkerError> {
    let (address, mut registrations, server_handle) = spawn_scripted_server(None).await?;
    let config = test_config(address);
    let activity_types = vec![String::from("greet")];
    let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();

    let session = GrpcWorkerSession::connect(config.clone()).await?;
    let registered = tokio::time::timeout(
        Duration::from_secs(5),
        register_connected_session(session, &config, activity_types.clone(), &handlers),
    )
    .await
    .map_err(|_| {
        failure(
            "register did not complete within five seconds: the worker is waiting \
             for an acknowledgement the wire protocol never sends",
        )
    })??;

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
async fn register_surfaces_server_permission_denial_as_non_retryable() -> Result<(), WorkerError> {
    let denial = Status::permission_denied(
        "namespace `default-queue` is not granted to subject `rust-worker-1`",
    );
    let (address, mut registrations, server_handle) = spawn_scripted_server(Some(denial)).await?;
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
