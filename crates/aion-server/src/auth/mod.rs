//! Feature-gated JWT authentication support.

/// JWKS cache and authenticated-claim validation support.
pub mod jwks;
/// Bearer-token extraction and authorization middleware helpers.
pub mod middleware;

pub use jwks::{AuthenticatedClaims, JwksCache, JwksError};
pub use middleware::{AuthError, BearerToken, authorize_bearer_token, extract_http_bearer};
