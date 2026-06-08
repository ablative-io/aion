//! Transport-boundary bearer extraction and authorization helpers.

use axum::http::HeaderMap;
use tonic::metadata::MetadataMap;

use crate::{CallerIdentity, auth::jwks::JwksCache};

/// Bearer token captured at a transport boundary.
#[derive(Clone, Debug)]
pub struct BearerToken {
    token: String,
}

impl BearerToken {
    /// Borrow the opaque token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.token
    }
}

/// Redacted authentication failure class.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum AuthError {
    /// No well-formed bearer token was present.
    #[error("missing bearer token")]
    MissingBearer,
    /// Token validation failed.
    #[error("invalid bearer token")]
    InvalidToken,
}

/// Extract `Authorization: Bearer <token>` from HTTP headers.
///
/// # Errors
///
/// Returns [`AuthError::MissingBearer`] when the header is absent, non-UTF-8, or not a Bearer token.
pub fn extract_http_bearer(headers: &HeaderMap) -> Result<BearerToken, AuthError> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer)
        .ok_or(AuthError::MissingBearer)
}

/// Extract `authorization: Bearer <token>` from gRPC metadata.
///
/// # Errors
///
/// Returns [`AuthError::MissingBearer`] when metadata is absent, non-UTF-8, or not a Bearer token.
pub fn extract_grpc_bearer(metadata: &MetadataMap) -> Result<BearerToken, AuthError> {
    metadata
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer)
        .ok_or(AuthError::MissingBearer)
}

/// Validate a bearer token and return the single-namespace caller identity.
///
/// # Errors
///
/// Returns [`AuthError::InvalidToken`] when JWT validation or required claim extraction fails.
pub async fn authorize_bearer_token(
    cache: &JwksCache,
    bearer: &BearerToken,
) -> Result<CallerIdentity, AuthError> {
    cache
        .validate(bearer.as_str())
        .await
        .map(|claims| claims.caller_identity())
        .map_err(|_error| AuthError::InvalidToken)
}

fn parse_bearer(value: &str) -> Option<BearerToken> {
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        return None;
    }
    Some(BearerToken {
        token: token.to_owned(),
    })
}
