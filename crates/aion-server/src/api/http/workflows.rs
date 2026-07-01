//! Workflow management handlers.

use aion_core::WorkflowSummary;
use aion_proto::{
    ProtoCancelResponse, ProtoCountWorkflowsRequest, ProtoListWorkflowsRequest, ProtoSignalResponse,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use std::collections::BTreeSet;

use aion_store::NamespacePlacement;

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

/// One durable namespace registry row projected for the ops console's namespace
/// panel columns (`GET /namespaces/records`).
///
/// Carries exactly the registry fields the live panel renders — name,
/// `created_at`, `last_seen`, and the stable `snake_case` `origin` label — so the
/// console can render the created / last-seen / origin columns without a second
/// fetch and reconcile them against the live `namespace created` socket delta.
/// `created_at`/`last_seen` are RFC 3339 strings (matching the durable record's
/// own instant encoding), so the wire form is timezone-explicit and the TS side
/// parses them with `Date`.
#[derive(serde::Serialize)]
pub(crate) struct NamespaceRecordSummary {
    /// The namespace name (registry primary key).
    name: String,
    /// When the registry first minted the namespace, RFC 3339.
    created_at: String,
    /// Most recent reference instant, RFC 3339.
    last_seen: String,
    /// How the namespace came to exist, as the stable `snake_case` label
    /// (`worker_mint` / `start_mint` / `explicit` / `inferred_from_state`).
    origin: String,
    /// The durable placement directive, as the stable wire projection (`kind` +
    /// node-label set), so the ops console renders the placement column and a
    /// caller can read back a `PUT /namespaces/{name}/placement` it just set
    /// (Control-Plane Phase 2, P2-P2).
    placement: aion_core::NamespacePlacementWire,
}

impl From<aion_store::NamespaceRecord> for NamespaceRecordSummary {
    fn from(record: aion_store::NamespaceRecord) -> Self {
        Self {
            name: record.name,
            created_at: record.created_at.to_rfc3339(),
            last_seen: record.last_seen.to_rfc3339(),
            origin: namespace_origin_label(record.origin).to_owned(),
            placement: placement_summary(&record.placement),
        }
    }
}

/// Project a durable [`NamespacePlacement`] onto the stable `{kind, nodes}` wire
/// form the record summary carries, matching the cluster socket delta's shape so
/// the console reconciles a freshly-fetched record against a live
/// placement-changed delta by value.
fn placement_summary(placement: &NamespacePlacement) -> aion_core::NamespacePlacementWire {
    match placement {
        NamespacePlacement::Unplaced => aion_core::NamespacePlacementWire {
            kind: "unplaced".to_owned(),
            nodes: Vec::new(),
        },
        NamespacePlacement::Prefer { nodes } => aion_core::NamespacePlacementWire {
            kind: "prefer".to_owned(),
            nodes: nodes.iter().cloned().collect(),
        },
        NamespacePlacement::Pinned { nodes } => aion_core::NamespacePlacementWire {
            kind: "pinned".to_owned(),
            nodes: nodes.iter().cloned().collect(),
        },
    }
}

/// Stable `snake_case` wire label for a [`aion_store::NamespaceOrigin`], matching
/// the label the mint audit event and the `namespace created` socket delta carry,
/// so the console can correlate a freshly-fetched row with a live delta by origin.
const fn namespace_origin_label(origin: aion_store::NamespaceOrigin) -> &'static str {
    match origin {
        aion_store::NamespaceOrigin::WorkerMint => "worker_mint",
        aion_store::NamespaceOrigin::StartMint => "start_mint",
        aion_store::NamespaceOrigin::Explicit => "explicit",
        aion_store::NamespaceOrigin::InferredFromState => "inferred_from_state",
    }
}

/// List the durable namespace RECORDS the caller can see, for the ops console's
/// namespace-panel columns (`GET /namespaces/records`).
///
/// The records counterpart to [`list_namespaces`]: same REAL durable set from the
/// registry, same grant filter (the existence-leak boundary — an unauthorized
/// caller never learns a namespace it cannot access exists), but projecting the
/// full created / last-seen / origin columns rather than only names. The existing
/// `GET /namespaces` string-list endpoint is unchanged so the namespace selector
/// keeps working; this is a purely additive endpoint. The result is sorted by
/// `created_at` then name (the registry's own list ordering), so the console
/// renders a stable column.
pub(crate) async fn list_namespace_records(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Result<Json<Vec<NamespaceRecordSummary>>, HttpWireError> {
    let records = state
        .namespace_store()
        .list_namespaces()
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))?;
    let visible = records
        .into_iter()
        .filter(|record| caller.can_access(&record.name))
        .map(NamespaceRecordSummary::from)
        .collect();
    Ok(Json(visible))
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
    // Route the explicit create through the shared mint choke-point so a
    // genuinely-new operator-minted namespace emits the SAME live "namespace
    // created" socket delta the worker-register (S5) and workflow-start (S6)
    // seams do — one durable delta per genuinely-new namespace, never a second
    // on an idempotent re-create.
    let outcome = state
        .namespace_minter()
        .create_explicit(&authorized)
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    Ok(Json(CreateNamespaceResponse {
        name: authorized,
        created: matches!(outcome, aion_store::MintOutcome::Created),
    }))
}

/// Request body for `PUT /namespaces/{name}/placement` (Control-Plane Phase 2,
/// P2-P2). The flat `{kind, nodes}` shape mirrors the durable
/// [`NamespacePlacement`] enum: `kind` is the `snake_case` variant tag
/// (`unplaced` / `prefer` / `pinned`) and `nodes` is the node-label set. `nodes`
/// is required and non-empty for `prefer`/`pinned`, and MUST be empty (or absent)
/// for `unplaced`.
#[derive(serde::Deserialize)]
pub(crate) struct SetPlacementRequest {
    /// The placement-kind tag: `unplaced` / `prefer` / `pinned`.
    kind: String,
    /// The node-label set. Defaults to empty so `{"kind":"unplaced"}` is valid.
    #[serde(default)]
    nodes: Vec<String>,
}

impl SetPlacementRequest {
    /// Validate and convert the wire body into a durable [`NamespacePlacement`],
    /// or a typed `invalid_input` wire error. Rejects an unknown `kind`, an empty
    /// label set for `prefer`/`pinned`, a non-empty set for `unplaced`, and any
    /// empty / blank label (a label set is free-form but never blank).
    fn into_placement(self) -> Result<NamespacePlacement, aion_proto::WireError> {
        let labels = self.parse_labels()?;
        match self.kind.as_str() {
            "unplaced" => {
                if labels.is_empty() {
                    Ok(NamespacePlacement::Unplaced)
                } else {
                    Err(aion_proto::WireError::invalid_input(
                        "unplaced placement must not carry node labels",
                    ))
                }
            }
            "prefer" => Ok(NamespacePlacement::Prefer { nodes: labels }),
            "pinned" => Ok(NamespacePlacement::Pinned { nodes: labels }),
            other => Err(aion_proto::WireError::invalid_input(format!(
                "unknown placement kind `{other}`: expected unplaced, prefer, or pinned"
            ))),
        }
    }

    /// Parse the node-label set, rejecting any blank label. For `prefer`/`pinned`
    /// the non-empty requirement is enforced by [`Self::into_placement`]; this
    /// only normalizes and dedups into the deterministic [`BTreeSet`].
    fn parse_labels(&self) -> Result<BTreeSet<String>, aion_proto::WireError> {
        let mut labels = BTreeSet::new();
        for label in &self.nodes {
            let trimmed = label.trim();
            if trimmed.is_empty() {
                return Err(aion_proto::WireError::invalid_input(
                    "placement node labels must not be empty",
                ));
            }
            labels.insert(trimmed.to_owned());
        }
        if matches!(self.kind.as_str(), "prefer" | "pinned") && labels.is_empty() {
            return Err(aion_proto::WireError::invalid_input(
                "prefer/pinned placement requires at least one node label",
            ));
        }
        Ok(labels)
    }
}

/// Set a namespace's durable placement directive (`PUT /namespaces/{name}/placement`).
///
/// Auth-scoped exactly like `POST /namespaces`: the caller must be authorized for
/// the namespace via [`NamespaceGuard::authorize_namespace`], so a caller can
/// never place — or learn the existence of — a namespace it cannot access. The
/// update is an idempotent quorum value-CAS on the record's `placement` field and
/// emits a placement-changed delta on the existing deploy-scoped cluster socket
/// publisher. A namespace with no registry row is a `404`-shaped not-found (the
/// placement targets an already-minted namespace; this endpoint never mints).
pub(crate) async fn set_namespace_placement(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(name): Path<String>,
    Json(request): Json<SetPlacementRequest>,
) -> Result<Json<CreateNamespaceResponse>, HttpWireError> {
    let placement = request.into_placement().map_err(HttpWireError)?;
    let authorized = state
        .namespace_guard()
        .authorize_namespace(&caller, name.trim())
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let set = state
        .namespace_minter()
        .set_placement(&authorized, placement)
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    if !set {
        return Err(HttpWireError(aion_proto::WireError::not_found(format!(
            "namespace {authorized} does not exist"
        ))));
    }
    Ok(Json(CreateNamespaceResponse {
        name: authorized,
        created: false,
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
    use futures::StreamExt;
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
                failed_step: None,
                failure_reason: None,
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
                failed_step: None,
                failure_reason: None,
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
                failed_step: None,
                failure_reason: None,
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
        // Build the state exactly as the compiled auth path requires: under
        // `feature = "auth"` an enumerated caller's bearer is validated against an
        // injected `JwksCache` (fed by a live fixture JWKS endpoint), so the
        // seeded-registry state MUST carry one — otherwise the auth extractor sees
        // no cache and rejects the caller with 401 regardless of the token.
        #[cfg(feature = "auth")]
        let state = {
            let url = crate::auth::test_support::serve_jwks()?;
            let refresh = std::time::Duration::from_secs(config.auth.jwks_refresh_seconds);
            let cache = crate::auth::JwksCache::new(url, refresh).await?;
            crate::ServerState::from_parts_with_namespace_store_and_jwks(
                resolver,
                config,
                Arc::clone(&namespace_store),
                cache,
            )
        };
        #[cfg(not(feature = "auth"))]
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

    /// `GET /namespaces/records` returns the REAL durable RECORDS (the columns
    /// the ops console panel renders) filtered by the caller's grant: an
    /// enumerated caller sees only the records for namespaces it can access, never
    /// the existence of namespaces it cannot (same anti-existence-leak boundary as
    /// the string-list endpoint), and each record carries name + `created_at` +
    /// `last_seen` + the `snake_case` origin label.
    #[tokio::test]
    async fn list_namespace_records_returns_durable_records_filtered_for_enumerated_caller()
    -> Result<(), Box<dyn std::error::Error>> {
        let (router, _store) =
            router_with_seeded_namespaces(runtime_config(), &[NAMESPACE, "tenant-b", "secret"])
                .await?;

        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
        #[cfg(not(feature = "auth"))]
        let bearer = super::super::test_support::TOKEN.to_owned();
        let request = axum::http::Request::builder()
            .uri("/namespaces/records")
            .method("GET")
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", NAMESPACE)
            .body(axum::body::Body::empty())?;

        let response = router.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let records: Vec<serde_json::Value> = read_json(response).await?;
        assert_eq!(
            records.len(),
            1,
            "enumerated caller sees only its authorized namespace's record"
        );
        assert_eq!(records[0]["name"], NAMESPACE);
        assert_eq!(
            records[0]["origin"], "explicit",
            "origin is the stable snake_case label the seed minted with"
        );
        assert!(
            records[0]["created_at"].is_string() && records[0]["last_seen"].is_string(),
            "each record carries RFC 3339 created_at + last_seen columns: {records:?}"
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

    /// Build a router PLUS the `ServerState` (so a test can subscribe to the
    /// SAME cluster publisher the handlers emit on), over a `SharedEngine`-mode
    /// resolver with `seed` namespaces pre-minted into the durable registry.
    async fn router_state_with_seeded_namespaces(
        config: crate::config::RuntimeConfig,
        seed: &[&str],
    ) -> Result<(Router, crate::ServerState), Box<dyn std::error::Error>> {
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
        let state =
            crate::ServerState::from_parts_with_namespace_store(resolver, config, namespace_store);
        Ok((workflow_router(state.clone()), state))
    }

    /// Build a `PUT /namespaces/{name}/placement` request with the given JSON body,
    /// authorized for `name` exactly like the namespace-create path.
    fn put_placement_request(
        name: &str,
        body: &serde_json::Value,
    ) -> Result<axum::http::Request<axum::body::Body>, Box<dyn std::error::Error>> {
        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", name)?;
        #[cfg(not(feature = "auth"))]
        let bearer = super::super::test_support::TOKEN.to_owned();
        Ok(axum::http::Request::builder()
            .uri(format!("/namespaces/{name}/placement"))
            .method("PUT")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", name)
            .body(axum::body::Body::from(serde_json::to_vec(body)?))?)
    }

    /// `PUT /namespaces/{name}/placement` durably sets the placement (read back via
    /// `GET /namespaces/records`), is idempotent, and emits exactly one
    /// placement-changed delta on the existing cluster publisher.
    #[tokio::test]
    async fn put_placement_sets_reads_back_and_emits_delta()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = runtime_config();
        config.auth.enabled = false;
        let (router, state) = router_state_with_seeded_namespaces(config, &["orders"]).await?;
        let mut deltas = state.cluster_publisher().subscribe(0);

        let body = json!({ "kind": "prefer", "nodes": ["n2", "n1"] });
        let response = router
            .clone()
            .oneshot(put_placement_request("orders", &body)?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        // Read back via GET /namespaces/records: placement is durably set, with the
        // deterministically-ordered label set.
        let records = router
            .clone()
            .oneshot(get_request("/namespaces/records")?)
            .await?;
        let records: Vec<serde_json::Value> = read_json(records).await?;
        let orders = records
            .iter()
            .find(|r| r["name"] == "orders")
            .ok_or("orders record must exist")?;
        assert_eq!(orders["placement"]["kind"], "prefer");
        assert_eq!(
            orders["placement"]["nodes"],
            json!(["n1", "n2"]),
            "labels are stored deterministically ordered"
        );

        // Exactly one placement-changed delta on the existing cluster publisher.
        let event = deltas
            .next()
            .await
            .ok_or("expected a placement-changed delta")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        match event {
            aion_core::ClusterEvent::NamespacePlacementChanged {
                name, placement, ..
            } => {
                assert_eq!(name, "orders");
                assert_eq!(placement.kind, "prefer");
                assert_eq!(placement.nodes, vec!["n1".to_owned(), "n2".to_owned()]);
            }
            other => {
                return Err(format!("expected NamespacePlacementChanged, got {other:?}").into());
            }
        }

        // Idempotent re-apply: still 200, still durable.
        let again = router
            .oneshot(put_placement_request("orders", &body)?)
            .await?;
        assert_eq!(again.status(), StatusCode::OK);
        Ok(())
    }

    /// `PUT /namespaces/{name}/placement` is auth-scoped: an enumerated caller
    /// without a grant for the namespace is rejected (FORBIDDEN), and nothing is
    /// written durably.
    #[cfg(not(feature = "auth"))]
    #[tokio::test]
    async fn put_placement_rejects_unauthorized_caller() -> Result<(), Box<dyn std::error::Error>> {
        // Auth-enabled (runtime_config default), caller granted only `tenant-a`
        // (NAMESPACE), attempting to place `forbidden`.
        let (router, state) =
            router_state_with_seeded_namespaces(runtime_config(), &["forbidden"]).await?;

        // Grant the caller ONLY `tenant-a` (NAMESPACE), but PUT placement on
        // `forbidden`: the grant header names a different namespace than the path.
        let body = json!({ "kind": "prefer", "nodes": ["n1"] });
        let bearer = super::super::test_support::TOKEN.to_owned();
        let request = axum::http::Request::builder()
            .uri("/namespaces/forbidden/placement")
            .method("PUT")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", NAMESPACE)
            .body(axum::body::Body::from(serde_json::to_vec(&body)?))?;
        let response = router.oneshot(request).await?;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a caller without a grant must be denied placement"
        );
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::NamespaceDenied);

        // The denial wrote nothing: placement is still the Unplaced default.
        let record = state
            .namespace_store()
            .get_namespace("forbidden")
            .await?
            .ok_or("seeded namespace must exist")?;
        assert_eq!(record.placement, aion_store::NamespacePlacement::Unplaced);
        Ok(())
    }

    /// `PUT /namespaces/{name}/placement` on an absent namespace is a not-found,
    /// and mints nothing (placement targets an already-minted namespace).
    #[tokio::test]
    async fn put_placement_absent_namespace_is_not_found() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = runtime_config();
        config.auth.enabled = false;
        let (router, state) = router_state_with_seeded_namespaces(config, &[]).await?;

        let body = json!({ "kind": "prefer", "nodes": ["n1"] });
        let response = router
            .oneshot(put_placement_request("ghost", &body)?)
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            state
                .namespace_store()
                .get_namespace("ghost")
                .await?
                .is_none(),
            "a not-found placement must mint nothing"
        );
        Ok(())
    }

    /// `PUT /namespaces/{name}/placement` validates the body: an unknown kind, an
    /// empty label set for prefer/pinned, and a non-empty set for unplaced are all
    /// typed `invalid_input` wire errors that write nothing.
    #[tokio::test]
    async fn put_placement_rejects_invalid_bodies() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = runtime_config();
        config.auth.enabled = false;
        let (router, state) = router_state_with_seeded_namespaces(config, &["orders"]).await?;

        for body in [
            json!({ "kind": "elsewhere", "nodes": ["n1"] }),
            json!({ "kind": "prefer", "nodes": [] }),
            json!({ "kind": "prefer", "nodes": ["  "] }),
            json!({ "kind": "unplaced", "nodes": ["n1"] }),
        ] {
            let response = router
                .clone()
                .oneshot(put_placement_request("orders", &body)?)
                .await?;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "invalid placement body must be rejected: {body}"
            );
            let error: WireError = read_json(response).await?;
            assert_eq!(error.code, WireErrorCode::InvalidInput);
        }

        // Nothing was written: still the Unplaced default.
        let record = state
            .namespace_store()
            .get_namespace("orders")
            .await?
            .ok_or("seeded namespace must exist")?;
        assert_eq!(record.placement, aion_store::NamespacePlacement::Unplaced);
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
