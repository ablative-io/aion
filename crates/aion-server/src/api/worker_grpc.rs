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

use crate::ServerState;
use crate::worker::PendingActivities;
use crate::worker::dispatch::{ActivityCompletion, ActivityCompletionSink};
use crate::worker::registry::{WorkerId, WorkerMessage};

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
            .register(
                &register.namespace,
                register.activity_types.iter(),
                worker_tx,
            )
            .map_err(|e| Status::internal(e.to_string()))?;

        let pending = self.state.pending_activities().clone();
        let heartbeat = self.state.heartbeat_tracker().clone();
        let drain = self.state.drain_state().clone();
        let worker_id = registration
            .worker_id()
            .ok_or_else(|| Status::internal("worker registration missing id"))?;

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

            let _read_result =
                process_inbound(inbound, worker_id, &pending, &heartbeat, &drain).await;

            write_handle.abort();
            drop(task_tx);
            let _ = registration.deregister();
        });

        Ok(Response::new(ReceiverStream::new(task_rx)))
    }
}

async fn process_inbound(
    mut inbound: Streaming<generated::WorkerToServer>,
    worker_id: WorkerId,
    pending: &PendingActivities,
    heartbeat: &crate::worker::HeartbeatTracker,
    drain: &crate::shutdown::DrainState,
) -> Result<(), Status> {
    while let Some(msg) = inbound.message().await? {
        let Some(inner) = msg.message else {
            continue;
        };
        match inner {
            generated::worker_to_server::Message::Result(result) => {
                let proto_result = decode_activity_result(result);
                if let Ok(completion) = ActivityCompletion::try_from(proto_result) {
                    let _ = heartbeat.complete_task(
                        worker_id,
                        &completion.workflow_id,
                        &completion.activity_id,
                    );
                    drain.notify_activity_drained();
                    let _ = pending.complete_activity(completion);
                }
            }
            generated::worker_to_server::Message::Heartbeat(heartbeat_msg) => {
                let _ = heartbeat.record_heartbeat(
                    worker_id,
                    decode_heartbeat(heartbeat_msg),
                    std::time::Instant::now(),
                );
            }
            generated::worker_to_server::Message::Register(_) => {}
        }
    }
    Ok(())
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
