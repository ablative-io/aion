//! Event subscription `Stream` and resumption.

use std::num::NonZeroU64;
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
                        resume_from_seq: None,
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

/// Reconnecting subscription stream.
///
/// Resumption is per-workflow only: per-workflow `seq` is the only ordering
/// that exists, so only [`SubscribeTarget::Workflow`] streams track a cursor
/// (`resume_from_seq = last delivered + 1`) and deduplicate by sequence
/// number. Filtered and firehose streams are live-only by design: a
/// transient disconnect after at least one delivered event ends the stream
/// with an honest [`ClientError::Unavailable`] instead of silently
/// reattaching a gapped stream; reconnect-live-only is allowed only while
/// nothing has been delivered yet.
///
/// Connect-failure contract (cross-SDK): a failed subscription attach is
/// classified exactly like a mid-stream drop. [`ClientError::Unavailable`]
/// (transport-level connect failure, DNS/TLS/socket failure, abnormal close)
/// is retryable and the stream re-attaches — on the initial attach as well as
/// after delivered events — until the caller drops the stream; every other
/// taxonomy error (`Unauthenticated`, `NamespaceDenied`, `NotFound`,
/// `InvalidArgument`, `Server`, ...) is terminal immediately.
pub struct ResumingEventStream {
    transport: Arc<dyn WorkflowTransport>,
    namespace: String,
    target: SubscribeTarget,
    last_seq: Option<u64>,
    delivered_any: bool,
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
            delivered_any: false,
            current: None,
            pending_subscribe: None,
            terminal_error: None,
            finished: false,
        }
    }

    /// Creates a per-workflow subscription stream that attaches with an
    /// explicit starting cursor.
    ///
    /// `resume_from` is the first per-workflow sequence number wanted
    /// (`resume_from_seq` on the wire); `1` replays the full recorded
    /// history before splicing into the live stream. The type makes the
    /// invalid cursor `0` unrepresentable.
    #[must_use]
    pub fn from_sequence(
        transport: Arc<dyn WorkflowTransport>,
        namespace: impl Into<String>,
        workflow_id: WorkflowId,
        resume_from: NonZeroU64,
    ) -> Self {
        let mut stream = Self::new(
            transport,
            namespace,
            SubscribeTarget::Workflow { workflow_id },
        );
        // The cursor sent on (re)attach is always `last_seq + 1`, so seeding
        // `last_seq = resume_from - 1` makes the first attach request exactly
        // `resume_from` and drops anything older on the dedupe path.
        stream.last_seq = Some(resume_from.get() - 1);
        stream
    }

    fn is_per_workflow(&self) -> bool {
        matches!(self.target, SubscribeTarget::Workflow { .. })
    }

    fn start_subscribe(&mut self) {
        let transport = Arc::clone(&self.transport);
        let request = self.target.request(&self.namespace);
        // Only per-workflow streams carry a resume cursor; filtered and
        // firehose reattach live-only (and only before any delivery).
        let resume_from_sequence = if self.is_per_workflow() {
            self.last_seq.map(|seq| seq.saturating_add(1))
        } else {
            None
        };
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
                        // Cross-SDK connect-failure contract: an attach
                        // failure is classified exactly like a mid-stream
                        // drop. `Unavailable` is retryable — per-workflow
                        // streams reconnect with their cursor, live-only
                        // streams reconnect only while nothing has been
                        // delivered. Every other taxonomy error is terminal.
                        this.pending_subscribe = None;
                        if is_retryable(&error) && (this.is_per_workflow() || !this.delivered_any) {
                            continue;
                        }
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
                    if this.is_per_workflow() {
                        // Sequence-number dedupe is coherent only within one
                        // workflow's history.
                        if this.last_seq.is_some_and(|seq| event.seq() <= seq) {
                            continue;
                        }
                        this.last_seq = Some(event.seq());
                    }
                    this.delivered_any = true;
                    return Poll::Ready(Some(Ok(event)));
                }
                Poll::Ready(Some(Err(error))) => {
                    this.current = None;
                    if is_retryable(&error) {
                        if this.is_per_workflow() {
                            continue;
                        }
                        if !this.delivered_any {
                            // Nothing delivered yet: a live-only reattach
                            // cannot gap, so reconnect.
                            continue;
                        }
                        // Filtered/firehose streams have no resume cursor; a
                        // reattach after delivered events would silently gap.
                        // Surface an honest terminal Unavailable instead.
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

/// Boxes a per-workflow stream attaching with an explicit starting cursor.
#[must_use]
pub fn event_stream_from(
    transport: Arc<dyn WorkflowTransport>,
    namespace: impl Into<String>,
    workflow_id: WorkflowId,
    resume_from: NonZeroU64,
) -> EventStream {
    Box::pin(ResumingEventStream::from_sequence(
        transport,
        namespace,
        workflow_id,
        resume_from,
    ))
}

fn is_retryable(error: &ClientError) -> bool {
    matches!(error, ClientError::Unavailable { .. })
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
        /// Attach failures consumed before any queued attempt: each entry is
        /// one subscribe call that fails before a stream exists.
        attach_failures: Mutex<VecDeque<ClientError>>,
        attempts: Mutex<VecDeque<SubscriptionAttempt>>,
        resume_points: Mutex<Vec<Option<u64>>>,
    }

    #[async_trait]
    impl WorkflowTransport for SubscribeStub {
        async fn start_workflow(
            &self,
            _: aion_proto::ProtoStartWorkflowRequest,
        ) -> Result<ProtoStartWorkflowResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn signal(
            &self,
            _: aion_proto::ProtoSignalRequest,
        ) -> Result<ProtoSignalResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn query(
            &self,
            _: aion_proto::ProtoQueryRequest,
        ) -> Result<ProtoQueryResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn cancel(
            &self,
            _: aion_proto::ProtoCancelRequest,
        ) -> Result<ProtoCancelResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn reopen(
            &self,
            _: aion_proto::ProtoReopenRequest,
        ) -> Result<aion_proto::ProtoReopenResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn list_workflows(
            &self,
            _: aion_proto::ProtoListWorkflowsRequest,
        ) -> Result<ProtoListWorkflowsResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn describe_workflow(
            &self,
            _: aion_proto::ProtoDescribeWorkflowRequest,
        ) -> Result<ProtoDescribeWorkflowResponse, ClientError> {
            Err(ClientError::unavailable("stub transport"))
        }

        async fn subscribe(
            &self,
            _: aion_proto::SubscriptionRequest,
            resume_from_sequence: Option<u64>,
        ) -> Result<SubscriptionAttempt, ClientError> {
            self.resume_points.lock().await.push(resume_from_sequence);
            if let Some(failure) = self.attach_failures.lock().await.pop_front() {
                return Err(failure);
            }
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
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
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
                    Err(ClientError::unavailable("transient disconnect")),
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
            let event = item
                .map_err(|e| format!("unexpected stream error: {e}"))
                .ok();
            if let Some(event) = event {
                seqs.push(event.seq());
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
                stream::iter(vec![Err(ClientError::unauthenticated("bad token"))]).boxed(),
            ));
        let mut events =
            ResumingEventStream::new(stub, "tenant-a", SubscribeTarget::Workflow { workflow_id });

        assert_eq!(
            events.next().await,
            Some(Err(ClientError::unauthenticated("bad token")))
        );
        assert_eq!(events.next().await, None);
    }

    #[tokio::test]
    async fn namespace_denied_is_terminal_and_never_retried() {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        let denied =
            ClientError::namespace_denied("namespace tenant-b is not granted to this caller");
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Err(denied.clone())]).boxed(),
            ));
        let mut events = ResumingEventStream::new(
            stub.clone(),
            "tenant-b",
            SubscribeTarget::Workflow { workflow_id },
        );

        assert_eq!(events.next().await, Some(Err(denied)));
        assert_eq!(events.next().await, None);
        assert_eq!(stub.resume_points.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn from_sequence_passes_the_cursor_on_the_initial_attach() {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Ok(event(1, &workflow_id)), Ok(event(2, &workflow_id))]).boxed(),
            ));
        let Some(resume_from) = std::num::NonZeroU64::new(1) else {
            unreachable!("1 is non-zero");
        };
        let mut events = super::ResumingEventStream::from_sequence(
            stub.clone(),
            "tenant-a",
            workflow_id,
            resume_from,
        );

        let mut seqs = Vec::new();
        while let Some(item) = events.next().await {
            if let Ok(event) = item {
                seqs.push(event.seq());
            }
        }

        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(
            *stub.resume_points.lock().await,
            vec![Some(1)],
            "the initial attach must carry the explicit cursor"
        );
    }

    #[tokio::test]
    async fn live_only_streams_reconnect_only_before_any_delivery() {
        // A filtered stream that drops before delivering anything may
        // reattach live-only — nothing can gap yet — and never with a cursor.
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Err(ClientError::unavailable("transient disconnect"))]).boxed(),
            ));
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Ok(event(1, &workflow_id))]).boxed(),
            ));
        let mut events = ResumingEventStream::new(
            stub.clone(),
            "tenant-a",
            SubscribeTarget::Filtered {
                filter: aion_core::WorkflowFilter::default(),
            },
        );

        let mut seqs = Vec::new();
        while let Some(item) = events.next().await {
            if let Ok(event) = item {
                seqs.push(event.seq());
            }
        }

        assert_eq!(seqs, vec![1]);
        assert_eq!(
            *stub.resume_points.lock().await,
            vec![None, None],
            "live-only streams never carry a resume cursor"
        );
    }

    #[tokio::test]
    async fn live_only_disconnect_after_delivery_is_honest_unavailable() {
        // Filtered/firehose streams have no resume cursor: a transient drop
        // after >= 1 delivered event must surface Unavailable, never a silent
        // gapped reattach.
        for target in [
            SubscribeTarget::Filtered {
                filter: aion_core::WorkflowFilter::default(),
            },
            SubscribeTarget::Firehose,
        ] {
            let workflow_id = WorkflowId::new_v4();
            let stub = Arc::new(SubscribeStub::default());
            stub.attempts
                .lock()
                .await
                .push_back(SubscriptionAttempt::new(
                    stream::iter(vec![
                        Ok(event(1, &workflow_id)),
                        Err(ClientError::unavailable("transient disconnect")),
                    ])
                    .boxed(),
                ));
            let mut events = ResumingEventStream::new(stub.clone(), "tenant-a", target);

            let first = events.next().await;
            assert!(matches!(first, Some(Ok(_))), "got {first:?}");
            assert_eq!(
                events.next().await,
                Some(Err(ClientError::unavailable("transient disconnect")))
            );
            assert_eq!(events.next().await, None);
            assert_eq!(
                stub.resume_points.lock().await.len(),
                1,
                "no reattach may follow a post-delivery live-only disconnect"
            );
        }
    }

    #[tokio::test]
    async fn live_only_streams_do_not_dedupe_sequence_numbers_across_workflows() {
        // Per-workflow seq is the only ordering that exists; two workflows
        // legitimately share sequence numbers on a filtered/firehose stream.
        let first_workflow = WorkflowId::new_v4();
        let second_workflow = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![
                    Ok(event(1, &first_workflow)),
                    Ok(event(1, &second_workflow)),
                ])
                .boxed(),
            ));
        let mut events = ResumingEventStream::new(stub, "tenant-a", SubscribeTarget::Firehose);

        let mut delivered = Vec::new();
        while let Some(item) = events.next().await {
            if let Ok(event) = item {
                delivered.push(event.envelope().workflow_id.clone());
            }
        }

        assert_eq!(delivered, vec![first_workflow, second_workflow]);
    }

    #[tokio::test]
    async fn not_found_is_terminal_and_never_retried() {
        // A workflow-level visibility miss surfaces as NotFound (the server's
        // anti-existence-leak contract); like every non-Unavailable error it
        // must end the stream instead of reconnecting forever.
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Err(ClientError::not_found("workflow was not found"))]).boxed(),
            ));
        let mut events = ResumingEventStream::new(
            stub.clone(),
            "tenant-a",
            SubscribeTarget::Workflow { workflow_id },
        );

        assert_eq!(
            events.next().await,
            Some(Err(ClientError::not_found("workflow was not found")))
        );
        assert_eq!(events.next().await, None);
        assert_eq!(stub.resume_points.lock().await.len(), 1);
    }

    /// Connect-failure contract: an `Unavailable` initial attach failure is
    /// retryable exactly like a mid-stream drop — the stream re-attaches and
    /// delivers, never surfacing the transient error as terminal.
    #[tokio::test]
    async fn unavailable_attach_failure_is_retried_until_attach_succeeds() -> Result<(), ClientError>
    {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attach_failures
            .lock()
            .await
            .push_back(ClientError::unavailable("connection refused"));
        stub.attach_failures
            .lock()
            .await
            .push_back(ClientError::unavailable("connection refused"));
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Ok(event(1, &workflow_id)), Ok(event(2, &workflow_id))]).boxed(),
            ));
        let mut events = ResumingEventStream::new(
            stub.clone(),
            "tenant-a",
            SubscribeTarget::Workflow { workflow_id },
        );

        let mut seqs = Vec::new();
        while let Some(item) = events.next().await {
            // A transient attach failure must not surface; `?` fails the test
            // with the offending error if it does.
            seqs.push(item?.seq());
        }

        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(
            *stub.resume_points.lock().await,
            vec![None, None, None],
            "every retried initial attach is still a live tail (no cursor)"
        );
        Ok(())
    }

    /// A mid-stream drop followed by an `Unavailable` reconnect failure keeps
    /// retrying with the SAME cursor until the reconnect succeeds.
    #[tokio::test]
    async fn unavailable_reconnect_failure_retries_with_the_same_cursor() -> Result<(), ClientError>
    {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![
                    Ok(event(1, &workflow_id)),
                    Err(ClientError::unavailable("transient disconnect")),
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
        let first = events.next().await;
        assert!(matches!(first, Some(Ok(_))), "got {first:?}");
        // The reconnect attempt fails transiently, then succeeds.
        stub.attach_failures
            .lock()
            .await
            .push_back(ClientError::unavailable("connection refused"));
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Ok(event(2, &workflow_id))]).boxed(),
            ));

        let mut seqs = vec![1];
        while let Some(item) = events.next().await {
            // A transient reconnect failure must not surface; `?` fails the
            // test with the offending error if it does.
            seqs.push(item?.seq());
        }

        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(
            *stub.resume_points.lock().await,
            vec![None, Some(2), Some(2)],
            "the failed reconnect and the successful retry carry the same cursor"
        );
        Ok(())
    }

    /// Non-`Unavailable` attach failures are terminal immediately: an
    /// `Unauthenticated` connect rejection must never be retried.
    #[tokio::test]
    async fn non_retryable_attach_failure_is_terminal() {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attach_failures
            .lock()
            .await
            .push_back(ClientError::unauthenticated("bad token"));
        let mut events = ResumingEventStream::new(
            stub.clone(),
            "tenant-a",
            SubscribeTarget::Workflow { workflow_id },
        );

        assert_eq!(
            events.next().await,
            Some(Err(ClientError::unauthenticated("bad token")))
        );
        assert_eq!(events.next().await, None);
        assert_eq!(stub.resume_points.lock().await.len(), 1);
    }

    /// Live-only streams also retry `Unavailable` attach failures while
    /// nothing has been delivered (a live-only reattach cannot gap yet).
    #[tokio::test]
    async fn live_only_unavailable_attach_failure_is_retried_before_any_delivery() {
        let workflow_id = WorkflowId::new_v4();
        let stub = Arc::new(SubscribeStub::default());
        stub.attach_failures
            .lock()
            .await
            .push_back(ClientError::unavailable("connection refused"));
        stub.attempts
            .lock()
            .await
            .push_back(SubscriptionAttempt::new(
                stream::iter(vec![Ok(event(1, &workflow_id))]).boxed(),
            ));
        let mut events =
            ResumingEventStream::new(stub.clone(), "tenant-a", SubscribeTarget::Firehose);

        let mut seqs = Vec::new();
        while let Some(item) = events.next().await {
            if let Ok(event) = item {
                seqs.push(event.seq());
            }
        }

        assert_eq!(seqs, vec![1]);
        assert_eq!(*stub.resume_points.lock().await, vec![None, None]);
    }
}
