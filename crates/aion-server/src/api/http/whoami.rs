//! Runtime capability discovery (`GET /whoami`).
//!
//! The console asserts no authorization at build time. It DISCOVERS its
//! capabilities at runtime by asking the server who the caller is and what it
//! is allowed to do, then renders affordances from that. This endpoint runs
//! through the same [`HttpCaller`] extractor as every data route, so it
//! reflects exactly the identity the server resolved for the request — there is
//! no second auth path.
//!
//! It is safe to expose without additional gating: it only reflects the
//! caller's OWN grants. It never enumerates other subjects, never lists the
//! deployment's namespaces, and reveals nothing an authorized request to the
//! data API would not already reveal to the same caller.

use axum::{Json, extract::State};
use serde::Serialize;

use super::auth::HttpCaller;
use crate::ServerState;

/// Capability snapshot for the resolved caller, consumed by the dashboard to
/// gate affordances at runtime.
#[derive(Debug, Serialize)]
pub(crate) struct WhoAmI {
    /// Caller subject as resolved by the transport (the audit label).
    subject: String,
    /// Whether the server has auth configured. When `false` the server is in
    /// single-tenant operator mode and the caller is the operator.
    auth_enabled: bool,
    /// Whether the caller holds the deployment-wide deploy grant.
    deploy_granted: bool,
    /// Whether the caller holds access to every namespace (operator mode),
    /// rather than the explicit `namespaces` set.
    all_namespaces: bool,
    /// The caller's explicitly granted namespaces, sorted. Empty for an
    /// operator (whose all-access is signaled by `all_namespaces`).
    namespaces: Vec<String>,
}

/// Reflect the resolved caller's identity and grants for runtime capability
/// discovery.
pub(crate) async fn whoami(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Json<WhoAmI> {
    Json(WhoAmI {
        subject: caller.subject().to_owned(),
        auth_enabled: state.runtime_config().auth.enabled,
        deploy_granted: caller.deploy_granted(),
        all_namespaces: caller.all_namespaces(),
        namespaces: caller.namespaces(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::EngineBuilder;
    use aion_store::{EventStore, InMemoryStore};
    use axum::{body, http::Request, http::StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::super::router::workflow_router;
    use super::super::test_support::{read_json, runtime_config, server_state};
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    async fn auth_off_router() -> Result<axum::Router, Box<dyn std::error::Error>> {
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
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let mut config = runtime_config();
        config.auth.enabled = false;
        Ok(workflow_router(server_state(resolver, config).await?))
    }

    /// Auth-off operator mode: `/whoami` reports the operator's full access with
    /// no development headers on the request. This is the runtime signal the
    /// dashboard reads to enable deploy/namespace affordances.
    #[tokio::test]
    async fn whoami_reports_operator_in_auth_off_mode() -> Result<(), Box<dyn std::error::Error>> {
        let response = auth_off_router()
            .await?
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .body(body::Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = read_json(response).await?;
        assert_eq!(body["auth_enabled"], serde_json::json!(false));
        assert_eq!(body["deploy_granted"], serde_json::json!(true));
        assert_eq!(body["all_namespaces"], serde_json::json!(true));
        assert_eq!(body["subject"], serde_json::json!("operator"));
        assert_eq!(body["namespaces"], serde_json::json!([]));
        Ok(())
    }
}
