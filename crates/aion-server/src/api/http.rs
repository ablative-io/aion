//! axum HTTP/JSON workflow facade.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoListWorkflowsRequest, ProtoListWorkflowsResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoSignalRequest, ProtoSignalResponse,
    ProtoStartWorkflowRequest, ProtoStartWorkflowResponse, WireError, WireErrorCode,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::post,
};

use crate::{CallerIdentity, ServerError, ServerState, api::handlers, dashboard::assets};

/// Build the public HTTP application: workflow-management routes first, then
/// the dashboard static asset fallback. The dashboard adds no data API.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when dashboard assets are misconfigured.
pub fn http_router(state: ServerState) -> Result<Router, ServerError> {
    let dashboard = assets::dashboard_router(&state.runtime_config().dashboard)?;
    Ok(workflow_router(state).merge(dashboard))
}

/// Build the public workflow-management HTTP router.
pub fn workflow_router(state: ServerState) -> Router {
    Router::new()
        .route("/workflows/start", post(start_workflow))
        .route("/workflows/signal", post(signal_workflow))
        .route("/workflows/query", post(query_workflow))
        .route("/workflows/cancel", post(cancel_workflow))
        .route("/workflows/list", post(list_workflows))
        .route("/workflows/describe", post(describe_workflow))
        .with_state(state)
}

struct HttpCaller(CallerIdentity);

impl FromRequestParts<ServerState> for HttpCaller {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(caller_from_headers(
            &parts.headers,
            state.runtime_config().auth.bearer_token.as_str(),
        )))
    }
}

async fn start_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoStartWorkflowRequest>,
) -> Result<Json<ProtoStartWorkflowResponse>, HttpWireError> {
    handlers::start(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
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

async fn list_workflows(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoListWorkflowsRequest>,
) -> Result<Json<ProtoListWorkflowsResponse>, HttpWireError> {
    handlers::list(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

async fn describe_workflow(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoDescribeWorkflowRequest>,
) -> Result<Json<ProtoDescribeWorkflowResponse>, HttpWireError> {
    handlers::describe(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

fn caller_from_headers(headers: &axum::http::HeaderMap, bearer_token: &str) -> CallerIdentity {
    let subject = headers
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("anonymous");
    let namespaces = headers
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map_or_else(Vec::new, parse_namespaces);
    let expected = format!("Bearer {bearer_token}");
    let authorized = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);

    if authorized {
        CallerIdentity::new(subject, namespaces)
    } else {
        CallerIdentity::new(subject, Vec::new())
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
        WireErrorCode::UnknownQuery => StatusCode::BAD_REQUEST,
        WireErrorCode::QueryTimeout => StatusCode::REQUEST_TIMEOUT,
        WireErrorCode::NotRunning => StatusCode::PRECONDITION_FAILED,
        WireErrorCode::Lagged => StatusCode::TOO_MANY_REQUESTS,
        WireErrorCode::Backend => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::SocketAddr, sync::Arc};

    use aion::EngineBuilder;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowFilter, WorkflowId, WorkflowStatus};
    use aion_proto::{
        ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoStartWorkflowRequest,
        WireError, WireErrorCode,
        convert::{ProtoPayload, decode_workflow_summary, encode_workflow_filter},
    };
    use aion_store::{EventStore, InMemoryStore};
    use axum::{body, http::Request};
    use chrono::Utc;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        NamespaceResolver, WorkflowOwnership,
        config::{
            AuthConfig, DashboardAssetSource, DashboardConfig, ListenConfig, NamespaceConfig,
            NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
        },
    };

    const NAMESPACE: &str = "tenant-a";
    const TOKEN: &str = "test-token";

    #[tokio::test]
    async fn http_start_and_list_match_handler_outcomes() -> Result<(), Box<dyn std::error::Error>>
    {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        store.append(&workflow_id(), &[started_event()?], 0).await?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            WorkflowOwnership::default(),
        );
        let state = ServerState::from_parts(resolver, runtime_config());
        let router = workflow_router(state);

        let start = ProtoStartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "missing-workflow".to_owned(),
            input: Some(proto_payload()?),
        };
        let start_response = router
            .clone()
            .oneshot(json_request("/workflows/start", &start)?)
            .await?;
        assert_eq!(start_response.status(), StatusCode::NOT_FOUND);
        let start_error: WireError = read_json(start_response).await?;
        assert_eq!(start_error.code, WireErrorCode::NotFound);

        let filter = encode_workflow_filter(
            NAMESPACE,
            None,
            &WorkflowFilter {
                status: Some(WorkflowStatus::Running),
                ..WorkflowFilter::default()
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
        let summary = decode_workflow_summary(&list_body.summaries[0])?;
        assert_eq!(summary.workflow_id, workflow_id());
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
            filter: None,
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
        Ok(Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("x-aion-subject", "alice")
            .header("x-aion-namespaces", NAMESPACE)
            .body(body::Body::from(body))?)
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
                bearer_token: TOKEN.to_owned(),
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
}
