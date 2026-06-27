//! `WorkerSession` trait and gRPC-backed implementation.

use std::collections::BTreeSet;
use std::pin::Pin;

use aion_core::{ActivityError, ActivityId, Payload, RunId, WorkflowId};
use aion_proto::{
    ProtoActivityId, ProtoActivityResult, ProtoActivityTask, ProtoHeartbeat, ProtoPayload,
    ProtoRunId, ProtoWorkflowId, proto_activity_result,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, metadata::MetadataValue, transport::Channel};

use crate::config::WorkerConfig;
use crate::error::{MissingActivityHandler, WorkerError};

type GeneratedClient = aion_proto::generated::worker_protocol_client::WorkerProtocolClient<Channel>;

/// Boxed receive stream returned by worker sessions.
pub type WorkerTaskStream =
    Pin<Box<dyn Stream<Item = Result<WorkerSessionEvent, WorkerError>> + Send>>;

/// Event pushed by the worker session receive stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerSessionEvent {
    /// A new activity task to execute.
    Task(ProtoActivityTask),
    /// Server-initiated drain: the server is going away (restart, deploy,
    /// rebalance). The worker finishes in-flight work, reports what it can,
    /// stops expecting new tasks, and reconnects after the schedule's initial
    /// backoff. A drain frame latches for the session: the eventual stream
    /// end — clean or abrupt — is drain-class and consumes no drop budget.
    Drain,
    /// The server consumed the identified `ActivityResult` frame; the worker
    /// may stop re-reporting it. Clears the matching unacked-tracker entry.
    ResultAck {
        /// Workflow owning the acknowledged result.
        workflow_id: WorkflowId,
        /// Activity whose result was acknowledged.
        activity_id: ActivityId,
    },
    /// Cooperative cancellation for an in-flight activity.
    ///
    /// The current AW worker proto in this worktree does not yet carry this
    /// frame, but fake sessions can emit it and the runtime handles it without
    /// forcing task termination. When AW lands the wire variant,
    /// `decode_server_message` should map it to this event.
    Cancel {
        /// Workflow owning the activity.
        workflow_id: WorkflowId,
        /// Activity to mark cancelled.
        activity_id: ActivityId,
    },
}

/// Transport abstraction for the AW-owned worker protocol.
///
/// The current `aion-proto` worker endpoint is `WorkerProtocol::StreamWorker`,
/// a single bidirectional gRPC stream. These methods intentionally present the
/// worker conversation as handshake/register/receive/report/heartbeat phases so
/// execution machinery can be tested against fakes and never touches generated
/// stubs directly. If AW changes the wire shape, this trait adapts in this module.
#[async_trait]
pub trait WorkerSession: Send {
    /// Performs the worker handshake for the configured namespace, task queue,
    /// and identity.
    ///
    /// Maps to transport/channel establishment for AW's `StreamWorker` RPC. The
    /// wire carries the genuine `namespace` (correctness boundary) and
    /// `task_queue` (pool selector) as disjoint registration fields; it has no
    /// identity field, so identity is retained at this SDK boundary until the
    /// wire adds a corresponding shape.
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError>;

    /// Registers activity-type names implemented by this worker.
    ///
    /// Maps to opening AW's `StreamWorker` RPC with `RegisterWorker` queued as
    /// the mandatory first frame and then awaiting the server's `RegisterAck`
    /// — the guaranteed first frame on the response stream. Registration
    /// succeeds only when the ack arrives; a denial fails the RPC with a gRPC
    /// error status (`PermissionDenied` / `Unauthenticated`), and an ack that
    /// does not arrive within the reconnect policy's `max_backoff` is a
    /// retryable registration failure. The caller supplies
    /// `available_handlers` so registration can be rejected before serving if
    /// any requested name lacks a handler.
    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError>;

    /// Opens the receive side of AW's `StreamWorker` RPC and yields pushed tasks.
    fn receive_tasks(&mut self) -> WorkerTaskStream;

    /// Reports successful activity output via `WorkerToServer.result`.
    async fn report_result(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        result: Payload,
    ) -> Result<(), WorkerError>;

    /// Reports explicit activity failure via `WorkerToServer.result`.
    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        failure: ActivityError,
    ) -> Result<(), WorkerError>;

    /// Sends cooperative progress via `WorkerToServer.heartbeat`.
    async fn send_heartbeat(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Option<Payload>,
    ) -> Result<(), WorkerError>;
}

/// Validates that every requested activity type has a registered handler.
///
/// # Errors
///
/// Returns [`WorkerError::Registration`] for the first missing handler name.
pub fn validate_activity_handlers(
    activity_types: &[String],
    available_handlers: &BTreeSet<String>,
) -> Result<(), WorkerError> {
    if let Some(activity_type) = activity_types
        .iter()
        .find(|activity_type| !available_handlers.contains(*activity_type))
    {
        return Err(WorkerError::registration(MissingActivityHandler {
            activity_type: activity_type.clone(),
        }));
    }

    Ok(())
}

/// Server-assigned registration facts carried by the `RegisterAck` frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredSessionInfo {
    /// Server-assigned stream identifier, for correlating worker logs with
    /// server logs (`worker_id=3 lost`).
    pub worker_id: u64,
    /// The namespace the registration was authorized against.
    pub namespace: String,
    /// The server's operator-configured liveness window: an in-flight
    /// activity must heartbeat at least this often or be declared lost.
    pub heartbeat_window: std::time::Duration,
}

/// gRPC-backed [`WorkerSession`] using `aion-proto` generated tonic stubs.
pub struct GrpcWorkerSession {
    config: WorkerConfig,
    activity_types: Vec<String>,
    client: Option<GeneratedClient>,
    sender: Option<mpsc::Sender<aion_proto::generated::WorkerToServer>>,
    receiver: Option<tonic::codec::Streaming<aion_proto::generated::ServerToWorker>>,
    registered_info: Option<RegisteredSessionInfo>,
}

impl GrpcWorkerSession {
    /// Connects to the configured worker endpoint.
    ///
    /// Opaque credentials are accepted by [`WorkerConfig`] but the current AW
    /// worker proto does not define a credential metadata convention, so no
    /// authentication scheme is interpreted here.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError::Connect`] if tonic cannot create the channel.
    pub async fn connect(config: WorkerConfig) -> Result<Self, WorkerError> {
        let client = GeneratedClient::connect(config.endpoint.clone())
            .await
            .map_err(|source| WorkerError::Connect { source })?;

        Ok(Self {
            config,
            activity_types: Vec::new(),
            client: Some(client),
            sender: None,
            receiver: None,
            registered_info: None,
        })
    }

    /// Creates a session from an existing tonic channel.
    #[must_use]
    pub fn from_channel(config: WorkerConfig, channel: Channel) -> Self {
        Self {
            config,
            activity_types: Vec::new(),
            client: Some(GeneratedClient::new(channel)),
            sender: None,
            receiver: None,
            registered_info: None,
        }
    }

    /// Server-assigned registration facts from the `RegisterAck`, available
    /// once [`WorkerSession::register`] has succeeded.
    #[must_use]
    pub const fn registered_info(&self) -> Option<&RegisteredSessionInfo> {
        self.registered_info.as_ref()
    }

    /// Opens AW's `StreamWorker` RPC with `RegisterWorker` queued as the first
    /// outbound frame and awaits the server's `RegisterAck`.
    ///
    /// The server reads `RegisterWorker` from the inbound stream *before* it
    /// returns its response stream (and therefore before tonic receives
    /// response headers), so the frame must already be queued when the RPC is
    /// issued. Awaiting `stream_worker` before sending `RegisterWorker`
    /// deadlocks: the client waits for headers the server withholds until it
    /// has read the registration.
    ///
    /// Registration succeeds only when the server's `RegisterAck` — its
    /// guaranteed first response frame — arrives. The ack wait is bounded by
    /// the reconnect policy's `max_backoff` (the operator's own definition of
    /// the longest tolerable pause); a timeout, a non-ack first frame, or a
    /// stream that ends before the ack is a retryable registration failure.
    /// Denials surface as the RPC's gRPC error status exactly as before.
    async fn open_registered_stream(
        &mut self,
        register: aion_proto::generated::RegisterWorker,
    ) -> Result<(), WorkerError> {
        let client = self.client.as_mut().ok_or_else(|| {
            WorkerError::registration(SessionStateError {
                message: String::from("worker session has not completed its handshake"),
            })
        })?;
        let (sender, outbound) = mpsc::channel(16);
        sender
            .try_send(aion_proto::generated::WorkerToServer {
                message: Some(aion_proto::generated::worker_to_server::Message::Register(
                    register,
                )),
            })
            .map_err(|_| {
                WorkerError::registration(SessionStateError {
                    message: String::from(
                        "could not queue RegisterWorker as the first stream frame",
                    ),
                })
            })?;
        let mut request = Request::new(ReceiverStream::new(outbound));
        apply_auth_metadata(request.metadata_mut(), &self.config)?;
        let response = client
            .stream_worker(request)
            .await
            .map_err(registration_denial_error)?;
        let mut receiver = response.into_inner();

        let first = tokio::time::timeout(self.config.reconnect.max_backoff, receiver.message())
            .await
            .map_err(|_| {
                WorkerError::registration(SessionStateError {
                    message: format!(
                        "server did not acknowledge registration within {:?}",
                        self.config.reconnect.max_backoff
                    ),
                })
            })?
            .map_err(registration_denial_error)?;
        let ack = match first.and_then(|frame| frame.message) {
            Some(aion_proto::generated::server_to_worker::Message::RegisterAck(ack)) => ack,
            Some(_) => {
                return Err(WorkerError::decode(SessionStateError {
                    message: String::from(
                        "protocol violation: server sent a non-RegisterAck frame before \
                         acknowledging registration",
                    ),
                }));
            }
            None => {
                return Err(WorkerError::registration(SessionStateError {
                    message: String::from(
                        "server ended the stream before acknowledging registration",
                    ),
                }));
            }
        };

        self.registered_info = Some(RegisteredSessionInfo {
            worker_id: ack.worker_id,
            namespace: ack.namespace,
            heartbeat_window: std::time::Duration::from_millis(ack.heartbeat_window_ms),
        });
        self.sender = Some(sender);
        self.receiver = Some(receiver);
        Ok(())
    }

    /// Sends one frame with a per-send deadline of the reconnect policy's
    /// `max_backoff`: a send that outlives the operator's longest tolerable
    /// pause is, by that same definition, a dead session and surfaces as a
    /// retryable transport error instead of hanging the worker forever.
    async fn send_to_server(
        &self,
        message: aion_proto::generated::worker_to_server::Message,
    ) -> Result<(), WorkerError> {
        let sender = self.sender.as_ref().ok_or_else(|| {
            WorkerError::registration(SessionStateError {
                message: String::from("worker stream has not been opened"),
            })
        })?;
        let send = sender.send(aion_proto::generated::WorkerToServer {
            message: Some(message),
        });
        tokio::time::timeout(self.config.reconnect.max_backoff, send)
            .await
            .map_err(|_| WorkerError::Transport {
                source: tonic::Status::unavailable(format!(
                    "worker stream send did not complete within {:?}",
                    self.config.reconnect.max_backoff
                )),
            })?
            .map_err(|source| WorkerError::Transport {
                source: tonic::Status::unavailable(format!("worker stream send failed: {source}")),
            })
    }
}

/// Maps the `StreamWorker` RPC's rejection status to the worker error taxonomy.
///
/// The server validates stream metadata (credentials) and the `RegisterWorker`
/// frame before returning response headers, so both failure classes surface
/// from the same await: `Unauthenticated` is a credential/handshake rejection,
/// everything else is a registration outcome (`PermissionDenied` for an
/// ungranted namespace, `Unavailable` for transient transport faults). Both
/// shapes preserve the status for `WorkerError::grpc_status` / `is_retryable`.
fn registration_denial_error(status: tonic::Status) -> WorkerError {
    if status.code() == tonic::Code::Unauthenticated {
        WorkerError::Handshake { source: status }
    } else {
        WorkerError::Registration {
            source: Box::new(status),
        }
    }
}

fn apply_auth_metadata(
    metadata: &mut tonic::metadata::MetadataMap,
    config: &WorkerConfig,
) -> Result<(), WorkerError> {
    let namespace =
        MetadataValue::try_from(config.namespace.as_str()).map_err(|_| WorkerError::Handshake {
            source: tonic::Status::invalid_argument("worker namespace is not valid gRPC metadata"),
        })?;
    let subject =
        MetadataValue::try_from(config.subject.as_str()).map_err(|_| WorkerError::Handshake {
            source: tonic::Status::invalid_argument("worker subject is not valid gRPC metadata"),
        })?;
    metadata.insert("x-aion-namespaces", namespace);
    metadata.insert("x-aion-subject", subject);
    Ok(())
}

#[async_trait]
impl WorkerSession for GrpcWorkerSession {
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
        self.config = config.clone();
        if self.client.is_none() {
            self.client = Some(
                GeneratedClient::connect(self.config.endpoint.clone())
                    .await
                    .map_err(|source| WorkerError::Connect { source })?,
            );
        }
        Ok(())
    }

    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError> {
        validate_activity_handlers(&activity_types, available_handlers)?;
        self.activity_types.clone_from(&activity_types);

        // OQ-5: the registration namespace is the SAME value advertised in the
        // `x-aion-namespaces` auth metadata (`apply_auth_metadata`), so a worker
        // registers into exactly the namespace it is authorized for. `task_queue`
        // is the disjoint pool/flavour selector within that namespace.
        let register = aion_proto::generated::RegisterWorker {
            namespace: self.config.namespace.clone(),
            activity_types,
            task_queue: self.config.task_queue.clone(),
        };
        self.open_registered_stream(register).await
    }

    fn receive_tasks(&mut self) -> WorkerTaskStream {
        match self.receiver.take() {
            Some(receiver) => Box::pin(receiver.filter_map(|message| async move {
                Some(match message {
                    Ok(server_message) => decode_server_message(server_message),
                    Err(source) => Err(WorkerError::Transport { source }),
                })
            })),
            None => Box::pin(futures::stream::iter([Err(WorkerError::Transport {
                source: tonic::Status::failed_precondition(
                    "worker receive stream has not been opened",
                ),
            })])),
        }
    }

    async fn report_result(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        result: Payload,
    ) -> Result<(), WorkerError> {
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
            run_id: run_id.map(ProtoRunId::from),
            outcome: Some(proto_activity_result::Outcome::Result(ProtoPayload::from(
                result,
            ))),
        };
        self.send_to_server(aion_proto::generated::worker_to_server::Message::Result(
            generated_activity_result(result),
        ))
        .await
    }

    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        run_id: Option<RunId>,
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
            run_id: run_id.map(ProtoRunId::from),
            outcome: Some(proto_activity_result::Outcome::Error(failure.into())),
        };
        self.send_to_server(aion_proto::generated::worker_to_server::Message::Result(
            generated_activity_result(result),
        ))
        .await
    }

    async fn send_heartbeat(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Option<Payload>,
    ) -> Result<(), WorkerError> {
        let heartbeat = ProtoHeartbeat {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
            progress: progress.map(ProtoPayload::from),
        };
        self.send_to_server(aion_proto::generated::worker_to_server::Message::Heartbeat(
            generated_heartbeat(heartbeat),
        ))
        .await
    }
}

fn decode_server_message(
    message: aion_proto::generated::ServerToWorker,
) -> Result<WorkerSessionEvent, WorkerError> {
    match message.message {
        Some(aion_proto::generated::server_to_worker::Message::Task(task)) => {
            Ok(WorkerSessionEvent::Task(proto_task(task)))
        }
        Some(aion_proto::generated::server_to_worker::Message::Drain(_)) => {
            Ok(WorkerSessionEvent::Drain)
        }
        Some(aion_proto::generated::server_to_worker::Message::ResultAck(ack)) => {
            decode_result_ack(ack)
        }
        Some(aion_proto::generated::server_to_worker::Message::RegisterAck(_)) => {
            // The ack is consumed inside `open_registered_stream`; a second
            // one mid-stream is a server ordering bug that must surface.
            Err(WorkerError::decode(SessionStateError {
                message: String::from(
                    "protocol violation: RegisterAck received after registration completed",
                ),
            }))
        }
        None => Err(WorkerError::decode(SessionStateError {
            message: String::from("server-to-worker message was empty"),
        })),
    }
}

fn decode_result_ack(
    ack: aion_proto::generated::ResultAck,
) -> Result<WorkerSessionEvent, WorkerError> {
    let workflow_id = ack
        .workflow_id
        .ok_or_else(|| {
            WorkerError::decode(SessionStateError {
                message: String::from("result ack workflow_id is missing"),
            })
        })
        .and_then(|id| {
            WorkflowId::try_from(ProtoWorkflowId { uuid: id.uuid }).map_err(|source| {
                WorkerError::decode(SessionStateError {
                    message: format!("result ack workflow_id is invalid: {source}"),
                })
            })
        })?;
    let activity_id = ack
        .activity_id
        .map(|id| ActivityId::from_sequence_position(id.sequence_position))
        .ok_or_else(|| {
            WorkerError::decode(SessionStateError {
                message: String::from("result ack activity_id is missing"),
            })
        })?;
    Ok(WorkerSessionEvent::ResultAck {
        workflow_id,
        activity_id,
    })
}

fn generated_activity_result(value: ProtoActivityResult) -> aion_proto::generated::ActivityResult {
    aion_proto::generated::ActivityResult {
        workflow_id: value.workflow_id.map(generated_workflow_id),
        activity_id: value.activity_id.map(generated_activity_id),
        run_id: value.run_id.map(generated_run_id),
        outcome: value.outcome.map(|outcome| match outcome {
            proto_activity_result::Outcome::Result(result) => {
                aion_proto::generated::activity_result::Outcome::Result(generated_payload(result))
            }
            proto_activity_result::Outcome::Error(error) => {
                aion_proto::generated::activity_result::Outcome::Error(generated_error(error))
            }
        }),
    }
}

fn generated_heartbeat(value: ProtoHeartbeat) -> aion_proto::generated::Heartbeat {
    aion_proto::generated::Heartbeat {
        workflow_id: value.workflow_id.map(generated_workflow_id),
        activity_id: value.activity_id.map(generated_activity_id),
        progress: value.progress.map(generated_payload),
    }
}

fn proto_task(value: aion_proto::generated::ActivityTask) -> ProtoActivityTask {
    ProtoActivityTask {
        workflow_id: value.workflow_id.map(proto_workflow_id),
        activity_id: value.activity_id.map(proto_activity_id),
        activity_type: value.activity_type,
        input: value.input.map(proto_payload),
        attempt: value.attempt,
        labels: value.labels,
        run_id: value.run_id.map(proto_run_id),
    }
}

fn generated_payload(value: ProtoPayload) -> aion_proto::generated::Payload {
    aion_proto::generated::Payload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn proto_payload(value: aion_proto::generated::Payload) -> ProtoPayload {
    ProtoPayload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn generated_workflow_id(value: ProtoWorkflowId) -> aion_proto::generated::WorkflowId {
    aion_proto::generated::WorkflowId { uuid: value.uuid }
}

fn proto_workflow_id(value: aion_proto::generated::WorkflowId) -> ProtoWorkflowId {
    ProtoWorkflowId { uuid: value.uuid }
}

fn generated_run_id(value: ProtoRunId) -> aion_proto::generated::RunId {
    aion_proto::generated::RunId { uuid: value.uuid }
}

fn proto_run_id(value: aion_proto::generated::RunId) -> ProtoRunId {
    ProtoRunId { uuid: value.uuid }
}

fn generated_activity_id(value: ProtoActivityId) -> aion_proto::generated::ActivityId {
    aion_proto::generated::ActivityId {
        sequence_position: value.sequence_position,
    }
}

fn proto_activity_id(value: aion_proto::generated::ActivityId) -> ProtoActivityId {
    ProtoActivityId {
        sequence_position: value.sequence_position,
    }
}

fn generated_error(value: aion_proto::ProtoActivityError) -> aion_proto::generated::ActivityError {
    aion_proto::generated::ActivityError {
        kind: value.kind,
        message: value.message,
        details: value.details.map(generated_payload),
    }
}

#[derive(thiserror::Error, Debug)]
#[error("{message}")]
struct SessionStateError {
    message: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use aion_proto::ProtoActivityTask;
    use async_trait::async_trait;
    use futures::{StreamExt, stream};

    use super::{
        WorkerSession, WorkerSessionEvent, WorkerTaskStream, apply_auth_metadata,
        validate_activity_handlers,
    };
    use crate::error::WorkerError;
    use crate::{ReconnectConfig, WorkerConfig};

    #[derive(Default)]
    struct FakeSession {
        handshakes: Vec<(String, String)>,
        registrations: Vec<Vec<String>>,
    }

    #[async_trait]
    impl WorkerSession for FakeSession {
        async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError> {
            self.handshakes
                .push((config.task_queue.clone(), config.identity.clone()));
            Ok(())
        }

        async fn register(
            &mut self,
            activity_types: Vec<String>,
            available_handlers: &BTreeSet<String>,
        ) -> Result<(), WorkerError> {
            validate_activity_handlers(&activity_types, available_handlers)?;
            self.registrations.push(activity_types);
            Ok(())
        }

        fn receive_tasks(&mut self) -> WorkerTaskStream {
            Box::pin(stream::iter([Ok(WorkerSessionEvent::Task(
                ProtoActivityTask {
                    workflow_id: None,
                    activity_id: None,
                    activity_type: String::from("charge-card"),
                    input: None,
                    attempt: 1,
                    labels: std::collections::HashMap::new(),
                    run_id: None,
                },
            ))]))
        }

        async fn report_result(
            &mut self,
            workflow_id: aion_core::WorkflowId,
            activity_id: aion_core::ActivityId,
            run_id: Option<aion_core::RunId>,
            result: aion_core::Payload,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, run_id, result));
            Ok(())
        }

        async fn report_failure(
            &mut self,
            workflow_id: aion_core::WorkflowId,
            activity_id: aion_core::ActivityId,
            run_id: Option<aion_core::RunId>,
            failure: aion_core::ActivityError,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, run_id, failure));
            Ok(())
        }

        async fn send_heartbeat(
            &mut self,
            workflow_id: aion_core::WorkflowId,
            activity_id: aion_core::ActivityId,
            progress: Option<aion_core::Payload>,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, progress));
            Ok(())
        }
    }

    #[test]
    fn apply_auth_metadata_sets_worker_authorization_headers() -> Result<(), WorkerError> {
        let config = WorkerConfig::builder()
            .endpoint("http://127.0.0.1:50051")
            .task_queue("payments")
            .identity("worker-a")
            .max_concurrency(4)
            .reconnect_initial_backoff(std::time::Duration::from_millis(5))
            .reconnect_max_backoff(std::time::Duration::from_millis(20))
            .reconnect_max_attempts(3)
            .namespace("payments")
            .subject("worker-a")
            .build()
            .map_err(WorkerError::registration)?;
        let mut metadata = tonic::metadata::MetadataMap::new();

        apply_auth_metadata(&mut metadata, &config)?;

        assert_eq!(
            metadata
                .get("x-aion-namespaces")
                .and_then(|value| value.to_str().ok()),
            Some("payments")
        );
        assert_eq!(
            metadata
                .get("x-aion-subject")
                .and_then(|value| value.to_str().ok()),
            Some("worker-a")
        );
        Ok(())
    }

    #[tokio::test]
    async fn fake_session_records_handshake_and_registration() -> Result<(), WorkerError> {
        let config = WorkerConfig::new(
            "http://127.0.0.1:50051",
            "payments",
            "worker-a",
            4,
            ReconnectConfig::new(
                std::time::Duration::from_millis(5),
                std::time::Duration::from_millis(20),
                3,
            ),
            None,
        );
        let activity_types = vec![String::from("charge-card"), String::from("send-email")];
        let handlers = activity_types.iter().cloned().collect::<BTreeSet<_>>();
        let mut session = FakeSession::default();

        session.handshake(&config).await?;
        session.register(activity_types.clone(), &handlers).await?;
        let received = session.receive_tasks().next().await;

        assert_eq!(
            session.handshakes,
            vec![(String::from("payments"), String::from("worker-a"))]
        );
        assert_eq!(session.registrations, vec![activity_types]);
        assert!(received.is_some());

        Ok(())
    }

    /// Brief test 16: a report send that never completes (server stopped
    /// reading; outbound channel full) times out retryably at the reconnect
    /// policy's `max_backoff` on a paused clock — the worker never hangs.
    #[tokio::test(start_paused = true)]
    async fn report_send_times_out_retryably_at_max_backoff() -> Result<(), WorkerError> {
        let config = WorkerConfig::new(
            "http://127.0.0.1:50051",
            "payments",
            "worker-a",
            1,
            ReconnectConfig::new(
                std::time::Duration::from_millis(5),
                std::time::Duration::from_millis(20),
                3,
            ),
            None,
        );
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        // Fill the channel so the next send blocks forever, modelling a
        // server that stopped draining its receive side.
        sender
            .try_send(aion_proto::generated::WorkerToServer { message: None })
            .map_err(WorkerError::decode)?;
        let mut session = super::GrpcWorkerSession {
            config,
            activity_types: Vec::new(),
            client: None,
            sender: Some(sender),
            receiver: None,
            registered_info: None,
        };

        let result = session
            .report_result(
                aion_core::WorkflowId::new_v4(),
                aion_core::ActivityId::from_sequence_position(1),
                None,
                aion_core::Payload::new(aion_core::ContentType::Json, b"{}".to_vec()),
            )
            .await;

        let Err(error) = result else {
            return Err(WorkerError::Transport {
                source: tonic::Status::internal("a hung send must time out, not hang"),
            });
        };
        assert!(
            matches!(error, WorkerError::Transport { .. }),
            "send deadline elapse must be a retryable transport error: {error}"
        );
        assert!(error.is_retryable());
        assert!(
            error.to_string().contains("did not complete"),
            "the error must name the deadline: {error}"
        );
        drop(receiver);
        Ok(())
    }

    #[test]
    fn registration_rejects_activity_without_handler() {
        let activity_types = vec![String::from("charge-card"), String::from("send-email")];
        let handlers = [String::from("charge-card")]
            .into_iter()
            .collect::<BTreeSet<_>>();

        let result = validate_activity_handlers(&activity_types, &handlers);
        assert!(result.is_err());
        let error = match result {
            Ok(()) => return,
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "worker registration failed: activity type `send-email` has no registered handler"
        );
    }
}
