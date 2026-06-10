//! axum HTTP/JSON workflow facade.

use std::collections::HashMap;

use aion_core::{SearchAttributeValue, WorkflowStatus};
use aion_proto::{
    FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoCancelRequest,
    ProtoCancelResponse, ProtoCountWorkflowsRequest, ProtoCreateScheduleRequest,
    ProtoCreateScheduleResponse, ProtoDeleteScheduleResponse, ProtoDescribeScheduleResponse,
    ProtoDescribeWorkflowRequest, ProtoDescribeWorkflowResponse, ProtoListSchedulesRequest,
    ProtoListSchedulesResponse, ProtoListWorkflowsRequest, ProtoListWorkflowsResponse,
    ProtoPauseScheduleResponse, ProtoQueryRequest, ProtoQueryResponse, ProtoResumeScheduleResponse,
    ProtoScheduleId, ProtoScheduleIdRequest, ProtoSignalRequest, ProtoSignalResponse,
    ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, ProtoUpdateScheduleRequest,
    ProtoUpdateScheduleResponse, ProtoWorkflowId, SubscriptionRequest, WireEnvelope, WireError,
    WireErrorCode, subscription_request,
};
use aion_store::visibility::{ListWorkflowsFilter, SearchAttributePredicate, WorkflowSummary};
#[cfg(feature = "auth")]
use axum::http::header;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        FromRequestParts, Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    CallerIdentity, NamespaceOperation, ServerError, ServerState, api::handlers, dashboard::assets,
    observability, stream::handle_subscription_socket,
};

const JSON_CONTENT_TYPE: &str = "application/json";

/// Build the public HTTP application: workflow-management routes first, then
/// the dashboard static asset fallback. The dashboard adds no data API.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when dashboard assets are misconfigured.
pub fn http_router(state: ServerState) -> Result<Router, ServerError> {
    let dashboard = assets::dashboard_router(&state.runtime_config().dashboard)?;
    let metrics = state.metrics().cloned();
    let health = state.health().cloned();
    let mut router = workflow_router(state);
    if let Some(metrics) = metrics {
        router = router.merge(Router::new().route(
            "/metrics",
            get(observability::metrics::metrics_handler).with_state(metrics),
        ));
    }
    if let Some(health) = health {
        router = router.merge(
            Router::new()
                .route("/health/live", get(observability::health::live))
                .route(
                    "/health/ready",
                    get(observability::health::ready).with_state(health),
                ),
        );
    }
    Ok(router.merge(dashboard))
}

/// Build the public workflow-management HTTP router.
pub fn workflow_router(state: ServerState) -> Router {
    Router::new()
        .route("/workflows", get(get_workflows))
        .route("/workflows/count", get(count_workflows))
        .route("/workflows/start", post(start_workflow))
        .route("/workflows/signal", post(signal_workflow))
        .route("/workflows/query", post(query_workflow))
        .route("/workflows/cancel", post(cancel_workflow))
        .route("/workflows/list", post(post_list_workflows))
        .route("/workflows/describe", post(describe_workflow))
        .route("/events/stream", get(subscribe_events_socket))
        .route("/schedules", post(create_schedule).get(list_schedules))
        .route(
            "/schedules/{id}",
            get(describe_schedule)
                .put(update_schedule)
                .delete(delete_schedule),
        )
        .route("/schedules/{id}/pause", post(pause_schedule))
        .route("/schedules/{id}/resume", post(resume_schedule))
        .with_state(state)
}

#[derive(Deserialize)]
struct NamespaceQuery {
    namespace: String,
}

struct HttpCaller(CallerIdentity);

impl FromRequestParts<ServerState> for HttpCaller {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        let caller = caller_from_headers(&parts.headers, state)
            .await
            .map_err(axum::response::IntoResponse::into_response)?;
        Ok(Self(caller))
    }
}

async fn subscribe_events_socket(
    websocket: WebSocketUpgrade,
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Response {
    websocket
        .on_upgrade(move |socket| async move {
            if let Err(error) = serve_subscription_socket(socket, state, caller).await {
                tracing::warn!(error = %error, "websocket event subscription ended with an error");
            }
        })
        .into_response()
}

async fn start_workflow(
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

async fn signal_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoSignalRequest>,
) -> Result<Json<ProtoSignalResponse>, HttpWireError> {
    handlers::signal(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

async fn query_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoQueryRequest>,
) -> Result<Json<ProtoQueryResponse>, HttpWireError> {
    handlers::query(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

async fn cancel_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoCancelRequest>,
) -> Result<Json<ProtoCancelResponse>, HttpWireError> {
    handlers::cancel(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

async fn post_list_workflows(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoListWorkflowsRequest>,
) -> Result<Json<ProtoListWorkflowsResponse>, HttpWireError> {
    handlers::list(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

async fn get_workflows(
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
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let filter = query.into_filter().map_err(HttpWireError)?;
    scoped
        .engine()
        .map_err(|error| HttpWireError(error.to_wire_error()))?
        .visibility_store()
        .list_workflows(filter)
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))
        .map(Json)
}

async fn count_workflows(
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
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    let filter = query.into_filter().map_err(HttpWireError)?;
    let count = scoped
        .engine()
        .map_err(|error| HttpWireError(error.to_wire_error()))?
        .visibility_store()
        .count_workflows(filter)
        .await
        .map_err(|error| HttpWireError(ServerError::from(error).to_wire_error()))?;

    Ok(Json(CountWorkflowsBody { count }))
}

async fn describe_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoDescribeWorkflowRequest>,
) -> Result<Json<HttpDescribeWorkflowResponse>, HttpWireError> {
    let response = handlers::describe(state.namespace_guard(), &caller, request)
        .await
        .map_err(HttpWireError)?;
    HttpDescribeWorkflowResponse::try_from(response).map(Json)
}

async fn create_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoCreateScheduleRequest>,
) -> Result<(StatusCode, Json<ProtoCreateScheduleResponse>), HttpWireError> {
    handlers::create_schedule(state.namespace_guard(), &caller, request)
        .await
        .map(|response| (StatusCode::CREATED, Json(response)))
        .map_err(HttpWireError)
}

async fn update_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Json(mut request): Json<ProtoUpdateScheduleRequest>,
) -> Result<Json<ProtoUpdateScheduleResponse>, HttpWireError> {
    request.schedule_id = Some(ProtoScheduleId { uuid: id });
    handlers::update_schedule(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

async fn pause_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoPauseScheduleResponse>, HttpWireError> {
    handlers::pause_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

async fn resume_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoResumeScheduleResponse>, HttpWireError> {
    handlers::resume_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

async fn delete_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoDeleteScheduleResponse>, HttpWireError> {
    handlers::delete_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

async fn list_schedules(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoListSchedulesResponse>, HttpWireError> {
    handlers::list_schedules(
        state.namespace_guard(),
        &caller,
        ProtoListSchedulesRequest {
            namespace: query.namespace,
        },
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

async fn describe_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoDescribeScheduleResponse>, HttpWireError> {
    handlers::describe_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

fn schedule_id_request(query: NamespaceQuery, id: String) -> ProtoScheduleIdRequest {
    ProtoScheduleIdRequest {
        namespace: query.namespace,
        schedule_id: Some(ProtoScheduleId { uuid: id }),
    }
}

#[derive(Debug, Deserialize)]
struct VisibilityQuery {
    namespace: String,
    workflow_type: Option<String>,
    status: Option<String>,
    started_after: Option<String>,
    started_before: Option<String>,
    closed_after: Option<String>,
    closed_before: Option<String>,
    search_attributes: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    #[serde(flatten)]
    extra: HashMap<String, String>,
}

#[derive(serde::Serialize)]
struct CountWorkflowsBody {
    count: u64,
}

#[derive(Debug, Deserialize)]
struct HttpStartWorkflowRequest {
    namespace: String,
    workflow_type: String,
    input: Option<Value>,
}

#[derive(Debug, Serialize)]
struct HttpDescribeWorkflowResponse {
    summary: Option<HttpEnvelope>,
    history: Vec<HttpEnvelope>,
}

#[derive(Debug, Serialize)]
struct HttpEnvelope {
    namespace: String,
    request_id: Option<String>,
    payload: Option<HttpPayload>,
}

#[derive(Debug, Serialize)]
struct HttpPayload {
    content_type: String,
    data: Value,
}

fn decode_start_workflow_request(body: &[u8]) -> Result<ProtoStartWorkflowRequest, HttpWireError> {
    serde_json::from_slice::<HttpStartWorkflowRequest>(body)
        .map_err(|_error| HttpWireError(invalid_start_input()))?
        .try_into()
        .map_err(HttpWireError)
}

impl TryFrom<HttpStartWorkflowRequest> for ProtoStartWorkflowRequest {
    type Error = WireError;

    fn try_from(request: HttpStartWorkflowRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: request.namespace,
            workflow_type: request.workflow_type,
            input: request.input.map(http_input_payload).transpose()?,
        })
    }
}

fn http_input_payload(input: Value) -> Result<aion_proto::convert::ProtoPayload, WireError> {
    if is_payload_envelope(&input) {
        serde_json::from_value(input).map_err(|_error| invalid_start_input())
    } else {
        serde_json::to_vec(&input)
            .map(|bytes| aion_proto::convert::ProtoPayload {
                content_type: JSON_CONTENT_TYPE.to_owned(),
                bytes,
            })
            .map_err(|_error| invalid_start_input())
    }
}

fn is_payload_envelope(input: &Value) -> bool {
    input
        .as_object()
        .is_some_and(|object| object.contains_key("content_type") && object.contains_key("bytes"))
}

fn invalid_start_input() -> WireError {
    WireError::invalid_input(
        "start workflow request must be JSON shaped like \
         {\"namespace\":\"tenant-a\",\"workflow_type\":\"example\",\"input\":{\"name\":\"Ada\"}} \
         or {\"namespace\":\"tenant-a\",\"workflow_type\":\"example\",\"input\":{\"content_type\":\"application/json\",\"bytes\":[123,125]}}",
    )
}

impl TryFrom<ProtoDescribeWorkflowResponse> for HttpDescribeWorkflowResponse {
    type Error = HttpWireError;

    fn try_from(response: ProtoDescribeWorkflowResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            summary: response.summary.map(HttpEnvelope::try_from).transpose()?,
            history: response
                .history
                .into_iter()
                .map(HttpEnvelope::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl TryFrom<WireEnvelope> for HttpEnvelope {
    type Error = HttpWireError;

    fn try_from(envelope: WireEnvelope) -> Result<Self, Self::Error> {
        Ok(Self {
            namespace: envelope.namespace,
            request_id: envelope.request_id,
            payload: envelope.payload.map(HttpPayload::try_from).transpose()?,
        })
    }
}

impl TryFrom<aion_proto::convert::ProtoPayload> for HttpPayload {
    type Error = HttpWireError;

    fn try_from(payload: aion_proto::convert::ProtoPayload) -> Result<Self, Self::Error> {
        let content_type = payload.content_type;
        Ok(Self {
            data: payload_data(&content_type, &payload.bytes)?,
            content_type,
        })
    }
}

fn http_payload_content_type(content_type: &str) -> &str {
    if content_type == "Json" {
        JSON_CONTENT_TYPE
    } else {
        content_type
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let normalized = http_payload_content_type(content_type);
    normalized
        .split_once(';')
        .map_or(normalized, |(media_type, _parameters)| media_type)
        .trim()
        .eq_ignore_ascii_case(JSON_CONTENT_TYPE)
}

fn payload_data(content_type: &str, bytes: &[u8]) -> Result<Value, HttpWireError> {
    if is_json_content_type(content_type) {
        let value = serde_json::from_slice(bytes).map_err(|_error| {
            HttpWireError(WireError::backend(
                "application/json payload contains invalid JSON",
            ))
        })?;
        rewrite_payload_values(value)
    } else {
        Ok(Value::String(BASE64_STANDARD.encode(bytes)))
    }
}

fn rewrite_payload_values(value: Value) -> Result<Value, HttpWireError> {
    match value {
        Value::Array(values) => values
            .into_iter()
            .map(rewrite_payload_values)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object)
            if object.contains_key("content_type") && object.contains_key("bytes") =>
        {
            rewrite_payload_object(object)
        }
        Value::Object(object) => object
            .into_iter()
            .map(|(key, value)| rewrite_payload_values(value).map(|value| (key, value)))
            .collect::<Result<Map<_, _>, _>>()
            .map(Value::Object),
        scalar => Ok(scalar),
    }
}

fn rewrite_payload_object(object: Map<String, Value>) -> Result<Value, HttpWireError> {
    let mut payload: aion_proto::convert::ProtoPayload =
        serde_json::from_value(Value::Object(object)).map_err(|_error| {
            HttpWireError(WireError::backend("stored payload envelope is malformed"))
        })?;
    if payload.content_type == "Json" {
        JSON_CONTENT_TYPE.clone_into(&mut payload.content_type);
    }
    let payload = HttpPayload::try_from(payload)?;
    Ok(json!({
        "content_type": payload.content_type,
        "data": payload.data,
    }))
}

impl VisibilityQuery {
    fn into_filter(self) -> Result<ListWorkflowsFilter, WireError> {
        let mut search_attributes = self.parse_search_attributes()?;
        search_attributes.extend(parse_attr_equals(self.extra));

        Ok(ListWorkflowsFilter {
            workflow_type: self.workflow_type,
            status: self.status.as_deref().map(parse_status).transpose()?,
            started_after: self
                .started_after
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            started_before: self
                .started_before
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            closed_after: self
                .closed_after
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            closed_before: self
                .closed_before
                .as_deref()
                .map(parse_datetime)
                .transpose()?,
            search_attributes,
            limit: self.limit,
            offset: self.offset,
        })
    }

    fn parse_search_attributes(&self) -> Result<Vec<SearchAttributePredicate>, WireError> {
        self.search_attributes.as_deref().map_or_else(
            || Ok(Vec::new()),
            |value| {
                serde_json::from_str(value).map_err(|_error| {
                    WireError::unknown_query("search_attributes query parameter is malformed")
                })
            },
        )
    }
}

fn parse_attr_equals(extra: HashMap<String, String>) -> Vec<SearchAttributePredicate> {
    extra
        .into_iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("attr.")
                .map(|name| SearchAttributePredicate::Equals {
                    name: name.to_owned(),
                    value: SearchAttributeValue::String(value),
                })
        })
        .collect()
}

fn parse_status(value: &str) -> Result<WorkflowStatus, WireError> {
    match value.to_ascii_lowercase().as_str() {
        "running" => Ok(WorkflowStatus::Running),
        "completed" => Ok(WorkflowStatus::Completed),
        "failed" => Ok(WorkflowStatus::Failed),
        "cancelled" | "canceled" => Ok(WorkflowStatus::Cancelled),
        "timed_out" | "timedout" | "timed-out" => Ok(WorkflowStatus::TimedOut),
        "continued_as_new" | "continuedasnew" | "continued-as-new" => {
            Ok(WorkflowStatus::ContinuedAsNew)
        }
        _ => Err(WireError::unknown_query(
            "workflow status query parameter is unknown",
        )),
    }
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>, WireError> {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| datetime.with_timezone(&Utc))
        .map_err(|_error| WireError::unknown_query("datetime query parameter is malformed"))
}

async fn caller_from_headers(
    headers: &axum::http::HeaderMap,
    state: &ServerState,
) -> Result<CallerIdentity, HttpAuthError> {
    let auth = &state.runtime_config().auth;
    if !auth.enabled {
        return Ok(development_caller_from_headers(headers));
    }
    #[cfg(feature = "auth")]
    {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_bearer)
            .ok_or(HttpAuthError)?;
        let Some(cache) = state.jwks_cache() else {
            return Err(HttpAuthError);
        };
        return cache
            .validate(&bearer)
            .await
            .map(|claims| claims.caller_identity())
            .map_err(|_error| HttpAuthError);
    }
    #[cfg(not(feature = "auth"))]
    {
        // Yield to preserve the async signature required by the auth-feature branch.
        tokio::task::yield_now().await;
        Ok(development_token_caller_from_headers(headers, auth))
    }
}

fn development_caller_from_headers(headers: &axum::http::HeaderMap) -> CallerIdentity {
    let subject = headers
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty());
    let namespaces = headers
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map_or_else(Vec::new, parse_namespaces);
    CallerIdentity::new(subject.unwrap_or("anonymous"), namespaces)
}

/// Development-mode token authentication used when `auth.enabled` is `true` but
/// the `auth` crate feature is not compiled.  Validates bearer tokens against the
/// configured `jwks_url` value (treated as a static shared secret) and returns
/// [`CallerIdentity::denied`] with a specific reason on each failure mode so the
/// namespace guard surfaces actionable 403 error messages.
fn development_token_caller_from_headers(
    headers: &axum::http::HeaderMap,
    auth: &crate::config::AuthConfig,
) -> CallerIdentity {
    let subject = headers
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty());
    let namespaces = headers
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map_or_else(Vec::new, parse_namespaces);

    let bearer_token = auth.jwks_url.as_deref().unwrap_or_default();
    let expected = format!("Bearer {bearer_token}");
    let Some(authorization) = headers.get("authorization") else {
        return CallerIdentity::denied(
            subject.unwrap_or("anonymous"),
            "missing Authorization header with Bearer token; \
             set authorization to `Bearer <token>` for this server",
        );
    };
    let authorization = authorization.to_str().ok();
    if authorization != Some(expected.as_str()) {
        return CallerIdentity::denied(
            subject.unwrap_or("anonymous"),
            "invalid or expired bearer token; \
             refresh the token and send authorization as `Bearer <token>`",
        );
    }

    let Some(subject) = subject else {
        return CallerIdentity::denied(
            "anonymous",
            "missing required header: x-aion-subject; \
             set x-aion-subject to the caller identity",
        );
    };

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

struct HttpAuthError;

impl IntoResponse for HttpAuthError {
    fn into_response(self) -> Response {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

fn parse_namespaces(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|namespace| !namespace.is_empty())
        .map(str::to_owned)
        .collect()
}

enum HttpStartError {
    Wire(HttpWireError),
    Draining,
}

impl IntoResponse for HttpStartError {
    fn into_response(self) -> Response {
        match self {
            Self::Wire(error) => error.into_response(),
            Self::Draining => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(WireError::backend(
                    "server is draining and not accepting new workflow starts",
                )),
            )
                .into_response(),
        }
    }
}

async fn serve_subscription_socket(
    mut socket: WebSocket,
    state: ServerState,
    caller: CallerIdentity,
) -> Result<(), ServerError> {
    let request = read_subscription_request(&mut socket).await?;
    handle_subscription_socket(socket, &state, &caller, &request).await
}

async fn read_subscription_request(
    socket: &mut WebSocket,
) -> Result<SubscriptionRequest, ServerError> {
    loop {
        let Some(message) = socket.recv().await else {
            return Err(
                WireError::invalid_input("websocket subscription request is missing").into(),
            );
        };
        let message = message.map_err(|source| {
            WireError::invalid_input(format!(
                "failed to read websocket subscription request: {source}"
            ))
        })?;

        match message {
            Message::Text(text) => return decode_subscription_request(text.as_bytes()),
            Message::Binary(bytes) => return decode_subscription_request(&bytes),
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => {
                return Err(WireError::invalid_input(
                    "websocket closed before subscription request",
                )
                .into());
            }
        }
    }
}

fn decode_subscription_request(bytes: &[u8]) -> Result<SubscriptionRequest, ServerError> {
    let value = serde_json::from_slice::<Value>(bytes).map_err(|source| {
        WireError::invalid_input(format!("invalid websocket subscription JSON: {source}"))
    })?;
    decode_subscription_value(&value)
}

fn decode_subscription_value(value: &Value) -> Result<SubscriptionRequest, ServerError> {
    if let Ok(request) = serde_json::from_value::<SubscriptionRequest>(value.clone()) {
        if request.subscription.is_some() {
            return Ok(request);
        }
    }

    let subscription = value.get("subscription").unwrap_or(value);
    let Some(subscription) = subscription.as_object() else {
        return Err(
            WireError::invalid_input("websocket subscription must be a JSON object").into(),
        );
    };

    if let Some(value) = subscription.get("per_workflow") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::PerWorkflow(
                decode_per_workflow_subscription(value)?,
            )),
        });
    }
    if let Some(value) = subscription.get("filtered") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Filtered(
                decode_filtered_subscription(value)?,
            )),
        });
    }
    if let Some(value) = subscription.get("firehose") {
        return Ok(SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Firehose(
                decode_firehose_subscription(value)?,
            )),
        });
    }

    Err(WireError::invalid_input(
        "websocket subscription must contain per_workflow, filtered, or firehose",
    )
    .into())
}

fn decode_per_workflow_subscription(value: &Value) -> Result<PerWorkflowSubscription, ServerError> {
    let object = subscription_object(value, "per-workflow")?;
    Ok(PerWorkflowSubscription {
        namespace: required_string(object, "namespace", "per-workflow subscription")?.to_owned(),
        workflow_id: Some(decode_workflow_id_value(
            object.get("workflow_id").ok_or_else(|| {
                WireError::invalid_input("per-workflow subscription requires workflow_id")
            })?,
        )?),
    })
}

fn decode_filtered_subscription(value: &Value) -> Result<FilteredSubscription, ServerError> {
    let object = subscription_object(value, "filtered")?;
    let status = match object.get("status") {
        Some(Value::String(status)) => Some(decode_status_name(status)?),
        Some(Value::Number(status)) => status.as_i64().and_then(|value| i32::try_from(value).ok()),
        Some(Value::Null) | None => None,
        Some(_other) => None,
    };
    Ok(FilteredSubscription {
        namespace: required_string(object, "namespace", "filtered subscription")?.to_owned(),
        workflow_type: object
            .get("workflow_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status,
        namespace_selector: object
            .get("namespace_selector")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn decode_firehose_subscription(value: &Value) -> Result<FirehoseSubscription, ServerError> {
    let object = subscription_object(value, "firehose")?;
    let namespace = object
        .get("namespace")
        .or_else(|| object.get("namespace_selector"))
        .and_then(Value::as_str)
        .ok_or_else(|| WireError::invalid_input("firehose subscription requires namespace"))?;
    Ok(FirehoseSubscription {
        namespace: namespace.to_owned(),
    })
}

fn subscription_object<'a>(
    value: &'a Value,
    subscription_name: &str,
) -> Result<&'a Map<String, Value>, ServerError> {
    value.as_object().ok_or_else(|| {
        WireError::invalid_input(format!(
            "{subscription_name} subscription must be a JSON object"
        ))
        .into()
    })
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<&'a str, ServerError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| WireError::invalid_input(format!("{context} requires {key}")).into())
}

fn decode_workflow_id_value(value: &Value) -> Result<ProtoWorkflowId, ServerError> {
    if let Some(uuid) = value.as_str() {
        return Ok(ProtoWorkflowId {
            uuid: uuid.to_owned(),
        });
    }
    serde_json::from_value::<ProtoWorkflowId>(value.clone()).map_err(|source| {
        WireError::invalid_input(format!(
            "invalid per-workflow subscription workflow_id: {source}"
        ))
        .into()
    })
}

fn decode_status_name(status: &str) -> Result<i32, ServerError> {
    match status {
        "running" | "Running" => Ok(aion_proto::ProtoWorkflowStatus::Running as i32),
        "completed" | "Completed" => Ok(aion_proto::ProtoWorkflowStatus::Completed as i32),
        "failed" | "Failed" => Ok(aion_proto::ProtoWorkflowStatus::Failed as i32),
        "cancelled" | "Cancelled" | "canceled" | "Canceled" => {
            Ok(aion_proto::ProtoWorkflowStatus::Cancelled as i32)
        }
        "timed_out" | "TimedOut" => Ok(aion_proto::ProtoWorkflowStatus::TimedOut as i32),
        "continued_as_new" | "ContinuedAsNew" => {
            Ok(aion_proto::ProtoWorkflowStatus::ContinuedAsNew as i32)
        }
        other => Err(WireError::invalid_input(format!(
            "invalid workflow status in websocket subscription: {other}"
        ))
        .into()),
    }
}

struct HttpWireError(WireError);

impl IntoResponse for HttpWireError {
    fn into_response(self) -> Response {
        (http_status(self.0.code), Json(self.0)).into_response()
    }
}

fn http_status(code: WireErrorCode) -> StatusCode {
    match code {
        WireErrorCode::NotFound => StatusCode::NOT_FOUND,
        WireErrorCode::NamespaceDenied => StatusCode::FORBIDDEN,
        WireErrorCode::SequenceConflict => StatusCode::CONFLICT,
        WireErrorCode::UnknownQuery | WireErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        WireErrorCode::QueryTimeout => StatusCode::REQUEST_TIMEOUT,
        WireErrorCode::NotRunning => StatusCode::PRECONDITION_FAILED,
        WireErrorCode::Lagged => StatusCode::TOO_MANY_REQUESTS,
        WireErrorCode::Backend => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::SocketAddr, sync::Arc};

    use aion::{EngineBuilder, EventFilter, EventPublisher};
    use aion_core::{
        Event, EventEnvelope, Payload, SearchAttributeValue, WorkflowId, WorkflowStatus,
    };
    use aion_proto::{
        ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoStartWorkflowRequest,
        StreamedEvent, WireError, WireErrorCode, convert::ProtoPayload,
    };
    use aion_store::{
        EventStore, InMemoryStore, WriteToken,
        visibility::{ListWorkflowsFilter, VisibilityRecord, VisibilityStore, WorkflowSummary},
    };
    use axum::{body, http::Request};
    use chrono::Utc;
    use futures::{SinkExt, StreamExt, stream, stream::BoxStream};
    use serde_json::json;
    use tokio::sync::{Semaphore, broadcast};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message as ClientMessage, client::IntoClientRequest},
    };
    use tower::ServiceExt;

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
    async fn websocket_events_route_upgrades_and_streams_client_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = Arc::new(TestEventPublisher::new());
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .event_publisher(publisher.clone())
                .build()
                .await?,
        );
        let ownership = WorkflowOwnership::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver =
            NamespaceResolver::from_parts(NamespaceMode::SharedEngine, Some(engine), ownership);
        let router = workflow_router(ServerState::from_parts(resolver, runtime_config()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "test websocket server exited with error");
            }
        });

        let mut request = format!("ws://{address}/events/stream").into_client_request()?;
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse()?);
        request
            .headers_mut()
            .insert("x-aion-subject", "alice".parse()?);
        request
            .headers_mut()
            .insert("x-aion-namespaces", NAMESPACE.parse()?);
        let (mut socket, response) = connect_async(request).await?;
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

        let subscription = json!({
            "type": "subscribe",
            "subscription_id": "dashboard-test",
            "subscription": {
                "per_workflow": {
                    "namespace": NAMESPACE,
                    "workflow_id": workflow_id().to_string()
                }
            }
        });
        socket
            .send(ClientMessage::Text(subscription.to_string().into()))
            .await?;
        publisher.wait_for_subscription().await;
        publisher.publish(started_event()?)?;

        let Some(frame) = socket.next().await else {
            return Err("websocket closed before streaming an event".into());
        };
        let frame = frame?;
        let ClientMessage::Text(text) = frame else {
            return Err("expected websocket text frame".into());
        };
        let streamed: StreamedEvent = serde_json::from_str(&text)?;
        assert_eq!(streamed.namespace, NAMESPACE);
        assert_eq!(streamed.decode_event()?.workflow_id(), &workflow_id());

        server.abort();
        Ok(())
    }

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
                search_attributes: std::collections::HashMap::new(),
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
            WorkflowOwnership::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config());
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

    #[test]
    fn http_start_input_normalization_accepts_plain_json_and_legacy_envelope()
    -> Result<(), Box<dyn std::error::Error>> {
        let plain = http_input_payload(json!({ "name": "Ada" }))?;
        assert_eq!(plain.content_type, JSON_CONTENT_TYPE);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&plain.bytes)?,
            json!({ "name": "Ada" })
        );

        let envelope = json!({
            "content_type": "application/json; charset=utf-8",
            "bytes": [123, 34, 110, 97, 109, 101, 34, 58, 34, 65, 100, 97, 34, 125],
        });
        let legacy = http_input_payload(envelope)?;
        assert_eq!(legacy.content_type, "application/json; charset=utf-8");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&legacy.bytes)?,
            json!({ "name": "Ada" })
        );

        let malformed = http_input_payload(
            json!({ "content_type": "application/json", "bytes": "not-a-byte-array" }),
        );
        assert!(matches!(malformed, Err(error) if error.code == WireErrorCode::InvalidInput));

        Ok(())
    }

    #[tokio::test]
    async fn describe_decodes_json_payloads_for_http() -> Result<(), Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = backing.clone();
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .visibility_store_arc(visibility_store)
                .scheduler_threads(1)
                .build()
                .await?,
        );
        store
            .append(
                WriteToken::recorder(),
                &workflow_id(),
                &[started_event()?],
                0,
            )
            .await?;
        let ownership = WorkflowOwnership::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver =
            NamespaceResolver::from_parts(NamespaceMode::SharedEngine, Some(engine), ownership);
        let router = workflow_router(ServerState::from_parts(resolver, runtime_config()));

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

    #[test]
    fn http_payload_base64_encodes_non_json_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let data = payload_data("application/octet-stream", &[0, 1, 2])
            .map_err(|error| std::io::Error::other(error.0.message))?;
        assert_eq!(data, json!("AAEC"));
        Ok(())
    }

    #[test]
    fn http_payload_decodes_json_content_type_with_parameters()
    -> Result<(), Box<dyn std::error::Error>> {
        let data = payload_data("application/json; charset=utf-8", br#"{"name":"Ada"}"#)
            .map_err(|error| std::io::Error::other(error.0.message))?;
        assert_eq!(data, json!({ "name": "Ada" }));
        Ok(())
    }

    #[tokio::test]
    async fn http_auth_failure_messages_are_specific() -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            WorkflowOwnership::default(),
        );
        let router = workflow_router(ServerState::from_parts(resolver, runtime_config()));
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: None,
        };

        assert_auth_error(
            router
                .clone()
                .oneshot(unauthorized_json_request(
                    &request,
                    HeaderCase::MissingAuthorization,
                )?)
                .await?,
            "missing Authorization header with Bearer token",
            "set authorization",
        )
        .await?;
        assert_auth_error(
            router
                .clone()
                .oneshot(unauthorized_json_request(
                    &request,
                    HeaderCase::InvalidToken,
                )?)
                .await?,
            "invalid or expired bearer token",
            "refresh the token",
        )
        .await?;
        assert_auth_error(
            router
                .clone()
                .oneshot(unauthorized_json_request(
                    &request,
                    HeaderCase::MissingSubject,
                )?)
                .await?,
            "missing required header: x-aion-subject",
            "set x-aion-subject",
        )
        .await?;
        assert_auth_error(
            router
                .oneshot(unauthorized_json_request(
                    &request,
                    HeaderCase::WrongNamespace,
                )?)
                .await?,
            "subject not authorized for namespace tenant-a",
            "x-aion-namespaces",
        )
        .await?;

        Ok(())
    }

    async fn assert_auth_error(
        response: Response,
        expected_phrase: &str,
        expected_hint: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::NamespaceDenied);
        assert!(
            error.message.contains(expected_phrase),
            "message `{}` did not contain `{expected_phrase}`",
            error.message
        );
        assert!(
            error.message.contains(expected_hint),
            "message `{}` did not contain hint `{expected_hint}`",
            error.message
        );
        Ok(())
    }

    #[tokio::test]
    async fn get_workflows_and_count_apply_visibility_query_parameters()
    -> Result<(), Box<dyn std::error::Error>> {
        let visibility_store = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = visibility_store.clone();
        let visibility: Arc<dyn VisibilityStore> = visibility_store.clone();
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .visibility_store_arc(Arc::clone(&visibility))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            WorkflowOwnership::default(),
        );
        let router = workflow_router(ServerState::from_parts(resolver, runtime_config()));

        visibility
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: run_id(),
                workflow_type: String::from("checkout"),
                status: WorkflowStatus::Running,
                start_time: recorded_at(1),
                close_time: None,
                search_attributes: std::collections::HashMap::from([(
                    String::from("customer_id"),
                    SearchAttributeValue::String(String::from("12345")),
                )]),
            })
            .await?;
        visibility
            .record_visibility(VisibilityRecord {
                workflow_id: WorkflowId::new(uuid::Uuid::from_u128(2)),
                run_id: aion_core::RunId::new(uuid::Uuid::from_u128(20)),
                workflow_type: String::from("support"),
                status: WorkflowStatus::Running,
                start_time: recorded_at(2),
                close_time: None,
                search_attributes: std::collections::HashMap::from([(
                    String::from("customer_id"),
                    SearchAttributeValue::String(String::from("12345")),
                )]),
            })
            .await?;

        let query = concat!(
            "/workflows?namespace=tenant-a",
            "&workflow_type=checkout",
            "&status=running",
            "&started_after=2023-11-14T22%3A13%3A19Z",
            "&started_before=2023-11-14T22%3A13%3A22Z",
            "&limit=10",
            "&offset=0",
            "&attr.customer_id=12345"
        );
        let list_response = router.clone().oneshot(get_request(query)?).await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let summaries: Vec<WorkflowSummary> = read_json(list_response).await?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].workflow_id, workflow_id());

        let count_response = router
            .oneshot(get_request(
                "/workflows/count?namespace=tenant-a&workflow_type=checkout&attr.customer_id=12345",
            )?)
            .await?;
        assert_eq!(count_response.status(), StatusCode::OK);
        let body: serde_json::Value = read_json(count_response).await?;
        assert_eq!(body["count"], 1);
        Ok(())
    }

    #[tokio::test]
    async fn dashboard_assets_serve_index_asset_and_do_not_shadow_public_api()
    -> Result<(), Box<dyn std::error::Error>> {
        let bundle = tempfile::tempdir()?;
        fs::write(
            bundle.path().join("index.html"),
            "<!doctype html><title>Aion</title><script src=\"/app.js\"></script>",
        )?;
        fs::write(bundle.path().join("app.js"), "window.AION = true;")?;

        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            WorkflowOwnership::default(),
        );
        let mut config = runtime_config();
        config.dashboard = DashboardConfig {
            source: DashboardAssetSource::FileSystem {
                asset_path: bundle.path().to_path_buf(),
            },
        };
        let router = http_router(ServerState::from_parts(resolver, config))?;

        let root = router
            .clone()
            .oneshot(Request::builder().uri("/").body(body::Body::empty())?)
            .await?;
        assert_eq!(root.status(), StatusCode::OK);
        assert!(read_text(root).await?.contains("<title>Aion</title>"));

        let asset = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/app.js")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(asset.status(), StatusCode::OK);
        assert_eq!(read_text(asset).await?, "window.AION = true;");

        let spa = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/workflows/demo")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(spa.status(), StatusCode::OK);
        assert!(read_text(spa).await?.contains("<title>Aion</title>"));

        let list = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(aion_proto::encode_core_value(
                NAMESPACE,
                None,
                &ListWorkflowsFilter {
                    workflow_type: Some(String::from("nonexistent")),
                    ..ListWorkflowsFilter::default()
                },
            )?),
        };
        let list_response = router
            .oneshot(json_request("/workflows/list", &list)?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: ProtoListWorkflowsResponse = read_json(list_response).await?;
        assert!(list_body.summaries.is_empty());
        Ok(())
    }

    fn json_request<T>(
        path: &str,
        value: &T,
    ) -> Result<Request<body::Body>, Box<dyn std::error::Error>>
    where
        T: serde::Serialize,
    {
        let body = serde_json::to_vec(value)?;
        Ok(authenticated_request(path)
            .method("POST")
            .header("content-type", "application/json")
            .body(body::Body::from(body))?)
    }

    #[derive(Clone, Copy)]
    enum HeaderCase {
        MissingAuthorization,
        InvalidToken,
        MissingSubject,
        WrongNamespace,
    }

    fn unauthorized_json_request<T>(
        value: &T,
        header_case: HeaderCase,
    ) -> Result<Request<body::Body>, Box<dyn std::error::Error>>
    where
        T: serde::Serialize,
    {
        let body = serde_json::to_vec(value)?;
        let mut builder = Request::builder()
            .uri("/workflows/list")
            .method("POST")
            .header("content-type", "application/json");
        if !matches!(header_case, HeaderCase::MissingAuthorization) {
            let token = match header_case {
                HeaderCase::InvalidToken => "wrong",
                HeaderCase::MissingAuthorization
                | HeaderCase::MissingSubject
                | HeaderCase::WrongNamespace => TOKEN,
            };
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if !matches!(header_case, HeaderCase::MissingSubject) {
            builder = builder.header("x-aion-subject", "alice");
        }
        let namespace = if matches!(header_case, HeaderCase::WrongNamespace) {
            "tenant-b"
        } else {
            NAMESPACE
        };
        Ok(builder
            .header("x-aion-namespaces", namespace)
            .body(body::Body::from(body))?)
    }

    fn get_request(path: &str) -> Result<Request<body::Body>, Box<dyn std::error::Error>> {
        Ok(authenticated_request(path)
            .method("GET")
            .body(body::Body::empty())?)
    }

    fn authenticated_request(path: &str) -> axum::http::request::Builder {
        Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", NAMESPACE)
    }

    struct TestEventPublisher {
        events: broadcast::Sender<Event>,
        subscribed: Semaphore,
    }

    impl TestEventPublisher {
        fn new() -> Self {
            let (events, _receiver) = broadcast::channel(8);
            Self {
                events,
                subscribed: Semaphore::new(0),
            }
        }

        async fn wait_for_subscription(&self) {
            if let Ok(permit) = self.subscribed.acquire().await {
                permit.forget();
            }
        }

        fn publish(&self, event: Event) -> Result<(), broadcast::error::SendError<Event>> {
            self.events.send(event).map(|_receivers| ())
        }
    }

    impl EventPublisher for TestEventPublisher {
        fn subscribe(&self, filter: EventFilter) -> BoxStream<'static, Event> {
            let receiver = self.events.subscribe();
            self.subscribed.add_permits(1);
            Box::pin(stream::unfold(
                (receiver, filter),
                |(mut receiver, filter)| async move {
                    loop {
                        match receiver.recv().await {
                            Ok(event) => {
                                if filter.matches(&event) {
                                    return Some((event, (receiver, filter)));
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return None,
                        }
                    }
                },
            ))
        }
    }

    async fn read_json<T>(response: Response) -> Result<T, Box<dyn std::error::Error>>
    where
        T: serde::de::DeserializeOwned,
    {
        let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn read_text(response: Response) -> Result<String, Box<dyn std::error::Error>> {
        let bytes = body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok(String::from_utf8(bytes.to_vec())?)
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

    fn proto_payload() -> Result<ProtoPayload, aion_core::PayloadError> {
        Ok(payload()?.into())
    }

    fn payload() -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "fixture": "input" }))
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }

    fn run_id() -> aion_core::RunId {
        aion_core::RunId::new(uuid::Uuid::from_u128(10))
    }

    fn recorded_at(offset_seconds: i64) -> chrono::DateTime<Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    #[tokio::test]
    async fn observability_routes_are_public_and_expose_expected_payloads()
    -> Result<(), Box<dyn std::error::Error>> {
        let router = http_router(
            ServerState::build_with_store(InMemoryStore::default(), runtime_config()).await?,
        )?;

        let metrics_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(metrics_response.status(), StatusCode::OK);
        assert_eq!(
            metrics_response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        let metrics_body = read_text(metrics_response).await?;
        assert!(metrics_body.contains("# HELP aion_workflows_started_total"));
        assert!(metrics_body.contains("# TYPE aion_workflows_started_total counter"));
        assert!(metrics_body.contains("# HELP aion_activity_duration_seconds"));
        assert!(metrics_body.contains("# TYPE aion_activity_duration_seconds histogram"));
        assert!(metrics_body.contains("aion_activity_duration_seconds_bucket"));
        assert!(metrics_body.contains("aion_store_operation_duration_seconds_bucket"));

        let live_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(live_response.status(), StatusCode::OK);

        let ready_response = router
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(ready_response.status(), StatusCode::OK);
        Ok(())
    }
}
