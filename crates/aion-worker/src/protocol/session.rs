//! `WorkerSession` trait and gRPC-backed implementation.

use std::collections::BTreeSet;
use std::pin::Pin;

use aion_core::{ActivityError, ActivityId, Payload, WorkflowId};
use aion_proto::{
    ProtoActivityId, ProtoActivityResult, ProtoActivityTask, ProtoHeartbeat, ProtoPayload,
    ProtoWorkflowId, proto_activity_result,
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
    /// Server is draining and will not dispatch more activity tasks on this stream.
    Drain,
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
    /// Performs the worker handshake for the configured task queue and identity.
    ///
    /// Maps to the initial `RegisterWorker` frame on AW's `StreamWorker` RPC.
    /// AW currently names the task-queue scope `namespace` and has no identity
    /// field, so identity is retained at this SDK boundary until the wire adds
    /// a corresponding shape.
    async fn handshake(&mut self, config: &WorkerConfig) -> Result<(), WorkerError>;

    /// Registers activity-type names implemented by this worker.
    ///
    /// Maps to `RegisterWorker.activity_types` on AW's `StreamWorker` RPC. The
    /// caller supplies `available_handlers` so registration can be rejected
    /// before serving if any requested name lacks a handler.
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
        result: Payload,
    ) -> Result<(), WorkerError>;

    /// Reports explicit activity failure via `WorkerToServer.result`.
    async fn report_failure(
        &mut self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
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

/// gRPC-backed [`WorkerSession`] using `aion-proto` generated tonic stubs.
pub struct GrpcWorkerSession {
    config: WorkerConfig,
    activity_types: Vec<String>,
    client: Option<GeneratedClient>,
    sender: Option<mpsc::Sender<aion_proto::generated::WorkerToServer>>,
    receiver: Option<tonic::codec::Streaming<aion_proto::generated::ServerToWorker>>,
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
        }
    }

    async fn open_stream(&mut self) -> Result<(), WorkerError> {
        if self.client.is_none() {
            self.client = Some(
                GeneratedClient::connect(self.config.endpoint.clone())
                    .await
                    .map_err(|source| WorkerError::Connect { source })?,
            );
        }
        let client = self.client.as_mut().ok_or_else(|| {
            WorkerError::registration(SessionStateError {
                message: String::from("worker client was not available after connect"),
            })
        })?;
        let (sender, outbound) = mpsc::channel(16);
        let mut request = Request::new(ReceiverStream::new(outbound));
        apply_auth_metadata(request.metadata_mut(), &self.config)?;
        let response = client
            .stream_worker(request)
            .await
            .map_err(|source| WorkerError::Handshake { source })?;

        self.sender = Some(sender);
        self.receiver = Some(response.into_inner());
        Ok(())
    }

    async fn send_to_server(
        &self,
        message: aion_proto::generated::worker_to_server::Message,
    ) -> Result<(), WorkerError> {
        let sender = self.sender.as_ref().ok_or_else(|| {
            WorkerError::registration(SessionStateError {
                message: String::from("worker stream has not been opened"),
            })
        })?;
        sender
            .send(aion_proto::generated::WorkerToServer {
                message: Some(message),
            })
            .await
            .map_err(|source| WorkerError::Transport {
                source: tonic::Status::unavailable(format!("worker stream send failed: {source}")),
            })
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
        self.open_stream().await
    }

    async fn register(
        &mut self,
        activity_types: Vec<String>,
        available_handlers: &BTreeSet<String>,
    ) -> Result<(), WorkerError> {
        validate_activity_handlers(&activity_types, available_handlers)?;
        self.activity_types.clone_from(&activity_types);

        let register = aion_proto::generated::RegisterWorker {
            namespace: self.config.task_queue.clone(),
            activity_types,
        };
        self.send_to_server(aion_proto::generated::worker_to_server::Message::Register(
            register,
        ))
        .await
        .map_err(|error| match error {
            WorkerError::Transport { source } => WorkerError::Registration {
                source: Box::new(source),
            },
            other => other,
        })
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
        result: Payload,
    ) -> Result<(), WorkerError> {
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
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
        failure: ActivityError,
    ) -> Result<(), WorkerError> {
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
            activity_id: Some(ProtoActivityId::from(activity_id)),
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
        None => Err(WorkerError::decode(SessionStateError {
            message: String::from("server-to-worker message was empty"),
        })),
    }
}

fn generated_activity_result(value: ProtoActivityResult) -> aion_proto::generated::ActivityResult {
    aion_proto::generated::ActivityResult {
        workflow_id: value.workflow_id.map(generated_workflow_id),
        activity_id: value.activity_id.map(generated_activity_id),
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
                },
            ))]))
        }

        async fn report_result(
            &mut self,
            workflow_id: aion_core::WorkflowId,
            activity_id: aion_core::ActivityId,
            result: aion_core::Payload,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, result));
            Ok(())
        }

        async fn report_failure(
            &mut self,
            workflow_id: aion_core::WorkflowId,
            activity_id: aion_core::ActivityId,
            failure: aion_core::ActivityError,
        ) -> Result<(), WorkerError> {
            drop((workflow_id, activity_id, failure));
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
