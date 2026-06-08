//! Shared server state constructed once at startup.

use std::{path::PathBuf, sync::Arc};

use aion::{EngineBuilder, RuntimeHandle, SignalRouter, signal::ConcreteSignalRouter};
use aion_store::EventStore;
use aion_store_libsql::LibSqlStore;

use crate::{
    config::{RuntimeConfig, ServerConfig, StoreBackend, StoreConfig},
    error::ServerError,
    namespace::{NamespaceGuard, resolver::NamespaceResolver},
    shutdown::DrainState,
    worker::{
        ConnectedWorkerRegistry, HeartbeatTracker, PendingActivities, WorkerActivityDispatcher,
    },
};

/// Cloneable shared state passed to all server transports.
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<ServerStateInner>,
}

struct ServerStateInner {
    namespace_guard: NamespaceGuard,
    runtime: RuntimeConfig,
    worker_registry: ConnectedWorkerRegistry,
    pending_activities: PendingActivities,
    heartbeat_tracker: HeartbeatTracker,
    drain_state: DrainState,
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
        Self::build_with_store_arc(store, runtime).await
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
        Self::build_with_store_arc(Arc::new(store), runtime).await
    }

    async fn build_with_store_arc(
        store: Arc<dyn EventStore>,
        runtime: RuntimeConfig,
    ) -> Result<Self, ServerError> {
        let worker_registry = ConnectedWorkerRegistry::default();
        let pending_activities = PendingActivities::default();
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        let drain_state = DrainState::default();
        let dispatcher = Arc::new(
            WorkerActivityDispatcher::new(
                worker_registry.clone(),
                runtime.default_namespace.clone(),
            )
            .with_pending(pending_activities.clone())
            .with_heartbeat_tracker(heartbeat_tracker.clone())
            .with_drain_state(drain_state.clone()),
        );

        let engine = EngineBuilder::new()
            .store_arc(store)
            .scheduler_threads(runtime.scheduler_threads)
            .activity_dispatcher(dispatcher)
            .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
                Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
            })
            .load_workflow_sources(runtime.workflow_packages.iter().map(PathBuf::as_path))
            .build()
            .await?;
        let engine = Arc::new(engine);
        let namespace_resolver = NamespaceResolver::from_config(runtime.namespace.clone(), engine);
        Ok(Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry,
                pending_activities,
                heartbeat_tracker,
                drain_state,
            }),
        })
    }

    /// Build shared state from explicit parts with a default worker registry.
    #[must_use]
    pub fn from_parts(namespace_resolver: NamespaceResolver, runtime: RuntimeConfig) -> Self {
        Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry: ConnectedWorkerRegistry::default(),
                pending_activities: PendingActivities::default(),
                heartbeat_tracker: HeartbeatTracker::new(std::time::Duration::from_secs(30)),
                drain_state: DrainState::default(),
            }),
        }
    }

    /// Build shared state from explicit parts with a caller-supplied registry.
    #[must_use]
    pub fn from_parts_with_registry(
        namespace_resolver: NamespaceResolver,
        runtime: RuntimeConfig,
        worker_registry: ConnectedWorkerRegistry,
    ) -> Self {
        Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry,
                pending_activities: PendingActivities::default(),
                heartbeat_tracker: HeartbeatTracker::new(std::time::Duration::from_secs(30)),
                drain_state: DrainState::default(),
            }),
        }
    }

    /// Borrow the namespace guard shared by all transports.
    #[must_use]
    pub fn namespace_guard(&self) -> &NamespaceGuard {
        &self.inner.namespace_guard
    }

    /// Borrow non-secret runtime settings needed by transports.
    #[must_use]
    pub fn runtime_config(&self) -> &RuntimeConfig {
        &self.inner.runtime
    }

    /// Borrow the connected-worker registry shared by worker transports and dispatch.
    #[must_use]
    pub fn worker_registry(&self) -> &ConnectedWorkerRegistry {
        &self.inner.worker_registry
    }

    /// Borrow the pending-activities tracker shared by the NIF bridge and worker stream handler.
    #[must_use]
    pub fn pending_activities(&self) -> &PendingActivities {
        &self.inner.pending_activities
    }

    /// Borrow the heartbeat/liveness tracker shared by dispatch and worker streams.
    #[must_use]
    pub fn heartbeat_tracker(&self) -> &HeartbeatTracker {
        &self.inner.heartbeat_tracker
    }

    /// Borrow the drain gate shared by transports and worker dispatch.
    #[must_use]
    pub fn drain_state(&self) -> &DrainState {
        &self.inner.drain_state
    }

    /// Shut down the embedded engine so in-flight durable appends can finish.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the namespace resolver has no engine handle or the engine rejects
    /// shutdown.
    pub fn shutdown(&self) -> Result<(), ServerError> {
        self.inner.namespace_guard.resolver().shutdown_engine()
    }
}

async fn connect_store(config: StoreConfig) -> Result<Arc<dyn EventStore>, ServerError> {
    match config.backend {
        StoreBackend::Memory => Ok(Arc::new(aion_store::InMemoryStore::default())),
        StoreBackend::LibSql => {
            let Some(url) = config.url else {
                return Err(ServerError::Config {
                    message: "store.url must not be empty when store.backend is libsql".to_owned(),
                });
            };
            let store = LibSqlStore::open(url).await.map_err(ServerError::from)?;
            Ok(Arc::new(store))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use aion_store::InMemoryStore;

    use super::ServerState;
    use crate::config::{
        AuthConfig, DashboardAssetSource, DashboardConfig, ListenConfig, MetricsConfig,
        NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig, WorkerConfig,
    };

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            listen: ListenConfig {
                grpc: SocketAddr::from(([127, 0, 0, 1], 50051)),
                http: SocketAddr::from(([127, 0, 0, 1], 8080)),
            },
            tls: None,
            auth: AuthConfig {
                enabled: false,
                jwks_url: None,
                jwks_refresh_seconds: 300,
            },
            dashboard: DashboardConfig {
                source: DashboardAssetSource::Embedded,
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
            workflow_packages: Vec::new(),
            scheduler_threads: 1,
            default_namespace: "default".to_owned(),
            drain_timeout: Duration::from_secs(30),
            metrics: MetricsConfig { enabled: true },
        }
    }

    #[tokio::test]
    async fn builds_state_with_in_memory_store() -> Result<(), Box<dyn std::error::Error>> {
        let state =
            ServerState::build_with_store(InMemoryStore::default(), runtime_config()).await?;

        std::hint::black_box(state.namespace_guard());
        std::hint::black_box(state.worker_registry());

        Ok(())
    }
}
