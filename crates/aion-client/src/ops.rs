//! start/signal/query/cancel/list/describe over the transport.

use std::num::NonZeroU64;
use std::time::Duration;

use aion_core::{
    Event, Payload, RunId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary,
};
use aion_proto::{
    ProtoCancelRequest, ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest, ProtoPauseRequest,
    ProtoPayload, ProtoQueryRequest, ProtoReopenRequest, ProtoResumeRequest, ProtoRunId,
    ProtoSignalRequest, ProtoStartWorkflowRequest, ProtoWorkflowId, ProtoWorkflowStatus, WireError,
    decode_core_value, decode_event, decode_workflow_summary, encode_core_value,
    proto_query_response,
};
use aion_store::visibility::ListWorkflowsFilter;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::client::Client;
use crate::error::ClientError;
use crate::handle::WorkflowHandle;
use crate::payload::{from_payload, to_payload};
use crate::stream::{EventStream, SubscribeTarget, event_stream, event_stream_from};

/// Options accepted by [`Client::start`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartOptions {
    /// Namespace override for this start request.
    pub namespace: Option<String>,
    /// Caller-supplied idempotency key for safe local retry replay.
    ///
    /// The current AW protobuf has not added an idempotency field yet, so this is
    /// enforced at the SDK boundary without inventing a client-owned wire field.
    /// Reusing a key for a different start request returns
    /// [`ClientError::AlreadyExists`].
    pub idempotency_key: Option<String>,
    /// R-4 steered-start routing key. When set, the cluster steers this start to
    /// `shard_for(routing_key)`'s owner (forwarding there when the dialed node is
    /// not the owner). `None` keeps the default unsteered placement.
    pub routing_key: Option<String>,
    /// Optional default task queue for this workflow's activities. When set, the
    /// server records it durably on the start (the namespace × `task_queue`
    /// targeting story); `None` keeps the namespace's default queue.
    pub task_queue: Option<String>,
}

/// Pagination options accepted by [`Client::list`].
///
/// The current AW protobuf carries `request_id` through the filter envelope,
/// but not `limit` or `cursor`; populated `limit`/`cursor` values return
/// [`ClientError::InvalidArgument`] instead of being silently ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListPage {
    /// Caller request identifier carried in the current filter envelope.
    pub request_id: Option<String>,
    /// Requested page size reserved by the contract.
    pub limit: Option<usize>,
    /// Continuation cursor reserved by the contract.
    pub cursor: Option<String>,
}

/// Workflow detail returned by [`Client::describe`].
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowDescription {
    /// Lightweight workflow summary reused from `aion-core`.
    pub summary: WorkflowSummary,
    /// Optional event history when the server includes it.
    pub history: Vec<Event>,
}

/// Outcome of [`Client::reopen`]: the reopened run and its projected status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReopenOutcome {
    /// The reopened concrete run identifier (now live again).
    pub run_id: RunId,
    /// The projected status after the reopen (Running).
    pub status: WorkflowStatus,
}

/// Outcome of [`Client::pause`]: the paused run and its projected status (#204).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PauseOutcome {
    /// The paused concrete run identifier.
    pub run_id: RunId,
    /// The projected status after the pause (Paused).
    pub status: WorkflowStatus,
}

/// Outcome of [`Client::resume`]: the resumed run and its projected status (#204).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeOutcome {
    /// The resumed concrete run identifier (now live again).
    pub run_id: RunId,
    /// The projected status after the resume (Running).
    pub status: WorkflowStatus,
}

impl Client {
    /// Starts a workflow and returns the assigned workflow and run identifiers.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or response conversion fails.
    pub async fn start(
        &self,
        workflow_type: impl Into<String>,
        input: Payload,
        opts: StartOptions,
    ) -> Result<WorkflowHandle, ClientError> {
        validate_start_options(&opts)?;
        let idempotency_key = opts.idempotency_key.clone();
        let routing_key = opts.routing_key.clone();
        let task_queue = opts.task_queue.clone();
        let namespace = operation_namespace(self, opts.namespace);
        let workflow_type = workflow_type.into();
        let fingerprint = idempotency_key.as_ref().map(|key| {
            StartFingerprint::new(
                namespace.clone(),
                workflow_type.clone(),
                &input,
                key.clone(),
            )
        });
        if let Some(fingerprint) = &fingerprint {
            if let Some(handle) = self.cached_start(fingerprint).await? {
                return Ok(handle);
            }
        }
        let response = self
            .transport
            .start_workflow(ProtoStartWorkflowRequest {
                namespace,
                workflow_type,
                input: Some(ProtoPayload::from(input)),
                routing_key,
                task_queue,
            })
            .await?;
        let workflow_id = decode_required_workflow_id(response.workflow_id, "start response")?;
        let run_id = decode_required_run_id(response.run_id, "start response")?;
        let handle = WorkflowHandle::from_ids(self.clone(), workflow_id, run_id);
        if let Some(fingerprint) = fingerprint {
            self.record_start(fingerprint, handle.clone()).await?;
        }
        Ok(handle)
    }

    /// Starts a workflow after serializing `input` as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidArgument`] when serialization fails, or the
    /// delegated start error otherwise.
    pub async fn start_typed<T>(
        &self,
        workflow_type: impl Into<String>,
        input: &T,
        opts: StartOptions,
    ) -> Result<WorkflowHandle, ClientError>
    where
        T: Serialize + ?Sized,
    {
        self.start(workflow_type, to_payload(input)?, opts).await
    }

    /// Sends a signal to the latest run, or to `run_id` when supplied.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or request conversion fails.
    pub async fn signal(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
        name: impl Into<String>,
        payload: Payload,
    ) -> Result<(), ClientError> {
        self.transport
            .signal(ProtoSignalRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
                signal_name: name.into(),
                payload: Some(ProtoPayload::from(payload)),
            })
            .await?;
        Ok(())
    }

    /// Serializes `value` as JSON and sends it as a signal payload.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidArgument`] when serialization fails, or the
    /// delegated signal error otherwise.
    pub async fn signal_typed<T>(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
        name: impl Into<String>,
        value: &T,
    ) -> Result<(), ClientError>
    where
        T: Serialize + ?Sized,
    {
        self.signal(workflow_id, run_id, name, to_payload(value)?)
            .await
    }

    /// Queries the latest run, or `run_id` when supplied, with a local deadline.
    ///
    /// The current AW protobuf does not yet carry query argument payloads, so a
    /// non-empty `args` payload returns [`ClientError::InvalidArgument`] instead
    /// of being silently dropped.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::QueryTimeout`] when `deadline` elapses.
    pub async fn query(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
        name: impl Into<String>,
        args: Payload,
        deadline: Duration,
    ) -> Result<Payload, ClientError> {
        validate_query_args(&args)?;
        let response = tokio::time::timeout(
            deadline,
            self.transport.query(ProtoQueryRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
                query_name: name.into(),
            }),
        )
        .await
        .map_err(|_| {
            ClientError::query_timeout(format!(
                "query deadline of {deadline:?} elapsed before the server replied"
            ))
        })??;

        match response.outcome {
            Some(proto_query_response::Outcome::Result(payload)) => {
                Payload::try_from(payload).map_err(ClientError::from_wire_error)
            }
            Some(proto_query_response::Outcome::Error(error)) => Err(query_error(error)),
            None => Err(ClientError::server("query response outcome is missing")),
        }
    }

    /// Serializes `args` as JSON, queries a workflow, and deserializes the JSON result.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidArgument`] when serialization or result
    /// decoding fails, or the delegated query error otherwise.
    pub async fn query_typed<A, R>(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
        name: impl Into<String>,
        args: &A,
        deadline: Duration,
    ) -> Result<R, ClientError>
    where
        A: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let payload = self
            .query(
                workflow_id,
                run_id,
                name,
                query_args_payload(args)?,
                deadline,
            )
            .await?;
        from_payload(&payload)
    }

    /// Requests cancellation of the latest run, or `run_id` when supplied.
    ///
    /// Success means the server accepted the cancellation request; it is not a
    /// confirmation that the workflow has reached a terminal cancelled state.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or request conversion fails.
    pub async fn cancel(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
        reason: impl Into<String>,
    ) -> Result<(), ClientError> {
        self.transport
            .cancel(ProtoCancelRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
                reason: reason.into(),
            })
            .await?;
        Ok(())
    }

    /// Reopens a terminal-reopenable run (Failed or Cancelled), re-driving it
    /// from where it left off. Targets the latest run, or `run_id` when supplied.
    ///
    /// Returns the reopened run and its projected status (Running). A run that is
    /// not a reopenable terminal (not terminal, terminal for a non-reopenable
    /// reason, or already Running) returns [`ClientError::InvalidState`]; an
    /// absent workflow returns [`ClientError::NotFound`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or response conversion fails.
    pub async fn reopen(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
    ) -> Result<ReopenOutcome, ClientError> {
        let response = self
            .transport
            .reopen(ProtoReopenRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
            })
            .await?;
        let run_id = response
            .run_id
            .ok_or_else(|| ClientError::server("reopen response run id is missing"))?
            .try_into()
            .map_err(ClientError::from_wire_error)?;
        let status = ProtoWorkflowStatus::try_from(response.status)
            .map_err(|_error| ClientError::server("reopen response status is unknown"))
            .and_then(|status| {
                WorkflowStatus::try_from(status).map_err(ClientError::from_wire_error)
            })?;
        Ok(ReopenOutcome { run_id, status })
    }

    /// Pauses a live `Running` run, durably holding new activity dispatch (#204).
    /// Targets the latest run, or `run_id` when supplied. Returns the run and its
    /// projected status (Paused). A run that is not `Running` returns
    /// [`ClientError::InvalidState`]; an absent workflow returns
    /// [`ClientError::NotFound`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or response conversion fails.
    pub async fn pause(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
        reason: impl Into<String>,
    ) -> Result<PauseOutcome, ClientError> {
        let response = self
            .transport
            .pause(ProtoPauseRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
                reason: reason.into(),
            })
            .await?;
        let run_id = response
            .run_id
            .ok_or_else(|| ClientError::server("pause response run id is missing"))?
            .try_into()
            .map_err(ClientError::from_wire_error)?;
        let status = ProtoWorkflowStatus::try_from(response.status)
            .map_err(|_error| ClientError::server("pause response status is unknown"))
            .and_then(|status| {
                WorkflowStatus::try_from(status).map_err(ClientError::from_wire_error)
            })?;
        Ok(PauseOutcome { run_id, status })
    }

    /// Resumes a `Paused` run, releasing the dispatch hold (#204). Targets the
    /// latest run, or `run_id` when supplied. Returns the run and its projected
    /// status (Running). A run that is not `Paused` returns
    /// [`ClientError::InvalidState`]; an absent workflow returns
    /// [`ClientError::NotFound`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or response conversion fails.
    pub async fn resume(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
    ) -> Result<ResumeOutcome, ClientError> {
        let response = self
            .transport
            .resume(ProtoResumeRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
            })
            .await?;
        let run_id = response
            .run_id
            .ok_or_else(|| ClientError::server("resume response run id is missing"))?
            .try_into()
            .map_err(ClientError::from_wire_error)?;
        let status = ProtoWorkflowStatus::try_from(response.status)
            .map_err(|_error| ClientError::server("resume response status is unknown"))
            .and_then(|status| {
                WorkflowStatus::try_from(status).map_err(ClientError::from_wire_error)
            })?;
        Ok(ResumeOutcome { run_id, status })
    }

    /// Lists workflows matching a filter.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or response conversion fails.
    pub async fn list(
        &self,
        filter: &WorkflowFilter,
        page: ListPage,
    ) -> Result<Vec<WorkflowSummary>, ClientError> {
        validate_list_page(&page)?;
        let namespace = self.namespace().to_owned();
        let filter = workflow_filter_to_visibility(filter)?;
        let filter = encode_core_value(namespace.clone(), page.request_id, &filter)
            .map_err(ClientError::from_wire_error)?;
        let response = self
            .transport
            .list_workflows(ProtoListWorkflowsRequest {
                namespace,
                filter: Some(filter),
            })
            .await?;

        response
            .summaries
            .iter()
            .map(decode_visibility_summary)
            .map(|result| result.map_err(ClientError::from_wire_error))
            .collect()
    }

    /// Describes the latest run, or `run_id` when supplied.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when transport, server, or response conversion fails.
    pub async fn describe(
        &self,
        workflow_id: &WorkflowId,
        run_id: Option<&RunId>,
    ) -> Result<WorkflowDescription, ClientError> {
        let response = self
            .transport
            .describe_workflow(ProtoDescribeWorkflowRequest {
                namespace: self.namespace().to_owned(),
                workflow_id: Some(ProtoWorkflowId::from(workflow_id.clone())),
                run_id: run_id.cloned().map(ProtoRunId::from),
                include_history: true,
            })
            .await?;
        let summary = response
            .summary
            .as_ref()
            .ok_or_else(|| ClientError::server("describe response summary is missing"))
            .and_then(|summary| {
                decode_workflow_summary(summary).map_err(ClientError::from_wire_error)
            })?;
        let history = response
            .history
            .iter()
            .map(decode_event)
            .map(|result| result.map_err(ClientError::from_wire_error))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WorkflowDescription { summary, history })
    }

    /// Subscribes to events for a workflow.
    #[must_use]
    pub fn subscribe_workflow(&self, workflow_id: &WorkflowId) -> EventStream {
        event_stream(
            self.transport.clone(),
            self.namespace().to_owned(),
            SubscribeTarget::Workflow {
                workflow_id: workflow_id.clone(),
            },
        )
    }

    /// Subscribes to events for a workflow, attaching from an explicit
    /// per-workflow sequence cursor.
    ///
    /// `resume_from` is the first sequence number wanted (`resume_from_seq`
    /// on the wire); `1` replays the workflow's full recorded history before
    /// splicing into the live stream, gap-free and duplicate-free.
    #[must_use]
    pub fn subscribe_workflow_from(
        &self,
        workflow_id: &WorkflowId,
        resume_from: NonZeroU64,
    ) -> EventStream {
        event_stream_from(
            self.transport.clone(),
            self.namespace().to_owned(),
            workflow_id.clone(),
            resume_from,
        )
    }

    /// Subscribes to events selected by the supplied workflow filter.
    #[must_use]
    pub fn subscribe(&self, filter: WorkflowFilter) -> EventStream {
        event_stream(
            self.transport.clone(),
            self.namespace().to_owned(),
            SubscribeTarget::Filtered { filter },
        )
    }

    /// Subscribes to every event visible to this client namespace.
    #[must_use]
    pub fn subscribe_firehose(&self) -> EventStream {
        event_stream(
            self.transport.clone(),
            self.namespace().to_owned(),
            SubscribeTarget::Firehose,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartFingerprint {
    namespace: String,
    workflow_type: String,
    content_type: aion_core::ContentType,
    bytes: Vec<u8>,
    idempotency_key: String,
}

impl StartFingerprint {
    fn new(
        namespace: String,
        workflow_type: String,
        input: &Payload,
        idempotency_key: String,
    ) -> Self {
        Self {
            namespace,
            workflow_type,
            content_type: input.content_type().clone(),
            bytes: input.bytes().to_vec(),
            idempotency_key,
        }
    }

    pub(crate) fn key(&self) -> &str {
        &self.idempotency_key
    }
}

fn operation_namespace(client: &Client, namespace: Option<String>) -> String {
    namespace.unwrap_or_else(|| client.namespace().to_owned())
}

fn validate_start_options(opts: &StartOptions) -> Result<(), ClientError> {
    if opts
        .idempotency_key
        .as_ref()
        .is_some_and(std::string::String::is_empty)
    {
        return Err(ClientError::invalid_argument(
            "idempotency_key must not be empty",
        ));
    }
    Ok(())
}

fn validate_query_args(args: &Payload) -> Result<(), ClientError> {
    if !args.bytes().is_empty() {
        return Err(ClientError::invalid_argument(
            "query arguments are not carried by the current wire contract; \
             pass an empty payload",
        ));
    }
    Ok(())
}

fn query_args_payload<T>(args: &T) -> Result<Payload, ClientError>
where
    T: Serialize + ?Sized,
{
    let payload = to_payload(args)?;
    if payload.bytes() == b"null" {
        Ok(Payload::new(payload.content_type().clone(), Vec::new()))
    } else {
        Ok(payload)
    }
}

fn validate_list_page(page: &ListPage) -> Result<(), ClientError> {
    if page.limit.is_some() || page.cursor.is_some() {
        return Err(ClientError::invalid_argument(
            "list pagination limit/cursor are reserved by the contract and \
             not yet carried by the wire",
        ));
    }
    Ok(())
}

fn workflow_filter_to_visibility(
    filter: &WorkflowFilter,
) -> Result<ListWorkflowsFilter, ClientError> {
    if filter.parent.is_some() {
        return Err(ClientError::invalid_argument(
            "parent workflow filters are not carried by the visibility wire contract",
        ));
    }

    Ok(ListWorkflowsFilter {
        workflow_type: filter.workflow_type.clone(),
        status: filter.status,
        started_after: filter.started_after,
        started_before: filter.started_before,
        ..ListWorkflowsFilter::default()
    })
}

fn decode_visibility_summary(
    envelope: &aion_proto::WireEnvelope,
) -> Result<WorkflowSummary, WireError> {
    let summary = decode_core_value::<aion_store::visibility::WorkflowSummary>(envelope)?;
    Ok(WorkflowSummary {
        workflow_id: summary.workflow_id,
        workflow_type: summary.workflow_type,
        status: summary.status,
        started_at: summary.start_time,
        ended_at: summary.close_time,
        parent: None,
        failed_step: summary.failed_step,
        failure_reason: summary.failure_reason,
    })
}

fn decode_required_workflow_id(
    value: Option<ProtoWorkflowId>,
    context: &str,
) -> Result<WorkflowId, ClientError> {
    value
        .ok_or_else(|| ClientError::server(format!("{context} workflow id is missing")))?
        .try_into()
        .map_err(ClientError::from_wire_error)
}

fn decode_required_run_id(value: Option<ProtoRunId>, context: &str) -> Result<RunId, ClientError> {
    value
        .ok_or_else(|| ClientError::server(format!("{context} run id is missing")))?
        .try_into()
        .map_err(ClientError::from_wire_error)
}

/// Maps a `QueryResponse.error` payload through the shared wire taxonomy.
///
/// The server reports query-handler application failures with the dedicated
/// `query_failed` wire code, so the shared map yields [`ClientError::QueryFailed`]
/// directly; `backend` stays an unexpected server fault.
fn query_error(error: aion_proto::ProtoWireError) -> ClientError {
    ClientError::from_proto_wire_error(error)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::{ContentType, Payload, WorkflowFilter, WorkflowId, WorkflowStatus};
    use aion_proto::{
        ProtoCancelResponse, ProtoDescribeWorkflowResponse, ProtoListWorkflowsResponse,
        ProtoQueryResponse, ProtoReopenResponse, ProtoRunId, ProtoSignalResponse,
        ProtoStartWorkflowResponse, ProtoWorkflowId, ProtoWorkflowStatus, WireError,
        encode_core_value, encode_workflow_summary, proto_query_response,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use futures::StreamExt;
    use futures::stream;
    use tokio::sync::Mutex;

    use super::{ListPage, StartOptions};
    use crate::client::{Client, ClientBuilder, ClientConfig};
    use crate::error::ClientError;
    use crate::transport::{SubscriptionAttempt, WorkflowTransport};

    #[derive(Default)]
    struct StubTransport {
        last_start: Mutex<Option<aion_proto::ProtoStartWorkflowRequest>>,
        last_signal: Mutex<Option<aion_proto::ProtoSignalRequest>>,
        last_query: Mutex<Option<aion_proto::ProtoQueryRequest>>,
        last_cancel: Mutex<Option<aion_proto::ProtoCancelRequest>>,
        last_reopen: Mutex<Option<aion_proto::ProtoReopenRequest>>,
        last_list: Mutex<Option<aion_proto::ProtoListWorkflowsRequest>>,
        last_describe: Mutex<Option<aion_proto::ProtoDescribeWorkflowRequest>>,
        start_error: Mutex<Option<ClientError>>,
        signal_error: Mutex<Option<ClientError>>,
        query_response: Mutex<Option<Result<ProtoQueryResponse, ClientError>>>,
        reopen_response: Mutex<Option<Result<ProtoReopenResponse, ClientError>>>,
    }

    #[async_trait]
    impl WorkflowTransport for StubTransport {
        async fn start_workflow(
            &self,
            request: aion_proto::ProtoStartWorkflowRequest,
        ) -> Result<ProtoStartWorkflowResponse, ClientError> {
            *self.last_start.lock().await = Some(request);
            if let Some(error) = self.start_error.lock().await.take() {
                return Err(error);
            }
            Ok(ProtoStartWorkflowResponse {
                workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
                run_id: Some(ProtoRunId::from(run_id())),
            })
        }

        async fn signal(
            &self,
            request: aion_proto::ProtoSignalRequest,
        ) -> Result<ProtoSignalResponse, ClientError> {
            *self.last_signal.lock().await = Some(request);
            if let Some(error) = self.signal_error.lock().await.take() {
                return Err(error);
            }
            Ok(ProtoSignalResponse {})
        }

        async fn query(
            &self,
            request: aion_proto::ProtoQueryRequest,
        ) -> Result<ProtoQueryResponse, ClientError> {
            *self.last_query.lock().await = Some(request);
            if let Some(response) = self.query_response.lock().await.take() {
                return response;
            }
            Ok(ProtoQueryResponse {
                outcome: Some(proto_query_response::Outcome::Result(
                    aion_proto::ProtoPayload::from(payload("result")),
                )),
            })
        }

        async fn cancel(
            &self,
            request: aion_proto::ProtoCancelRequest,
        ) -> Result<ProtoCancelResponse, ClientError> {
            *self.last_cancel.lock().await = Some(request);
            Ok(ProtoCancelResponse {})
        }

        async fn reopen(
            &self,
            request: aion_proto::ProtoReopenRequest,
        ) -> Result<ProtoReopenResponse, ClientError> {
            *self.last_reopen.lock().await = Some(request);
            if let Some(response) = self.reopen_response.lock().await.take() {
                return response;
            }
            Ok(ProtoReopenResponse {
                run_id: Some(ProtoRunId::from(run_id())),
                status: ProtoWorkflowStatus::Running as i32,
            })
        }

        async fn pause(
            &self,
            _request: aion_proto::ProtoPauseRequest,
        ) -> Result<aion_proto::ProtoPauseResponse, ClientError> {
            Ok(aion_proto::ProtoPauseResponse {
                run_id: Some(ProtoRunId::from(run_id())),
                status: ProtoWorkflowStatus::Paused as i32,
            })
        }

        async fn resume(
            &self,
            _request: aion_proto::ProtoResumeRequest,
        ) -> Result<aion_proto::ProtoResumeResponse, ClientError> {
            Ok(aion_proto::ProtoResumeResponse {
                run_id: Some(ProtoRunId::from(run_id())),
                status: ProtoWorkflowStatus::Running as i32,
            })
        }

        async fn list_workflows(
            &self,
            request: aion_proto::ProtoListWorkflowsRequest,
        ) -> Result<ProtoListWorkflowsResponse, ClientError> {
            *self.last_list.lock().await = Some(request);
            Ok(ProtoListWorkflowsResponse {
                summaries: vec![
                    encode_core_value("tenant-a", None, &visibility_summary())
                        .map_err(ClientError::from_wire_error)?,
                ],
            })
        }

        async fn describe_workflow(
            &self,
            request: aion_proto::ProtoDescribeWorkflowRequest,
        ) -> Result<ProtoDescribeWorkflowResponse, ClientError> {
            *self.last_describe.lock().await = Some(request);
            Ok(ProtoDescribeWorkflowResponse {
                summary: Some(
                    encode_workflow_summary("tenant-a", None, &summary())
                        .map_err(ClientError::from_wire_error)?,
                ),
                history: Vec::new(),
            })
        }

        async fn subscribe(
            &self,
            _: aion_proto::SubscriptionRequest,
            _: Option<u64>,
        ) -> Result<SubscriptionAttempt, ClientError> {
            Ok(SubscriptionAttempt::new(stream::empty().boxed()))
        }
    }

    fn client_with(stub: Arc<StubTransport>) -> Client {
        Client::from_transport(
            ClientConfig::from(
                ClientBuilder::new("http://localhost:50051").with_namespace("tenant-a"),
            ),
            stub,
        )
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new_v4()
    }

    fn run_id() -> aion_core::RunId {
        aion_core::RunId::new_v4()
    }

    fn payload(label: &str) -> Payload {
        Payload::new(
            ContentType::Json,
            format!("{{\"label\":\"{label}\"}}").into_bytes(),
        )
    }

    fn empty_payload() -> Payload {
        Payload::new(ContentType::Json, Vec::new())
    }

    fn summary() -> aion_core::WorkflowSummary {
        aion_core::WorkflowSummary {
            workflow_id: workflow_id(),
            workflow_type: String::from("checkout"),
            status: WorkflowStatus::Running,
            started_at: Utc::now(),
            ended_at: None,
            parent: None,
            failed_step: None,
            failure_reason: None,
        }
    }

    fn visibility_summary() -> aion_store::visibility::WorkflowSummary {
        aion_store::visibility::WorkflowSummary {
            workflow_id: workflow_id(),
            run_id: run_id(),
            workflow_type: String::from("checkout"),
            status: WorkflowStatus::Running,
            start_time: Utc::now(),
            close_time: None,
            failed_step: None,
            failure_reason: None,
            search_attributes: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn start_maps_request_and_returns_handle() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));

        let result = client
            .start("checkout", payload("input"), StartOptions::default())
            .await?;

        let recorded = stub.last_start.lock().await.clone();
        assert!(recorded.is_some());
        let request = recorded.ok_or_else(|| ClientError::server("missing recorded start"))?;
        assert_eq!(request.namespace, "tenant-a");
        assert_eq!(request.workflow_type, "checkout");
        assert!(request.input.is_some());
        assert_ne!(result.workflow_id(), &WorkflowId::new(uuid::Uuid::nil()));
        Ok(())
    }

    #[tokio::test]
    async fn start_idempotency_replays_identical_and_rejects_conflicts() -> Result<(), ClientError>
    {
        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));
        let opts = StartOptions {
            namespace: None,
            idempotency_key: Some(String::from("retry-key")),
            routing_key: None,
            task_queue: None,
        };

        let original = client
            .start("checkout", payload("input"), opts.clone())
            .await?;
        let replayed = client
            .start("checkout", payload("input"), opts.clone())
            .await?;
        let conflict = client.start("checkout", payload("other"), opts).await;

        assert_eq!(replayed, original);
        assert!(
            matches!(conflict, Err(ClientError::AlreadyExists { .. })),
            "got {conflict:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn signal_maps_latest_run_and_error() {
        let stub = Arc::new(StubTransport::default());
        *stub.signal_error.lock().await = Some(ClientError::not_found("workflow was not found"));
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();

        let result = client.signal(&id, None, "approve", payload("signal")).await;

        assert_eq!(
            result,
            Err(ClientError::not_found("workflow was not found"))
        );
        let recorded = stub.last_signal.lock().await.clone();
        assert!(recorded.is_some());
        let Some(request) = recorded else {
            return;
        };
        assert!(request.run_id.is_none());
    }

    #[tokio::test]
    async fn query_maps_result_error_and_deadline() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        *stub.query_response.lock().await = Some(Ok(ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Error(
                aion_proto::ProtoWireError::from(WireError::query_timeout("slow")),
            )),
        }));
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();

        let result = client
            .query(
                &id,
                Some(&run_id()),
                "state",
                empty_payload(),
                Duration::from_secs(1),
            )
            .await;
        let unsupported_args = client
            .query(&id, None, "state", payload("args"), Duration::from_secs(1))
            .await;

        assert_eq!(result, Err(ClientError::query_timeout("slow")));
        assert!(
            matches!(unsupported_args, Err(ClientError::InvalidArgument { .. })),
            "got {unsupported_args:?}"
        );
        let recorded = stub.last_query.lock().await.clone();
        assert!(recorded.is_some());
        let request = recorded.ok_or_else(|| ClientError::server("missing query"))?;
        assert!(request.run_id.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn query_failed_outcome_error_maps_to_query_failed() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        *stub.query_response.lock().await = Some(Ok(ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Error(
                aion_proto::ProtoWireError::from(WireError::query_failed("handler raised")),
            )),
        }));
        let client = client_with(Arc::clone(&stub));

        let result = client
            .query(
                &workflow_id(),
                Some(&run_id()),
                "state",
                empty_payload(),
                Duration::from_secs(1),
            )
            .await;

        assert_eq!(result, Err(ClientError::query_failed("handler raised")));
        Ok(())
    }

    #[tokio::test]
    async fn backend_outcome_error_is_a_server_fault_not_query_failed() -> Result<(), ClientError> {
        // `backend` in QueryResponse.error is an unexpected server fault; the
        // application-level handler failure has its own `query_failed` code.
        let stub = Arc::new(StubTransport::default());
        *stub.query_response.lock().await = Some(Ok(ProtoQueryResponse {
            outcome: Some(proto_query_response::Outcome::Error(
                aion_proto::ProtoWireError::from(WireError::backend("store down")),
            )),
        }));
        let client = client_with(Arc::clone(&stub));

        let result = client
            .query(
                &workflow_id(),
                Some(&run_id()),
                "state",
                empty_payload(),
                Duration::from_secs(1),
            )
            .await;

        assert_eq!(result, Err(ClientError::server("store down")));
        Ok(())
    }

    #[tokio::test]
    async fn query_typed_decodes_no_arg_query_result() -> Result<(), ClientError> {
        #[derive(serde::Deserialize, PartialEq, Eq, Debug)]
        struct QueryResult {
            label: String,
        }

        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();

        let result: QueryResult = client
            .query_typed(&id, Some(&run_id()), "state", &(), Duration::from_secs(1))
            .await?;

        assert_eq!(
            result,
            QueryResult {
                label: String::from("result")
            }
        );
        assert!(stub.last_query.lock().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn query_typed_rejects_non_empty_args_without_silent_drop() {
        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();

        let result = client
            .query_typed::<_, serde_json::Value>(
                &id,
                Some(&run_id()),
                "state",
                &serde_json::json!({ "filter": "open" }),
                Duration::from_secs(1),
            )
            .await;

        assert!(
            matches!(result, Err(ClientError::InvalidArgument { .. })),
            "got {result:?}"
        );
        assert!(stub.last_query.lock().await.is_none());
    }

    #[tokio::test]
    async fn cancel_list_and_describe_map_requests() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();
        let run = run_id();

        client.cancel(&id, Some(&run), "not needed").await?;
        let listed = client
            .list(&WorkflowFilter::default(), ListPage::default())
            .await?;
        let described = client.describe(&id, None).await?;

        assert!(stub.last_cancel.lock().await.is_some());
        assert!(stub.last_list.lock().await.is_some());
        let describe = stub
            .last_describe
            .lock()
            .await
            .clone()
            .ok_or_else(|| ClientError::server("missing describe"))?;
        assert!(describe.run_id.is_none());
        assert!(describe.include_history);
        assert_eq!(listed.len(), 1);
        assert_eq!(described.history.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn reopen_returns_running_run_and_maps_request() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();
        let run = run_id();

        let outcome = client.reopen(&id, Some(&run)).await?;

        assert_eq!(outcome.status, WorkflowStatus::Running);
        let request = stub
            .last_reopen
            .lock()
            .await
            .clone()
            .ok_or_else(|| ClientError::server("missing reopen"))?;
        assert_eq!(request.namespace, "tenant-a");
        assert!(request.run_id.is_some());
        Ok(())
    }

    /// The `InvalidState` wire code maps to the distinct typed
    /// [`ClientError::InvalidState`], never conflated with not-found.
    #[tokio::test]
    async fn reopen_maps_invalid_state_to_distinct_typed_error() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        *stub.reopen_response.lock().await = Some(Err(ClientError::from_wire_error(
            WireError::invalid_state_with_type("InvalidState", "run is not reopenable"),
        )));
        let client = client_with(Arc::clone(&stub));

        let result = client.reopen(&workflow_id(), None).await;

        assert!(
            matches!(result, Err(ClientError::InvalidState { .. })),
            "got {result:?}"
        );
        Ok(())
    }
}
