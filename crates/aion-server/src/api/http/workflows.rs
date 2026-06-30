//! Workflow management handlers.

use aion_core::WorkflowSummary;
use aion_proto::{
    ProtoCancelResponse, ProtoCountWorkflowsRequest, ProtoListWorkflowsRequest, ProtoSignalResponse,
};
use axum::{
    Json,
    extract::{Query, State},
};

use super::auth::HttpCaller;
use super::clean_dtos::{
    CancelWorkflowRequest, DescribeWorkflowRequest, ListWorkflowsRequest, ListWorkflowsResponse,
    QueryWorkflowRequest, QueryWorkflowResponse, SignalWorkflowRequest, StartWorkflowRequest,
    StartWorkflowResponse, core_summary_from_store,
};
use super::error::{HttpStartError, HttpWireError};
use super::payload::describe_response_to_ops_console;
use super::visibility::{VisibilityQuery, scope_visibility_filter};
use crate::{NamespaceOperation, ServerError, ServerState, api::handlers};

pub(crate) async fn start_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<StartWorkflowRequest>,
) -> Result<Json<StartWorkflowResponse>, HttpStartError> {
    if state.drain_state().is_draining() {
        return Err(HttpStartError::Draining);
    }
    let request = request
        .try_into()
        .map_err(|error| HttpStartError::Wire(HttpWireError(error)))?;
    // The minted-on-use safety net (Phase 1 S6): an authorized start into an
    // unseen namespace durably mints (open) or is gated (closed) before the
    // engine start, so a client that starts before any worker registers still
    // gets a durable namespace record. The HTTP path mints locally (`placement =
    // None`); steered placement is a gRPC/cluster concern.
    let minter = state.namespace_minter();
    let response = handlers::start_with_placement(
        state.namespace_guard(),
        &caller,
        request,
        None,
        Some(&minter),
    )
    .await
    .map_err(|error| HttpStartError::Wire(HttpWireError(error)))?;
    StartWorkflowResponse::try_from(response)
        .map(Json)
        .map_err(HttpStartError::Wire)
}

pub(crate) async fn signal_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<SignalWorkflowRequest>,
) -> Result<Json<ProtoSignalResponse>, HttpWireError> {
    let request = request.try_into().map_err(HttpWireError)?;
    handlers::signal(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn query_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<QueryWorkflowRequest>,
) -> Result<Json<QueryWorkflowResponse>, HttpWireError> {
    let request = request.try_into().map_err(HttpWireError)?;
    let response = handlers::query(state.namespace_guard(), &caller, request)
        .await
        .map_err(HttpWireError)?;
    QueryWorkflowResponse::try_from(response).map(Json)
}

pub(crate) async fn cancel_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<CancelWorkflowRequest>,
) -> Result<Json<ProtoCancelResponse>, HttpWireError> {
    let request = request.try_into().map_err(HttpWireError)?;
    handlers::cancel(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn post_list_workflows(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ListWorkflowsRequest>,
) -> Result<Json<ListWorkflowsResponse>, HttpWireError> {
    let request = request.try_into().map_err(HttpWireError)?;
    let response = handlers::list(state.namespace_guard(), &caller, request)
        .await
        .map_err(HttpWireError)?;
    ListWorkflowsResponse::try_from(response).map(Json)
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
    let summaries = summaries
        .into_iter()
        .map(core_summary_from_store)
        .collect::<Vec<WorkflowSummary>>();
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
    Json(request): Json<DescribeWorkflowRequest>,
) -> Result<Json<aion_core::DescribeWorkflowResponse>, HttpWireError> {
    let request = request.try_into().map_err(HttpWireError)?;
    let response = handlers::describe(state.namespace_guard(), &caller, request)
        .await
        .map_err(HttpWireError)?;
    describe_response_to_ops_console(&response).map(Json)
}

/// List the namespaces the caller can select, sorted.
///
/// Backs the ops console's namespace discovery (`client.listNamespaces()` ->
/// `GET /namespaces`). Returns the REAL durable set from the registry
/// ([`ServerState::namespace_store`]), filtered by the caller's grant: an
/// OPERATOR (auth-off single-tenant mode) sees every durable namespace, while an
/// enumerated caller sees only the namespaces it [`CallerIdentity::can_access`].
///
/// The filter is the existence-leak boundary (CVE-2025-14986 family): a caller
/// must never learn that a namespace it cannot access exists, so unauthorized
/// names are dropped before the response is built. The result is sorted and
/// deduplicated, keeping the `Vec<String>` response shape the ops console reads.
pub(crate) async fn list_namespaces(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Result<Json<Vec<String>>, HttpWireError> {
    let records = state
        .namespace_store()
        .list_namespaces()
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))?;
    let mut names: Vec<String> = records
        .into_iter()
        .map(|record| record.name)
        .filter(|name| caller.can_access(name))
        .collect();
    names.sort();
    names.dedup();
    Ok(Json(names))
}

/// Request body for an explicit operator namespace create (`POST /namespaces`).
#[derive(serde::Deserialize)]
pub(crate) struct CreateNamespaceRequest {
    /// The namespace name to create. Free-form, exactly as carried elsewhere on
    /// the wire; must be non-empty.
    name: String,
}

/// Response for an explicit namespace create: the resulting name plus whether
/// this call brought the durable record into being or observed an existing one.
#[derive(serde::Serialize)]
pub(crate) struct CreateNamespaceResponse {
    /// The durable namespace name.
    name: String,
    /// `true` when this call minted the record, `false` when it already existed
    /// (the idempotent re-create path).
    created: bool,
}

/// Explicit operator namespace create (`POST /namespaces`).
///
/// Auth-scoped: the caller must be authorized for the requested namespace via
/// the SAME grant check the access path runs ([`NamespaceGuard::authorize_namespace`]),
/// so a caller can never create — or learn the existence of — a namespace it
/// cannot access. Idempotent: the durable upsert via `register_namespace` mints
/// the record on the first call and reconciles a subsequent call as an existing
/// record, reporting which occurred through `created`.
pub(crate) async fn post_namespace(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<CreateNamespaceRequest>,
) -> Result<Json<CreateNamespaceResponse>, HttpWireError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(HttpWireError(aion_proto::WireError::invalid_input(
            "namespace name must not be empty",
        )));
    }
    let authorized = state
        .namespace_guard()
        .authorize_namespace(&caller, name)
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let outcome = state
        .namespace_store()
        .register_namespace(&authorized, aion_store::NamespaceOrigin::Explicit)
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))?;
    Ok(Json(CreateNamespaceResponse {
        name: authorized,
        created: matches!(outcome, aion_store::MintOutcome::Created),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{WorkflowStatus, WorkflowSummary};
    use aion_proto::{WireError, WireErrorCode};
    use aion_store::{
        WriteToken,
        visibility::{VisibilityRecord, VisibilityStore},
    };
    use axum::{Router, http::StatusCode};
    use chrono::Utc;
    use serde_json::json;
    use tower::ServiceExt;

    use super::super::router::workflow_router;
    use super::super::test_support::{
        NAMESPACE, get_request, json_request, read_json, read_text, run_id, runtime_config,
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
        // Clean wire contract: filter is plain JSON with string-keyed
        // predicates, and the response carries clean summaries (string ids).
        let list = json!({
            "namespace": NAMESPACE,
            "filter": { "workflow_type": "fixture", "status": "Running" },
        });
        let list_response = router
            .oneshot(json_request("/workflows/list", &list)?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: serde_json::Value = read_json(list_response).await?;
        let summaries = list_body["summaries"]
            .as_array()
            .ok_or("summaries missing")?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0]["workflow_id"],
            workflow_id().to_string(),
            "list summaries must expose clean string ids"
        );
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
        // Clean wire contract: input is plain domain JSON.
        let start = json!({
            "namespace": NAMESPACE,
            "workflow_type": "missing-workflow",
            "input": { "fixture": "input" },
        });
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

        let list = json!({ "namespace": NAMESPACE });
        let list_response = router
            .oneshot(json_request("/workflows/list", &list)?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: serde_json::Value = read_json(list_response).await?;
        assert_eq!(
            list_body["summaries"]
                .as_array()
                .ok_or("summaries missing")?
                .len(),
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

        // Clean wire contract: workflow_id is a plain UUID string.
        let describe = json!({
            "namespace": NAMESPACE,
            "workflow_id": coordinator_id.to_string(),
            "run_id": null,
            "include_history": false,
        });
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

        // Clean wire contract: ids are plain UUID strings (matches the
        // ops console's getHistory request body).
        let describe = json!({
            "namespace": NAMESPACE,
            "workflow_id": workflow_id().to_string(),
            "run_id": run_id().to_string(),
            "include_history": true,
        });
        let response = router
            .oneshot(json_request("/workflows/describe", &describe)?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        // Clean wire contract: the describe response is the generated
        // `DescribeWorkflowResponse` shape — a `WorkflowSummary` projection
        // (workflow_id/workflow_type/status/started_at/ended_at/parent) plus a
        // plain `Event[]` history the ops console decodes directly.
        let body: serde_json::Value = read_json(response).await?;
        assert_eq!(
            body["summary"]["workflow_id"],
            workflow_id().to_string(),
            "summary carries the generated WorkflowSummary fields, not a proto envelope"
        );
        assert_eq!(body["summary"]["workflow_type"], "fixture");
        assert!(
            body["summary"]["started_at"].is_string(),
            "summary exposes started_at, matching the generated TS type"
        );
        assert_eq!(
            body["history"][0]["type"], "WorkflowStarted",
            "history entries are plain Event JSON the ops console decodes directly"
        );
        assert_eq!(
            body["history"][0]["data"]["workflow_type"], "fixture",
            "the decoded WorkflowStarted event carries its workflow_type"
        );
        Ok(())
    }

    /// Build a router whose durable namespace registry is seeded with `seed`,
    /// over a `SharedEngine`-mode resolver. The returned `Arc<dyn NamespaceStore>`
    /// is the SAME store the handlers read/write, so a test can assert durable
    /// reads after a `POST`.
    async fn router_with_seeded_namespaces(
        config: crate::config::RuntimeConfig,
        seed: &[&str],
    ) -> Result<(Router, Arc<dyn aion_store::NamespaceStore>), Box<dyn std::error::Error>> {
        let store: Arc<dyn aion_store::EventStore> = Arc::new(aion_store::InMemoryStore::default());
        let engine = Arc::new(
            aion::EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let namespace_store: Arc<dyn aion_store::NamespaceStore> =
            Arc::new(aion_store::InMemoryStore::default());
        for name in seed {
            namespace_store
                .register_namespace(name, aion_store::NamespaceOrigin::Explicit)
                .await?;
        }
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let state = crate::ServerState::from_parts_with_namespace_store(
            resolver,
            config,
            Arc::clone(&namespace_store),
        );
        Ok((workflow_router(state), namespace_store))
    }

    /// Request to `GET /namespaces` as an enumerated caller granted exactly the
    /// `tenant-a` namespace (the dev-header grant under the non-auth path, the
    /// signed `namespace` claim under the auth path).
    fn list_request_for_tenant_a()
    -> Result<axum::http::Request<axum::body::Body>, Box<dyn std::error::Error>> {
        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
        #[cfg(not(feature = "auth"))]
        let bearer = super::super::test_support::TOKEN.to_owned();
        Ok(axum::http::Request::builder()
            .uri("/namespaces")
            .method("GET")
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", NAMESPACE)
            .body(axum::body::Body::empty())?)
    }

    /// `GET /namespaces` returns the REAL durable set filtered by the caller's
    /// grant: an enumerated caller sees ONLY the durable namespaces it can
    /// access, never the existence of namespaces it cannot (anti-existence-leak),
    /// and the response is JSON, not the ops-console SPA HTML.
    #[tokio::test]
    async fn list_namespaces_returns_durable_set_filtered_for_enumerated_caller()
    -> Result<(), Box<dyn std::error::Error>> {
        // Seed three durable namespaces; the enumerated caller is granted only
        // `tenant-a` (NAMESPACE). `tenant-b` and `secret` must never appear.
        let (router, _store) =
            router_with_seeded_namespaces(runtime_config(), &[NAMESPACE, "tenant-b", "secret"])
                .await?;

        let response = router.oneshot(list_request_for_tenant_a()?).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "GET /namespaces must return JSON, not the ops console SPA HTML"
        );
        let body = read_text(response).await?;
        assert!(!body.contains('<'), "must not return HTML: {body}");
        let namespaces: Vec<String> = serde_json::from_str(&body)?;
        assert_eq!(
            namespaces,
            vec![NAMESPACE.to_owned()],
            "enumerated caller sees only its authorized durable namespace, never the others' existence"
        );
        Ok(())
    }

    /// The operator (auth-off single-tenant mode) sees EVERY durable namespace,
    /// sorted — the real registry set, not the synthetic configured-namespace
    /// echo the stopgap returned.
    #[tokio::test]
    async fn list_namespaces_returns_full_durable_set_for_operator()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = runtime_config();
        config.auth.enabled = false;
        let (router, _store) =
            router_with_seeded_namespaces(config, &["zeta", "alpha", "tenant-a"]).await?;

        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/namespaces")
                    .method("GET")
                    .body(axum::body::Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let namespaces: Vec<String> = read_json(response).await?;
        assert_eq!(
            namespaces,
            vec!["alpha".to_owned(), "tenant-a".to_owned(), "zeta".to_owned()],
            "operator sees the full durable set, sorted"
        );
        Ok(())
    }

    /// `POST /namespaces` is idempotent: the first create mints the record
    /// (`created = true`), a second create observes the existing one
    /// (`created = false`), and the durable store holds exactly one record.
    #[tokio::test]
    async fn post_namespace_is_idempotent_create_then_already_existed()
    -> Result<(), Box<dyn std::error::Error>> {
        // Operator mode so the caller is authorized for the namespace it creates.
        let mut config = runtime_config();
        config.auth.enabled = false;
        let (router, store) = router_with_seeded_namespaces(config, &[]).await?;

        let first = router
            .clone()
            .oneshot(json_request("/namespaces", &json!({ "name": "orders" }))?)
            .await?;
        assert_eq!(first.status(), StatusCode::OK);
        let first_body: serde_json::Value = read_json(first).await?;
        assert_eq!(first_body["name"], "orders");
        assert_eq!(
            first_body["created"], true,
            "first create must mint the record"
        );

        let second = router
            .oneshot(json_request("/namespaces", &json!({ "name": "orders" }))?)
            .await?;
        assert_eq!(second.status(), StatusCode::OK);
        let second_body: serde_json::Value = read_json(second).await?;
        assert_eq!(
            second_body["created"], false,
            "second create must observe the existing record (idempotent)"
        );

        // Exactly one durable record, read back from the same store.
        let listed = store.list_namespaces().await?;
        assert_eq!(
            listed.iter().filter(|r| r.name == "orders").count(),
            1,
            "an idempotent create yields exactly one durable record"
        );
        let record = store
            .get_namespace("orders")
            .await?
            .ok_or("created namespace must be durably retrievable")?;
        assert_eq!(record.origin, aion_store::NamespaceOrigin::Explicit);
        Ok(())
    }

    /// `POST /namespaces` is auth-scoped: an enumerated caller cannot create a
    /// namespace it has no grant for, and the attempt writes NOTHING durably
    /// (no enumeration oracle, no unauthorized mint).
    #[cfg(not(feature = "auth"))]
    #[tokio::test]
    async fn post_namespace_rejects_unauthorized_caller() -> Result<(), Box<dyn std::error::Error>>
    {
        // Auth-enabled, enumerated caller granted only `tenant-a` (via
        // `json_request`'s `x-aion-namespaces` header), attempting to create
        // `forbidden`.
        let (router, store) = router_with_seeded_namespaces(runtime_config(), &[]).await?;

        let response = router
            .oneshot(json_request(
                "/namespaces",
                &json!({ "name": "forbidden" }),
            )?)
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a caller without a grant must be denied namespace create"
        );
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::NamespaceDenied);

        // The denial must not have minted anything: no durable trace of the
        // unauthorized namespace.
        assert!(
            store.get_namespace("forbidden").await?.is_none(),
            "an unauthorized create must write nothing durably"
        );
        Ok(())
    }

    /// `POST /namespaces` rejects an empty name with a typed `invalid_input`
    /// wire error rather than panicking or minting a blank record.
    #[tokio::test]
    async fn post_namespace_rejects_empty_name() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = runtime_config();
        config.auth.enabled = false;
        let (router, store) = router_with_seeded_namespaces(config, &[]).await?;

        let response = router
            .oneshot(json_request("/namespaces", &json!({ "name": "   " }))?)
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::InvalidInput);
        assert!(
            store.list_namespaces().await?.is_empty(),
            "a rejected create must mint nothing"
        );
        Ok(())
    }
}
