//! Feature-gated JWT authentication support.

pub mod jwks;
pub mod middleware;

pub use jwks::{AuthenticatedClaims, JwksCache, JwksError};
pub use middleware::{AuthError, BearerToken, authorize_bearer_token, extract_http_bearer};
