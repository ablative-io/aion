//! Public HTTP router construction.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    routing::{any, get, post},
};
use tower_http::cors::CorsLayer;

use super::authoring::compile_source;
use super::awl::{
    bind_run, check, create_document, deploy_authoring, edit, emit, format, get_document,
    get_layout, get_revision, get_run_status, list_documents, put_document, put_layout, scaffold,
    worker_availability,
};
use super::cluster_command::cluster_command;
use super::deploy::{list_versions, route_version, unload_version, upload_package};
use super::dev_ui::{dev_register_mock, dev_replay_run, dev_trigger_run};
use super::events::subscribe_events_socket;
use super::intervene::{intervene, list_attempts};
use super::schedules::{
    create_schedule, delete_schedule, describe_schedule, list_schedules, pause_schedule,
    resume_schedule, update_schedule,
};
use super::transcripts::{fetch_transcript, list_transcript_streams};
use super::whoami::whoami;
use super::workflows::{
    cancel_workflow, count_workflows, describe_workflow, get_workflows, list_namespace_records,
    list_namespaces, post_list_workflows, post_namespace, query_workflow, reopen_workflow,
    set_namespace_placement, signal_workflow, start_workflow,
};
use crate::{ServerError, ServerState, observability, ops_console::assets};

/// Build the public HTTP application: workflow-management routes first, then
/// the ops-console static asset fallback. The ops console adds no data API.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when ops-console assets are misconfigured.
pub fn http_router(state: ServerState) -> Result<Router, ServerError> {
    let ops_console = assets::ops_console_router(&state.runtime_config().ops_console)?;
    let cors = cors_layer(&state.runtime_config().cors_allowed_origins)?;
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
    let router = router.merge(ops_console);
    // CORS is applied last so it wraps every public route the browser ops console
    // calls (the workflow API, /metrics, /health/*, the ops-console fallback).
    // With no configured origins `cors_layer` returns None and the router is
    // byte-identical to before — no cross-origin request is allowed (the secure
    // default). With origins set the layer also answers OPTIONS preflight.
    Ok(match cors {
        Some(cors) => router.layer(cors),
        None => router,
    })
}

/// Build the CORS layer for the public HTTP router from the operator-configured
/// allowed origins.
///
/// Returns `Ok(None)` when no origins are configured — the secure default:
/// the layer is not installed and no cross-origin request is permitted. When
/// origins are configured the layer is scoped to exactly those origins (never
/// `Any`, so it is safe to pair with credentialed requests), permits the
/// methods the ops console uses (GET, POST, and OPTIONS preflight), and allows
/// exactly the request headers the API consumes.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when a configured origin is not a valid HTTP
/// header value. Startup validation already rejects malformed origins, so this
/// is defense in depth.
fn cors_layer(allowed_origins: &[String]) -> Result<Option<CorsLayer>, ServerError> {
    if allowed_origins.is_empty() {
        return Ok(None);
    }
    let mut origins = Vec::with_capacity(allowed_origins.len());
    for origin in allowed_origins {
        let value = origin
            .parse::<HeaderValue>()
            .map_err(|source| ServerError::Config {
                message: format!("invalid CORS origin `{origin}`: {source}"),
            })?;
        origins.push(value);
    }
    let layer = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static("x-aion-namespaces"),
            HeaderName::from_static("x-aion-subject"),
        ]);
    Ok(Some(layer))
}

/// Disabled deploy surface: a plain 404 with no body, indistinguishable
/// from an unmounted route family.
async fn deploy_disabled() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// Disabled authoring surface: a plain 404 with no body, indistinguishable
/// from an unmounted route family. When `[authoring].gleam_path` is absent the
/// server compiles no Gleam and deploys pre-built `.aion` files only (CN7).
async fn authoring_disabled() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// Disabled dev surface: a plain 404 with no body, indistinguishable from an
/// unmounted route family. When `[dev].enabled` is false the server mounts no
/// dev endpoints and installs no activity-mock decorator (CN4).
async fn dev_disabled() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// Build the public workflow-management HTTP router.
pub fn workflow_router(state: ServerState) -> Router {
    // The deploy surface is dark by default: when `[deploy].enabled` is
    // false the routes are not mounted and every `/deploy/*` path is a
    // plain 404 (the explicit catch-all keeps the ops-console SPA fallback
    // from answering for the deploy namespace). The archive upload route
    // disables the default body limit because `read_archive_body` enforces
    // the operator-configured `deploy.max_archive_bytes` ceiling while
    // streaming.
    let deploy = if state.runtime_config().deploy.enabled {
        Router::new()
            .route(
                "/deploy/packages",
                post(upload_package).layer(DefaultBodyLimit::disable()),
            )
            .route("/deploy/versions", get(list_versions))
            .route("/deploy/route", post(route_version))
            .route("/deploy/unload", post(unload_version))
    } else {
        Router::new().route("/deploy/{*rest}", any(deploy_disabled))
    };
    // The authoring surface is dark by default, gated on
    // `[authoring].gleam_path`: when it is unset the routes are not mounted and
    // every `/authoring/*` path is a plain 404 (the explicit catch-all keeps
    // the ops-console SPA fallback from answering for the authoring namespace).
    // With it absent the server compiles no Gleam and deploys pre-built `.aion`
    // files only (CN7).
    let authoring = if state.runtime_config().authoring.gleam_path.is_some() {
        Router::new().route("/authoring/compile", post(compile_source))
    } else {
        Router::new().route("/authoring/{*rest}", any(authoring_disabled))
    };
    // The full AWL studio is always mounted. Stock config supplies the
    // `aion-authoring` workspace; the typed unconfigured refusal remains for
    // manually constructed runtime configs that explicitly omit it.
    let awl_documents = Router::new()
        .route("/awl/documents", get(list_documents).post(create_document))
        .route(
            "/awl/documents/{*path}",
            get(get_document).put(put_document),
        )
        .route("/awl/layout/{*path}", get(get_layout).put(put_layout));
    let awl = Router::new()
        .route("/awl/check", post(check))
        .route("/awl/emit", post(emit))
        .route("/awl/deploy", post(deploy_authoring))
        .route("/awl/revisions/{hash}", get(get_revision))
        .route("/awl/workers/availability", post(worker_availability))
        .route("/awl/runs/{deployment_id}", get(get_run_status))
        .route("/awl/runs/{deployment_id}/binding", post(bind_run))
        .route("/awl/edit", post(edit))
        .route("/awl/fmt", post(format))
        .route("/awl/scaffold", post(scaffold))
        .merge(awl_documents);
    // The dev surface is dark by default, gated on `[dev].enabled`: when off the
    // routes are not mounted and every `/dev/*` path is a plain 404 (the
    // explicit catch-all keeps the ops-console SPA fallback from answering for
    // the dev namespace), and the engine runs the bare production dispatcher.
    let dev = if state.runtime_config().dev.enabled {
        Router::new()
            .route("/dev/runs", post(dev_trigger_run))
            .route("/dev/mocks", post(dev_register_mock))
            .route("/dev/replay", post(dev_replay_run))
    } else {
        Router::new().route("/dev/{*rest}", any(dev_disabled))
    };
    deploy
        .merge(authoring)
        .merge(awl)
        .merge(dev)
        .route("/whoami", get(whoami))
        .route("/namespaces", get(list_namespaces).post(post_namespace))
        .route("/namespaces/records", get(list_namespace_records))
        .route(
            "/namespaces/{name}/placement",
            axum::routing::put(set_namespace_placement),
        )
        .route("/workflows", get(get_workflows))
        .route("/workflows/count", get(count_workflows))
        .route("/workflows/start", post(start_workflow))
        .route("/workflows/signal", post(signal_workflow))
        .route("/workflows/query", post(query_workflow))
        .route("/workflows/cancel", post(cancel_workflow))
        .route("/workflows/reopen", post(reopen_workflow))
        .route("/workflows/list", post(post_list_workflows))
        .route("/workflows/describe", post(describe_workflow))
        .route("/workflows/intervene", post(intervene))
        .route("/workflows/attempts", post(list_attempts))
        .route("/workflows/transcript", post(fetch_transcript))
        .route("/workflows/transcripts", post(list_transcript_streams))
        .route("/events/stream", get(subscribe_events_socket))
        .route("/cluster/command", post(cluster_command))
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

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use aion::EngineBuilder;
    use aion_store::{EventStore, InMemoryStore};
    use axum::{body, http::Request, http::StatusCode};
    use tower::ServiceExt;

    use super::super::test_support::{
        NAMESPACE, json_request, read_json, read_text, runtime_config, server_state,
    };
    use super::*;
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::{NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig},
    };

    #[tokio::test]
    async fn ops_console_assets_serve_index_asset_and_do_not_shadow_public_api()
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
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let mut config = runtime_config();
        config.ops_console = OpsConsoleConfig {
            source: OpsConsoleAssetSource::FileSystem {
                asset_path: bundle.path().to_path_buf(),
            },
        };
        let router = http_router(server_state(resolver, config).await?)?;

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
                    .uri("/ops-console/workflows/demo")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(spa.status(), StatusCode::OK);
        assert!(read_text(spa).await?.contains("<title>Aion</title>"));

        let list = serde_json::json!({
            "namespace": NAMESPACE,
            "filter": { "workflow_type": "nonexistent" },
        });
        let list_response = router
            .oneshot(json_request("/workflows/list", &list)?)
            .await?;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body: serde_json::Value = read_json(list_response).await?;
        assert!(
            list_body["summaries"]
                .as_array()
                .ok_or("summaries missing")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cors_preflight_and_actual_request_carry_allow_headers_for_configured_origin()
    -> Result<(), Box<dyn std::error::Error>> {
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
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let mut config = runtime_config();
        config.cors_allowed_origins = vec!["http://localhost:5173".to_owned()];
        let router = http_router(server_state(resolver, config).await?)?;

        // Preflight: the browser sends OPTIONS with the requested method/header;
        // the layer must answer with the matching allow-origin and allow-methods.
        let preflight = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/workflows/list")
                    .header("origin", "http://localhost:5173")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "x-aion-namespaces")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(
            preflight
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:5173")
        );

        // Actual request from the allowed origin echoes the allow-origin header.
        let actual = router
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .header("origin", "http://localhost:5173")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(actual.status(), StatusCode::OK);
        assert_eq!(
            actual
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:5173")
        );
        Ok(())
    }

    #[tokio::test]
    async fn cors_absent_origins_install_no_layer() -> Result<(), Box<dyn std::error::Error>> {
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
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        // runtime_config() leaves cors_allowed_origins empty (the secure default).
        let router = http_router(server_state(resolver, runtime_config()).await?)?;

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .header("origin", "http://localhost:5173")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "no CorsLayer must be installed when no origins are configured"
        );
        Ok(())
    }

    #[tokio::test]
    async fn observability_routes_are_public_and_expose_expected_payloads()
    -> Result<(), Box<dyn std::error::Error>> {
        // This test rides the production `build_with_store` startup path, so
        // under `feature = "auth"` the configured jwks_url must be a live
        // endpoint for the initial JWKS fetch.
        #[cfg(feature = "auth")]
        let config = {
            let mut config = runtime_config();
            config.auth.jwks_url = Some(crate::auth::test_support::serve_jwks()?);
            config
        };
        #[cfg(not(feature = "auth"))]
        let config = runtime_config();
        let router = http_router(
            crate::ServerState::build_with_store(InMemoryStore::default(), config).await?,
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
