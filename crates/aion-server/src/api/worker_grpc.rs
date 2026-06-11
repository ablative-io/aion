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
use crate::worker::registry::{WorkerId, WorkerMessage};
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
            .await
            .map_err(|error| status_from_server_error(&error))?;

        let pending = self.state.pending_activities().clone();
        let heartbeat = self.state.heartbeat_tracker().clone();
        let drain = self.state.drain_state().clone();
        let worker_id = registration
            .worker_id()
            .ok_or_else(|| Status::internal("worker registration missing id"))?;
        let authorized_namespace = registration
            .namespace()
            .ok_or_else(|| Status::internal("worker registration missing namespace"))?
            .to_owned();

        // RegisterAck ordering guarantee: the ack is enqueued on `task_tx`
        // BEFORE the write forwarder that copies dispatched tasks onto the
        // same channel is spawned, so no task frame can precede it on the
        // wire. This is a structural ordering proof, not a timing hope.
        task_tx
            .try_send(Ok(register_ack_frame(
                worker_id,
                &authorized_namespace,
                heartbeat_grace,
            )))
            .map_err(|_| Status::internal("worker response channel closed before RegisterAck"))?;

        tokio::spawn(async move {
            let write_handle = tokio::spawn({
                let task_tx = task_tx.clone();
                async move {
                    while let Some(message) = worker_rx.recv().await {
                        let msg = encode_server_to_worker(message);
                        if task_tx.send(Ok(msg)).await.is_err() {
                            break;
                        }
                    }
                }
            });

            let session = WorkerSession {
                worker_id,
                pending: &pending,
                heartbeat: &heartbeat,
                drain: &drain,
                token_expires_at,
                heartbeat_grace,
                task_tx: task_tx.clone(),
            };
            let _read_result = process_inbound(inbound, session).await;

            write_handle.abort();
            drop(task_tx);
            let _ = registration.deregister();
        });

        Ok(Response::new(ReceiverStream::new(task_rx)))
    }
}

struct WorkerSession<'a> {
    worker_id: WorkerId,
    pending: &'a PendingActivities,
    heartbeat: &'a crate::worker::HeartbeatTracker,
    drain: &'a crate::shutdown::DrainState,
    token_expires_at: Option<u64>,
    heartbeat_grace: std::time::Duration,
    task_tx: mpsc::Sender<Result<generated::ServerToWorker, Status>>,
}

async fn process_inbound(
    mut inbound: Streaming<generated::WorkerToServer>,
    session: WorkerSession<'_>,
) -> Result<(), Status> {
    let mut expired_since: Option<std::time::Instant> = None;
    while let Some(msg) = inbound.message().await? {
        let Some(inner) = msg.message else {
            continue;
        };
        match inner {
            generated::worker_to_server::Message::Result(result) => {
                let proto_result = decode_activity_result(result);
                match ActivityCompletion::try_from(proto_result) {
                    Ok(completion) => {
                        let workflow_id = completion.workflow_id.clone();
                        let activity_id = completion.activity_id.clone();
                        let _ = session.heartbeat.complete_task(
                            session.worker_id,
                            &workflow_id,
                            &activity_id,
                        );
                        session.drain.notify_activity_drained();
                        if let Err(error) = session.pending.complete_activity(completion) {
                            tracing::error!(
                                worker_id = ?session.worker_id,
                                workflow_id = %workflow_id,
                                activity_id = %activity_id,
                                %error,
                                "activity completion handoff failed"
                            );
                        }
                        // Ack every well-formed result frame — including
                        // duplicates with no pending waiter; their re-report
                        // obligation is equally discharged. `try_send`: a
                        // worker that stopped draining its receive side must
                        // not wedge the inbound loop; a dropped ack is
                        // recovered by the next-session re-report.
                        let ack = result_ack_frame(&workflow_id, &activity_id);
                        if let Err(error) = session.task_tx.try_send(Ok(ack)) {
                            tracing::warn!(
                                worker_id = ?session.worker_id,
                                workflow_id = %workflow_id,
                                activity_id = %activity_id,
                                %error,
                                "result ack dropped: worker stream channel unavailable"
                            );
                        }
                    }
                    Err(error) => {
                        // Malformed result: no ids to ack with. Loud, never
                        // silent — the worker's entry will re-report and
                        // re-fail visibly each session.
                        tracing::error!(
                            worker_id = ?session.worker_id,
                            %error,
                            "malformed activity result frame; no ack sent"
                        );
                    }
                }
            }
            generated::worker_to_server::Message::Register(_) => {
                tracing::warn!(
                    worker_id = ?session.worker_id,
                    "ignoring subsequent RegisterWorker message; \
                     only the first registration is accepted per stream"
                );
            }
            generated::worker_to_server::Message::Heartbeat(heartbeat_msg) => {
                let _ = session.heartbeat.record_heartbeat(
                    session.worker_id,
                    decode_heartbeat(heartbeat_msg),
                    std::time::Instant::now(),
                );
                if token_expired(session.token_expires_at) {
                    let first_expired = *expired_since.get_or_insert_with(std::time::Instant::now);
                    let _ = session
                        .task_tx
                        .send(Err(Status::unauthenticated(
                            "worker token expired; re-authentication required",
                        )))
                        .await;
                    if first_expired.elapsed() >= session.heartbeat_grace {
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
        // Yield to preserve the async signature required by the auth-feature branch.
        tokio::task::yield_now().await;
        Ok(None)
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

fn status_from_server_error(error: &crate::ServerError) -> Status {
    let wire = error.to_wire_error();
    if wire.code == aion_proto::WireErrorCode::NamespaceDenied {
        Status::permission_denied(wire.message)
    } else {
        Status::internal(wire.message)
    }
}

/// Build the positive registration acknowledgement frame — the guaranteed
/// first frame on every successful worker response stream.
fn register_ack_frame(
    worker_id: WorkerId,
    namespace: &str,
    heartbeat_window: std::time::Duration,
) -> generated::ServerToWorker {
    generated::ServerToWorker {
        message: Some(generated::server_to_worker::Message::RegisterAck(
            generated::RegisterAck {
                worker_id: worker_id.value(),
                namespace: namespace.to_owned(),
                heartbeat_window_ms: u64::try_from(heartbeat_window.as_millis())
                    .unwrap_or(u64::MAX),
            },
        )),
    }
}

/// Build the per-result acknowledgement frame for a consumed `ActivityResult`.
fn result_ack_frame(
    workflow_id: &aion_core::WorkflowId,
    activity_id: &aion_core::ActivityId,
) -> generated::ServerToWorker {
    generated::ServerToWorker {
        message: Some(generated::server_to_worker::Message::ResultAck(
            generated::ResultAck {
                workflow_id: Some(generated::WorkflowId {
                    uuid: workflow_id.to_string(),
                }),
                activity_id: Some(generated::ActivityId {
                    sequence_position: activity_id.sequence_position(),
                }),
            },
        )),
    }
}

fn decode_register(r: generated::RegisterWorker) -> ProtoRegisterWorker {
    ProtoRegisterWorker {
        namespace: r.namespace,
        activity_types: r.activity_types,
    }
}

fn encode_server_to_worker(message: WorkerMessage) -> generated::ServerToWorker {
    let message = match message {
        WorkerMessage::ActivityTask(task) => {
            generated::server_to_worker::Message::Task(encode_task(task))
        }
        WorkerMessage::DrainRequest => {
            generated::server_to_worker::Message::Drain(generated::DrainRequest {})
        }
    };
    generated::ServerToWorker {
        message: Some(message),
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
        attempt: task.attempt,
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

fn decode_heartbeat(r: generated::Heartbeat) -> aion_proto::ProtoHeartbeat {
    aion_proto::ProtoHeartbeat {
        workflow_id: r
            .workflow_id
            .map(|id| aion_proto::ProtoWorkflowId { uuid: id.uuid }),
        activity_id: r.activity_id.map(|id| aion_proto::ProtoActivityId {
            sequence_position: id.sequence_position,
        }),
        progress: r.progress.map(|p| aion_proto::ProtoPayload {
            content_type: p.content_type,
            bytes: p.bytes,
        }),
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
