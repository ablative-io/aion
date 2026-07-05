//! Network transport over the AW-owned `aion-proto` workflow service.

use async_trait::async_trait;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use crate::client::ClientConfig;
use crate::error::ClientError;
use crate::transport::contract::{SubscriptionAttempt, WorkflowTransport};
use crate::transport::convert::{
    decode_cancel_response, decode_describe_response, decode_list_response, decode_pause_response,
    decode_query_response, decode_reopen_response, decode_resume_response, decode_signal_response,
    decode_start_response, encode_cancel_request, encode_describe_request, encode_list_request,
    encode_pause_request, encode_query_request, encode_reopen_request, encode_resume_request,
    encode_signal_request, encode_start_request,
};
use crate::transport::ws;

/// Cloneable gRPC transport backed by tonic's reusable channel.
///
/// The six unary workflow-management RPCs ride the tonic channel; event
/// subscriptions ride the server's WebSocket endpoint (the server's only
/// streaming surface) through [`ws::open_subscription`], which requires the
/// explicit [`crate::ClientBuilder::with_stream_endpoint`] configuration.
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
            .map_err(|error| ClientError::from_transport_error(&error))?;
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

    async fn reopen(
        &self,
        request: aion_proto::ProtoReopenRequest,
    ) -> Result<aion_proto::ProtoReopenResponse, ClientError> {
        let response = self
            .client()
            .reopen(self.request(encode_reopen_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_reopen_response(response.into_inner()))
    }

    async fn pause(
        &self,
        request: aion_proto::ProtoPauseRequest,
    ) -> Result<aion_proto::ProtoPauseResponse, ClientError> {
        let response = self
            .client()
            .pause(self.request(encode_pause_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_pause_response(response.into_inner()))
    }

    async fn resume(
        &self,
        request: aion_proto::ProtoResumeRequest,
    ) -> Result<aion_proto::ProtoResumeResponse, ClientError> {
        let response = self
            .client()
            .resume(self.request(encode_resume_request(request))?)
            .await
            .map_err(|status| ClientError::from_status(&status))?;
        Ok(decode_resume_response(response.into_inner()))
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
        request: aion_proto::SubscriptionRequest,
        resume_from_sequence: Option<u64>,
    ) -> Result<SubscriptionAttempt, ClientError> {
        ws::open_subscription(&self.config, request, resume_from_sequence).await
    }
}

fn endpoint_from_config(config: &ClientConfig) -> Result<Endpoint, ClientError> {
    let uri = config.endpoint.parse::<http::Uri>().map_err(|source| {
        ClientError::unavailable(format!(
            "endpoint {} is not a valid URI: {source}",
            config.endpoint
        ))
    })?;
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
            .map_err(|error| ClientError::from_transport_error(&error))
    } else {
        Ok(endpoint)
    }
}

pub(crate) fn apply_metadata(
    metadata: &mut tonic::metadata::MetadataMap,
    config: &ClientConfig,
) -> Result<(), ClientError> {
    if let Some(auth) = &config.auth {
        let value = format!("Bearer {}", auth.token());
        let metadata_value = MetadataValue::try_from(value.as_str())
            .map_err(|_| ClientError::invalid_argument("auth token is not a valid header value"))?;
        metadata.insert("authorization", metadata_value);
    }
    if let Some(subject) = &config.subject {
        let metadata_value = MetadataValue::try_from(subject.as_str()).map_err(|_| {
            ClientError::invalid_argument("subject is not a valid x-aion-subject header value")
        })?;
        metadata.insert("x-aion-subject", metadata_value);
    }
    if !config.authorized_namespaces.is_empty() {
        let namespaces = config.authorized_namespaces.join(",");
        let metadata_value = MetadataValue::try_from(namespaces.as_str()).map_err(|_| {
            ClientError::invalid_argument(
                "authorized namespaces are not a valid x-aion-namespaces header value",
            )
        })?;
        metadata.insert("x-aion-namespaces", metadata_value);
    }
    Ok(())
}
