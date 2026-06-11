//! Public HTTP router construction.

use axum::{
    Router,
    routing::{get, post},
};

use super::events::subscribe_events_socket;
use super::schedules::{
    create_schedule, delete_schedule, describe_schedule, list_schedules, pause_schedule,
    resume_schedule, update_schedule,
};
use super::workflows::{
    cancel_workflow, count_workflows, describe_workflow, get_workflows, post_list_workflows,
    query_workflow, signal_workflow, start_workflow,
};
use crate::{ServerError, ServerState, dashboard::assets, observability};

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

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use aion::EngineBuilder;
    use aion_proto::{ProtoListWorkflowsRequest, ProtoListWorkflowsResponse};
    use aion_store::{EventStore, InMemoryStore, visibility::ListWorkflowsFilter};
    use axum::{body, http::Request, http::StatusCode};
    use tower::ServiceExt;

    use super::super::test_support::{
        NAMESPACE, json_request, read_json, read_text, runtime_config,
    };
    use super::*;
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::{DashboardAssetSource, DashboardConfig, NamespaceMode},
    };

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
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
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

    #[tokio::test]
    async fn observability_routes_are_public_and_expose_expected_payloads()
    -> Result<(), Box<dyn std::error::Error>> {
        let router = http_router(
            crate::ServerState::build_with_store(InMemoryStore::default(), runtime_config())
                .await?,
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
