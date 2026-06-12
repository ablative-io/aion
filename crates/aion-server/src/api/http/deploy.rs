//! `/deploy/*` HTTP facade over the shared deploy handlers.
//!
//! Mounted only when `[deploy].enabled` is true; the disabled surface is a
//! plain 404 (see `router.rs`). The archive upload is a raw
//! `application/octet-stream` body enforced against
//! `deploy.max_archive_bytes` while streaming — oversized uploads are
//! refused with 413 naming the config key, never buffered past the ceiling.

use aion_proto::{
    ProtoListVersionsResponse, ProtoLoadPackageResponse, ProtoRouteVersionRequest,
    ProtoRouteVersionResponse, ProtoUnloadVersionRequest, ProtoUnloadVersionResponse, WireError,
};
use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::api::handlers::deploy::{self, DeployApiError};
use crate::config::DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED;
use crate::{ServerError, ServerState};

const TRANSPORT: &str = "http";

/// Deploy failure response: 503 for drain/shutdown, 413 for oversized
/// archives, and the standard wire-code table otherwise.
pub(crate) struct DeployHttpError(pub(crate) DeployApiError);

impl IntoResponse for DeployHttpError {
    fn into_response(self) -> Response {
        match self.0 {
            DeployApiError::Unavailable(wire) => {
                (StatusCode::SERVICE_UNAVAILABLE, Json(wire)).into_response()
            }
            DeployApiError::ArchiveTooLarge(wire) => {
                (StatusCode::PAYLOAD_TOO_LARGE, Json(wire)).into_response()
            }
            DeployApiError::Wire(wire) => HttpWireError(wire).into_response(),
        }
    }
}

pub(crate) async fn upload_package(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    request: Request,
) -> Result<Json<ProtoLoadPackageResponse>, DeployHttpError> {
    let archive = read_archive_body(&state, request).await?;
    deploy::load_package(&state, &caller, TRANSPORT, archive)
        .await
        .map(Json)
        .map_err(DeployHttpError)
}

pub(crate) async fn list_versions(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
) -> Result<Json<ProtoListVersionsResponse>, DeployHttpError> {
    deploy::list_versions(&state, &caller, TRANSPORT)
        .map(Json)
        .map_err(DeployHttpError)
}

pub(crate) async fn route_version(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoRouteVersionRequest>,
) -> Result<Json<ProtoRouteVersionResponse>, DeployHttpError> {
    deploy::route_version(&state, &caller, TRANSPORT, request)
        .await
        .map(Json)
        .map_err(DeployHttpError)
}

pub(crate) async fn unload_version(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoUnloadVersionRequest>,
) -> Result<Json<ProtoUnloadVersionResponse>, DeployHttpError> {
    deploy::unload_version(&state, &caller, TRANSPORT, request)
        .await
        .map(Json)
        .map_err(DeployHttpError)
}

/// Reads the raw archive body, enforcing `deploy.max_archive_bytes` while
/// streaming so an oversized upload is refused with the key-naming 413
/// instead of being buffered unbounded.
async fn read_archive_body(
    state: &ServerState,
    request: Request,
) -> Result<Vec<u8>, DeployHttpError> {
    let Some(limit) = state.runtime_config().deploy.max_archive_bytes else {
        // Mounting requires validated config; reaching this is a wiring bug.
        return Err(DeployHttpError(DeployApiError::Wire(
            ServerError::Config {
                message: DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED.to_owned(),
            }
            .to_wire_error(),
        )));
    };
    let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
    match axum::body::to_bytes(request.into_body(), limit_usize).await {
        Ok(bytes) => Ok(bytes.to_vec()),
        Err(error) if is_length_limit(&error) => Err(DeployHttpError(
            DeployApiError::ArchiveTooLarge(WireError::invalid_input(format!(
                "archive exceeds the deploy.max_archive_bytes limit of {limit} bytes; raise deploy.max_archive_bytes (or AION_DEPLOY_MAX_ARCHIVE_BYTES) if this package size is intended"
            ))),
        )),
        Err(error) => Err(DeployHttpError(DeployApiError::Wire(
            WireError::invalid_input(format!("failed to read archive body: {error}")),
        ))),
    }
}

/// Whether a body read failure was the typed length-limit refusal (versus a
/// transport read error).
fn is_length_limit(error: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = current.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::EngineBuilder;
    use aion_proto::{ProtoListVersionsResponse, WireError, WireErrorCode};
    use aion_store::{EventStore, InMemoryStore};
    use axum::{body, http::Request, http::StatusCode};
    use tower::ServiceExt;

    use super::super::router::workflow_router;
    #[cfg(not(feature = "auth"))]
    use super::super::test_support::TOKEN;
    use super::super::test_support::{read_json, runtime_config, server_state};
    use crate::config::{DeployConfig, NamespaceMode};
    use crate::{NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces};

    const NAMESPACE: &str = "tenant-a";

    /// Router with the deploy surface mounted (or not) and authentication
    /// configured per case; the engine is real but carries no packages.
    async fn deploy_router(
        auth_enabled: bool,
        deploy: DeployConfig,
    ) -> Result<axum::Router, Box<dyn std::error::Error>> {
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
        config.auth.enabled = auth_enabled;
        config.deploy = deploy;
        Ok(workflow_router(server_state(resolver, config).await?))
    }

    fn enabled_deploy() -> DeployConfig {
        DeployConfig {
            enabled: true,
            max_archive_bytes: Some(1024),
        }
    }

    fn versions_request(
        deploy_header: Option<&str>,
    ) -> Result<Request<body::Body>, axum::http::Error> {
        let mut builder = Request::builder()
            .uri("/deploy/versions")
            .method("GET")
            .header("x-aion-subject", "ci")
            .header("x-aion-namespaces", NAMESPACE);
        if let Some(value) = deploy_header {
            builder = builder.header("x-aion-deploy", value);
        }
        builder.body(body::Body::empty())
    }

    /// Absent `[deploy]` section: every deploy route is a plain 404 — the
    /// surface is not mounted at all (and the dashboard SPA fallback never
    /// answers for `/deploy/*`).
    #[tokio::test]
    async fn disabled_surface_is_404_on_every_route() -> Result<(), Box<dyn std::error::Error>> {
        let router = deploy_router(false, DeployConfig::default()).await?;

        let cases = [
            ("GET", "/deploy/versions"),
            ("POST", "/deploy/packages"),
            ("POST", "/deploy/route"),
            ("POST", "/deploy/unload"),
        ];
        for (method, uri) in cases {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("x-aion-deploy", "true")
                        .header("x-aion-subject", "ci")
                        .body(body::Body::empty())?,
                )
                .await?;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {uri} must be 404 when deploy is disabled"
            );
        }
        Ok(())
    }

    /// Dev mode (`auth.enabled = false`): the `x-aion-deploy: true` header is
    /// the grant; without it the caller is denied with the header-naming hint.
    #[tokio::test]
    async fn dev_mode_header_grants_and_denies_deploy() -> Result<(), Box<dyn std::error::Error>> {
        let router = deploy_router(false, enabled_deploy()).await?;

        let granted = router
            .clone()
            .oneshot(versions_request(Some("true"))?)
            .await?;
        assert_eq!(granted.status(), StatusCode::OK);
        let listing: ProtoListVersionsResponse = read_json(granted).await?;
        assert!(listing.versions.is_empty());

        let denied = router.clone().oneshot(versions_request(None)?).await?;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let error: WireError = read_json(denied).await?;
        assert_eq!(error.code, WireErrorCode::DeployDenied);
        assert!(
            error.message.contains("x-aion-deploy"),
            "dev-mode denial must hint the header: {}",
            error.message
        );

        let false_valued = router.oneshot(versions_request(Some("false"))?).await?;
        assert_eq!(false_valued.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    /// JWT path (`feature = "auth"`): the deploy claim is the grant — absent
    /// claim, claim false, and claim true behave distinctly, and missing
    /// bearers stay redacted 401s.
    #[cfg(feature = "auth")]
    #[tokio::test]
    async fn jwt_deploy_claim_matrix() -> Result<(), Box<dyn std::error::Error>> {
        use crate::auth::test_support::{mint_token, mint_token_with_deploy};

        let router = deploy_router(true, enabled_deploy()).await?;
        let request = |bearer: Option<String>| {
            let mut builder = Request::builder().uri("/deploy/versions").method("GET");
            if let Some(bearer) = bearer {
                builder = builder.header("authorization", format!("Bearer {bearer}"));
            }
            builder.body(body::Body::empty())
        };

        let missing = router.clone().oneshot(request(None)?).await?;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let no_claim = router
            .clone()
            .oneshot(request(Some(mint_token("ci", NAMESPACE)?))?)
            .await?;
        assert_eq!(no_claim.status(), StatusCode::FORBIDDEN);
        let error: WireError = read_json(no_claim).await?;
        assert_eq!(error.code, WireErrorCode::DeployDenied);
        assert!(
            error.message.contains("deploy claim"),
            "JWT denial must hint the token claim: {}",
            error.message
        );
        assert!(
            !error.message.contains("x-aion-deploy"),
            "JWT denial must not hint the dev header: {}",
            error.message
        );

        let claim_false = router
            .clone()
            .oneshot(request(Some(mint_token_with_deploy(
                "ci", NAMESPACE, false,
            )?))?)
            .await?;
        assert_eq!(claim_false.status(), StatusCode::FORBIDDEN);

        let claim_true = router
            .oneshot(request(Some(mint_token_with_deploy(
                "ci", NAMESPACE, true,
            )?))?)
            .await?;
        assert_eq!(claim_true.status(), StatusCode::OK);
        let listing: ProtoListVersionsResponse = read_json(claim_true).await?;
        assert!(listing.versions.is_empty());
        Ok(())
    }

    /// Dev-token path (`auth.enabled = true` without the `auth` feature):
    /// shared-secret check first, then the same `x-aion-deploy` header.
    #[cfg(not(feature = "auth"))]
    #[tokio::test]
    async fn dev_token_path_requires_secret_and_deploy_header()
    -> Result<(), Box<dyn std::error::Error>> {
        let router = deploy_router(true, enabled_deploy()).await?;
        let request = |token: Option<&str>, deploy: Option<&str>| {
            let mut builder = Request::builder()
                .uri("/deploy/versions")
                .method("GET")
                .header("x-aion-subject", "ci")
                .header("x-aion-namespaces", NAMESPACE);
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            if let Some(deploy) = deploy {
                builder = builder.header("x-aion-deploy", deploy);
            }
            builder.body(body::Body::empty())
        };

        let granted = router
            .clone()
            .oneshot(request(Some(TOKEN), Some("true"))?)
            .await?;
        assert_eq!(granted.status(), StatusCode::OK);

        let no_header = router.clone().oneshot(request(Some(TOKEN), None)?).await?;
        assert_eq!(no_header.status(), StatusCode::FORBIDDEN);
        let error: WireError = read_json(no_header).await?;
        assert_eq!(error.code, WireErrorCode::DeployDenied);
        assert!(
            error.message.contains("x-aion-deploy"),
            "dev-token denial must hint the header: {}",
            error.message
        );

        let bad_token = router
            .oneshot(request(Some("wrong"), Some("true"))?)
            .await?;
        assert_eq!(bad_token.status(), StatusCode::FORBIDDEN);
        let error: WireError = read_json(bad_token).await?;
        assert_eq!(error.code, WireErrorCode::DeployDenied);
        assert!(
            error.message.contains("invalid or expired bearer token"),
            "credential failure must carry the transport reason: {}",
            error.message
        );
        Ok(())
    }

    /// Oversized uploads are refused with 413 naming the config key while
    /// streaming — never buffered past the ceiling.
    #[tokio::test]
    async fn oversized_archive_is_413_naming_the_config_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let router = deploy_router(false, enabled_deploy()).await?;

        let oversized = vec![0_u8; 2048];
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/deploy/packages")
                    .method("POST")
                    .header("content-type", "application/octet-stream")
                    .header("x-aion-subject", "ci")
                    .header("x-aion-deploy", "true")
                    .body(body::Body::from(oversized))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::InvalidInput);
        assert!(
            error.message.contains("deploy.max_archive_bytes"),
            "413 must name the config key: {}",
            error.message
        );
        Ok(())
    }

    /// A within-limit body that is not a valid `.aion` archive is a 400
    /// `invalid_input` carrying the package taxonomy.
    #[tokio::test]
    async fn malformed_archive_is_invalid_input() -> Result<(), Box<dyn std::error::Error>> {
        let router = deploy_router(false, enabled_deploy()).await?;

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/deploy/packages")
                    .method("POST")
                    .header("content-type", "application/octet-stream")
                    .header("x-aion-subject", "ci")
                    .header("x-aion-deploy", "true")
                    .body(body::Body::from(vec![1_u8, 2, 3]))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: WireError = read_json(response).await?;
        assert_eq!(error.code, WireErrorCode::InvalidInput);
        assert_eq!(error.error_type.as_deref(), Some("Package"));
        Ok(())
    }
}
