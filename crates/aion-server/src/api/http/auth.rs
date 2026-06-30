//! Caller-identity extraction from HTTP request headers.

use std::collections::HashMap;

#[cfg(feature = "auth")]
use axum::http::header;
use axum::{
    extract::{FromRequestParts, Query},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};

use crate::{CallerIdentity, ServerState};

pub(crate) struct HttpCaller(pub(crate) CallerIdentity);

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

/// Caller identity for the `/events/stream` WebSocket handshake.
///
/// Browsers cannot attach custom request headers (`x-aion-namespaces`,
/// `x-aion-subject`, `Authorization`) to a WebSocket handshake, so the same
/// credentials the REST API takes as headers are also accepted here as query
/// parameters and promoted to their header form before the single shared
/// header-based resolution ([`caller_from_headers`]) runs. An explicit header,
/// when present, always wins over its query-parameter fallback. This is the
/// standard browser-WebSocket authorization pattern; it introduces no second
/// auth code path.
pub(crate) struct WsCaller(pub(crate) CallerIdentity);

impl FromRequestParts<ServerState> for WsCaller {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        let query = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_or_else(|_error| HashMap::new(), |Query(params)| params);
        let mut headers = parts.headers.clone();
        promote_query_credentials(&query, &mut headers);
        let caller = caller_from_headers(&headers, state)
            .await
            .map_err(axum::response::IntoResponse::into_response)?;
        Ok(Self(caller))
    }
}

/// Promote recognized credential query parameters into their request-header
/// equivalents so [`caller_from_headers`] resolves the caller identically to a
/// header-bearing REST request. A header already present on the handshake is
/// never overwritten. `access_token` / `token` are wrapped in the `Bearer`
/// scheme to match the `Authorization` header form.
fn promote_query_credentials(params: &HashMap<String, String>, headers: &mut HeaderMap) {
    for (key, value) in params {
        let header_name: &'static str = match key.as_str() {
            "x-aion-namespaces" | "namespaces" => "x-aion-namespaces",
            "x-aion-subject" | "subject" => "x-aion-subject",
            "x-aion-deploy" => "x-aion-deploy",
            "authorization" | "access_token" | "token" => "authorization",
            _ => continue,
        };
        if headers.contains_key(header_name) {
            continue;
        }
        let header_value = if matches!(key.as_str(), "access_token" | "token") {
            format!("Bearer {value}")
        } else {
            value.clone()
        };
        let Ok(header_value) = HeaderValue::from_str(&header_value) else {
            continue;
        };
        headers.insert(HeaderName::from_static(header_name), header_value);
    }
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

/// Auth-off single-tenant operator mode: when no auth is configured the server
/// decides server-side, at request time, that the caller IS the operator and
/// holds full access (every namespace + the deployment-wide deploy grant). No
/// development header is required for access; the `x-aion-subject` header is
/// honored only as the audit label when present and non-empty.
///
/// The `x-aion-namespaces` / `x-aion-deploy` headers are intentionally NOT read
/// here — the operator already has all access, so they would assert nothing.
fn development_caller_from_headers(headers: &axum::http::HeaderMap) -> CallerIdentity {
    let subject = headers
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("operator");
    CallerIdentity::operator(subject)
}

/// Deployment-wide deploy grant from the development `x-aion-deploy` header,
/// the dev-mode analog of the JWT `deploy` claim. Absent or non-true = no
/// grant.
#[cfg(not(feature = "auth"))]
fn deploy_header_granted(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-aion-deploy")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// Development-mode token authentication used when `auth.enabled` is `true` but
/// the `auth` crate feature is not compiled.  Validates bearer tokens against the
/// configured `jwks_url` value (treated as a static shared secret) and returns
/// [`CallerIdentity::denied`] with a specific reason on each failure mode so the
/// namespace guard surfaces actionable 403 error messages.
#[cfg(not(feature = "auth"))]
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

    CallerIdentity::new(subject, namespaces).with_deploy(deploy_header_granted(headers))
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

#[cfg(not(feature = "auth"))]
fn parse_namespaces(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|namespace| !namespace.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::EngineBuilder;
    use aion_proto::{ProtoListWorkflowsRequest, WireError, WireErrorCode};
    use aion_store::{EventStore, InMemoryStore};
    #[cfg(not(feature = "auth"))]
    use axum::response::Response;
    use axum::{body, http::HeaderMap, http::Request, http::StatusCode};
    use tower::ServiceExt;

    use super::super::router::workflow_router;
    #[cfg(not(feature = "auth"))]
    use super::super::test_support::TOKEN;
    use super::super::test_support::{NAMESPACE, read_json, runtime_config, server_state};
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    async fn list_router() -> Result<axum::Router, Box<dyn std::error::Error>> {
        router_with(runtime_config()).await
    }

    async fn router_with(
        config: crate::config::RuntimeConfig,
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
        Ok(workflow_router(server_state(resolver, config).await?))
    }

    /// Auth-off single-tenant operator mode: a caller with NO development
    /// headers at all is the operator and is authorized for an arbitrary
    /// namespace (cross-namespace access) AND holds the deploy grant. This is
    /// the request-time, server-side authorization decision the operator
    /// experience depends on — the client asserts nothing.
    #[tokio::test]
    async fn auth_off_operator_authorizes_namespace_and_deploy()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = runtime_config();
        config.auth.enabled = false;
        let router = router_with(config).await?;

        // A namespace the caller never enumerated, with no x-aion-* headers.
        let list = ProtoListWorkflowsRequest {
            namespace: "some-other-tenant".to_owned(),
            filter: None,
        };
        let body = serde_json::to_vec(&list)?;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/workflows/list")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(body::Body::from(body))?,
            )
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "auth-off operator must be authorized for any namespace with no headers"
        );

        // And the resolved identity carries the deploy grant.
        let resolved = super::development_caller_from_headers(&HeaderMap::new());
        assert!(resolved.deploy_granted());
        assert!(resolved.all_namespaces());
        assert_eq!(resolved.subject(), "operator");
        Ok(())
    }

    /// The `x-aion-subject` header is honored only as the audit label in
    /// operator mode; it is never required, and never narrows access.
    #[tokio::test]
    async fn auth_off_operator_honors_subject_as_audit_label()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert("x-aion-subject", "ci-bot".parse()?);
        let resolved = super::development_caller_from_headers(&headers);
        assert_eq!(resolved.subject(), "ci-bot");
        assert!(resolved.all_namespaces());
        assert!(resolved.deploy_granted());
        Ok(())
    }

    /// JWT-path failure modes: missing, malformed, and expired bearers are
    /// redacted 401s (no oracle for why validation failed), while an
    /// authenticated subject lacking the requested grant gets the specific
    /// namespace denial.
    #[cfg(feature = "auth")]
    #[tokio::test]
    async fn http_auth_failure_messages_are_specific() -> Result<(), Box<dyn std::error::Error>> {
        use crate::auth::test_support::{mint_expired_token, mint_token};

        let router = list_router().await?;
        let request = ProtoListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: None,
        };

        let missing = router.clone().oneshot(jwt_request(&request, None)?).await?;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let malformed = router
            .clone()
            .oneshot(jwt_request(&request, Some("not-a-jwt".to_owned()))?)
            .await?;
        assert_eq!(malformed.status(), StatusCode::UNAUTHORIZED);

        let expired = router
            .clone()
            .oneshot(jwt_request(
                &request,
                Some(mint_expired_token("alice", NAMESPACE)?),
            )?)
            .await?;
        assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);

        let foreign = router
            .oneshot(jwt_request(
                &request,
                Some(mint_token("alice", "tenant-b")?),
            )?)
            .await?;
        assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
        let error: WireError = read_json(foreign).await?;
        assert_eq!(error.code, WireErrorCode::NamespaceDenied);
        assert!(
            error
                .message
                .contains("subject not authorized for namespace tenant-a"),
            "denial must name the ungranted namespace: {}",
            error.message
        );
        assert!(
            error.message.contains("namespace claim"),
            "JWT-path denial must hint the token's namespace claim: {}",
            error.message
        );
        assert!(
            !error.message.contains("x-aion-namespaces"),
            "JWT-path denial must not hint the development header: {}",
            error.message
        );

        Ok(())
    }

    #[cfg(feature = "auth")]
    fn jwt_request<T>(
        value: &T,
        bearer: Option<String>,
    ) -> Result<Request<body::Body>, Box<dyn std::error::Error>>
    where
        T: serde::Serialize,
    {
        let body = serde_json::to_vec(value)?;
        let mut builder = Request::builder()
            .uri("/workflows/list")
            .method("POST")
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            builder = builder.header("authorization", format!("Bearer {bearer}"));
        }
        Ok(builder.body(body::Body::from(body))?)
    }

    /// Development-token-path failure modes: each failure surfaces a specific,
    /// actionable denial message.
    #[cfg(not(feature = "auth"))]
    #[tokio::test]
    async fn http_auth_failure_messages_are_specific() -> Result<(), Box<dyn std::error::Error>> {
        let router = list_router().await?;
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

    #[cfg(not(feature = "auth"))]
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

    #[cfg(not(feature = "auth"))]
    #[derive(Clone, Copy)]
    enum HeaderCase {
        MissingAuthorization,
        InvalidToken,
        MissingSubject,
        WrongNamespace,
    }

    #[cfg(not(feature = "auth"))]
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
}
