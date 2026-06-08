//! tonic `WorkerProtocol` service — bidirectional stream handler.

use aion_proto::{
    ProtoActivityResult, ProtoRegisterWorker,
    generated::{
        self,
        worker_protocol_server::{WorkerProtocol, WorkerProtocolServer},
    },
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::worker::PendingActivities;
use crate::worker::dispatch::{ActivityCompletion, ActivityCompletionSink};
use crate::{CallerIdentity, ServerState};

/// Cloneable tonic implementation for the worker bidirectional stream.
#[derive(Clone)]
pub struct WorkerGrpcService {
    state: ServerState,
}

impl WorkerGrpcService {
    /// Build a tonic worker service from shared server state.
    #[must_use]
    pub const fn new(state: ServerState) -> Self {
        Self { state }
    }
}

/// Construct the generated tonic server wrapper for the worker protocol.
#[must_use]
pub fn worker_service(state: ServerState) -> WorkerProtocolServer<WorkerGrpcService> {
    WorkerProtocolServer::new(WorkerGrpcService::new(state))
}

#[tonic::async_trait]
impl WorkerProtocol for WorkerGrpcService {
    type StreamWorkerStream = ReceiverStream<Result<generated::ServerToWorker, Status>>;

    async fn stream_worker(
        &self,
        request: Request<Streaming<generated::WorkerToServer>>,
    ) -> Result<Response<Self::StreamWorkerStream>, Status> {
        let metadata = request.metadata().clone();
        let caller = worker_caller_from_metadata(&metadata, &self.state).await?;
        let token_expires_at = token_expiration_from_metadata(&metadata, &self.state).await?;
        let heartbeat_grace = self.state.runtime_config().worker.heartbeat_window;
        let mut inbound = request.into_inner();

        let first = inbound
            .message()
            .await?
            .and_then(|msg| msg.message)
            .ok_or_else(|| Status::invalid_argument("first message must be RegisterWorker"))?;

        let register = match first {
            generated::worker_to_server::Message::Register(r) => decode_register(r),
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be RegisterWorker",
                ));
            }
        };

        let (task_tx, task_rx) = mpsc::channel::<Result<generated::ServerToWorker, Status>>(32);
        let (worker_tx, mut worker_rx) = mpsc::channel(32);

        let registration = self
            .state
            .worker_registry()
            .accept_registration(self.state.namespace_guard(), &caller, &register, worker_tx)
            .map_err(status_from_server_error)?;

        let pending = self.state.pending_activities().clone();

        tokio::spawn(async move {
            let write_handle = tokio::spawn({
                let task_tx = task_tx.clone();
                async move {
                    while let Some(task) = worker_rx.recv().await {
                        let msg = generated::ServerToWorker {
                            message: Some(generated::server_to_worker::Message::Task(encode_task(
                                task,
                            ))),
                        };
                        if task_tx.send(Ok(msg)).await.is_err() {
                            break;
                        }
                    }
                }
            });

            let _read_result = process_inbound(
                inbound,
                &pending,
                token_expires_at,
                heartbeat_grace,
                task_tx.clone(),
            )
            .await;

            write_handle.abort();
            drop(task_tx);
            let _ = registration.deregister();
        });

        Ok(Response::new(ReceiverStream::new(task_rx)))
    }
}

async fn process_inbound(
    mut inbound: Streaming<generated::WorkerToServer>,
    pending: &PendingActivities,
    token_expires_at: Option<u64>,
    heartbeat_grace: std::time::Duration,
    task_tx: mpsc::Sender<Result<generated::ServerToWorker, Status>>,
) -> Result<(), Status> {
    let mut expired_since: Option<std::time::Instant> = None;
    while let Some(msg) = inbound.message().await? {
        let Some(inner) = msg.message else {
            continue;
        };
        match inner {
            generated::worker_to_server::Message::Result(result) => {
                let proto_result = decode_activity_result(result);
                if let Ok(completion) = ActivityCompletion::try_from(proto_result) {
                    let _ = pending.complete_activity(completion);
                }
            }
            generated::worker_to_server::Message::Register(_) => {}
            generated::worker_to_server::Message::Heartbeat(_) => {
                if token_expired(token_expires_at) {
                    let first_expired = *expired_since.get_or_insert_with(std::time::Instant::now);
                    let _ = task_tx
                        .send(Err(Status::unauthenticated(
                            "worker token expired; re-authentication required",
                        )))
                        .await;
                    if first_expired.elapsed() >= heartbeat_grace {
                        return Err(Status::unauthenticated("worker token expired"));
                    }
                }
            }
        }
    }
    Ok(())
}

async fn worker_caller_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    state: &ServerState,
) -> Result<CallerIdentity, Status> {
    crate::api::grpc::caller_from_metadata(metadata, state).await
}

async fn token_expiration_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    state: &ServerState,
) -> Result<Option<u64>, Status> {
    if !state.runtime_config().auth.enabled {
        return Ok(None);
    }
    #[cfg(feature = "auth")]
    {
        let bearer = metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_bearer)
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?;
        let Some(cache) = state.jwks_cache() else {
            return Err(Status::unauthenticated("invalid bearer token"));
        };
        return cache
            .validate(&bearer)
            .await
            .map(|claims| Some(claims.expires_at()))
            .map_err(|_error| Status::unauthenticated("invalid bearer token"));
    }
    #[cfg(not(feature = "auth"))]
    {
        let _ = metadata;
        Err(Status::unauthenticated("authentication unavailable"))
    }
}

#[cfg(feature = "auth")]
fn parse_bearer(value: &str) -> Option<String> {
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
}

fn token_expired(expires_at: Option<u64>) -> bool {
    expires_at.is_some_and(|expires_at| {
        #[cfg(feature = "auth")]
        {
            crate::auth::jwks::is_expired(expires_at)
        }
        #[cfg(not(feature = "auth"))]
        {
            let _ = expires_at;
            false
        }
    })
}

fn status_from_server_error(error: crate::ServerError) -> Status {
    let wire = error.to_wire_error();
    if wire.code == aion_proto::WireErrorCode::NamespaceDenied {
        Status::permission_denied(wire.message)
    } else {
        Status::internal(wire.message)
    }
}

fn decode_register(r: generated::RegisterWorker) -> ProtoRegisterWorker {
    ProtoRegisterWorker {
        namespace: r.namespace,
        activity_types: r.activity_types,
    }
}

fn encode_task(task: aion_proto::ProtoActivityTask) -> generated::ActivityTask {
    generated::ActivityTask {
        workflow_id: task
            .workflow_id
            .map(|id| generated::WorkflowId { uuid: id.uuid }),
        activity_id: task.activity_id.map(|id| generated::ActivityId {
            sequence_position: id.sequence_position,
        }),
        activity_type: task.activity_type,
        input: task.input.map(|p| generated::Payload {
            content_type: p.content_type,
            bytes: p.bytes,
        }),
    }
}

fn decode_activity_result(r: generated::ActivityResult) -> ProtoActivityResult {
    ProtoActivityResult {
        workflow_id: r
            .workflow_id
            .map(|id| aion_proto::ProtoWorkflowId { uuid: id.uuid }),
        activity_id: r.activity_id.map(|id| aion_proto::ProtoActivityId {
            sequence_position: id.sequence_position,
        }),
        outcome: r.outcome.map(decode_outcome),
    }
}

fn decode_outcome(
    outcome: generated::activity_result::Outcome,
) -> aion_proto::proto_activity_result::Outcome {
    match outcome {
        generated::activity_result::Outcome::Result(p) => {
            aion_proto::proto_activity_result::Outcome::Result(aion_proto::ProtoPayload {
                content_type: p.content_type,
                bytes: p.bytes,
            })
        }
        generated::activity_result::Outcome::Error(e) => {
            aion_proto::proto_activity_result::Outcome::Error(aion_proto::ProtoActivityError {
                kind: e.kind,
                message: e.message,
                details: e.details.map(|p| aion_proto::ProtoPayload {
                    content_type: p.content_type,
                    bytes: p.bytes,
                }),
            })
        }
    }
}
