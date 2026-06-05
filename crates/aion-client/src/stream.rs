//! Event subscription `Stream` and resumption.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use aion_core::{Event, WorkflowFilter, WorkflowId};
use aion_proto::{
    FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoWorkflowId,
    SubscriptionRequest, subscription_request,
};
use futures::Stream;
use futures::future::BoxFuture;
use futures::stream::BoxStream;

use crate::error::ClientError;
use crate::transport::{SubscriptionAttempt, WorkflowTransport};

/// Boxed event stream returned by subscribe operations.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, ClientError>> + Send>>;

/// Builder for the AW-owned subscription variants supported by the SDK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscribeTarget {
    /// Subscribe to events for one workflow.
    Workflow {
        /// Workflow whose events are requested.
        workflow_id: WorkflowId,
    },
    /// Subscribe to workflow metadata selected events.
    Filtered {
        /// Workflow metadata filter used for the subscription.
        filter: WorkflowFilter,
    },
    /// Subscribe to all visible events in the client's namespace.
    Firehose,
}

impl SubscribeTarget {
    pub(crate) fn request(&self, namespace: &str) -> SubscriptionRequest {
        match self {
            Self::Workflow { workflow_id } => SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::PerWorkflow(
                    PerWorkflowSubscription {
                        namespace: namespace.to_owned(),
                        workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                    },
                )),
            },
            Self::Filtered { filter } => SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::Filtered(
                    FilteredSubscription {
                        namespace: namespace.to_owned(),
                        workflow_type: filter.workflow_type.clone(),
                        status: filter
                            .status
                            .map(|status| aion_proto::ProtoWorkflowStatus::from(status) as i32),
                        namespace_selector: None,
                    },
                )),
            },
            Self::Firehose => SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::Firehose(
                    FirehoseSubscription {
                        namespace: namespace.to_owned(),
                    },
                )),
            },
        }
    }
}

/// Reconnecting, duplicate-filtering subscription stream.
pub struct ResumingEventStream {
    transport: Arc<dyn WorkflowTransport>,
    namespace: String,
    target: SubscribeTarget,
    last_seq: Option<u64>,
    current: Option<BoxStream<'static, Result<Event, ClientError>>>,
    pending_subscribe: Option<BoxFuture<'static, Result<SubscriptionAttempt, ClientError>>>,
    terminal_error: Option<ClientError>,
    finished: bool,
}

impl ResumingEventStream {
    /// Creates a subscription stream for `target`.
    #[must_use]
    pub fn new(
        transport: Arc<dyn WorkflowTransport>,
        namespace: impl Into<String>,
        target: SubscribeTarget,
    ) -> Self {
        Self {
            transport,
            namespace: namespace.into(),
            target,
            last_seq: None,
            current: None,
            pending_subscribe: None,
            terminal_error: None,
            finished: false,
        }
    }

    fn start_subscribe(&mut self) {
        let transport = Arc::clone(&self.transport);
        let request = self.target.request(&self.namespace);
        let resume_from_sequence = self.last_seq.map(|seq| seq.saturating_add(1));
        self.pending_subscribe = Some(Box::pin(async move {
            transport.subscribe(request, resume_from_sequence).await
        }));
    }
}

impl Stream for ResumingEventStream {
    type Item = Result<Event, ClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.finished {
                return Poll::Ready(None);
            }

            if let Some(error) = this.terminal_error.take() {
                this.finished = true;
                return Poll::Ready(Some(Err(error)));
            }

            if this.current.is_none() && this.pending_subscribe.is_none() {
                this.start_subscribe();
            }

            if let Some(pending) = this.pending_subscribe.as_mut() {
                match pending.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(attempt)) => {
                        this.pending_subscribe = None;
                        this.current = Some(attempt.events);
                    }
                    Poll::Ready(Err(error)) => {
                        this.pending_subscribe = None;
                        this.finished = true;
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }

            let Some(current) = this.current.as_mut() else {
                continue;
            };
            match current.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(event))) => {
                    if this.last_seq.is_some_and(|seq| event.seq() <= seq) {
                        continue;
                    }
                    this.last_seq = Some(event.seq());
                    return Poll::Ready(Some(Ok(event)));
                }
                Poll::Ready(Some(Err(error))) => {
                    this.current = None;
                    if is_retryable(&error) {
                        continue;
                    }
                    this.terminal_error = Some(error);
                }
                Poll::Ready(None) => {
                    this.current = None;
                    this.finished = true;
                    return Poll::Ready(None);
                }
            }
        }
    }
}

/// Boxes a resuming event stream behind the public return type.
#[must_use]
pub fn event_stream(
    transport: Arc<dyn WorkflowTransport>,
    namespace: impl Into<String>,
    target: SubscribeTarget,
) -> EventStream {
    Box::pin(ResumingEventStream::new(transport, namespace, target))
}

fn is_retryable(error: &ClientError) -> bool {
    matches!(error, ClientError::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;

    use aion_core::{ContentType, Event, EventEnvelope, Payload, WorkflowId};
    use aion_proto::{
        ProtoCancelResponse, ProtoDescribeWorkflowResponse, ProtoListWorkflowsResponse,
        ProtoQueryResponse, ProtoSignalResponse, ProtoStartWorkflowResponse,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use futures::StreamExt;
    use futures::stream;
    use tokio::sync::Mutex;

    use super::{ResumingEventStream, SubscribeTarget};
    use crate::error::ClientError;
    use crate::transport::{SubscriptionAttempt, WorkflowTransport};

    #[derive(Default)]
    struct SubscribeStub {
        attempts: Mutex<VecDeque<SubscriptionAttempt>>,
        resume_points: Mutex<Vec<Option<u64>>>,
    }

    #[async_trait]
    impl WorkflowTransport for SubscribeStub {
        async fn start_workflow(
            &self,
            _: aion_proto::ProtoStartWorkflowRequest,
        ) -> Result<ProtoStartWorkflowResponse, ClientError> {
            Err(ClientError::Unavailable)
        }

        async fn signal(
            &self,
            _: aion_proto::ProtoSignalRequest,
        ) -> Result<ProtoSignalResponse, ClientError> {
            Err(ClientError::Unavailable)
        }

        async fn query(
            &self,
            _: aion_proto::ProtoQueryRequest,
        ) -> Result<ProtoQueryResponse, ClientError> {
            Err(ClientError::Unavailable)
        }

        async fn cancel(
            &self,
            _: aion_proto::ProtoCancelRequest,
        ) -> Result<ProtoCancelResponse, ClientError> {
            Err(ClientError::Unavailable)
        }

        async fn list_workflows(
            &self,
            _: aion_proto::ProtoListWorkflowsRequest,
        ) -> Result<ProtoListWorkflowsResponse, ClientError> {
            Err(ClientError::Unavailable)
        }

        async fn describe_workflow(
            &self,
            _: aion_proto::ProtoDescribeWorkflowRequest,
        ) -> Result<ProtoDescribeWorkflowResponse, ClientError> {
            Err(ClientError::Unavailable)
        }

        async fn subscribe(
            &self,
            _: aion_proto::SubscriptionRequest,
            resume_from_sequence: Option<u64>,
        ) -> Result<SubscriptionAttempt, ClientError> {
            self.resume_points.lock().await.push(resume_from_sequence);
            self.attempts
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| ClientError::server("missing subscribe attempt"))
        }
    }

    fn event(seq: u64, workflow_id: &WorkflowId) -> Event {
        Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("checkout"),
            input: Payload::new(ContentType::Json, Vec::new()),
        }
    }

    #[tokio::test]
    async fn resumes_after_transient_disconnect_without_gaps_or_duplicates() {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![
                    Ok(event(1, &workflow_id)),
                    Ok(event(2, &workflow_id)),
                    Err(ClientError::Unavailable),
                ])
                .boxed(),
            ));
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![
                    Ok(event(2, &workflow_id)),
                    Ok(event(3, &workflow_id)),
                    Ok(event(4, &workflow_id)),
                ])
                .boxed(),
            ));
        let mut events = ResumingEventStream::new(
            stub.clone(),
            "tenant-a",
            SubscribeTarget::Workflow {
                workflow_id: workflow_id.clone(),
            },
        );

        let mut seqs = Vec::new();
        while let Some(item) = events.next().await {
            match item {
                Ok(event) => seqs.push(event.seq()),
                Err(error) => panic!("unexpected stream error: {error}"),
            }
        }

        assert_eq!(seqs, vec![1, 2, 3, 4]);
        assert_eq!(*stub.resume_points.lock().await, vec![None, Some(3)]);
    }

    #[tokio::test]
    async fn terminal_failure_is_yielded_before_end() {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Err(ClientError::Unauthenticated)]).boxed(),
            ));
        let mut events =
            ResumingEventStream::new(stub, "tenant-a", SubscribeTarget::Workflow { workflow_id });

        assert_eq!(events.next().await, Some(Err(ClientError::Unauthenticated)));
        assert_eq!(events.next().await, None);
    }
}
