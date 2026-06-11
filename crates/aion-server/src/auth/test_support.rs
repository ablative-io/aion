//! Test-only JWKS endpoint and JWT minting for auth-feature tests.
//!
//! Serves a real JWKS document over local HTTP and mints HS256 tokens against
//! the same key, so tests exercise the production fetch-validate path end to
//! end instead of bypassing it.

use axum::{Json, Router, routing::get};
use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};

/// Key id served by the fixture JWKS endpoint and stamped into minted tokens.
const KEY_ID: &str = "aion-test-key";

/// Shared HS256 secret backing both minting and JWKS-served validation.
const SECRET: &[u8] = b"aion-test-jwt-shared-secret";

/// Serve a JWKS document on an ephemeral local port and return its URL.
///
/// The server task lives on the calling test's runtime; the production
/// [`crate::auth::JwksCache`] fetches from it exactly as it would from a real
/// issuer.
pub(crate) fn serve_jwks() -> Result<String, std::io::Error> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    tokio::spawn(async move {
        let router = Router::new().route("/jwks.json", get(jwks_document));
        if let Err(error) = axum::serve(listener, router).await {
            tracing::warn!(%error, "fixture jwks server exited with error");
        }
    });
    Ok(format!("http://{address}/jwks.json"))
}

async fn jwks_document() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "keys": [{
            "kty": "oct",
            "kid": KEY_ID,
            "alg": "HS256",
            "k": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(SECRET),
        }]
    }))
}

/// Mint a valid signed caller token for `subject` granting `namespace`.
pub(crate) fn mint_token(
    subject: &str,
    namespace: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    mint(
        subject,
        namespace,
        jsonwebtoken::get_current_timestamp() + 3600,
    )
}

/// Mint a correctly signed but already-expired token.
pub(crate) fn mint_expired_token(
    subject: &str,
    namespace: &str,
) -> Result<String, jsonwebtoken::errors::Error> {
    mint(
        subject,
        namespace,
        jsonwebtoken::get_current_timestamp().saturating_sub(3600),
    )
}

fn mint(subject: &str, namespace: &str, exp: u64) -> Result<String, jsonwebtoken::errors::Error> {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KEY_ID.to_owned());
    let claims = serde_json::json!({
        "sub": subject,
        "namespace": namespace,
        "exp": exp,
    });
    jsonwebtoken::encode(&header, &claims, &EncodingKey::from_secret(SECRET))
}
