//! JWKS-backed JWT validation cache.

use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{JwkSet, KeyAlgorithm},
};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::{CallerIdentity, ServerError};

/// Authenticated JWT claims used by transport adapters.
#[derive(Clone, Debug)]
pub struct AuthenticatedClaims {
    subject: String,
    namespace: String,
    expires_at: u64,
}

impl AuthenticatedClaims {
    /// Caller subject from the `sub` claim.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Namespace grant from the `namespace` claim.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Expiration timestamp from the `exp` claim, in Unix seconds.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Convert validated claims into a single-namespace caller identity whose
    /// grant is attributed to the token's namespace claim, so namespace
    /// denials hint at the token grant rather than the development header.
    #[must_use]
    pub fn caller_identity(&self) -> CallerIdentity {
        CallerIdentity::from_token_claims(self.subject.clone(), [self.namespace.clone()])
    }
}

/// Shared JWKS cache refreshed in the background.
#[derive(Clone)]
pub struct JwksCache {
    inner: Arc<JwksCacheInner>,
}

struct JwksCacheInner {
    url: String,
    client: reqwest::Client,
    keys: RwLock<HashMap<String, CachedKey>>,
}

#[derive(Clone)]
struct CachedKey {
    key: DecodingKey,
    algorithm: Algorithm,
}

#[derive(Debug, Deserialize)]
struct TokenClaims {
    sub: String,
    namespace: String,
    exp: u64,
}

/// Redacted JWT/JWKS validation errors.
#[derive(Debug, thiserror::Error)]
pub enum JwksError {
    /// Bearer token was malformed or could not be verified.
    #[error("invalid bearer token")]
    InvalidToken,
    /// The validated token did not include required Aion claims.
    #[error("bearer token is missing required claims")]
    MissingClaims,
    /// JWKS retrieval failed before a usable cache was available.
    #[error("jwks fetch failed: {0}")]
    Fetch(String),
}

impl JwksCache {
    /// Fetch the initial JWKS and start a periodic refresh task.
    ///
    /// # Errors
    ///
    /// Returns [`JwksError::Fetch`] when the initial key set cannot be fetched or parsed.
    pub async fn new(url: String, refresh_interval: Duration) -> Result<Self, JwksError> {
        let cache = Self {
            inner: Arc::new(JwksCacheInner {
                url,
                client: reqwest::Client::new(),
                keys: RwLock::new(HashMap::new()),
            }),
        };
        cache.refresh_now().await?;
        cache.spawn_refresh(refresh_interval);
        Ok(cache)
    }

    /// Validate a JWT against cached keys, refreshing once when `kid` is unknown.
    ///
    /// # Errors
    ///
    /// Returns [`JwksError::InvalidToken`] for malformed, expired, or signature-invalid tokens.
    pub async fn validate(&self, token: &str) -> Result<AuthenticatedClaims, JwksError> {
        let header = decode_header(token).map_err(|_error| JwksError::InvalidToken)?;
        let kid = header.kid.ok_or(JwksError::InvalidToken)?;
        let cached = self.cached_key(&kid).await;
        let cached = if let Some(cached) = cached {
            cached
        } else {
            if let Err(error) = self.refresh_now().await {
                warn!(error = %error, kid = %kid, "jwks refresh failed for unknown key id");
            }
            self.cached_key(&kid).await.ok_or(JwksError::InvalidToken)?
        };

        let mut validation = Validation::new(cached.algorithm);
        validation.validate_aud = false;
        let claims = decode::<TokenClaims>(token, &cached.key, &validation)
            .map_err(|_error| JwksError::InvalidToken)?
            .claims;
        if claims.sub.is_empty() || claims.namespace.is_empty() {
            return Err(JwksError::MissingClaims);
        }
        Ok(AuthenticatedClaims {
            subject: claims.sub,
            namespace: claims.namespace,
            expires_at: claims.exp,
        })
    }

    /// Fetch and install the latest JWKS, preserving cached keys if the fetch fails.
    ///
    /// # Errors
    ///
    /// Returns [`JwksError::Fetch`] when the endpoint cannot be fetched or contains no usable keyed keys.
    pub async fn refresh_now(&self) -> Result<(), JwksError> {
        let response = self
            .inner
            .client
            .get(&self.inner.url)
            .send()
            .await
            .map_err(|error| JwksError::Fetch(error.to_string()))?;
        let jwks = response
            .error_for_status()
            .map_err(|error| JwksError::Fetch(error.to_string()))?
            .json::<JwkSet>()
            .await
            .map_err(|error| JwksError::Fetch(error.to_string()))?;
        let keys = keys_from_set(jwks)?;
        *self.inner.keys.write().await = keys;
        debug!(url = %self.inner.url, "jwks cache refreshed");
        Ok(())
    }

    async fn cached_key(&self, kid: &str) -> Option<CachedKey> {
        self.inner.keys.read().await.get(kid).cloned()
    }

    fn spawn_refresh(&self, refresh_interval: Duration) {
        let cache = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(refresh_interval);
            loop {
                interval.tick().await;
                if let Err(error) = cache.refresh_now().await {
                    warn!(error = %error, "scheduled jwks refresh failed; retaining cached keys");
                }
            }
        });
    }
}

impl TryFrom<JwksError> for ServerError {
    type Error = JwksError;

    fn try_from(error: JwksError) -> Result<Self, Self::Error> {
        match error {
            JwksError::Fetch(message) => Ok(Self::Config {
                message: format!("auth jwks initial fetch failed: {message}"),
            }),
            other => Err(other),
        }
    }
}

fn keys_from_set(jwks: JwkSet) -> Result<HashMap<String, CachedKey>, JwksError> {
    let mut keys = HashMap::new();
    for jwk in jwks.keys {
        let Some(kid) = jwk.common.key_id.clone() else {
            continue;
        };
        let Some(algorithm) = jwk.common.key_algorithm.and_then(key_algorithm) else {
            continue;
        };
        let key = DecodingKey::from_jwk(&jwk).map_err(|_error| JwksError::InvalidToken)?;
        keys.insert(kid, CachedKey { key, algorithm });
    }
    if keys.is_empty() {
        return Err(JwksError::Fetch(
            "jwks endpoint returned no usable keyed signing keys".to_owned(),
        ));
    }
    Ok(keys)
}

fn key_algorithm(algorithm: KeyAlgorithm) -> Option<Algorithm> {
    Algorithm::from_str(&algorithm.to_string()).ok()
}

fn now_unix_seconds() -> u64 {
    jsonwebtoken::get_current_timestamp()
}

/// Return true when a validated token is expired at the current wall clock.
#[must_use]
pub fn is_expired(expires_at: u64) -> bool {
    expires_at <= now_unix_seconds()
}
