//! Shared server state constructed once at startup.

use std::{path::PathBuf, sync::Arc};

use aion::{EngineBuilder, RuntimeHandle, SignalRouter, signal::ConcreteSignalRouter};
use aion_store::EventStore;
use aion_store_libsql::LibSqlStore;

#[cfg(feature = "auth")]
use crate::auth::JwksCache;
use crate::{
    config::{RuntimeConfig, ServerConfig, StoreBackend, StoreConfig},
    error::ServerError,
    namespace::{NamespaceGuard, resolver::NamespaceResolver},
    observability::{
        Metrics, health::HealthState, instrumented_store::InstrumentedEventStore,
        metrics::MetricsError,
    },
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
    metrics: Option<Metrics>,
    health: Option<HealthState>,
    #[cfg(feature = "auth")]
    jwks_cache: Option<JwksCache>,
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
        // The server unconditionally mounts /events/stream, so the engine's
        // broadcast channel must be installed and explicitly sized here —
        // a mounted-but-unconfigured streaming endpoint is never acceptable.
        let event_broadcast_capacity = runtime
            .websocket
            .event_broadcast_capacity
            .and_then(std::num::NonZeroUsize::new)
            .ok_or_else(|| ServerError::Config {
                message: crate::config::EVENT_BROADCAST_CAPACITY_REQUIRED.to_owned(),
            })?;
        // The server unconditionally mounts /workflows/query, so the engine's
        // query seam must be installed with an explicitly configured reply
        // deadline here — a mounted-but-unconfigured query surface is never
        // acceptable.
        let query_timeout = runtime
            .query_timeout
            .filter(|timeout| !timeout.is_zero())
            .ok_or_else(|| ServerError::Config {
                message: crate::config::QUERY_TIMEOUT_REQUIRED.to_owned(),
            })?;
        let metrics = Metrics::new().map_err(|error| metrics_config_error(&error))?;
        let instrumented_store = Arc::new(InstrumentedEventStore::new(
            store.clone(),
            metrics.clone(),
            runtime.default_namespace.clone(),
        ));
        let exported_metrics = runtime.metrics.enabled.then_some(metrics.clone());
        let worker_registry = ConnectedWorkerRegistry::default();
        let active_registry = Arc::new(aion::Registry::default());
        let pending_activities = PendingActivities::default();
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        let drain_state = DrainState::default();
        let dispatcher = WorkerActivityDispatcher::new(
            worker_registry.clone(),
            runtime.default_namespace.clone(),
            heartbeat_tracker.clone(),
        )
        .with_pending(pending_activities.clone())
        .with_drain_state(drain_state.clone())
        .with_tokio_handle(tokio::runtime::Handle::current());
        let dispatcher = Arc::new(dispatcher);

        let mut search_attribute_schema = aion_core::SearchAttributeSchema::new();
        search_attribute_schema
            .register(
                crate::namespace::NAMESPACE_ATTRIBUTE,
                aion_core::SearchAttributeType::String,
            )
            .map_err(|error| ServerError::Config {
                message: format!("failed to register namespace search attribute: {error}"),
            })?;
        let engine = EngineBuilder::new()
            .store_arc(instrumented_store.clone())
            .event_streaming(event_broadcast_capacity)
            .in_memory_visibility()
            .search_attribute_schema(search_attribute_schema)
            .scheduler_threads(runtime.scheduler_threads)
            .activity_dispatcher(dispatcher)
            .active_registry(active_registry)
            .production_recovery_seam()
            .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
                Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
            })
            .query_timeout(query_timeout)
            .load_workflow_sources(runtime.workflow_packages.iter().map(PathBuf::as_path))
            .build()
            .await?;
        let engine = Arc::new(engine);
        let namespace_resolver = NamespaceResolver::from_config(runtime.namespace.clone(), engine);
        #[cfg(feature = "auth")]
        let jwks_cache = build_jwks_cache(&runtime).await?;
        Ok(Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry,
                pending_activities,
                heartbeat_tracker,
                drain_state,
                metrics: exported_metrics,
                health: Some(HealthState::new(instrumented_store, true)),
                #[cfg(feature = "auth")]
                jwks_cache,
            }),
        })
    }

    /// Build shared state from explicit parts with a default worker registry.
    #[must_use]
    pub fn from_parts(namespace_resolver: NamespaceResolver, runtime: RuntimeConfig) -> Self {
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry: ConnectedWorkerRegistry::default(),
                pending_activities: PendingActivities::default(),
                heartbeat_tracker,
                drain_state: DrainState::default(),
                metrics: None,
                health: None,
                #[cfg(feature = "auth")]
                jwks_cache: None,
            }),
        }
    }

    /// Build shared state from explicit parts with a caller-supplied JWKS cache.
    ///
    /// Embedders that construct their own [`JwksCache`] (for example against a
    /// private issuer) can install it here; transports then validate bearer
    /// tokens against it exactly as with a [`Self::build`]-constructed state.
    #[cfg(feature = "auth")]
    #[must_use]
    pub fn from_parts_with_jwks(
        namespace_resolver: NamespaceResolver,
        runtime: RuntimeConfig,
        jwks_cache: JwksCache,
    ) -> Self {
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry: ConnectedWorkerRegistry::default(),
                pending_activities: PendingActivities::default(),
                heartbeat_tracker,
                drain_state: DrainState::default(),
                metrics: None,
                health: None,
                jwks_cache: Some(jwks_cache),
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
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(namespace_resolver),
                runtime,
                worker_registry,
                pending_activities: PendingActivities::default(),
                heartbeat_tracker,
                drain_state: DrainState::default(),
                metrics: None,
                health: None,
                #[cfg(feature = "auth")]
                jwks_cache: None,
            }),
        }
    }

    /// Borrow the namespace guard shared by all transports.
    #[must_use]
    pub fn namespace_guard(&self) -> &NamespaceGuard {
        &self.inner.namespace_guard
    }

    /// Build the deploy authorization guard over the shared resolver.
    #[must_use]
    pub fn deploy_guard(&self) -> crate::deploy::DeployGuard {
        crate::deploy::DeployGuard::new(self.inner.namespace_guard.resolver().clone())
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

    /// Borrow the prometheus metrics handle when this state was built with a store.
    #[must_use]
    pub fn metrics(&self) -> Option<&Metrics> {
        self.inner.metrics.as_ref()
    }

    /// Borrow health probe state when this state was built with a store.
    #[must_use]
    pub fn health(&self) -> Option<&HealthState> {
        self.inner.health.as_ref()
    }

    /// Borrow the shared JWKS cache when authentication is enabled.
    #[cfg(feature = "auth")]
    #[must_use]
    pub fn jwks_cache(&self) -> Option<&JwksCache> {
        self.inner.jwks_cache.as_ref()
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

#[cfg(feature = "auth")]
async fn build_jwks_cache(runtime: &RuntimeConfig) -> Result<Option<JwksCache>, ServerError> {
    if !runtime.auth.enabled {
        return Ok(None);
    }
    let Some(url) = runtime.auth.jwks_url.clone() else {
        return Err(ServerError::Config {
            message: "auth.jwks_url must not be empty when auth.enabled is true".to_owned(),
        });
    };
    let interval = std::time::Duration::from_secs(runtime.auth.jwks_refresh_seconds);
    let cache = JwksCache::new(url, interval)
        .await
        .map_err(|error| ServerError::Config {
            message: format!("auth jwks initial fetch failed: {error}"),
        })?;
    Ok(Some(cache))
}

fn metrics_config_error(error: &MetricsError) -> ServerError {
    ServerError::Config {
        message: error.to_string(),
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
            let store = LibSqlStore::open(url.clone())
                .await
                .map_err(ServerError::from)?;
            store
                .validate_event_compatibility()
                .await
                .map_err(|error| match error {
                    aion_store::StoreError::Serialization(_) => ServerError::Config {
                        message: format!(
                            "Database schema mismatch — delete {url} and restart, or run migrations."
                        ),
                    },
                    other => ServerError::from(other),
                })?;
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
        AuthConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
        MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig,
        WorkerConfig,
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
                event_broadcast_capacity: Some(64),
            },
            workflow_packages: Vec::new(),
            deploy: DeployConfig::default(),
            scheduler_threads: 1,
            query_timeout: Some(Duration::from_millis(10_000)),
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

    #[tokio::test]
    async fn state_build_fails_without_event_broadcast_capacity()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut runtime = runtime_config();
        runtime.websocket.event_broadcast_capacity = None;

        let error = ServerState::build_with_store(InMemoryStore::default(), runtime)
            .await
            .err()
            .ok_or("state build must fail when event streaming is unsized")?;

        assert!(error.is_config(), "expected a config error, got {error}");
        assert!(
            error
                .to_string()
                .contains("websocket.event_broadcast_capacity"),
            "error must name the missing key: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn state_build_fails_without_query_timeout() -> Result<(), Box<dyn std::error::Error>> {
        let mut runtime = runtime_config();
        runtime.query_timeout = None;

        let error = ServerState::build_with_store(InMemoryStore::default(), runtime)
            .await
            .err()
            .ok_or("state build must fail when the query reply deadline is unset")?;

        assert!(error.is_config(), "expected a config error, got {error}");
        assert!(
            error.to_string().contains("runtime.query_timeout_ms"),
            "error must name the missing key: {error}"
        );
        assert!(
            error.to_string().contains("AION_RUNTIME_QUERY_TIMEOUT_MS"),
            "error must name the environment override: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn state_build_fails_with_zero_query_timeout() -> Result<(), Box<dyn std::error::Error>> {
        let mut runtime = runtime_config();
        runtime.query_timeout = Some(Duration::ZERO);

        let error = ServerState::build_with_store(InMemoryStore::default(), runtime)
            .await
            .err()
            .ok_or("state build must fail when the query reply deadline is zero")?;

        assert!(error.is_config(), "expected a config error, got {error}");
        assert!(
            error.to_string().contains("runtime.query_timeout_ms"),
            "error must name the zero-valued key: {error}"
        );
        Ok(())
    }
}
