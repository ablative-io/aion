//! Shared server state constructed once at startup.

use std::sync::Arc;

use aion::{Engine, EngineBuilder};
use aion_store::EventStore;
use aion_store_libsql::LibSqlStore;

use crate::{
    config::{RuntimeConfig, ServerConfig, StoreConfig},
    error::ServerError,
    namespace::resolver::NamespaceResolver,
};

/// Cloneable shared state passed to all server transports.
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<ServerStateInner>,
}

struct ServerStateInner {
    engine: Arc<Engine>,
    namespace_resolver: NamespaceResolver,
    runtime: RuntimeConfig,
}

impl ServerState {
    /// Build shared state from operator configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the store cannot connect or the engine cannot
    /// be constructed.
    pub async fn build(config: ServerConfig) -> Result<Self, ServerError> {
        let (store_config, runtime) = config.into_parts();
        let store = connect_store(store_config).await?;
        Self::build_with_store(store, runtime).await
    }

    /// Build shared state from an already-constructed store.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::EngineCall`] if the engine cannot be constructed.
    pub async fn build_with_store<S>(store: S, runtime: RuntimeConfig) -> Result<Self, ServerError>
    where
        S: EventStore,
    {
        let namespace_resolver = NamespaceResolver::from_config(runtime.namespace.clone());
        let engine = EngineBuilder::new().store(store).build().await?;
        Ok(Self::from_parts(
            Arc::new(engine),
            namespace_resolver,
            runtime,
        ))
    }

    /// Build shared state from explicit parts.
    #[must_use]
    pub fn from_parts(
        engine: Arc<Engine>,
        namespace_resolver: NamespaceResolver,
        runtime: RuntimeConfig,
    ) -> Self {
        Self {
            inner: Arc::new(ServerStateInner {
                engine,
                namespace_resolver,
                runtime,
            }),
        }
    }

    /// Borrow the engine handle. Handlers in later briefs should only use this
    /// after namespace resolution at the adapter boundary.
    #[must_use]
    pub fn engine(&self) -> &Arc<Engine> {
        &self.inner.engine
    }

    /// Borrow the namespace resolver shared by all transports.
    #[must_use]
    pub fn namespace_resolver(&self) -> &NamespaceResolver {
        &self.inner.namespace_resolver
    }

    /// Borrow non-secret runtime settings needed by transports.
    #[must_use]
    pub fn runtime_config(&self) -> &RuntimeConfig {
        &self.inner.runtime
    }
}

async fn connect_store(config: StoreConfig) -> Result<LibSqlStore, ServerError> {
    LibSqlStore::connect(config.libsql)
        .await
        .map_err(ServerError::from)
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::PathBuf, time::Duration};

    use aion_store::InMemoryStore;

    use super::ServerState;
    use crate::config::{
        AuthConfig, DashboardConfig, ListenConfig, NamespaceConfig, NamespaceMode, RuntimeConfig,
        WebSocketConfig, WorkerConfig,
    };

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            listen: ListenConfig {
                grpc: SocketAddr::from(([127, 0, 0, 1], 50051)),
                http: SocketAddr::from(([127, 0, 0, 1], 8080)),
            },
            tls: None,
            auth: AuthConfig {
                bearer_token: "test-token".to_owned(),
            },
            dashboard: DashboardConfig {
                asset_path: PathBuf::from("dist"),
            },
            namespace: NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            worker: WorkerConfig {
                heartbeat_window: Duration::from_millis(30_000),
            },
            websocket: WebSocketConfig {
                outbound_buffer_bound: 32,
            },
        }
    }

    #[tokio::test]
    async fn builds_state_with_in_memory_store() -> Result<(), Box<dyn std::error::Error>> {
        let state =
            ServerState::build_with_store(InMemoryStore::default(), runtime_config()).await?;

        std::hint::black_box(state.engine());
        std::hint::black_box(state.namespace_resolver());

        Ok(())
    }
}
