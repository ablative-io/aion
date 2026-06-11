//! Workflow management handlers.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest, ProtoListWorkflowsResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoSignalRequest, ProtoSignalResponse,
    ProtoStartWorkflowResponse,
};
use aion_store::visibility::WorkflowSummary;
use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
};

use super::auth::HttpCaller;
use super::error::{HttpStartError, HttpWireError};
use super::payload::{HttpDescribeWorkflowResponse, decode_start_workflow_request};
use super::visibility::{VisibilityQuery, scope_visibility_filter};
use crate::{NamespaceOperation, ServerError, ServerState, api::handlers};

pub(crate) async fn start_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    body: Bytes,
) -> Result<Json<ProtoStartWorkflowResponse>, HttpStartError> {
    if state.drain_state().is_draining() {
        return Err(HttpStartError::Draining);
    }
    let request = decode_start_workflow_request(&body).map_err(HttpStartError::Wire)?;
    handlers::start(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(|error| HttpStartError::Wire(HttpWireError(error)))
}

pub(crate) async fn signal_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoSignalRequest>,
) -> Result<Json<ProtoSignalResponse>, HttpWireError> {
    handlers::signal(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn query_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoQueryRequest>,
) -> Result<Json<ProtoQueryResponse>, HttpWireError> {
    handlers::query(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn cancel_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoCancelRequest>,
) -> Result<Json<ProtoCancelResponse>, HttpWireError> {
    handlers::cancel(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn post_list_workflows(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoListWorkflowsRequest>,
) -> Result<Json<ProtoListWorkflowsResponse>, HttpWireError> {
    handlers::list(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn get_workflows(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Query(query): Query<VisibilityQuery>,
) -> Result<Json<Vec<WorkflowSummary>>, HttpWireError> {
    let request = ProtoListWorkflowsRequest {
        namespace: query.namespace.clone(),
        filter: None,
    };
    let scoped = state
        .namespace_guard()
        .scope(
            &caller,
            &NamespaceOperation::list(&request, &aion_core::WorkflowFilter::default()),
        )
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let filter = scope_visibility_filter(
        query.into_filter().map_err(HttpWireError)?,
        scoped.namespace(),
    );
    let mut summaries = scoped
        .engine()
        .map_err(|error| HttpWireError(error.to_wire_error()))?
        .visibility_store()
        .list_workflows(filter)
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))?;
    crate::internal_workflow::retain_user_workflows(&mut summaries);
    Ok(Json(summaries))
}

#[derive(serde::Serialize)]
pub(crate) struct CountWorkflowsBody {
    count: u64,
}

pub(crate) async fn count_workflows(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Query(query): Query<VisibilityQuery>,
) -> Result<Json<CountWorkflowsBody>, HttpWireError> {
    let request = ProtoCountWorkflowsRequest {
        namespace: query.namespace.clone(),
        filter: None,
    };
    let scoped = state
        .namespace_guard()
        .scope(&caller, &NamespaceOperation::count(&request))
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let filter = scope_visibility_filter(
        query.into_filter().map_err(HttpWireError)?,
        scoped.namespace(),
    );
    let visibility_store = scoped
        .engine()
        .map_err(|error| HttpWireError(error.to_wire_error()))?
        .visibility_store();
    let count = crate::internal_workflow::count_user_workflows(&visibility_store, filter)
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))?;

    Ok(Json(CountWorkflowsBody { count }))
}

pub(crate) async fn describe_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoDescribeWorkflowRequest>,
) -> Result<Json<HttpDescribeWorkflowResponse>, HttpWireError> {
    let response = handlers::describe(state.namespace_guard(), &caller, request)
        .await
        .map_err(HttpWireError)?;
    HttpDescribeWorkflowResponse::try_from(response).map(Json)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::WorkflowStatus;
    use aion_proto::{
        ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest, ProtoListWorkflowsResponse,
        ProtoStartWorkflowRequest, WireError, WireErrorCode,
    };
    use aion_store::{
        WriteToken,
        visibility::{ListWorkflowsFilter, VisibilityRecord, VisibilityStore, WorkflowSummary},
    };
    use axum::{Router, http::StatusCode};
    use chrono::Utc;
    use serde_json::json;
    use tower::ServiceExt;

    use super::super::router::workflow_router;
    use super::super::test_support::{
        NAMESPACE, get_request, json_request, proto_payload, read_json, run_id, runtime_config,
        server_state, shared_engine, started_event, workflow_id,
    };
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    #[tokio::test]
    async fn http_start_and_list_match_handler_outcomes() -> Result<(), Box<dyn std::error::Error>>
    {
        let (router, visibility_store) = workflow_router_with_visibility().await?;

        assert_start_missing_workflow(&router).await?;
        assert_start_plain_json_missing_workflow(&router).await?;
        assert_start_invalid_payload_envelope(&router).await?;

        visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: std::collections::HashMap::from([(
                    crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
                    aion_core::SearchAttributeValue::String(NAMESPACE.to_owned()),
                )]),
            })
            .await?;
        let filter = aion_proto::encode_core_value(
            NAMESPACE,
            None,
            &ListWorkflowsFilter {
                workflow_type: Some(String::from("fixture")),
                status: Some(WorkflowStatus::Running),
                ..ListWorkflowsFilter::default()
            },
        )?;
        let list = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(filter),
        };
        let list_response = router
            .oneshot(json_request("/workflows/list", &list)?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: ProtoListWorkflowsResponse = read_json(list_response).await?;
        assert_eq!(list_body.summaries.len(), 1);
        let summary = aion_proto::decode_core_value::<WorkflowSummary>(&list_body.summaries[0])?;
        assert_eq!(summary.workflow_id, workflow_id());
        Ok(())
    }

    async fn workflow_router_with_visibility()
    -> Result<(Router, Arc<dyn VisibilityStore>), Box<dyn std::error::Error>> {
        let (engine, store, visibility_store) = shared_engine().await?;
        store
            .append(
                WriteToken::recorder(),
                &workflow_id(),
                &[started_event()?],
                0,
            )
            .await?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let state = server_state(resolver, runtime_config()).await?;
        Ok((workflow_router(state), visibility_store))
    }

    async fn assert_start_missing_workflow(
        router: &Router,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let start = ProtoStartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "missing-workflow".to_owned(),
            input: Some(proto_payload()?),
        };
        let response = router
            .clone()
            .oneshot(json_request("/workflows/start", &start)?)
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        assert_eq!(error.error_type.as_deref(), Some("WorkflowTypeNotFound"));
        assert!(error.message.contains("missing-workflow"));
        Ok(())
    }

    async fn assert_start_plain_json_missing_workflow(
        router: &Router,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let plain_start = json!({
            "namespace": NAMESPACE,
            "workflow_type": "missing-workflow",
            "input": { "name": "Ada" },
        });
        let response = router
            .clone()
            .oneshot(json_request("/workflows/start", &plain_start)?)
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::NotFound);
        Ok(())
    }

    async fn assert_start_invalid_payload_envelope(
        router: &Router,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let invalid_start = json!({
            "namespace": NAMESPACE,
            "workflow_type": "missing-workflow",
            "input": { "content_type": "application/json", "bytes": "not-a-byte-array" },
        });
        let response = router
            .clone()
            .oneshot(json_request("/workflows/start", &invalid_start)?)
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::InvalidInput);
        assert!(error.message.contains("{\"name\":\"Ada\"}"));
        Ok(())
    }

    /// Regression test (#51): the engine's internal schedule-coordinator
    /// workflow must never appear in the HTTP enumeration surfaces. The
    /// coordinator record carries the tenant namespace attribute to model any
    /// path that scopes the coordinator into a tenant — namespace scoping must
    /// not be the only thing hiding engine internals.
    #[tokio::test]
    async fn http_list_and_count_surfaces_hide_engine_internal_workflows()
    -> Result<(), Box<dyn std::error::Error>> {
        let (router, visibility_store) = workflow_router_with_visibility().await?;
        let namespace_attributes = std::collections::HashMap::from([(
            crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
            aion_core::SearchAttributeValue::String(NAMESPACE.to_owned()),
        )]);
        visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: namespace_attributes.clone(),
            })
            .await?;
        visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: aion_core::WorkflowId::new(uuid::Uuid::from_u128(0xa10a)),
                run_id: aion_core::RunId::new(uuid::Uuid::from_u128(0xa10b)),
                workflow_type: String::from("aion.schedule_coordinator"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: namespace_attributes,
            })
            .await?;

        let list_response = router
            .clone()
            .oneshot(get_request("/workflows?namespace=tenant-a")?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let summaries: Vec<WorkflowSummary> = read_json(list_response).await?;
        assert_eq!(
            summaries.len(),
            1,
            "GET /workflows must hide engine-internal workflows"
        );
        assert_eq!(summaries[0].workflow_type, "fixture");

        let count_response = router
            .clone()
            .oneshot(get_request("/workflows/count?namespace=tenant-a")?)
            .await?;
        assert_eq!(count_response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(count_response).await?;
        assert_eq!(
            body["count"], 1,
            "GET /workflows/count must exclude engine-internal workflows"
        );

        let list = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(aion_proto::encode_core_value(
                NAMESPACE,
                None,
                &ListWorkflowsFilter::default(),
            )?),
        };
        let list_response = router
            .oneshot(json_request("/workflows/list", &list)?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: ProtoListWorkflowsResponse = read_json(list_response).await?;
        assert_eq!(
            list_body.summaries.len(),
            1,
            "POST /workflows/list must hide engine-internal workflows"
        );
        Ok(())
    }

    /// Companion to the #51 exclusion: `describe` by explicit workflow id is
    /// the operator escape hatch and must still resolve the engine-internal
    /// schedule coordinator.
    #[tokio::test]
    async fn describe_by_explicit_id_still_resolves_internal_workflow()
    -> Result<(), Box<dyn std::error::Error>> {
        let (engine, _store, _visibility_store) = shared_engine().await?;
        // The engine bootstraps the coordinator's WorkflowStarted event, so
        // describing it by its real id resolves against genuine history.
        let coordinator_id = engine.schedule_coordinator_workflow_id().clone();
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(coordinator_id.clone(), NAMESPACE)?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);

        let describe = ProtoDescribeWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(coordinator_id.into()),
            run_id: None,
            include_history: false,
        };
        let response = router
            .oneshot(json_request("/workflows/describe", &describe)?)
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "describe by explicit id is the operator escape hatch"
        );
        Ok(())
    }

    #[tokio::test]
    async fn describe_decodes_json_payloads_for_http() -> Result<(), Box<dyn std::error::Error>> {
        let (engine, store, _visibility_store) = shared_engine().await?;
        store
            .append(
                WriteToken::recorder(),
                &workflow_id(),
                &[started_event()?],
                0,
            )
            .await?;
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);

        let describe = ProtoDescribeWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(workflow_id().into()),
            run_id: Some(run_id().into()),
            include_history: true,
        };
        let response = router
            .oneshot(json_request("/workflows/describe", &describe)?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        let body: serde_json::Value = read_json(response).await?;
        assert_eq!(
            body["summary"]["payload"]["content_type"],
            "application/json"
        );
        assert_eq!(
            body["summary"]["payload"]["data"]["workflow_id"],
            workflow_id().to_string()
        );
        assert_eq!(
            body["history"][0]["payload"]["data"]["data"]["input"],
            json!({"content_type": "application/json", "data": {"fixture": "input"}})
        );
        Ok(())
    }
}
