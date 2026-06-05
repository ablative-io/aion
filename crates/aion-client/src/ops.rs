//! start/signal/query/cancel/list/describe over the transport.

use std::time::Duration;

use aion_core::{Event, Payload, RunId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_proto::{
    ProtoCancelRequest, ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest, ProtoPayload,
    ProtoQueryRequest, ProtoRunId, ProtoSignalRequest, ProtoStartWorkflowRequest, ProtoWorkflowId,
    WireError, WireErrorCode, decode_event, decode_workflow_summary, encode_workflow_filter,
    proto_query_response,
};

use crate::client::Client;
use crate::error::ClientError;

/// Options accepted by [`Client::start`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartOptions {
    /// Namespace override for this start request.
    pub namespace: Option<String>,
    /// Caller-supplied idempotency key reserved by the contract.
    ///
    /// The current AW protobuf has not added this field yet, so AL-002 rejects a
    /// populated key with [`ClientError::InvalidArgument`] instead of inventing a
    /// client-owned wire field or silently dropping retry-safety semantics.
    pub idempotency_key: Option<String>,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowDescription {
    /// Lightweight workflow summary reused from `aion-core`.
    pub summary: WorkflowSummary,
    /// Optional event history when the server includes it.
    pub history: Vec<Event>,
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
    ) -> Result<(WorkflowId, RunId), ClientError> {
        validate_start_options(&opts)?;
        let namespace = operation_namespace(self, opts.namespace);
        let response = self
            .transport
            .start_workflow(ProtoStartWorkflowRequest {
                namespace,
                workflow_type: workflow_type.into(),
                input: Some(ProtoPayload::from(input)),
            })
            .await?;
        let workflow_id = decode_required_workflow_id(response.workflow_id, "start response")?;
        let run_id = decode_required_run_id(response.run_id, "start response")?;
        Ok((workflow_id, run_id))
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
        .map_err(|_| ClientError::QueryTimeout)??;

        match response.outcome {
            Some(proto_query_response::Outcome::Result(payload)) => {
                Payload::try_from(payload).map_err(ClientError::from_wire_error)
            }
            Some(proto_query_response::Outcome::Error(error)) => Err(query_error(error)),
            None => Err(ClientError::server("query response outcome is missing")),
        }
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
        let filter = encode_workflow_filter(namespace.clone(), page.request_id, filter)
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
            .map(decode_workflow_summary)
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
}

fn operation_namespace(client: &Client, namespace: Option<String>) -> String {
    namespace.unwrap_or_else(|| client.namespace().to_owned())
}

fn validate_start_options(opts: &StartOptions) -> Result<(), ClientError> {
    if opts.idempotency_key.is_some() {
        return Err(ClientError::InvalidArgument);
    }
    Ok(())
}

fn validate_query_args(args: &Payload) -> Result<(), ClientError> {
    if !args.bytes().is_empty() {
        return Err(ClientError::InvalidArgument);
    }
    Ok(())
}

fn validate_list_page(page: &ListPage) -> Result<(), ClientError> {
    if page.limit.is_some() || page.cursor.is_some() {
        return Err(ClientError::InvalidArgument);
    }
    Ok(())
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

fn query_error(error: aion_proto::ProtoWireError) -> ClientError {
    match WireError::try_from(error) {
        Ok(error) if error.code == WireErrorCode::Backend => ClientError::QueryFailed,
        Ok(error) | Err(error) => ClientError::from_wire_error(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::{ContentType, Payload, WorkflowFilter, WorkflowId, WorkflowStatus};
    use aion_proto::{
        ProtoCancelResponse, ProtoDescribeWorkflowResponse, ProtoListWorkflowsResponse,
        ProtoQueryResponse, ProtoRunId, ProtoSignalResponse, ProtoStartWorkflowResponse,
        ProtoWorkflowId, WireError, encode_workflow_summary, proto_query_response,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use tokio::sync::Mutex;

    use super::{ListPage, StartOptions};
    use crate::client::{Client, ClientBuilder, ClientConfig};
    use crate::error::ClientError;
    use crate::transport::WorkflowTransport;

    #[derive(Default)]
    struct StubTransport {
        last_start: Mutex<Option<aion_proto::ProtoStartWorkflowRequest>>,
        last_signal: Mutex<Option<aion_proto::ProtoSignalRequest>>,
        last_query: Mutex<Option<aion_proto::ProtoQueryRequest>>,
        last_cancel: Mutex<Option<aion_proto::ProtoCancelRequest>>,
        last_list: Mutex<Option<aion_proto::ProtoListWorkflowsRequest>>,
        last_describe: Mutex<Option<aion_proto::ProtoDescribeWorkflowRequest>>,
        start_error: Mutex<Option<ClientError>>,
        signal_error: Mutex<Option<ClientError>>,
        query_response: Mutex<Option<Result<ProtoQueryResponse, ClientError>>>,
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

        async fn list_workflows(
            &self,
            request: aion_proto::ProtoListWorkflowsRequest,
        ) -> Result<ProtoListWorkflowsResponse, ClientError> {
            *self.last_list.lock().await = Some(request);
            Ok(ProtoListWorkflowsResponse {
                summaries: vec![
                    encode_workflow_summary("tenant-a", None, &summary())
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
        }
    }

    #[tokio::test]
    async fn start_maps_request_and_returns_ids() -> Result<(), ClientError> {
        let stub = Arc::new(StubTransport::default());
        let client = client_with(Arc::clone(&stub));

        let result = client
            .start("checkout", payload("input"), StartOptions::default())
            .await?;
        let unsupported_key = client
            .start(
                "checkout",
                payload("input"),
                StartOptions {
                    namespace: None,
                    idempotency_key: Some(String::from("retry-key")),
                },
            )
            .await;

        let recorded = stub.last_start.lock().await.clone();
        assert!(recorded.is_some());
        let request = recorded.ok_or_else(|| ClientError::server("missing recorded start"))?;
        assert_eq!(request.namespace, "tenant-a");
        assert_eq!(request.workflow_type, "checkout");
        assert!(request.input.is_some());
        assert_ne!(result.0, WorkflowId::new(uuid::Uuid::nil()));
        assert_eq!(unsupported_key, Err(ClientError::InvalidArgument));
        Ok(())
    }

    #[tokio::test]
    async fn signal_maps_latest_run_and_error() {
        let stub = Arc::new(StubTransport::default());
        *stub.signal_error.lock().await = Some(ClientError::NotFound);
        let client = client_with(Arc::clone(&stub));
        let id = workflow_id();

        let result = client.signal(&id, None, "approve", payload("signal")).await;

        assert_eq!(result, Err(ClientError::NotFound));
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

        assert_eq!(result, Err(ClientError::QueryTimeout));
        assert_eq!(unsupported_args, Err(ClientError::InvalidArgument));
        let recorded = stub.last_query.lock().await.clone();
        assert!(recorded.is_some());
        let request = recorded.ok_or_else(|| ClientError::server("missing query"))?;
        assert!(request.run_id.is_some());
        Ok(())
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
}
