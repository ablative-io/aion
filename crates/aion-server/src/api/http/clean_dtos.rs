//! Clean HTTP request/response DTOs for the workflow POST endpoints.
//!
//! Web clients speak a clean domain JSON contract: ids are plain UUID strings
//! (matching what the GET surfaces return), payloads and filters are plain JSON,
//! and responses never leak the protobuf-derived shapes (`{"uuid": "..."}`
//! id objects, `WireEnvelope` lists). Each DTO converts to/from the proto types
//! that the shared `handlers::*` layer consumes, keeping the gRPC transport and
//! the `aion-proto` message definitions untouched — this is an HTTP-layer wire
//! change only.

use aion_core::{WorkflowStatus, WorkflowSummary};
use aion_proto::{
    ProtoCancelRequest, ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest,
    ProtoListWorkflowsResponse, ProtoQueryRequest, ProtoQueryResponse, ProtoRunId,
    ProtoSignalRequest, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, ProtoWorkflowId,
    WireError, proto_query_response,
};
use aion_store::visibility::{ListWorkflowsFilter, WorkflowSummary as StoreWorkflowSummary};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::HttpWireError;
use super::payload::http_input_payload;

/// Clean start-workflow request: `input` is plain JSON (auto-wrapped as an
/// `application/json` payload) or a legacy `{content_type, bytes}` envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct StartWorkflowRequest {
    namespace: String,
    workflow_type: String,
    #[serde(default)]
    input: Option<Value>,
    /// R-4 steered-start routing key (optional; absent keeps unsteered placement).
    #[serde(default)]
    routing_key: Option<String>,
    /// Optional default task queue for this workflow's activities (absent =
    /// the namespace's default queue). Recorded durably on the start.
    #[serde(default)]
    task_queue: Option<String>,
}

impl TryFrom<StartWorkflowRequest> for ProtoStartWorkflowRequest {
    type Error = WireError;

    fn try_from(request: StartWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_type: request.workflow_type,
            input: request.input.map(http_input_payload).transpose()?,
            routing_key: request.routing_key,
            task_queue: request.task_queue,
        })
    }
}

/// Clean signal request: ids are plain UUID strings, payload is plain JSON.
#[derive(Debug, Deserialize)]
pub(crate) struct SignalWorkflowRequest {
    namespace: String,
    workflow_id: String,
    #[serde(default)]
    run_id: Option<String>,
    signal_name: String,
    #[serde(default)]
    payload: Option<Value>,
}

impl TryFrom<SignalWorkflowRequest> for ProtoSignalRequest {
    type Error = WireError;

    fn try_from(request: SignalWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_id: Some(proto_workflow_id(&request.workflow_id)?),
            run_id: optional_proto_run_id(request.run_id.as_deref())?,
            signal_name: request.signal_name,
            payload: request.payload.map(http_input_payload).transpose()?,
        })
    }
}

/// Clean query request: ids are plain UUID strings.
#[derive(Debug, Deserialize)]
pub(crate) struct QueryWorkflowRequest {
    namespace: String,
    workflow_id: String,
    #[serde(default)]
    run_id: Option<String>,
    query_name: String,
}

impl TryFrom<QueryWorkflowRequest> for ProtoQueryRequest {
    type Error = WireError;

    fn try_from(request: QueryWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_id: Some(proto_workflow_id(&request.workflow_id)?),
            run_id: optional_proto_run_id(request.run_id.as_deref())?,
            query_name: request.query_name,
        })
    }
}

/// Clean cancel request: ids are plain UUID strings.
#[derive(Debug, Deserialize)]
pub(crate) struct CancelWorkflowRequest {
    namespace: String,
    workflow_id: String,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    reason: String,
}

impl TryFrom<CancelWorkflowRequest> for ProtoCancelRequest {
    type Error = WireError;

    fn try_from(request: CancelWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_id: Some(proto_workflow_id(&request.workflow_id)?),
            run_id: optional_proto_run_id(request.run_id.as_deref())?,
            reason: request.reason,
        })
    }
}

/// Clean describe request: ids are plain UUID strings.
#[derive(Debug, Deserialize)]
pub(crate) struct DescribeWorkflowRequest {
    namespace: String,
    workflow_id: String,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    include_history: bool,
}

impl TryFrom<DescribeWorkflowRequest> for ProtoDescribeWorkflowRequest {
    type Error = WireError;

    fn try_from(request: DescribeWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_id: Some(proto_workflow_id(&request.workflow_id)?),
            run_id: optional_proto_run_id(request.run_id.as_deref())?,
            include_history: request.include_history,
        })
    }
}

/// Clean list-workflows request: `filter` is a plain JSON object with every
/// predicate optional (mirrors the dashboard's `WorkflowFilter`), not a
/// serde-encoded `WireEnvelope`. Unknown fields (e.g. the dashboard's `parent`,
/// or pagination keys the dashboard sends alongside) are ignored.
#[derive(Debug, Deserialize)]
pub(crate) struct ListWorkflowsRequest {
    namespace: String,
    #[serde(default)]
    filter: Option<WorkflowFilterDto>,
}

/// Clean, fully-optional list filter. Maps to the store's `ListWorkflowsFilter`,
/// defaulting any predicate the client omits.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct WorkflowFilterDto {
    #[serde(default)]
    workflow_type: Option<String>,
    #[serde(default)]
    status: Option<WorkflowStatus>,
    #[serde(default)]
    started_after: Option<DateTime<Utc>>,
    #[serde(default)]
    started_before: Option<DateTime<Utc>>,
    #[serde(default)]
    closed_after: Option<DateTime<Utc>>,
    #[serde(default)]
    closed_before: Option<DateTime<Utc>>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

impl From<WorkflowFilterDto> for ListWorkflowsFilter {
    fn from(filter: WorkflowFilterDto) -> Self {
        Self {
            workflow_type: filter.workflow_type,
            status: filter.status,
            started_after: filter.started_after,
            started_before: filter.started_before,
            closed_after: filter.closed_after,
            closed_before: filter.closed_before,
            search_attributes: Vec::new(),
            limit: filter.limit,
            offset: filter.offset,
        }
    }
}

impl TryFrom<ListWorkflowsRequest> for ProtoListWorkflowsRequest {
    type Error = WireError;

    fn try_from(request: ListWorkflowsRequest) -> Result<Self, Self::Error> {
        let filter = request
            .filter
            .map(|filter| {
                aion_proto::encode_core_value(
                    request.namespace.clone(),
                    None,
                    &ListWorkflowsFilter::from(filter),
                )
            })
            .transpose()?;
        Ok(Self {
            namespace: request.namespace,
            filter,
        })
    }
}

/// Clean start response: ids are plain UUID strings, consistent with GET.
#[derive(Debug, Serialize)]
pub(crate) struct StartWorkflowResponse {
    workflow_id: String,
    run_id: String,
}

impl TryFrom<ProtoStartWorkflowResponse> for StartWorkflowResponse {
    type Error = HttpWireError;

    fn try_from(response: ProtoStartWorkflowResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            workflow_id: required_uuid(response.workflow_id.map(|id| id.uuid), "workflow id")?,
            run_id: required_uuid(response.run_id.map(|id| id.uuid), "run id")?,
        })
    }
}

/// Clean list response: a plain array of [`WorkflowSummary`] projections whose
/// field names match the generated TypeScript bindings exactly
/// (`workflow_id`/`workflow_type`/`status`/`started_at`/`ended_at`/`parent`),
/// decoded from the proto `WireEnvelope` list and converted from the store's
/// visibility projection at the HTTP boundary. Consistent with GET /workflows.
#[derive(Debug, Serialize)]
pub(crate) struct ListWorkflowsResponse {
    summaries: Vec<WorkflowSummary>,
}

impl TryFrom<ProtoListWorkflowsResponse> for ListWorkflowsResponse {
    type Error = HttpWireError;

    fn try_from(response: ProtoListWorkflowsResponse) -> Result<Self, Self::Error> {
        let summaries = response
            .summaries
            .iter()
            .map(aion_proto::decode_core_value::<StoreWorkflowSummary>)
            .map(|summary| summary.map(core_summary_from_store))
            .collect::<Result<Vec<_>, _>>()
            .map_err(HttpWireError)?;
        Ok(Self { summaries })
    }
}

/// Convert the store's visibility projection into the dashboard-facing
/// [`WorkflowSummary`] wire shape. `start_time`/`close_time` map to
/// `started_at`/`ended_at`; `parent` is not carried in the visibility
/// projection, so it is `None` (matching `from_history`).
pub(crate) fn core_summary_from_store(summary: StoreWorkflowSummary) -> WorkflowSummary {
    WorkflowSummary {
        workflow_id: summary.workflow_id,
        workflow_type: summary.workflow_type,
        status: summary.status,
        started_at: summary.start_time,
        ended_at: summary.close_time,
        parent: None,
    }
}

/// Clean query response: a typed JSON union of `result` (decoded JSON payload)
/// or `error` (the stable `WireError`), instead of the prost oneof shape.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueryWorkflowResponse {
    Result(Value),
    Error(WireError),
}

impl TryFrom<ProtoQueryResponse> for QueryWorkflowResponse {
    type Error = HttpWireError;

    fn try_from(response: ProtoQueryResponse) -> Result<Self, HttpWireError> {
        match response.outcome {
            Some(proto_query_response::Outcome::Result(payload)) => {
                let payload = aion_core::Payload::try_from(payload).map_err(HttpWireError)?;
                let value = payload.to_json().map_err(|_error| {
                    HttpWireError(WireError::backend("query result payload is not JSON"))
                })?;
                Ok(Self::Result(value))
            }
            Some(proto_query_response::Outcome::Error(error)) => Ok(Self::Error(
                WireError::try_from(error).map_err(HttpWireError)?,
            )),
            None => Err(HttpWireError(WireError::backend(
                "query response is missing an outcome",
            ))),
        }
    }
}

fn proto_workflow_id(value: &str) -> Result<ProtoWorkflowId, WireError> {
    let id = parse_uuid(value, "workflow id")?;
    Ok(aion_core::WorkflowId::new(id).into())
}

fn optional_proto_run_id(value: Option<&str>) -> Result<Option<ProtoRunId>, WireError> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| parse_uuid(value, "run id").map(|id| aion_core::RunId::new(id).into()))
        .transpose()
}

fn parse_uuid(value: &str, label: &str) -> Result<uuid::Uuid, WireError> {
    uuid::Uuid::parse_str(value)
        .map_err(|_error| WireError::invalid_input(format!("{label} is not a valid UUID")))
}

fn required_uuid(value: Option<String>, label: &str) -> Result<String, HttpWireError> {
    value.ok_or_else(|| HttpWireError(WireError::backend(format!("{label} is missing"))))
}

#[cfg(test)]
mod tests {
    use aion_proto::WireErrorCode;
    use serde_json::json;

    use super::*;

    #[test]
    fn clean_signal_request_converts_string_ids_to_proto() -> Result<(), Box<dyn std::error::Error>>
    {
        let request: SignalWorkflowRequest = serde_json::from_value(json!({
            "namespace": "tenant-a",
            "workflow_id": "00000000-0000-0000-0000-000000000001",
            "run_id": "00000000-0000-0000-0000-00000000000a",
            "signal_name": "poke",
            "payload": { "value": 1 },
        }))?;
        let proto = ProtoSignalRequest::try_from(request)?;
        assert_eq!(proto.namespace, "tenant-a");
        assert_eq!(
            proto.workflow_id.as_ref().map(|id| id.uuid.as_str()),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(
            proto.run_id.as_ref().map(|id| id.uuid.as_str()),
            Some("00000000-0000-0000-0000-00000000000a")
        );
        let payload = proto.payload.ok_or("payload missing")?;
        assert_eq!(payload.content_type, "application/json");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&payload.bytes)?,
            json!({ "value": 1 })
        );
        Ok(())
    }

    #[test]
    fn clean_request_rejects_non_uuid_workflow_id() -> Result<(), Box<dyn std::error::Error>> {
        let request: DescribeWorkflowRequest = serde_json::from_value(json!({
            "namespace": "tenant-a",
            "workflow_id": "not-a-uuid",
            "include_history": true,
        }))?;
        let error = ProtoDescribeWorkflowRequest::try_from(request)
            .err()
            .ok_or("expected conversion error")?;
        assert_eq!(error.code, WireErrorCode::InvalidInput);
        Ok(())
    }

    #[test]
    fn clean_describe_request_omits_blank_run_id() -> Result<(), Box<dyn std::error::Error>> {
        let request: DescribeWorkflowRequest = serde_json::from_value(json!({
            "namespace": "tenant-a",
            "workflow_id": "00000000-0000-0000-0000-000000000001",
            "run_id": null,
            "include_history": true,
        }))?;
        let proto = ProtoDescribeWorkflowRequest::try_from(request)?;
        assert!(proto.run_id.is_none());
        assert!(proto.include_history);
        Ok(())
    }

    #[test]
    fn clean_list_request_wraps_plain_filter_in_envelope() -> Result<(), Box<dyn std::error::Error>>
    {
        let request: ListWorkflowsRequest = serde_json::from_value(json!({
            "namespace": "tenant-a",
            "filter": { "workflow_type": "checkout", "status": "Running" },
        }))?;
        let proto = ProtoListWorkflowsRequest::try_from(request)?;
        let envelope = proto.filter.ok_or("filter missing")?;
        let filter = aion_proto::decode_core_value::<ListWorkflowsFilter>(&envelope)?;
        assert_eq!(filter.workflow_type.as_deref(), Some("checkout"));
        assert_eq!(filter.status, Some(aion_core::WorkflowStatus::Running));
        Ok(())
    }

    #[test]
    fn clean_list_request_defaults_missing_filter_to_none() -> Result<(), Box<dyn std::error::Error>>
    {
        let request: ListWorkflowsRequest = serde_json::from_value(json!({
            "namespace": "tenant-a",
        }))?;
        let proto = ProtoListWorkflowsRequest::try_from(request)?;
        assert!(proto.filter.is_none());
        Ok(())
    }

    #[test]
    fn clean_start_response_exposes_string_ids() -> Result<(), Box<dyn std::error::Error>> {
        let proto = ProtoStartWorkflowResponse {
            workflow_id: Some(aion_core::WorkflowId::new(uuid::Uuid::from_u128(1)).into()),
            run_id: Some(aion_core::RunId::new(uuid::Uuid::from_u128(10)).into()),
        };
        let clean = StartWorkflowResponse::try_from(proto).map_err(|error| error.0.message)?;
        let value = serde_json::to_value(&clean)?;
        assert_eq!(value["workflow_id"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(value["run_id"], "00000000-0000-0000-0000-00000000000a");
        Ok(())
    }
}
