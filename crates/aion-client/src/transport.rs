//! Network transport over the AW-owned `aion-proto` workflow service.

use aion_core::Event;
use async_trait::async_trait;
#[cfg(feature = "embedded")]
use futures::StreamExt;
use futures::stream::BoxStream;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use crate::client::ClientConfig;
use crate::error::ClientError;

/// Transport abstraction over the six unary workflow-management RPCs.
#[async_trait]
pub trait WorkflowTransport: Send + Sync {
    /// Sends `StartWorkflow` over the transport.
    async fn start_workflow(
        &self,
        request: aion_proto::ProtoStartWorkflowRequest,
    ) -> Result<aion_proto::ProtoStartWorkflowResponse, ClientError>;

    /// Sends `Signal` over the transport.
    async fn signal(
        &self,
        request: aion_proto::ProtoSignalRequest,
    ) -> Result<aion_proto::ProtoSignalResponse, ClientError>;

    /// Sends `Query` over the transport.
    async fn query(
        &self,
        request: aion_proto::ProtoQueryRequest,
    ) -> Result<aion_proto::ProtoQueryResponse, ClientError>;

    /// Sends `Cancel` over the transport.
    async fn cancel(
        &self,
        request: aion_proto::ProtoCancelRequest,
    ) -> Result<aion_proto::ProtoCancelResponse, ClientError>;

    /// Sends `ListWorkflows` over the transport.
    async fn list_workflows(
        &self,
        request: aion_proto::ProtoListWorkflowsRequest,
    ) -> Result<aion_proto::ProtoListWorkflowsResponse, ClientError>;

    /// Sends `DescribeWorkflow` over the transport.
    async fn describe_workflow(
        &self,
        request: aion_proto::ProtoDescribeWorkflowRequest,
    ) -> Result<aion_proto::ProtoDescribeWorkflowResponse, ClientError>;

    /// Opens an event subscription attempt.
    ///
    /// `resume_from_sequence` is carried at the transport seam so SDK-level
    /// resumption is observable in tests and can be mapped directly once AW adds
    /// the server-owned cursor field.
    async fn subscribe(
        &self,
        request: aion_proto::SubscriptionRequest,
        resume_from_sequence: Option<u64>,
    ) -> Result<SubscriptionAttempt, ClientError>;
}

/// One transport-level event subscription attempt.
pub struct SubscriptionAttempt {
    /// Decoded events for this attempt. A transient disconnect is represented by
    /// an `Err(ClientError::Unavailable)` item.
    pub events: BoxStream<'static, Result<Event, ClientError>>,
}

impl SubscriptionAttempt {
    /// Creates a subscription attempt from an event stream.
    #[must_use]
    pub fn new(events: BoxStream<'static, Result<Event, ClientError>>) -> Self {
        Self { events }
    }
}

/// Cloneable gRPC transport backed by tonic's reusable channel.
#[derive(Clone)]
pub struct GrpcWorkflowTransport {
    channel: Channel,
    config: ClientConfig,
}

impl GrpcWorkflowTransport {
    /// Connects a reusable gRPC transport for the supplied client configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Unavailable`] when endpoint parsing, TLS setup, or
    /// channel connection fails.
    pub async fn connect(config: ClientConfig) -> Result<Self, ClientError> {
        let endpoint = endpoint_from_config(&config)?;
        let channel = endpoint
            .connect()
            .await
            .map_err(ClientError::from_transport_error)?;
        Ok(Self { channel, config })
    }

    /// Builds a transport from an existing tonic channel.
    #[must_use]
    pub const fn from_channel(config: ClientConfig, channel: Channel) -> Self {
        Self { channel, config }
    }

    fn client(&self) -> GeneratedClient {
        GeneratedClient::new(self.channel.clone())
    }

    fn request<T>(&self, message: T) -> Result<Request<T>, ClientError> {
        let mut request = Request::new(message);
        apply_metadata(request.metadata_mut(), &self.config)?;
        Ok(request)
    }
}

type GeneratedClient =
    aion_proto::generated::workflow_service_client::WorkflowServiceClient<Channel>;

#[async_trait]
impl WorkflowTransport for GrpcWorkflowTransport {
    async fn start_workflow(
        &self,
        request: aion_proto::ProtoStartWorkflowRequest,
    ) -> Result<aion_proto::ProtoStartWorkflowResponse, ClientError> {
        let response = self
            .client()
            .start_workflow(self.request(encode_start_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_start_response(response.into_inner()))
    }

    async fn signal(
        &self,
        request: aion_proto::ProtoSignalRequest,
    ) -> Result<aion_proto::ProtoSignalResponse, ClientError> {
        let response = self
            .client()
            .signal(self.request(encode_signal_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_signal_response(response.into_inner()))
    }

    async fn query(
        &self,
        request: aion_proto::ProtoQueryRequest,
    ) -> Result<aion_proto::ProtoQueryResponse, ClientError> {
        let response = self
            .client()
            .query(self.request(encode_query_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_query_response(response.into_inner()))
    }

    async fn cancel(
        &self,
        request: aion_proto::ProtoCancelRequest,
    ) -> Result<aion_proto::ProtoCancelResponse, ClientError> {
        let response = self
            .client()
            .cancel(self.request(encode_cancel_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_cancel_response(response.into_inner()))
    }

    async fn list_workflows(
        &self,
        request: aion_proto::ProtoListWorkflowsRequest,
    ) -> Result<aion_proto::ProtoListWorkflowsResponse, ClientError> {
        let response = self
            .client()
            .list_workflows(self.request(encode_list_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_list_response(response.into_inner()))
    }

    async fn describe_workflow(
        &self,
        request: aion_proto::ProtoDescribeWorkflowRequest,
    ) -> Result<aion_proto::ProtoDescribeWorkflowResponse, ClientError> {
        let response = self
            .client()
            .describe_workflow(self.request(encode_describe_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_describe_response(response.into_inner()))
    }

    async fn subscribe(
        &self,
        _: aion_proto::SubscriptionRequest,
        _: Option<u64>,
    ) -> Result<SubscriptionAttempt, ClientError> {
        Err(ClientError::Unavailable)
    }
}

#[cfg(feature = "embedded")]
/// Transport backed by an in-process [`aion::Engine`].
pub struct EmbeddedWorkflowTransport {
    engine: std::sync::Arc<aion::Engine>,
}

#[cfg(feature = "embedded")]
impl EmbeddedWorkflowTransport {
    /// Creates an embedded transport for `engine`.
    #[must_use]
    pub fn new(engine: std::sync::Arc<aion::Engine>) -> Self {
        Self { engine }
    }
}

#[cfg(feature = "embedded")]
#[async_trait]
impl WorkflowTransport for EmbeddedWorkflowTransport {
    async fn start_workflow(
        &self,
        request: aion_proto::ProtoStartWorkflowRequest,
    ) -> Result<aion_proto::ProtoStartWorkflowResponse, ClientError> {
        let input = request
            .input
            .ok_or(ClientError::InvalidArgument)
            .and_then(|payload| {
                aion_core::Payload::try_from(payload).map_err(ClientError::from_wire_error)
            })?;
        let handle = self
            .engine
            .start_workflow(&request.workflow_type, input)
            .await
            .map_err(map_engine_error)?;
        Ok(aion_proto::ProtoStartWorkflowResponse {
            workflow_id: Some(aion_proto::ProtoWorkflowId::from(
                handle.workflow_id().clone(),
            )),
            run_id: Some(aion_proto::ProtoRunId::from(handle.run_id().clone())),
        })
    }

    async fn signal(
        &self,
        request: aion_proto::ProtoSignalRequest,
    ) -> Result<aion_proto::ProtoSignalResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let run_id = decode_required_run_id(request.run_id)?;
        let payload = request
            .payload
            .ok_or(ClientError::InvalidArgument)
            .and_then(|payload| {
                aion_core::Payload::try_from(payload).map_err(ClientError::from_wire_error)
            })?;
        self.engine
            .signal(&workflow_id, &run_id, request.signal_name, payload)
            .await
            .map_err(map_engine_error)?;
        Ok(aion_proto::ProtoSignalResponse {})
    }

    async fn query(
        &self,
        request: aion_proto::ProtoQueryRequest,
    ) -> Result<aion_proto::ProtoQueryResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let run_id = decode_required_run_id(request.run_id)?;
        let payload = self
            .engine
            .query(&workflow_id, &run_id, request.query_name)
            .await
            .map_err(map_engine_error)?;
        Ok(aion_proto::ProtoQueryResponse {
            outcome: Some(aion_proto::proto_query_response::Outcome::Result(
                aion_proto::ProtoPayload::from(payload),
            )),
        })
    }

    async fn cancel(
        &self,
        request: aion_proto::ProtoCancelRequest,
    ) -> Result<aion_proto::ProtoCancelResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let run_id = decode_required_run_id(request.run_id)?;
        self.engine
            .cancel(&workflow_id, &run_id, request.reason)
            .await
            .map_err(map_engine_error)?;
        Ok(aion_proto::ProtoCancelResponse {})
    }

    async fn list_workflows(
        &self,
        request: aion_proto::ProtoListWorkflowsRequest,
    ) -> Result<aion_proto::ProtoListWorkflowsResponse, ClientError> {
        let filter = match request.filter.as_ref() {
            Some(filter) => {
                aion_proto::decode_workflow_filter(filter).map_err(ClientError::from_wire_error)?
            }
            None => aion_core::WorkflowFilter::default(),
        };
        let summaries = self
            .engine
            .list_workflows(filter)
            .await
            .map_err(map_engine_error)?
            .iter()
            .map(|summary| {
                aion_proto::encode_workflow_summary(request.namespace.clone(), None, summary)
            })
            .map(|result| result.map_err(ClientError::from_wire_error))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(aion_proto::ProtoListWorkflowsResponse { summaries })
    }

    async fn describe_workflow(
        &self,
        request: aion_proto::ProtoDescribeWorkflowRequest,
    ) -> Result<aion_proto::ProtoDescribeWorkflowResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let history = self
            .engine
            .store()
            .read_history(&workflow_id)
            .await
            .map_err(|error| ClientError::server(error.to_string()))?;
        let Some(summary) = aion_core::WorkflowSummary::from_history(&history) else {
            return Err(ClientError::NotFound);
        };
        let summary = Some(
            aion_proto::encode_workflow_summary(request.namespace.clone(), None, &summary)
                .map_err(ClientError::from_wire_error)?,
        );
        let history = if request.include_history {
            history
                .iter()
                .map(|event| aion_proto::encode_event(request.namespace.clone(), None, event))
                .map(|result| result.map_err(ClientError::from_wire_error))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        Ok(aion_proto::ProtoDescribeWorkflowResponse { summary, history })
    }

    async fn subscribe(
        &self,
        request: aion_proto::SubscriptionRequest,
        _: Option<u64>,
    ) -> Result<SubscriptionAttempt, ClientError> {
        let filter = embedded_event_filter(request)?;
        Ok(SubscriptionAttempt::new(
            self.engine.subscribe(filter).map(Ok).boxed(),
        ))
    }
}

#[cfg(feature = "embedded")]
fn decode_required_workflow_id(
    value: Option<aion_proto::ProtoWorkflowId>,
) -> Result<aion_core::WorkflowId, ClientError> {
    value
        .ok_or(ClientError::InvalidArgument)?
        .try_into()
        .map_err(ClientError::from_wire_error)
}

#[cfg(feature = "embedded")]
fn decode_required_run_id(
    value: Option<aion_proto::ProtoRunId>,
) -> Result<aion_core::RunId, ClientError> {
    value
        .ok_or(ClientError::InvalidArgument)?
        .try_into()
        .map_err(ClientError::from_wire_error)
}

#[cfg(feature = "embedded")]
fn embedded_event_filter(
    request: aion_proto::SubscriptionRequest,
) -> Result<aion::EventFilter, ClientError> {
    match request.subscription {
        Some(aion_proto::subscription_request::Subscription::PerWorkflow(subscription)) => {
            Ok(aion::EventFilter {
                workflow_id: subscription
                    .workflow_id
                    .map(aion_core::WorkflowId::try_from)
                    .transpose()
                    .map_err(ClientError::from_wire_error)?,
                run: None,
                family: None,
            })
        }
        Some(
            aion_proto::subscription_request::Subscription::Filtered(_)
            | aion_proto::subscription_request::Subscription::Firehose(_),
        ) => Ok(aion::EventFilter::default()),
        None => Err(ClientError::InvalidArgument),
    }
}

#[cfg(feature = "embedded")]
fn map_engine_error(error: aion::EngineError) -> ClientError {
    match error {
        aion::EngineError::WorkflowNotFound { .. } => ClientError::NotFound,
        aion::EngineError::ShuttingDown => ClientError::Unavailable,
        other => ClientError::server(other.to_string()),
    }
}

fn endpoint_from_config(config: &ClientConfig) -> Result<Endpoint, ClientError> {
    let uri = config
        .endpoint
        .parse::<http::Uri>()
        .map_err(|_| ClientError::Unavailable)?;
    let endpoint = Endpoint::from(uri);
    if let Some(tls) = &config.tls {
        let mut tls_config = ClientTlsConfig::new();
        if let Some(domain) = &tls.domain_name {
            tls_config = tls_config.domain_name(domain.clone());
        }
        if let Some(ca_certificate_pem) = &tls.ca_certificate_pem {
            tls_config =
                tls_config.ca_certificate(Certificate::from_pem(ca_certificate_pem.clone()));
        }
        endpoint
            .tls_config(tls_config)
            .map_err(ClientError::from_transport_error)
    } else {
        Ok(endpoint)
    }
}

fn apply_metadata(
    metadata: &mut tonic::metadata::MetadataMap,
    config: &ClientConfig,
) -> Result<(), ClientError> {
    if let Some(auth) = &config.auth {
        let value = format!("Bearer {}", auth.token());
        let metadata_value =
            MetadataValue::try_from(value.as_str()).map_err(|_| ClientError::InvalidArgument)?;
        metadata.insert("authorization", metadata_value);
    }
    if let Some(subject) = &config.subject {
        let metadata_value =
            MetadataValue::try_from(subject.as_str()).map_err(|_| ClientError::InvalidArgument)?;
        metadata.insert("x-aion-subject", metadata_value);
    }
    if !config.authorized_namespaces.is_empty() {
        let namespaces = config.authorized_namespaces.join(",");
        let metadata_value = MetadataValue::try_from(namespaces.as_str())
            .map_err(|_| ClientError::InvalidArgument)?;
        metadata.insert("x-aion-namespaces", metadata_value);
    }
    Ok(())
}

fn encode_workflow_id(value: aion_proto::ProtoWorkflowId) -> aion_proto::generated::WorkflowId {
    aion_proto::generated::WorkflowId { uuid: value.uuid }
}

fn decode_workflow_id(value: aion_proto::generated::WorkflowId) -> aion_proto::ProtoWorkflowId {
    aion_proto::ProtoWorkflowId { uuid: value.uuid }
}

fn encode_run_id(value: aion_proto::ProtoRunId) -> aion_proto::generated::RunId {
    aion_proto::generated::RunId { uuid: value.uuid }
}

fn decode_run_id(value: aion_proto::generated::RunId) -> aion_proto::ProtoRunId {
    aion_proto::ProtoRunId { uuid: value.uuid }
}

fn encode_payload(value: aion_proto::ProtoPayload) -> aion_proto::generated::Payload {
    aion_proto::generated::Payload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn decode_payload(value: aion_proto::generated::Payload) -> aion_proto::ProtoPayload {
    aion_proto::ProtoPayload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn decode_wire_error(value: aion_proto::generated::WireError) -> aion_proto::ProtoWireError {
    aion_proto::ProtoWireError {
        code: value.code,
        message: value.message,
        error_type: value.error_type,
    }
}

fn encode_envelope(value: aion_proto::WireEnvelope) -> aion_proto::generated::WireEnvelope {
    aion_proto::generated::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(encode_payload),
    }
}

fn decode_envelope(value: aion_proto::generated::WireEnvelope) -> aion_proto::WireEnvelope {
    aion_proto::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(decode_payload),
    }
}

fn encode_start_request(
    value: aion_proto::ProtoStartWorkflowRequest,
) -> aion_proto::generated::StartWorkflowRequest {
    aion_proto::generated::StartWorkflowRequest {
        namespace: value.namespace,
        workflow_type: value.workflow_type,
        input: value.input.map(encode_payload),
    }
}

fn decode_start_response(
    value: aion_proto::generated::StartWorkflowResponse,
) -> aion_proto::ProtoStartWorkflowResponse {
    aion_proto::ProtoStartWorkflowResponse {
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
    }
}

fn encode_signal_request(
    value: aion_proto::ProtoSignalRequest,
) -> aion_proto::generated::SignalRequest {
    aion_proto::generated::SignalRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        signal_name: value.signal_name,
        payload: value.payload.map(encode_payload),
    }
}

fn decode_signal_response(
    _: aion_proto::generated::SignalResponse,
) -> aion_proto::ProtoSignalResponse {
    aion_proto::ProtoSignalResponse {}
}

fn encode_query_request(
    value: aion_proto::ProtoQueryRequest,
) -> aion_proto::generated::QueryRequest {
    aion_proto::generated::QueryRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        query_name: value.query_name,
    }
}

fn decode_query_response(
    value: aion_proto::generated::QueryResponse,
) -> aion_proto::ProtoQueryResponse {
    aion_proto::ProtoQueryResponse {
        outcome: value.outcome.map(decode_query_outcome),
    }
}

fn decode_query_outcome(
    value: aion_proto::generated::query_response::Outcome,
) -> aion_proto::proto_query_response::Outcome {
    match value {
        aion_proto::generated::query_response::Outcome::Result(payload) => {
            aion_proto::proto_query_response::Outcome::Result(decode_payload(payload))
        }
        aion_proto::generated::query_response::Outcome::Error(error) => {
            aion_proto::proto_query_response::Outcome::Error(decode_wire_error(error))
        }
    }
}

fn encode_cancel_request(
    value: aion_proto::ProtoCancelRequest,
) -> aion_proto::generated::CancelRequest {
    aion_proto::generated::CancelRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        reason: value.reason,
    }
}

fn decode_cancel_response(
    _: aion_proto::generated::CancelResponse,
) -> aion_proto::ProtoCancelResponse {
    aion_proto::ProtoCancelResponse {}
}

fn encode_list_request(
    value: aion_proto::ProtoListWorkflowsRequest,
) -> aion_proto::generated::ListWorkflowsRequest {
    aion_proto::generated::ListWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(encode_envelope),
    }
}

fn decode_list_response(
    value: aion_proto::generated::ListWorkflowsResponse,
) -> aion_proto::ProtoListWorkflowsResponse {
    aion_proto::ProtoListWorkflowsResponse {
        summaries: value.summaries.into_iter().map(decode_envelope).collect(),
    }
}

fn encode_describe_request(
    value: aion_proto::ProtoDescribeWorkflowRequest,
) -> aion_proto::generated::DescribeWorkflowRequest {
    aion_proto::generated::DescribeWorkflowRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
        include_history: value.include_history,
    }
}

fn decode_describe_response(
    value: aion_proto::generated::DescribeWorkflowResponse,
) -> aion_proto::ProtoDescribeWorkflowResponse {
    aion_proto::ProtoDescribeWorkflowResponse {
        summary: value.summary.map(decode_envelope),
        history: value.history.into_iter().map(decode_envelope).collect(),
    }
}
