//! Caller-identity extraction from gRPC request metadata for the workflow
//! service (auth-on JWT validation and the auth-off operator / dev-token paths).
//!
//! Peeled out of `grpc/mod.rs` (AO-007 500-code-line split). `caller_from_metadata`
//! is `pub(crate)` and re-exported by `mod.rs` so `deploy_grpc` keeps its
//! `crate::api::grpc::caller_from_metadata` path.

use tonic::Status;

use crate::CallerIdentity;
use crate::ServerState;

pub(crate) async fn caller_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    state: &ServerState,
) -> Result<CallerIdentity, Status> {
    if !state.runtime_config().auth.enabled {
        return Ok(development_caller_from_metadata(metadata));
    }
    #[cfg(feature = "auth")]
    {
        let bearer = metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_bearer)
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?;
        let Some(cache) = state.jwks_cache() else {
            return Err(Status::unauthenticated("invalid bearer token"));
        };
        return cache
            .validate(&bearer)
            .await
            .map(|claims| claims.caller_identity())
            .map_err(|_error| Status::unauthenticated("invalid bearer token"));
    }
    #[cfg(not(feature = "auth"))]
    {
        // Yield to preserve the async signature required by the auth-feature branch.
        tokio::task::yield_now().await;
        Ok(development_token_caller_from_metadata(
            metadata,
            &state.runtime_config().auth,
        ))
    }
}

/// Auth-off single-tenant operator mode for the gRPC boundary, mirroring the
/// HTTP path exactly: with no auth configured the server decides server-side,
/// at request time, that the caller IS the operator and holds full access
/// (every namespace + the deployment-wide deploy grant). No metadata is
/// required for access; `x-aion-subject` is honored only as the audit label.
fn development_caller_from_metadata(metadata: &tonic::metadata::MetadataMap) -> CallerIdentity {
    let subject = metadata
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("operator");
    CallerIdentity::operator(subject)
}

/// Deployment-wide deploy grant from the development `x-aion-deploy`
/// metadata entry, the dev-mode analog of the JWT `deploy` claim.
#[cfg(not(feature = "auth"))]
fn deploy_metadata_granted(metadata: &tonic::metadata::MetadataMap) -> bool {
    metadata
        .get("x-aion-deploy")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// Development-mode token authentication for gRPC metadata, mirroring the HTTP
/// development token auth.  Used when `auth.enabled` is `true` but the `auth`
/// crate feature is not compiled.
#[cfg(not(feature = "auth"))]
fn development_token_caller_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    auth: &crate::config::AuthConfig,
) -> CallerIdentity {
    let subject = metadata
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty());
    let namespaces = metadata
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map(parse_namespaces)
        .unwrap_or_default();

    let bearer_token = auth.jwks_url.as_deref().unwrap_or_default();
    let expected = format!("Bearer {bearer_token}");
    let Some(authorization) = metadata.get("authorization") else {
        return CallerIdentity::denied(subject.unwrap_or("anonymous"), "missing bearer token");
    };
    let authorization = authorization.to_str().ok();
    if authorization != Some(expected.as_str()) {
        return CallerIdentity::denied(subject.unwrap_or("anonymous"), "invalid bearer token");
    }

    let Some(subject) = subject else {
        return CallerIdentity::denied("anonymous", "missing required metadata: x-aion-subject");
    };

    CallerIdentity::new(subject, namespaces).with_deploy(deploy_metadata_granted(metadata))
}

#[cfg(feature = "auth")]
fn parse_bearer(value: &str) -> Option<String> {
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
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
