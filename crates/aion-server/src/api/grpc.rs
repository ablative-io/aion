//! tonic workflow service adapter.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoCreateScheduleRequest, ProtoCreateScheduleResponse,
    ProtoDeleteScheduleResponse, ProtoDescribeScheduleResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoPauseScheduleResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoResumeScheduleResponse, ProtoScheduleIdRequest,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    ProtoUpdateScheduleRequest, ProtoUpdateScheduleResponse, ProtoWireError, WireError,
    generated::{self, workflow_service_server::WorkflowServiceServer},
};
use prost::Message;
use tonic::{Code, Request, Response, Status};

use crate::{CallerIdentity, ServerState, api::handlers};

/// Cloneable tonic implementation for workflow management.
#[derive(Clone)]
pub struct WorkflowGrpcService {
    state: ServerState,
}

impl WorkflowGrpcService {
    /// Build a tonic workflow service from shared server state.
    #[must_use]
    pub const fn new(state: ServerState) -> Self {
        Self { state }
    }

    async fn caller<T>(&self, request: &Request<T>) -> Result<CallerIdentity, Status> {
        caller_from_metadata(request.metadata(), &self.state).await
    }
}

/// Construct the generated tonic server wrapper.
#[must_use]
pub fn workflow_service(state: ServerState) -> WorkflowServiceServer<WorkflowGrpcService> {
    WorkflowServiceServer::new(WorkflowGrpcService::new(state))
}

#[tonic::async_trait]
impl generated::workflow_service_server::WorkflowService for WorkflowGrpcService {
    async fn start_workflow(
        &self,
        request: Request<generated::StartWorkflowRequest>,
    ) -> Result<Response<generated::StartWorkflowResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::start(
            self.state.namespace_guard(),
            &caller,
            decode_start_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_start_response(response)))
    }

    async fn signal(
        &self,
        request: Request<generated::SignalRequest>,
    ) -> Result<Response<generated::SignalResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::signal(
            self.state.namespace_guard(),
            &caller,
            decode_signal_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_signal_response(response)))
    }

    async fn query(
        &self,
        request: Request<generated::QueryRequest>,
    ) -> Result<Response<generated::QueryResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::query(
            self.state.namespace_guard(),
            &caller,
            decode_query_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_query_response(response)))
    }

    async fn cancel(
        &self,
        request: Request<generated::CancelRequest>,
    ) -> Result<Response<generated::CancelResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::cancel(
            self.state.namespace_guard(),
            &caller,
            decode_cancel_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_cancel_response(response)))
    }

    async fn list_workflows(
        &self,
        request: Request<generated::ListWorkflowsRequest>,
    ) -> Result<Response<generated::ListWorkflowsResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::list(
            self.state.namespace_guard(),
            &caller,
            decode_list_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_list_response(response)))
    }

    async fn count_workflows(
        &self,
        request: Request<generated::CountWorkflowsRequest>,
    ) -> Result<Response<generated::CountWorkflowsResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::count(
            self.state.namespace_guard(),
            &caller,
            decode_count_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_count_response(response)))
    }

    async fn describe_workflow(
        &self,
        request: Request<generated::DescribeWorkflowRequest>,
    ) -> Result<Response<generated::DescribeWorkflowResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::describe(
            self.state.namespace_guard(),
            &caller,
            decode_describe_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_describe_response(response)))
    }

    async fn create_schedule(
        &self,
        request: Request<generated::CreateScheduleRequest>,
    ) -> Result<Response<generated::CreateScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::create_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_create_schedule_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_create_schedule_response(response)))
    }

    async fn update_schedule(
        &self,
        request: Request<generated::UpdateScheduleRequest>,
    ) -> Result<Response<generated::UpdateScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::update_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_update_schedule_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_update_schedule_response(response)))
    }

    async fn pause_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::PauseScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::pause_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_pause_schedule_response(response)))
    }

    async fn resume_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::ResumeScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::resume_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_resume_schedule_response(response)))
    }

    async fn delete_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::DeleteScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::delete_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_delete_schedule_response(response)))
    }

    async fn list_schedules(
        &self,
        request: Request<generated::ListSchedulesRequest>,
    ) -> Result<Response<generated::ListSchedulesResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::list_schedules(
            self.state.namespace_guard(),
            &caller,
            decode_list_schedules_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_list_schedules_response(response)))
    }

    async fn describe_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::DescribeScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::describe_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_describe_schedule_response(response)))
    }
}

pub(crate) async fn caller_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    state: &ServerState,
) -> Result<CallerIdentity, Status> {
    if !state.runtime_config().auth.enabled {
        return Ok(development_caller_from_metadata(metadata));
    }
    #[cfg(feature = "auth")]
    {
        let bearer = metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_bearer)
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?;
        let Some(cache) = state.jwks_cache() else {
            return Err(Status::unauthenticated("invalid bearer token"));
        };
        return cache
            .validate(&bearer)
            .await
            .map(|claims| claims.caller_identity())
            .map_err(|_error| Status::unauthenticated("invalid bearer token"));
    }
    #[cfg(not(feature = "auth"))]
    {
        std::future::ready(()).await;
        Err(Status::unauthenticated("authentication unavailable"))
    }
}

fn development_caller_from_metadata(metadata: &tonic::metadata::MetadataMap) -> CallerIdentity {
    let subject = metadata
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("anonymous");
    let namespaces = metadata
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map(parse_namespaces)
        .unwrap_or_default();
    CallerIdentity::new(subject, namespaces)
}

#[cfg(feature = "auth")]
fn parse_bearer(value: &str) -> Option<String> {
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
}

fn parse_namespaces(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|namespace| !namespace.is_empty())
        .map(str::to_owned)
        .collect()
}

fn status_from_wire_error(error: WireError) -> Status {
    let code = grpc_code(error.code);
    let message = error.message.clone();
    let mut details = Vec::new();
    let proto_error = ProtoWireError::from(error);
    if proto_error.encode(&mut details).is_ok() {
        Status::with_details(code, message, details.into())
    } else {
        Status::new(code, message)
    }
}

fn grpc_code(code: aion_proto::WireErrorCode) -> Code {
    match code {
        aion_proto::WireErrorCode::NotFound => Code::NotFound,
        aion_proto::WireErrorCode::NamespaceDenied => Code::PermissionDenied,
        aion_proto::WireErrorCode::SequenceConflict => Code::Aborted,
        aion_proto::WireErrorCode::UnknownQuery | aion_proto::WireErrorCode::InvalidInput => {
            Code::InvalidArgument
        }
        aion_proto::WireErrorCode::QueryTimeout => Code::DeadlineExceeded,
        aion_proto::WireErrorCode::NotRunning => Code::FailedPrecondition,
        aion_proto::WireErrorCode::Lagged => Code::ResourceExhausted,
        aion_proto::WireErrorCode::Backend => Code::Internal,
    }
}

fn decode_workflow_id(value: generated::WorkflowId) -> aion_proto::ProtoWorkflowId {
    aion_proto::ProtoWorkflowId { uuid: value.uuid }
}

fn encode_workflow_id(value: aion_proto::ProtoWorkflowId) -> generated::WorkflowId {
    generated::WorkflowId { uuid: value.uuid }
}

fn decode_run_id(value: generated::RunId) -> aion_proto::ProtoRunId {
    aion_proto::ProtoRunId { uuid: value.uuid }
}

fn encode_run_id(value: aion_proto::ProtoRunId) -> generated::RunId {
    generated::RunId { uuid: value.uuid }
}

fn decode_schedule_id(value: generated::ScheduleId) -> aion_proto::ProtoScheduleId {
    aion_proto::ProtoScheduleId { uuid: value.uuid }
}

fn encode_schedule_id(value: aion_proto::ProtoScheduleId) -> generated::ScheduleId {
    generated::ScheduleId { uuid: value.uuid }
}

fn decode_payload(value: generated::Payload) -> aion_proto::ProtoPayload {
    aion_proto::ProtoPayload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn encode_payload(value: aion_proto::ProtoPayload) -> generated::Payload {
    generated::Payload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn decode_envelope(value: generated::WireEnvelope) -> aion_proto::WireEnvelope {
    aion_proto::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(decode_payload),
    }
}

fn encode_envelope(value: aion_proto::WireEnvelope) -> generated::WireEnvelope {
    generated::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(encode_payload),
    }
}

fn decode_start_request(value: generated::StartWorkflowRequest) -> ProtoStartWorkflowRequest {
    ProtoStartWorkflowRequest {
        namespace: value.namespace,
        workflow_type: value.workflow_type,
        input: value.input.map(decode_payload),
    }
}

fn encode_start_response(value: ProtoStartWorkflowResponse) -> generated::StartWorkflowResponse {
    generated::StartWorkflowResponse {
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
    }
}

fn decode_signal_request(value: generated::SignalRequest) -> ProtoSignalRequest {
    ProtoSignalRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        signal_name: value.signal_name,
        payload: value.payload.map(decode_payload),
    }
}

fn encode_signal_response(_: ProtoSignalResponse) -> generated::SignalResponse {
    generated::SignalResponse {}
}

fn decode_query_request(value: generated::QueryRequest) -> ProtoQueryRequest {
    ProtoQueryRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        query_name: value.query_name,
    }
}

fn encode_query_response(value: ProtoQueryResponse) -> generated::QueryResponse {
    generated::QueryResponse {
        outcome: value.outcome.map(encode_query_outcome),
    }
}

fn encode_query_outcome(
    value: aion_proto::proto_query_response::Outcome,
) -> generated::query_response::Outcome {
    match value {
        aion_proto::proto_query_response::Outcome::Result(payload) => {
            generated::query_response::Outcome::Result(encode_payload(payload))
        }
        aion_proto::proto_query_response::Outcome::Error(error) => {
            generated::query_response::Outcome::Error(encode_wire_error(error))
        }
    }
}

fn encode_wire_error(value: ProtoWireError) -> generated::WireError {
    generated::WireError {
        code: value.code,
        message: value.message,
    }
}

fn decode_cancel_request(value: generated::CancelRequest) -> ProtoCancelRequest {
    ProtoCancelRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        reason: value.reason,
    }
}

fn encode_cancel_response(_: ProtoCancelResponse) -> generated::CancelResponse {
    generated::CancelResponse {}
}

fn decode_list_request(value: generated::ListWorkflowsRequest) -> ProtoListWorkflowsRequest {
    ProtoListWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(decode_envelope),
    }
}

fn encode_list_response(value: ProtoListWorkflowsResponse) -> generated::ListWorkflowsResponse {
    generated::ListWorkflowsResponse {
        summaries: value.summaries.into_iter().map(encode_envelope).collect(),
    }
}

fn decode_count_request(value: generated::CountWorkflowsRequest) -> ProtoCountWorkflowsRequest {
    ProtoCountWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(decode_envelope),
    }
}

fn encode_count_response(value: ProtoCountWorkflowsResponse) -> generated::CountWorkflowsResponse {
    generated::CountWorkflowsResponse { count: value.count }
}

fn decode_describe_request(
    value: generated::DescribeWorkflowRequest,
) -> ProtoDescribeWorkflowRequest {
    ProtoDescribeWorkflowRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        include_history: value.include_history,
    }
}

fn encode_describe_response(
    value: ProtoDescribeWorkflowResponse,
) -> generated::DescribeWorkflowResponse {
    generated::DescribeWorkflowResponse {
        summary: value.summary.map(encode_envelope),
        history: value.history.into_iter().map(encode_envelope).collect(),
    }
}

fn decode_create_schedule_request(
    value: generated::CreateScheduleRequest,
) -> ProtoCreateScheduleRequest {
    ProtoCreateScheduleRequest {
        namespace: value.namespace,
        config: value.config.map(decode_envelope),
    }
}

fn encode_create_schedule_response(
    value: ProtoCreateScheduleResponse,
) -> generated::CreateScheduleResponse {
    generated::CreateScheduleResponse {
        schedule_id: value.schedule_id.map(encode_schedule_id),
        state: value.state.map(encode_envelope),
    }
}

fn decode_update_schedule_request(
    value: generated::UpdateScheduleRequest,
) -> ProtoUpdateScheduleRequest {
    ProtoUpdateScheduleRequest {
        namespace: value.namespace,
        schedule_id: value.schedule_id.map(decode_schedule_id),
        config: value.config.map(decode_envelope),
    }
}

fn encode_update_schedule_response(
    value: ProtoUpdateScheduleResponse,
) -> generated::UpdateScheduleResponse {
    generated::UpdateScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

fn decode_schedule_id_request(value: generated::ScheduleIdRequest) -> ProtoScheduleIdRequest {
    ProtoScheduleIdRequest {
        namespace: value.namespace,
        schedule_id: value.schedule_id.map(decode_schedule_id),
    }
}

fn encode_pause_schedule_response(
    value: ProtoPauseScheduleResponse,
) -> generated::PauseScheduleResponse {
    generated::PauseScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

fn encode_resume_schedule_response(
    value: ProtoResumeScheduleResponse,
) -> generated::ResumeScheduleResponse {
    generated::ResumeScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

fn encode_delete_schedule_response(
    _: ProtoDeleteScheduleResponse,
) -> generated::DeleteScheduleResponse {
    generated::DeleteScheduleResponse {}
}

fn decode_list_schedules_request(
    value: generated::ListSchedulesRequest,
) -> ProtoListSchedulesRequest {
    ProtoListSchedulesRequest {
        namespace: value.namespace,
    }
}

fn encode_list_schedules_response(
    value: ProtoListSchedulesResponse,
) -> generated::ListSchedulesResponse {
    generated::ListSchedulesResponse {
        schedules: value.schedules.into_iter().map(encode_envelope).collect(),
    }
}

fn encode_describe_schedule_response(
    value: ProtoDescribeScheduleResponse,
) -> generated::DescribeScheduleResponse {
    generated::DescribeScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use aion::EngineBuilder;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};
    use aion_proto::{
        WireErrorCode,
        convert::{decode_core_value, encode_core_value},
        generated::workflow_service_server::WorkflowService,
    };
    use aion_store::{
        EventStore, InMemoryStore,
        visibility::{VisibilityRecord, VisibilityStore},
    };
    use chrono::Utc;
    use serde_json::json;
    use tonic::Request;

    use super::*;
    use crate::{
        NamespaceResolver, WorkflowOwnership,
        config::{
            AuthConfig, DashboardAssetSource, DashboardConfig, ListenConfig, MetricsConfig,
            NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
        },
    };

    const NAMESPACE: &str = "tenant-a";
    const TOKEN: &str = "test-token";

    #[tokio::test]
    async fn in_process_tonic_start_and_list_use_shared_handlers()
    -> Result<(), Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = backing.clone();
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .visibility_store_arc(Arc::clone(&visibility_store))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        store.append(&workflow_id(), &[started_event()?], 0).await?;
        visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: aion_core::RunId::new(uuid::Uuid::from_u128(2)),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: std::collections::HashMap::new(),
            })
            .await?;
        let ownership = WorkflowOwnership::default();
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            ownership.clone(),
        );
        let state = ServerState::from_parts(resolver, runtime_config());
        let service = WorkflowGrpcService::new(state);

        let mut start = Request::new(generated::StartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "missing-workflow".to_owned(),
            input: Some(encode_payload(proto_payload()?)),
        });
        apply_metadata(start.metadata_mut())?;
        let start_error = service.start_workflow(start).await;
        assert_eq!(
            start_error.err().map(|status| status.code()),
            Some(Code::NotFound)
        );

        let list_filter = encode_core_value(
            NAMESPACE,
            None,
            &aion_store::visibility::ListWorkflowsFilter {
                status: Some(WorkflowStatus::Running),
                ..aion_store::visibility::ListWorkflowsFilter::default()
            },
        )?;
        let mut list = Request::new(generated::ListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(encode_envelope(list_filter)),
        });
        apply_metadata(list.metadata_mut())?;
        let response = service.list_workflows(list).await?.into_inner();

        assert_eq!(response.summaries.len(), 1);
        let summary = response
            .summaries
            .into_iter()
            .next()
            .map(decode_envelope)
            .map(|envelope| decode_core_value::<aion_store::visibility::WorkflowSummary>(&envelope))
            .transpose()?
            .ok_or_else(|| WireError::backend("summary missing"))?;
        assert_eq!(summary.workflow_id, workflow_id());
        assert_eq!(
            ownership
                .verify(NAMESPACE, &workflow_id())
                .err()
                .map(|error| error.to_wire_error().code),
            Some(WireErrorCode::NamespaceDenied)
        );
        Ok(())
    }

    fn apply_metadata(
        metadata: &mut tonic::metadata::MetadataMap,
    ) -> Result<(), tonic::metadata::errors::InvalidMetadataValue> {
        metadata.insert("authorization", format!("Bearer {TOKEN}").parse()?);
        metadata.insert("x-aion-subject", "alice".parse()?);
        metadata.insert("x-aion-namespaces", NAMESPACE.parse()?);
        Ok(())
    }

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            listen: ListenConfig {
                grpc: SocketAddr::from(([127, 0, 0, 1], 50051)),
                http: SocketAddr::from(([127, 0, 0, 1], 8080)),
            },
            tls: None,
            auth: AuthConfig {
                enabled: true,
                jwks_url: Some(TOKEN.to_owned()),
                jwks_refresh_seconds: 300,
            },
            dashboard: DashboardConfig {
                source: DashboardAssetSource::Embedded,
            },
            namespace: NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            worker: WorkerConfig {
                heartbeat_window: std::time::Duration::from_millis(30_000),
            },
            websocket: WebSocketConfig {
                outbound_buffer_bound: 32,
            },
            workflow_packages: Vec::new(),
            scheduler_threads: 1,
            default_namespace: "default".to_owned(),
            drain_timeout: std::time::Duration::from_secs(30),
            metrics: MetricsConfig { enabled: true },
        }
    }

    fn started_event() -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id(),
            },
            workflow_type: "fixture".to_owned(),
            input: payload()?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
        })
    }

    fn proto_payload() -> Result<aion_proto::ProtoPayload, aion_core::PayloadError> {
        Ok(payload()?.into())
    }

    fn payload() -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "fixture": "input" }))
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }
}
