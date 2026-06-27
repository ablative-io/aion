//! Shared server state constructed once at startup.

use std::{path::PathBuf, sync::Arc};

use aion::{
    ActivityDispatcher, EngineBuilder, RuntimeHandle, SignalRouter, signal::ConcreteSignalRouter,
};
use aion_store::{EventStore, OutboxStore};
use aion_store_libsql::LibSqlStore;

use crate::dev_ui::{ActivityMockRegistry, DevMockingDispatcher};

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
    /// Shared per-run activity-mock registry. Present only when the dev surface
    /// is commissioned; the engine's dispatcher consults this exact instance.
    activity_mock_registry: Option<ActivityMockRegistry>,
    /// The leaf libSQL store cast as an [`OutboxStore`], shared with the engine's
    /// `EventStore` so the outbox dispatcher writes through the same single
    /// `libsql::Connection`. `None` for the in-memory backend (no outbox table).
    outbox_store: Option<Arc<dyn OutboxStore>>,
    /// Owns the distributed haematite inbound-write responder thread, kept alive
    /// for the server's lifetime so a cluster node keeps answering peers'
    /// replication/election traffic. `None` for non-distributed boots. Dropping
    /// the state stops the responder.
    #[cfg(feature = "haematite-backend")]
    cluster_responder: Option<aion_store_haematite::ClusterResponder>,
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
        let connected = connect_store(store_config).await?;
        Self::build_with_connected_store(connected, runtime).await
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
        Self::build_with_connected_store(ConnectedStore::local(Arc::new(store), None), runtime)
            .await
    }

    async fn build_with_connected_store(
        connected: ConnectedStore,
        runtime: RuntimeConfig,
    ) -> Result<Self, ServerError> {
        let store = connected.event_store;
        let outbox_store = connected.outbox_store;
        let bootstrap_coordinator = connected.bootstrap_coordinator;
        #[cfg(feature = "haematite-backend")]
        let cluster_responder = connected.cluster_responder;
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
        // Dark by default: only when the dev surface is commissioned does the
        // engine receive the per-run activity-mock decorator. With it off the
        // engine gets the bare production dispatcher, so a production server has
        // no mocking path at all (CN4).
        let (activity_dispatcher, activity_mock_registry): (Arc<dyn ActivityDispatcher>, _) =
            if runtime.dev.enabled {
                let registry = ActivityMockRegistry::new();
                let decorated = DevMockingDispatcher::new(Arc::new(dispatcher), registry.clone());
                (Arc::new(decorated), Some(registry))
            } else {
                (Arc::new(dispatcher), None)
            };

        let engine = build_engine(EngineAssembly {
            instrumented_store: &instrumented_store,
            event_broadcast_capacity,
            query_timeout,
            activity_dispatcher,
            active_registry,
            bootstrap_coordinator,
            runtime: &runtime,
        })
        .await?;
        let engine = Arc::new(engine);
        // Outbox ON: route unmatched worker completions arriving at the sink
        // into the live workflow's mailbox. Flag-off this callback is never
        // installed, so the sink's unmatched branch stays a silent drop. The
        // dispatcher is not rebuilt — it shares this exact pending tracker.
        if runtime.outbox.enabled {
            let callback = Arc::new(crate::worker::ServerOutboxDeliveryCallback::new(
                Arc::clone(&engine),
            ));
            pending_activities.set_outbox_delivery(callback);
        }
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
                activity_mock_registry,
                outbox_store,
                #[cfg(feature = "haematite-backend")]
                cluster_responder,
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
                activity_mock_registry: None,
                outbox_store: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
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
                activity_mock_registry: None,
                outbox_store: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
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
                activity_mock_registry: None,
                outbox_store: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
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

    /// Borrow the shared per-run activity-mock registry when the dev surface is
    /// commissioned. Returns [`None`] on a server with the dev surface dark, so
    /// the dev handlers refuse cleanly rather than mocking on a production
    /// server.
    #[must_use]
    pub fn activity_mock_registry(&self) -> Option<&ActivityMockRegistry> {
        self.inner.activity_mock_registry.as_ref()
    }

    /// Borrow the outbox store the dispatcher claims rows from, when the durable
    /// (libSQL) backend is in use. This is the SAME leaf `Arc<LibSqlStore>` the
    /// engine writes through, so the dispatcher shares its single
    /// `libsql::Connection` rather than opening a second contending one. Returns
    /// [`None`] for the in-memory backend, which has no outbox table.
    #[must_use]
    pub fn outbox_store(&self) -> Option<Arc<dyn OutboxStore>> {
        self.inner.outbox_store.clone()
    }

    /// Whether this server is a node in a distributed haematite cluster.
    ///
    /// `true` when boot constructed the distributed backend (a `[store.cluster]`
    /// section was present) and is holding its inbound-write responder alive;
    /// `false` for every single-node / non-haematite boot.
    #[cfg(feature = "haematite-backend")]
    #[must_use]
    pub fn is_clustered(&self) -> bool {
        self.inner.cluster_responder.is_some()
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

/// Borrowed inputs assembled into the embedded engine by [`build_engine`].
struct EngineAssembly<'a> {
    /// The metrics-instrumented store the engine writes through.
    instrumented_store: &'a Arc<InstrumentedEventStore>,
    /// Explicitly-sized broadcast channel capacity for `/events/stream`.
    event_broadcast_capacity: std::num::NonZeroUsize,
    /// Explicit workflow-query reply deadline for `/workflows/query`.
    query_timeout: std::time::Duration,
    /// The activity dispatcher (optionally dev-mock-decorated) the engine uses.
    activity_dispatcher: Arc<dyn ActivityDispatcher>,
    /// The shared active-workflow registry server dispatchers correlate against.
    active_registry: Arc<aion::Registry>,
    /// Whether THIS node seeds the schedule coordinator (SS-2 ownership gate).
    bootstrap_coordinator: bool,
    /// Non-secret runtime settings driving scheduler/outbox/package/shard knobs.
    runtime: &'a RuntimeConfig,
}

/// Assemble the embedded engine from the server's runtime configuration.
///
/// Factored out of [`ServerState::build_with_connected_store`] to keep that
/// method within length bounds; it carries the SS-2 wiring — the coordinator
/// bootstrap gate fed from real ownership and the `owned_shards` hook that drives
/// both scoping and the per-shard election before recovery.
async fn build_engine(assembly: EngineAssembly<'_>) -> Result<aion::Engine, ServerError> {
    let mut search_attribute_schema = aion_core::SearchAttributeSchema::new();
    search_attribute_schema
        .register(
            crate::namespace::NAMESPACE_ATTRIBUTE,
            aion_core::SearchAttributeType::String,
        )
        .map_err(|error| ServerError::Config {
            message: format!("failed to register namespace search attribute: {error}"),
        })?;
    let runtime = assembly.runtime;
    let builder = EngineBuilder::new()
        .store_arc(assembly.instrumented_store.clone())
        .event_streaming(assembly.event_broadcast_capacity)
        .in_memory_visibility()
        .search_attribute_schema(search_attribute_schema)
        .scheduler_threads(runtime.scheduler_threads)
        .outbox_enabled(runtime.outbox.enabled)
        .activity_dispatcher(assembly.activity_dispatcher)
        .active_registry(assembly.active_registry)
        .production_recovery_seam()
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .query_timeout(assembly.query_timeout)
        // SS-2: only the node owning the schedule-coordinator's shard seeds and
        // serves it. `true` for every non-distributed boot (owns all shards); a
        // distributed non-owner passes `false` so it does not fence the
        // coordinator stream (AA-4-4). Default `true`, so a single-node boot is
        // byte-identical to today.
        .bootstrap_schedule_coordinator(assembly.bootstrap_coordinator)
        .load_workflow_sources(runtime.workflow_packages.iter().map(PathBuf::as_path));
    // Owned-shard assignment: when the operator pins this node to a shard subset,
    // scope the engine to it AND (SS-2) elect those shards before recovery — the
    // builder's `owned_shards` hook drives both. Empty (the default) leaves the
    // builder untouched, so single-node boot owns ALL shards, elects nothing, and
    // is byte-identical to today.
    let builder = if runtime.owned_shards.is_empty() {
        builder
    } else {
        builder.owned_shards(runtime.owned_shards.iter().copied())
    };
    builder.build().await.map_err(ServerError::from)
}

/// A connected durable store plus the lifecycle pieces the boot path needs.
///
/// `outbox_store` is the SAME leaf store cast as an [`OutboxStore`] for backends
/// with a durable outbox table (libSQL, haematite); the in-memory backend yields
/// `None`. `bootstrap_coordinator` gates the schedule-coordinator seed on real
/// ownership (SS-2 / AA-4-4): `true` for every non-distributed boot (single-node
/// owns the coordinator's shard), and for a distributed node only when it owns
/// that shard. `cluster_responder` owns the distributed inbound-write responder
/// thread, kept alive for the server's lifetime; `None` for non-distributed boots.
struct ConnectedStore {
    event_store: Arc<dyn EventStore>,
    outbox_store: Option<Arc<dyn OutboxStore>>,
    bootstrap_coordinator: bool,
    #[cfg(feature = "haematite-backend")]
    cluster_responder: Option<aion_store_haematite::ClusterResponder>,
}

impl ConnectedStore {
    /// A non-distributed connected store: owns the coordinator's shard (so it
    /// bootstraps the coordinator) and has no cluster responder.
    fn local(event_store: Arc<dyn EventStore>, outbox_store: Option<Arc<dyn OutboxStore>>) -> Self {
        Self {
            event_store,
            outbox_store,
            bootstrap_coordinator: true,
            #[cfg(feature = "haematite-backend")]
            cluster_responder: None,
        }
    }
}

/// Connect the durable store, yielding the engine's [`EventStore`] handle and,
/// for the libSQL backend, the SAME leaf store cast as an [`OutboxStore`].
///
/// Both handles are clones of one `Arc<LibSqlStore>`, which holds a single
/// `libsql::Connection`. Sharing that connection with the outbox dispatcher
/// serializes the engine's `append_with_outbox` and the dispatcher's
/// `claim_outbox_rows` writes, so the two never contend across separate
/// connections and never raise `SQLITE_BUSY`. The in-memory backend has no
/// outbox table, so it yields `None`.
async fn connect_store(config: StoreConfig) -> Result<ConnectedStore, ServerError> {
    match config.backend {
        StoreBackend::Memory => Ok(ConnectedStore::local(
            Arc::new(aion_store::InMemoryStore::default()),
            None,
        )),
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
            let leaf = Arc::new(store);
            let event_store: Arc<dyn EventStore> = leaf.clone();
            let outbox_store: Arc<dyn OutboxStore> = leaf;
            Ok(ConnectedStore::local(event_store, Some(outbox_store)))
        }
        StoreBackend::Haematite => {
            #[cfg(feature = "haematite-backend")]
            {
                connect_haematite_store(config).await
            }
            #[cfg(not(feature = "haematite-backend"))]
            {
                let _ = config;
                connect_haematite_store_unavailable()
            }
        }
    }
}

/// Connect the haematite backend, opening the on-disk database if `store.data_dir`
/// already holds one and otherwise creating it with `store.shard_count` shards.
///
/// Without a `[store.cluster]` section this is the SINGLE-NODE path
/// ([`HaematiteStore::open`] / [`create_with_shard_count`]), byte-identical to
/// before: no endpoint, no election, owns everything, bootstraps the coordinator.
/// With a cluster section this is the DISTRIBUTED path
/// ([`HaematiteStore::open_or_create_distributed`]): it binds the replication
/// endpoint, builds the quorum membership, dials peers, starts the responder, and
/// computes whether THIS node owns the schedule-coordinator's shard so the engine
/// boot path seeds the coordinator on exactly one owner cluster-wide (SS-2).
///
/// The SAME leaf `Arc<HaematiteStore>` is shared as both the engine's
/// [`EventStore`] and the dispatcher's [`OutboxStore`] (one inner haematite
/// database), mirroring the libSQL backend.
///
/// [`HaematiteStore::open`]: aion_store_haematite::HaematiteStore::open
/// [`create_with_shard_count`]: aion_store_haematite::HaematiteStore::create_with_shard_count
/// [`HaematiteStore::open_or_create_distributed`]: aion_store_haematite::HaematiteStore::open_or_create_distributed
#[cfg(feature = "haematite-backend")]
async fn connect_haematite_store(config: StoreConfig) -> Result<ConnectedStore, ServerError> {
    let Some(data_dir) = config.data_dir else {
        return Err(ServerError::Config {
            message: "store.data_dir must not be empty when store.backend is haematite".to_owned(),
        });
    };
    let shard_count = config.shard_count;
    let owned_shards = config.owned_shards.clone();
    let cluster = config.cluster.clone();
    // Construction (and, for the distributed path, the off-runtime endpoint bind)
    // must not stall the async runtime, so run it on the blocking pool. The
    // distributed constructor itself steps onto a bare thread for the bind.
    let (store, responder) =
        tokio::task::spawn_blocking(move || build_haematite_store(&data_dir, shard_count, cluster))
            .await
            .map_err(|error| ServerError::Config {
                message: format!("haematite store initialization task failed: {error}"),
            })??;

    // Gate the coordinator bootstrap on real ownership: a distributed node that
    // does NOT own the coordinator's shard must not seed/fence it (AA-4-4). A
    // single-node boot owns all shards, so it always bootstraps.
    let bootstrap_coordinator = if owned_shards.is_empty() {
        true
    } else {
        store.set_owned_shards(owned_shards.iter().copied());
        store.owns_workflow_shard(&aion::schedule_coordinator_workflow_id())
    };

    let leaf = Arc::new(store);
    let event_store: Arc<dyn EventStore> = leaf.clone();
    let outbox_store: Arc<dyn OutboxStore> = leaf;
    Ok(ConnectedStore {
        event_store,
        outbox_store: Some(outbox_store),
        bootstrap_coordinator,
        cluster_responder: responder,
    })
}

/// Build the haematite store: the distributed path when a cluster section is
/// present, otherwise the single-node path. Returns the store and (for the
/// distributed path) its inbound-write responder. Restart-safe: an existing
/// on-disk database is reused (its shard count wins) rather than re-created.
#[cfg(feature = "haematite-backend")]
fn build_haematite_store(
    data_dir: &str,
    shard_count: usize,
    cluster: Option<crate::config::ClusterConfig>,
) -> Result<
    (
        aion_store_haematite::HaematiteStore,
        Option<aion_store_haematite::ClusterResponder>,
    ),
    ServerError,
> {
    use aion_store_haematite::{ClusterBootstrap, HaematiteStore};

    let Some(cluster) = cluster else {
        // Single-node path: byte-identical to before.
        let path = std::path::Path::new(data_dir);
        let store = if path.join("config.json").exists() {
            HaematiteStore::open(path).map_err(ServerError::from)?
        } else {
            HaematiteStore::create_with_shard_count(path, shard_count).map_err(ServerError::from)?
        };
        return Ok((store, None));
    };

    let boot = ClusterBootstrap {
        node_id: cluster.node_id,
        bind_address: cluster.bind_address,
        members: cluster.members,
        peers: cluster
            .peers
            .into_iter()
            .map(|peer| (peer.name, peer.address))
            .collect(),
        timeout: HAEMATITE_CLUSTER_OP_TIMEOUT,
    };
    let (store, responder) =
        HaematiteStore::open_or_create_distributed(data_dir, shard_count, boot)
            .map_err(ServerError::from)?;
    Ok((store, Some(responder)))
}

/// Per-operation quorum/election timeout for the distributed haematite backend.
#[cfg(feature = "haematite-backend")]
const HAEMATITE_CLUSTER_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Reject `backend = haematite` cleanly when the optional `haematite-backend`
/// feature is not compiled in, so a default build gives a precise operator
/// error instead of a silent fallthrough.
#[cfg(not(feature = "haematite-backend"))]
fn connect_haematite_store_unavailable() -> Result<ConnectedStore, ServerError> {
    Err(ServerError::Config {
        message: "store.backend = haematite requires the aion-server `haematite-backend` feature"
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use aion_store::InMemoryStore;

    use super::ServerState;
    use crate::config::{
        AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig,
        DevConfig, ListenConfig, MetricsConfig, NamespaceConfig, NamespaceMode, OutboxConfig,
        RuntimeConfig, WebSocketConfig, WorkerConfig,
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
            authoring: AuthoringConfig::default(),
            dev: DevConfig::default(),
            outbox: OutboxConfig::default(),
            scheduler_threads: 1,
            query_timeout: Some(Duration::from_millis(10_000)),
            default_namespace: "default".to_owned(),
            drain_timeout: Duration::from_secs(30),
            metrics: MetricsConfig { enabled: true },
            owned_shards: Vec::new(),
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

    #[cfg(feature = "haematite-backend")]
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_store_haematite_round_trips_through_event_store()
    -> Result<(), Box<dyn std::error::Error>> {
        use aion_core::{ContentType, EventEnvelope, PackageVersion, Payload, RunId, WorkflowId};
        use aion_store::WriteToken;
        use chrono::Utc;

        use crate::config::{StoreBackend, StoreConfig};

        let data_dir = tempfile::tempdir()?;
        // Single shard, a fresh temp data_dir: the production connect path opens
        // an existing haematite database or creates one, then shares the leaf as
        // both the engine EventStore and the dispatcher OutboxStore.
        let connected = super::connect_store(StoreConfig {
            backend: StoreBackend::Haematite,
            url: None,
            owned_shards: Vec::new(),
            data_dir: Some(data_dir.path().to_string_lossy().into_owned()),
            shard_count: 1,
            cluster: None,
        })
        .await?;
        let event_store = connected.event_store;
        assert!(
            connected.outbox_store.is_some(),
            "the haematite backend shares its leaf store as the dispatcher's outbox store"
        );
        assert!(
            connected.bootstrap_coordinator,
            "a single-node haematite boot owns all shards and bootstraps the coordinator"
        );
        assert!(
            connected.cluster_responder.is_none(),
            "a single-node (no [cluster]) haematite boot has no distributed responder"
        );

        let workflow_id = WorkflowId::new_v4();
        let event = aion_core::Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("checkout"),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            run_id: RunId::new_v4(),
            parent_run_id: None,
            package_version: PackageVersion::new("a".repeat(64)),
        };
        event_store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                0,
            )
            .await?;
        let history = event_store.read_history(&workflow_id).await?;
        assert_eq!(
            history.len(),
            1,
            "an event appended through the server's dyn EventStore reads back"
        );
        Ok(())
    }

    #[tokio::test]
    async fn connect_store_shares_outbox_store_only_for_libsql()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::config::{StoreBackend, StoreConfig};

        // Memory backend: no durable outbox table, so no outbox store handle —
        // and `outbox.enabled` over memory is rejected at dispatcher commission.
        let connected = super::connect_store(StoreConfig {
            backend: StoreBackend::Memory,
            url: None,
            owned_shards: Vec::new(),
            data_dir: None,
            shard_count: 1,
            cluster: None,
        })
        .await?;
        assert!(
            connected.outbox_store.is_none(),
            "the in-memory backend exposes no outbox store"
        );

        // LibSql backend: the leaf Arc<LibSqlStore> is shared as BOTH the engine's
        // EventStore and the dispatcher's OutboxStore (one libsql::Connection), so
        // the dispatcher reuses the engine's connection rather than opening a
        // second contending one (the inc-8 contention fix).
        let path = std::env::temp_dir().join(format!(
            "aion-connect-store-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let connected = super::connect_store(StoreConfig {
            backend: StoreBackend::LibSql,
            url: Some(path.to_string_lossy().into_owned()),
            owned_shards: Vec::new(),
            data_dir: None,
            shard_count: 1,
            cluster: None,
        })
        .await?;
        assert!(
            connected.outbox_store.is_some(),
            "the libSQL backend shares its leaf store as the dispatcher's outbox store"
        );
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
