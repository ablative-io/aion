//! The transport seam: the trait every adapter implements and the
//! per-attempt subscription stream it returns.

use aion_core::Event;
use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::error::ClientError;

/// Transport abstraction over the six unary workflow-management RPCs plus
/// event subscription.
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
    /// `resume_from_sequence` is the wire resume cursor (`resume_from_seq`,
    /// the FIRST per-workflow sequence number wanted — `last delivered + 1`).
    /// It is only ever supplied for per-workflow subscriptions; filtered and
    /// firehose streams are live-only by design and must reject a cursor.
    async fn subscribe(
        &self,
        request: aion_proto::SubscriptionRequest,
        resume_from_sequence: Option<u64>,
    ) -> Result<SubscriptionAttempt, ClientError>;
}

/// One transport-level event subscription attempt.
pub struct SubscriptionAttempt {
    /// Decoded events for this attempt. A transient disconnect is represented
    /// by an `Err(ClientError::Unavailable)` item; any other error item is
    /// terminal for the surrounding resume loop.
    pub events: BoxStream<'static, Result<Event, ClientError>>,
}

impl SubscriptionAttempt {
    /// Creates a subscription attempt from an event stream.
    #[must_use]
    pub fn new(events: BoxStream<'static, Result<Event, ClientError>>) -> Self {
        Self { events }
    }
}
