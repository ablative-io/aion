//! Deserialize-only operator configuration for the server.

use std::{fs, net::SocketAddr, path::PathBuf, time::Duration};

use aion_store_libsql::LibSqlConfig;
use serde::Deserialize;

use crate::error::ServerError;

/// Complete operator-supplied server configuration.
#[derive(Deserialize)]
pub struct ServerConfig {
    /// Event-store backend configuration.
    pub store: StoreConfig,
    /// Listener addresses for public transports.
    pub listen: ListenConfig,
    /// Optional TLS material for transports that require it.
    pub tls: Option<TlsConfig>,
    /// Authentication configuration applied at adapter boundaries.
    pub auth: AuthConfig,
    /// Static dashboard asset bundle location.
    pub dashboard: DashboardConfig,
    /// Namespace isolation mode.
    pub namespace: NamespaceConfig,
    /// Remote-worker heartbeat policy.
    pub worker: WorkerConfig,
    /// WebSocket event streaming policy.
    pub websocket: WebSocketConfig,
}

/// Event store backend selection.
#[derive(Deserialize)]
pub struct StoreConfig {
    /// libSQL backend configuration.
    pub libsql: LibSqlConfig,
}

/// Public transport listener addresses.
#[derive(Clone, Deserialize)]
pub struct ListenConfig {
    /// gRPC API and worker-protocol listener.
    pub grpc: SocketAddr,
    /// HTTP/JSON and dashboard listener.
    pub http: SocketAddr,
}

/// TLS certificate and private-key material.
#[derive(Clone, Deserialize)]
pub struct TlsConfig {
    /// Certificate chain path supplied by the operator.
    pub certificate_chain_path: PathBuf,
    /// Private-key path supplied by the operator.
    pub private_key_path: PathBuf,
}

/// Authentication mode and credentials.
#[derive(Clone, Deserialize)]
pub struct AuthConfig {
    /// Bearer token required by clients and workers.
    pub bearer_token: String,
}

/// Dashboard static asset configuration.
#[derive(Clone, Deserialize)]
pub struct DashboardConfig {
    /// Directory containing the built dashboard bundle.
    pub asset_path: PathBuf,
}

/// Namespace resolver construction mode.
#[derive(Clone, Deserialize)]
pub struct NamespaceConfig {
    /// Deployment-selected namespace mapping mode.
    pub mode: NamespaceMode,
}

/// Supported namespace mapping modes.
#[derive(Clone, Deserialize)]
pub enum NamespaceMode {
    /// All authorized namespaces share the configured engine instance.
    SharedEngine,
    /// Namespace authorization is disabled only for single-tenant deployments.
    SingleTenant {
        /// The only namespace accepted by the deployment.
        namespace: String,
    },
}

/// Remote worker heartbeat configuration.
#[derive(Clone, Deserialize)]
pub struct WorkerConfig {
    /// Window after which a silent worker is considered lost.
    #[serde(with = "duration_millis")]
    pub heartbeat_window: Duration,
}

/// WebSocket stream configuration.
#[derive(Clone, Deserialize)]
pub struct WebSocketConfig {
    /// Per-connection outbound buffer bound.
    pub outbound_buffer_bound: usize,
}

/// Runtime settings retained in shared server state for transport adapters.
#[derive(Clone)]
pub struct RuntimeConfig {
    /// Listener addresses for public transports.
    pub listen: ListenConfig,
    /// Optional TLS material for public transports.
    pub tls: Option<TlsConfig>,
    /// Authentication configuration shared by transports.
    pub auth: AuthConfig,
    /// Dashboard asset location.
    pub dashboard: DashboardConfig,
    /// Namespace resolver construction mode.
    pub namespace: NamespaceConfig,
    /// Remote worker heartbeat configuration.
    pub worker: WorkerConfig,
    /// WebSocket stream configuration.
    pub websocket: WebSocketConfig,
}

impl ServerConfig {
    /// Load server configuration from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] when the file cannot be read, cannot be
    /// parsed, or omits any required operational value.
    pub fn load_from_path(path: impl Into<PathBuf>) -> Result<Self, ServerError> {
        let path = path.into();
        let bytes = fs::read(&path).map_err(|source| ServerError::Config {
            message: format!("failed to read config `{}`: {source}", path.display()),
        })?;
        Self::from_slice(&bytes)
    }

    /// Parse server configuration from JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] when parsing fails or required fields are
    /// absent.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ServerError> {
        serde_json::from_slice(bytes).map_err(|source| ServerError::Config {
            message: format!("invalid server config: {source}"),
        })
    }

    /// Split store configuration from non-secret runtime settings.
    #[must_use]
    pub fn into_parts(self) -> (StoreConfig, RuntimeConfig) {
        let runtime = RuntimeConfig {
            listen: self.listen,
            tls: self.tls,
            auth: self.auth,
            dashboard: self.dashboard,
            namespace: self.namespace,
            worker: self.worker,
            websocket: self.websocket,
        };
        (self.store, runtime)
    }
}

mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer};

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::ServerConfig;

    #[test]
    fn missing_required_operational_value_is_config_error() {
        let result = ServerConfig::from_slice(
            br#"{
                "store": { "libsql": { "mode": { "Embedded": { "path": "aion.db" } } } },
                "listen": { "grpc": "127.0.0.1:50051", "http": "127.0.0.1:8080" },
                "auth": { "bearer_token": "secret" },
                "dashboard": { "asset_path": "dist" },
                "namespace": { "mode": "SharedEngine" },
                "worker": { "heartbeat_window": 30000 },
                "websocket": {}
            }"#,
        );

        assert!(result.is_err());
    }
}
