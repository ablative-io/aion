//! Shared server state constructed once at startup.

use std::{path::PathBuf, sync::Arc};

use aion::{
    ActivityDispatcher, EngineBuilder, RuntimeHandle, SignalRouter, signal::ConcreteSignalRouter,
};
use aion_store::{EventStore, NamespaceStore, OutboxStore};
#[cfg(feature = "libsql-backend")]
use aion_store_libsql::LibSqlStore;

use crate::dev_ui::{ActivityMockRegistry, DevMockingDispatcher};

#[cfg(feature = "auth")]
use crate::auth::JwksCache;
use crate::{
    config::{RuntimeConfig, ServerConfig, StoreBackend, StoreConfig},
    error::ServerError,
    namespace::{NamespaceGuard, NamespaceMinter, resolver::NamespaceResolver},
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
    /// The durable namespace registry, captured from the SAME concrete leaf
    /// backend as the engine's `EventStore` BEFORE that leaf is wrapped in the
    /// decorator chain (`PublishingEventStore` → `InstrumentedEventStore`),
    /// which do not implement [`NamespaceStore`]. The haematite backend supplies
    /// the quorum-replicated implementation; the libSQL and in-memory backends
    /// supply a local-only one. Always present so the control-plane mint
    /// (Phase 1 S5) and `GET /namespaces` (S7) can reach a real store on every
    /// boot. Mirrors the `cluster_store` retention pattern.
    namespace_store: Arc<dyn NamespaceStore>,
    /// Advisory outbox wake (LSUB-2): the in-process `Notify` shared by the
    /// engine's stage seam (the `InstrumentedEventStore`'s `append_with_outbox`)
    /// and the [`OutboxDispatcher`](crate::worker::OutboxDispatcher) run loop, so
    /// a committed fan-out row wakes the dispatcher in ~RTT instead of waiting up
    /// to one poll interval. Always present (cheap, no `Option`): the handle is
    /// harmless when the outbox is not commissioned, since nothing pulses it.
    outbox_wake: Arc<tokio::sync::Notify>,
    /// WS3 cluster topology/ownership publisher. Always present: the ops console's
    /// cluster channel is served on every boot (calm state with no peers on a
    /// single-node server). Sized from `websocket.cluster_broadcast_capacity`.
    cluster_publisher: crate::cluster_publisher::ClusterEventPublisher,
    /// NOI-5b agent-observability transcript sequencer + live fan-out. Always
    /// present: the transcript channel is served on every boot. The backing
    /// [`ObservabilityStore`](aion_store::ObservabilityStore) is the durable
    /// `O`-keyspace impl on a haematite boot and an in-memory impl on every other
    /// backend (libSQL / in-memory have no `O` keyspace), so the transcript path
    /// is uniform across backends while only haematite persists across restart.
    /// Sized from `websocket.cluster_broadcast_capacity` (the same deployment-wide
    /// real-time channel capacity the cluster tail uses).
    transcript_publisher: crate::activity_publisher::ActivityEventPublisher,
    /// NOI-6 server-side intervention routing: the `attempt -> owning-worker`
    /// back-index the intervention router resolves a command's target through.
    /// Always present (cheap, no `Option`): the agent-dispatch path binds an owner
    /// when it dispatches an agent attempt and releases it on completion, so the
    /// router resolves the CURRENT owner. Empty until an agent attempt is
    /// dispatched — a command to an unbound attempt is the attempt-scoped no-op.
    attempt_owners: crate::worker::AttemptOwnerIndex,
    /// This node's distribution name for the WS3 cluster snapshot self-identity.
    /// `Some` on a distributed haematite boot (the configured `store.cluster.node_id`),
    /// `None` on a single-node boot — the snapshot then reports the standalone
    /// self-label so the ops console still has a node to render.
    cluster_self_node: Option<String>,
    /// Owns the distributed haematite inbound-write responder thread, kept alive
    /// for the server's lifetime so a cluster node keeps answering peers'
    /// replication/election traffic. `None` for non-distributed boots. Dropping
    /// the state stops the responder.
    #[cfg(feature = "haematite-backend")]
    cluster_responder: Option<aion_store_haematite::ClusterResponder>,
    /// The concrete distributed haematite store the SS-5b supervisor polls for
    /// peer liveness. `None` for every non-distributed boot.
    #[cfg(feature = "haematite-backend")]
    cluster_store: Option<Arc<aion_store_haematite::HaematiteStore>>,
    /// The peers the SS-5b supervisor watches (each with the shards this node
    /// adopts on its death). Empty for non-distributed boots.
    #[cfg(feature = "haematite-backend")]
    watched_peers: Vec<crate::cluster::WatchedPeer>,
    /// The request-routing shard directory (R-2), built over the cluster store +
    /// static peer config. `None` for every non-distributed boot, so the routing
    /// edge falls back to the bare R-1 ownership check (and the default path is a
    /// no-op).
    #[cfg(feature = "haematite-backend")]
    shard_directory: Option<Arc<crate::routing::StaticShardDirectory>>,
    /// The request forwarder (R-3): relays a non-local signal/query/cancel to the
    /// shard owner's gRPC address. `None` for non-distributed boots. The trait
    /// object makes the liminal forwarder a one-line swap when 13-L0/L1 land (R-6).
    #[cfg(feature = "haematite-backend")]
    request_forwarder: Option<Arc<dyn crate::routing::RequestForwarder>>,
    #[cfg(feature = "auth")]
    jwks_cache: Option<JwksCache>,
}

impl ServerState {
    /// Fallback cluster broadcast capacity for the `from_parts*` embedder/test
    /// constructors, which bypass config validation. The config-driven
    /// [`Self::build`] path always sizes the publisher from the validated
    /// `websocket.cluster_broadcast_capacity` instead.
    ///
    /// `NonZeroUsize::new(64)` is statically non-`None`, so the
    /// [`Option::unwrap`]-free `match` keeps the value `const` without tripping
    /// the workspace `unwrap_used`/`expect_used` deny lints.
    const FALLBACK_CLUSTER_BROADCAST_CAPACITY: std::num::NonZeroUsize =
        match std::num::NonZeroUsize::new(64) {
            Some(value) => value,
            None => std::num::NonZeroUsize::MIN,
        };

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
        S: EventStore + NamespaceStore,
    {
        // Capture the concrete leaf as BOTH the event store and the namespace
        // registry before it is wrapped in the (NamespaceStore-unaware) decorator
        // chain — the same leaf, two trait objects.
        let leaf = Arc::new(store);
        let namespace_store: Arc<dyn NamespaceStore> = leaf.clone();
        Self::build_with_connected_store(
            ConnectedStore::local(leaf, None, namespace_store),
            runtime,
        )
        .await
    }

    async fn build_with_connected_store(
        connected: ConnectedStore,
        runtime: RuntimeConfig,
    ) -> Result<Self, ServerError> {
        let outbox_store = connected.outbox_store;
        let bootstrap_coordinator = connected.bootstrap_coordinator;
        #[cfg(feature = "haematite-backend")]
        let cluster_responder = connected.cluster_responder;
        #[cfg(feature = "haematite-backend")]
        let cluster_store = connected.cluster_store;
        #[cfg(feature = "haematite-backend")]
        let watched_peers = connected.watched_peers;
        // Capture this node's self-identity for the WS3 cluster snapshot before
        // `self_node_id` is moved into the routing-state builder below.
        #[cfg(feature = "haematite-backend")]
        let cluster_self_node = connected.self_node_id.clone();
        #[cfg(not(feature = "haematite-backend"))]
        let cluster_self_node: Option<String> = None;
        // Build the R-2 directory + R-3 forwarder over the (live, failover-aware)
        // cluster store and static peer config. Both present only for a
        // distributed boot; `None` otherwise leaves the routing edge a no-op.
        #[cfg(feature = "haematite-backend")]
        let RoutingState {
            shard_directory,
            request_forwarder,
        } = build_routing_state(
            cluster_store.as_ref(),
            connected.directory_peers,
            connected.self_node_id,
        );
        let (event_broadcast_capacity, query_timeout) = required_engine_seams(&runtime)?;
        let (cluster_publisher, transcript_publisher) =
            build_real_time_publishers(&runtime, connected.observability_store)?;
        let metrics = Metrics::new().map_err(|error| metrics_config_error(&error))?;
        // LSUB-2 advisory wake: one process-wide `Notify` shared by the engine's
        // stage seam and the outbox dispatcher. A single handle is correct here
        // because there is exactly one in-process dispatcher that sweeps all owned
        // shards per tick — a wake just means "something was staged; sweep".
        let outbox_wake = Arc::new(tokio::sync::Notify::new());
        let instrumented_store = Arc::new(
            InstrumentedEventStore::new(
                connected.event_store,
                metrics.clone(),
                runtime.default_namespace.clone(),
            )
            .with_outbox_wake(Arc::clone(&outbox_wake)),
        );
        let exported_metrics = runtime.metrics.enabled.then_some(metrics.clone());
        // WS3 topology deltas + Control-Plane Phase 1 mint hook (`with_namespace_minting`).
        let worker_registry = ConnectedWorkerRegistry::default()
            .with_cluster_publisher(cluster_publisher.clone())
            .with_namespace_minting(connected.namespace_store.clone(), runtime.auto_create);
        let pending_activities = PendingActivities::default();
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        let drain_state = DrainState::default();
        let (dispatcher, attempt_owners) = build_bridge_dispatcher(
            &runtime,
            &worker_registry,
            &pending_activities,
            &heartbeat_tracker,
            &drain_state,
        );
        let (activity_dispatcher, activity_mock_registry) =
            decorate_activity_dispatcher(dispatcher, runtime.dev.enabled);

        let engine = build_engine(EngineAssembly {
            instrumented_store: &instrumented_store,
            event_broadcast_capacity,
            query_timeout,
            activity_dispatcher,
            active_registry: Arc::new(aion::Registry::default()),
            bootstrap_coordinator,
            runtime: &runtime,
        })
        .await?;
        let engine = Arc::new(engine);
        install_outbox_delivery(&pending_activities, &engine, runtime.outbox.enabled);
        let resolver = NamespaceResolver::from_config(runtime.namespace.clone(), engine);
        #[cfg(feature = "auth")]
        let jwks_cache = build_jwks_cache(&runtime).await?;
        Ok(Self {
            inner: Arc::new(ServerStateInner {
                namespace_guard: NamespaceGuard::new(resolver),
                runtime,
                worker_registry,
                pending_activities,
                heartbeat_tracker,
                drain_state,
                metrics: exported_metrics,
                health: Some(HealthState::new(instrumented_store, true)),
                activity_mock_registry,
                outbox_store,
                namespace_store: connected.namespace_store,
                outbox_wake,
                cluster_publisher,
                transcript_publisher,
                attempt_owners,
                cluster_self_node,
                #[cfg(feature = "haematite-backend")]
                cluster_responder,
                #[cfg(feature = "haematite-backend")]
                cluster_store,
                #[cfg(feature = "haematite-backend")]
                watched_peers,
                #[cfg(feature = "haematite-backend")]
                shard_directory,
                #[cfg(feature = "haematite-backend")]
                request_forwarder,
                #[cfg(feature = "auth")]
                jwks_cache,
            }),
        })
    }

    /// Build shared state from explicit parts with a default worker registry.
    #[must_use]
    pub fn from_parts(namespace_resolver: NamespaceResolver, runtime: RuntimeConfig) -> Self {
        // No durable store was supplied (this constructor builds state from a
        // resolver only), so the registry is a local-only in-memory store —
        // present so `namespace_store()` is always reachable, never mutating any
        // durable backend.
        Self::from_parts_with_namespace_store(
            namespace_resolver,
            runtime,
            Arc::new(aion_store::InMemoryStore::default()),
        )
    }

    /// Build shared state from explicit parts with a caller-supplied durable
    /// namespace registry.
    ///
    /// Identical to [`Self::from_parts`] except the namespace registry is the
    /// supplied store rather than a fresh in-memory one, so a caller can seed the
    /// durable set the control-plane read/create paths (`GET`/`POST
    /// /namespaces`) observe.
    #[must_use]
    pub fn from_parts_with_namespace_store(
        namespace_resolver: NamespaceResolver,
        runtime: RuntimeConfig,
        namespace_store: Arc<dyn NamespaceStore>,
    ) -> Self {
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        // Computed before `runtime` moves into the state: the retention bounds
        // flow from `[observability]` config on the embedder path too, so a
        // from-parts server enforces the same truncation/cap as a full boot.
        let bounds = transcript_bounds(&runtime);
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
                namespace_store,
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                cluster_publisher: crate::cluster_publisher::ClusterEventPublisher::new(
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                ),
                // NOI-5b: a from-parts / embedder state has no durable store, so
                // the transcript sequencer runs over an in-memory `O`-keyspace
                // impl — the transcript channel is served on every boot.
                transcript_publisher: build_transcript_publisher(
                    None,
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                    bounds,
                ),
                attempt_owners: crate::worker::AttemptOwnerIndex::new(),
                cluster_self_node: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
                #[cfg(feature = "haematite-backend")]
                cluster_store: None,
                #[cfg(feature = "haematite-backend")]
                watched_peers: Vec::new(),
                #[cfg(feature = "haematite-backend")]
                shard_directory: None,
                #[cfg(feature = "haematite-backend")]
                request_forwarder: None,
                #[cfg(feature = "auth")]
                jwks_cache: None,
            }),
        }
    }

    /// Build shared state from explicit parts with BOTH a caller-supplied
    /// durable namespace registry AND a caller-supplied JWKS cache.
    ///
    /// The combined seam of [`Self::from_parts_with_namespace_store`] (seed the
    /// durable registry the control-plane read/create paths observe) and
    /// [`Self::from_parts_with_jwks`] (validate bearer tokens against an injected
    /// issuer): an enumerated caller can exercise the real JWT authorization path
    /// against a seeded registry without a full [`Self::build`] boot.
    #[cfg(feature = "auth")]
    #[must_use]
    pub fn from_parts_with_namespace_store_and_jwks(
        namespace_resolver: NamespaceResolver,
        runtime: RuntimeConfig,
        namespace_store: Arc<dyn NamespaceStore>,
        jwks_cache: JwksCache,
    ) -> Self {
        let heartbeat_tracker = HeartbeatTracker::new(runtime.worker.heartbeat_window);
        // Computed before `runtime` moves into the state: the retention bounds
        // flow from `[observability]` config on the embedder path too, so a
        // from-parts server enforces the same truncation/cap as a full boot.
        let bounds = transcript_bounds(&runtime);
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
                namespace_store,
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                cluster_publisher: crate::cluster_publisher::ClusterEventPublisher::new(
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                ),
                // NOI-5b: a from-parts / embedder state has no durable store, so
                // the transcript sequencer runs over an in-memory `O`-keyspace
                // impl — the transcript channel is served on every boot.
                transcript_publisher: build_transcript_publisher(
                    None,
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                    bounds,
                ),
                attempt_owners: crate::worker::AttemptOwnerIndex::new(),
                cluster_self_node: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
                #[cfg(feature = "haematite-backend")]
                cluster_store: None,
                #[cfg(feature = "haematite-backend")]
                watched_peers: Vec::new(),
                #[cfg(feature = "haematite-backend")]
                shard_directory: None,
                #[cfg(feature = "haematite-backend")]
                request_forwarder: None,
                jwks_cache: Some(jwks_cache),
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
        // Computed before `runtime` moves into the state: the retention bounds
        // flow from `[observability]` config on the embedder path too, so a
        // from-parts server enforces the same truncation/cap as a full boot.
        let bounds = transcript_bounds(&runtime);
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
                // No durable store was supplied (these constructors build state
                // from a resolver only), so the registry is a local-only
                // in-memory store — present so `namespace_store()` is always
                // reachable, never mutating any durable backend.
                namespace_store: Arc::new(aion_store::InMemoryStore::default()),
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                cluster_publisher: crate::cluster_publisher::ClusterEventPublisher::new(
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                ),
                // NOI-5b: a from-parts / embedder state has no durable store, so
                // the transcript sequencer runs over an in-memory `O`-keyspace
                // impl — the transcript channel is served on every boot.
                transcript_publisher: build_transcript_publisher(
                    None,
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                    bounds,
                ),
                attempt_owners: crate::worker::AttemptOwnerIndex::new(),
                cluster_self_node: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
                #[cfg(feature = "haematite-backend")]
                cluster_store: None,
                #[cfg(feature = "haematite-backend")]
                watched_peers: Vec::new(),
                #[cfg(feature = "haematite-backend")]
                shard_directory: None,
                #[cfg(feature = "haematite-backend")]
                request_forwarder: None,
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
        // Computed before `runtime` moves into the state: the retention bounds
        // flow from `[observability]` config on the embedder path too, so a
        // from-parts server enforces the same truncation/cap as a full boot.
        let bounds = transcript_bounds(&runtime);
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
                // No durable store was supplied (these constructors build state
                // from a resolver only), so the registry is a local-only
                // in-memory store — present so `namespace_store()` is always
                // reachable, never mutating any durable backend.
                namespace_store: Arc::new(aion_store::InMemoryStore::default()),
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                cluster_publisher: crate::cluster_publisher::ClusterEventPublisher::new(
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                ),
                // NOI-5b: a from-parts / embedder state has no durable store, so
                // the transcript sequencer runs over an in-memory `O`-keyspace
                // impl — the transcript channel is served on every boot.
                transcript_publisher: build_transcript_publisher(
                    None,
                    Self::FALLBACK_CLUSTER_BROADCAST_CAPACITY,
                    bounds,
                ),
                attempt_owners: crate::worker::AttemptOwnerIndex::new(),
                cluster_self_node: None,
                #[cfg(feature = "haematite-backend")]
                cluster_responder: None,
                #[cfg(feature = "haematite-backend")]
                cluster_store: None,
                #[cfg(feature = "haematite-backend")]
                watched_peers: Vec::new(),
                #[cfg(feature = "haematite-backend")]
                shard_directory: None,
                #[cfg(feature = "haematite-backend")]
                request_forwarder: None,
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

    /// Borrow the WS3 cluster-event publisher shared by the cluster state-change
    /// sites (supervisor, worker registry) and the cluster subscription endpoint.
    /// Always present, on every boot.
    #[must_use]
    pub fn cluster_publisher(&self) -> &crate::cluster_publisher::ClusterEventPublisher {
        &self.inner.cluster_publisher
    }

    /// Borrow the NOI-5b transcript sequencer shared by the worker->server
    /// ingestion seam (which publishes a running activity's `ActivityEvent`s) and
    /// the transcript subscription endpoint (which tails + resumes them). Always
    /// present, on every boot.
    #[must_use]
    pub fn transcript_publisher(&self) -> &crate::activity_publisher::ActivityEventPublisher {
        &self.inner.transcript_publisher
    }

    /// Borrow the NOI-6 `attempt -> owning-worker` back-index. The agent-dispatch
    /// path binds an owner when it dispatches an agent attempt and releases it on
    /// completion, so the intervention router always resolves the CURRENT owner.
    #[must_use]
    pub fn attempt_owners(&self) -> &crate::worker::AttemptOwnerIndex {
        &self.inner.attempt_owners
    }

    /// Build the NOI-6 intervention router over the connected-worker registry, the
    /// attempt-owner back-index, and the active intervention transport.
    ///
    /// The transport is the liminal server-push
    /// ([`LiminalInterventionTransport`](crate::worker::LiminalInterventionTransport))
    /// when the `liminal-transport` feature is compiled in — the production path
    /// that pushes a routed command out on the owning worker's connection — and a
    /// null transport otherwise, which reports the target unreachable so every
    /// command NACKs the attempt-scoped no-op rather than silently vanishing. The
    /// router is cheap to build (it clones cloneable handles), so it is constructed
    /// per request at the endpoint rather than stored.
    #[must_use]
    pub fn intervention_router(&self) -> crate::worker::InterventionRouter {
        let transport: std::sync::Arc<dyn crate::worker::InterventionTransport> = {
            #[cfg(feature = "liminal-transport")]
            {
                std::sync::Arc::new(crate::worker::LiminalInterventionTransport)
            }
            #[cfg(not(feature = "liminal-transport"))]
            {
                std::sync::Arc::new(NullInterventionTransport)
            }
        };
        crate::worker::InterventionRouter::new(
            self.inner.worker_registry.clone(),
            self.inner.attempt_owners.clone(),
            transport,
        )
        // Lane #229: an APPLIED InjectMessage is teed into the durable
        // transcript, so the retained record holds the operator's words.
        .with_transcript_publisher(self.inner.transcript_publisher.clone())
    }

    /// This node's configured cluster distribution name for the WS3 snapshot
    /// self-identity, or `None` on a single-node boot (the snapshot then reports
    /// the standalone self-label).
    #[must_use]
    pub fn cluster_self_node(&self) -> Option<&str> {
        self.inner.cluster_self_node.as_deref()
    }

    /// Clone the live engine handle the completion path records terminals through.
    ///
    /// This is the SAME `Arc<Engine>` the gRPC completion callback is built over
    /// (state.rs installs `ServerOutboxDeliveryCallback::new(engine)` on the
    /// pending tracker when `outbox.enabled`), so the liminal completion path
    /// re-enters worker results through the identical `record_fan_out_completion`
    /// seam rather than inventing a second one.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the namespace resolver has no engine handle
    /// (a state built from parts without an engine).
    pub fn engine(&self) -> Result<Arc<aion::Engine>, ServerError> {
        self.inner
            .namespace_guard
            .resolver()
            .engine()
            .map(Arc::clone)
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

    /// Borrow the durable namespace registry shared by the control plane.
    ///
    /// This is the SAME concrete leaf backend the engine writes events through
    /// (haematite quorum-replicated, or libSQL / in-memory local-only),
    /// captured as a [`NamespaceStore`] before the decorator chain wrapped it.
    /// Always present on every boot, so the mint-on-register path (Phase 1 S5)
    /// and `GET /namespaces` (S7) can reach a real registry regardless of
    /// backend.
    #[must_use]
    pub fn namespace_store(&self) -> &Arc<dyn NamespaceStore> {
        &self.inner.namespace_store
    }

    /// Build the shared minted-on-use hook over the durable namespace store and
    /// the configured [`AutoCreate`](crate::config::AutoCreate) policy.
    ///
    /// This is the SAME policy logic the worker-registration seam applies (S5);
    /// the workflow-start safety net (S6) calls it after authorization so a
    /// client that starts a workflow before any worker registers still gets a
    /// durable namespace record. Cheap to build (clones an `Arc` + a `Copy`
    /// policy), so transports construct it per request rather than holding it.
    #[must_use]
    pub fn namespace_minter(&self) -> NamespaceMinter {
        NamespaceMinter::new(
            Arc::clone(&self.inner.namespace_store),
            self.inner.runtime.auto_create,
        )
        // Thread the deployment-global cluster channel so the start-time safety
        // net (S6) and the explicit `POST /namespaces` path (S7) emit the same
        // live "namespace created" delta the worker-mint seam (S5) does — all
        // three mint choke-points surface on the one ops-console push channel.
        .with_cluster_publisher(self.inner.cluster_publisher.clone())
    }

    /// Clone the advisory outbox wake (LSUB-2) shared with the engine's stage
    /// seam. The outbox dispatcher installs this handle so a committed fan-out row
    /// wakes its run loop in ~RTT rather than waiting for the next poll tick. The
    /// handle is always present; it is simply never pulsed when the outbox is not
    /// commissioned, so wiring it is free and behaviour is unchanged.
    #[must_use]
    pub fn outbox_wake(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.inner.outbox_wake)
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

    /// The concrete distributed haematite store the request-routing edge consults
    /// for shard ownership (`shard_for_workflow` / `owns_workflow_shard`) and
    /// unsteered-start remint. `None` for every single-node / non-clustered boot,
    /// so the routing pre-step is a no-op and the default path is unchanged.
    #[cfg(feature = "haematite-backend")]
    #[must_use]
    pub fn cluster_store(&self) -> Option<&Arc<aion_store_haematite::HaematiteStore>> {
        self.inner.cluster_store.as_ref()
    }

    /// The request-routing shard directory (R-2) the edge consults to resolve a
    /// non-owned shard's owner. `None` for single-node / non-clustered boots, so
    /// the edge falls back to the bare R-1 ownership check.
    #[cfg(feature = "haematite-backend")]
    #[must_use]
    pub fn shard_directory(&self) -> Option<&Arc<crate::routing::StaticShardDirectory>> {
        self.inner.shard_directory.as_ref()
    }

    /// The R-3 request forwarder used to relay a non-local signal/query/cancel to
    /// the shard owner. `None` for single-node / non-clustered boots.
    #[cfg(feature = "haematite-backend")]
    #[must_use]
    pub fn request_forwarder(&self) -> Option<&Arc<dyn crate::routing::RequestForwarder>> {
        self.inner.request_forwarder.as_ref()
    }

    /// Spawn the worker heartbeat expiry sweeper (#176): the production driver
    /// of [`HeartbeatTracker::fail_expired_workers`], failing every worker with
    /// an in-flight task beyond the operator's `worker.heartbeat_window` and
    /// deregistering it with the provable
    /// [`WorkerDeathReason::Timeout`](aion_core::WorkerDeathReason::Timeout).
    ///
    /// Always spawned on the server boot path — dead-worker detection is a
    /// liveness correctness property, not an opt-in feature. The cadence is
    /// derived from the heartbeat window
    /// ([`sweep_interval`](crate::worker::sweep_interval): a quarter of the
    /// window clamped to `[1s, window]`, so the default 30s window sweeps every
    /// 7.5s); there is deliberately no separate config knob. The task exits
    /// when `shutdown` flips to `true`, exactly like the transports; the
    /// returned handle may be dropped to detach it (dropping a tokio
    /// `JoinHandle` never cancels the task) and is returned so tests can await
    /// clean shutdown.
    #[must_use]
    pub fn spawn_heartbeat_sweeper(
        &self,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let sweeper = crate::worker::HeartbeatSweeper::new(
            self.inner.heartbeat_tracker.clone(),
            self.inner.worker_registry.clone(),
            self.inner.pending_activities.clone(),
            self.inner.drain_state.clone(),
            self.inner.runtime.worker.heartbeat_window,
        );
        tokio::spawn(sweeper.run(shutdown))
    }

    /// Spawn the SS-5b cluster supervisor: a background task that watches every
    /// declared peer's replication liveness and, on a confirmed peer death,
    /// calls `adopt_shards` for that peer's shards on THIS node's live engine —
    /// automatic failover with no manual trigger.
    ///
    /// Does nothing (returns `Ok(())` without spawning) unless this is a
    /// distributed boot whose cluster config declared at least one peer with
    /// `owned_shards`. A single-node / non-clustered server therefore never runs
    /// a supervisor, so default behaviour is unchanged.
    ///
    /// The spawned task drains on `shutdown` exactly like the transports.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the engine handle cannot be resolved.
    #[cfg(feature = "haematite-backend")]
    pub fn spawn_cluster_supervisor(
        &self,
        config: crate::cluster::SupervisorConfig,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<bool, ServerError> {
        let Some(cluster_store) = self.inner.cluster_store.clone() else {
            return Ok(false);
        };
        if self.inner.watched_peers.is_empty() {
            return Ok(false);
        }
        let engine = Arc::clone(self.inner.namespace_guard.resolver().engine()?);
        // WS3: feed cluster topology deltas from the supervisor's existing
        // decision points into the ops console channel. `self_node` is the
        // configured distribution name (already captured for the snapshot).
        let publisher = Arc::new(self.inner.cluster_publisher.clone());
        let self_node = self.inner.cluster_self_node.clone().unwrap_or_default();
        // #253: adoption re-runs the terminal-workflow outbox settlement sweep
        // over the widened owned-shard scope, so a dead peer's stranded row for
        // a terminal workflow is settled — never re-armed — by its adopter.
        // With no outbox commissioned there is nothing to settle and the
        // adopter delegates straight to the engine.
        let adopter = Arc::new(crate::cluster::OutboxSettlingAdopter::new(
            engine,
            self.inner.outbox_store.clone(),
        ));
        let supervisor = crate::cluster::ClusterSupervisor::new(
            cluster_store,
            adopter,
            self.inner.watched_peers.clone(),
            config,
        )
        .with_publisher(publisher, self_node);
        if !supervisor.watches_any() {
            return Ok(false);
        }
        tokio::spawn(supervisor.run(shutdown));
        Ok(true)
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
    search_attribute_schema
        .register(
            crate::namespace::TASK_QUEUE_ATTRIBUTE,
            aion_core::SearchAttributeType::String,
        )
        .map_err(|error| ServerError::Config {
            message: format!("failed to register task_queue search attribute: {error}"),
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

/// Validate the two engine seams the server unconditionally mounts: the event
/// broadcast channel capacity (`/events/stream`) and the query reply deadline
/// (`/workflows/query`). Both are explicit-no-default — a mounted-but-
/// unconfigured surface is never acceptable.
fn required_engine_seams(
    runtime: &RuntimeConfig,
) -> Result<(std::num::NonZeroUsize, std::time::Duration), ServerError> {
    let event_broadcast_capacity = runtime
        .websocket
        .event_broadcast_capacity
        .and_then(std::num::NonZeroUsize::new)
        .ok_or_else(|| ServerError::Config {
            message: crate::config::EVENT_BROADCAST_CAPACITY_REQUIRED.to_owned(),
        })?;
    let query_timeout = runtime
        .query_timeout
        .filter(|timeout| !timeout.is_zero())
        .ok_or_else(|| ServerError::Config {
            message: crate::config::QUERY_TIMEOUT_REQUIRED.to_owned(),
        })?;
    Ok((event_broadcast_capacity, query_timeout))
}

/// Install the outbox delivery callback when the durable outbox is commissioned.
///
/// Routes unmatched worker completions arriving at the sink into the live
/// workflow's mailbox. Flag-off, no callback is installed and the sink's
/// unmatched branch stays a silent drop. The dispatcher is not rebuilt — it
/// shares this exact pending tracker.
fn install_outbox_delivery(
    pending_activities: &PendingActivities,
    engine: &Arc<aion::Engine>,
    outbox_enabled: bool,
) {
    if outbox_enabled {
        let callback = Arc::new(crate::worker::ServerOutboxDeliveryCallback::new(
            Arc::clone(engine),
        ));
        pending_activities.set_outbox_delivery(callback);
    }
}

/// Decorate the worker activity dispatcher with the per-run activity-mock layer
/// when the dev surface is commissioned, returning the dispatcher and the shared
/// mock registry (if any).
///
/// Dark by default: with the dev surface off the engine gets the bare production
/// dispatcher and there is no mocking path at all (CN4).
/// Compose the engine-seam bridge dispatcher over the state's shared parts.
///
/// Also mints the NOI-6 attempt→owner index and returns it alongside: the
/// bridge binds each liminal-delivered attempt into it for the dispatch's
/// lifetime, and the state stores the SAME instance for the intervention
/// router to read, so the ops console can enumerate and target live attempts.
fn build_bridge_dispatcher(
    runtime: &RuntimeConfig,
    worker_registry: &ConnectedWorkerRegistry,
    pending_activities: &PendingActivities,
    heartbeat_tracker: &HeartbeatTracker,
    drain_state: &DrainState,
) -> (WorkerActivityDispatcher, crate::worker::AttemptOwnerIndex) {
    let attempt_owners = crate::worker::AttemptOwnerIndex::new();
    let dispatcher = WorkerActivityDispatcher::new(
        worker_registry.clone(),
        runtime.default_namespace.clone(),
        heartbeat_tracker.clone(),
    )
    .with_pending(pending_activities.clone())
    .with_drain_state(drain_state.clone())
    .with_tokio_handle(tokio::runtime::Handle::current())
    .with_attempt_owners(attempt_owners.clone());
    (dispatcher, attempt_owners)
}

fn decorate_activity_dispatcher(
    dispatcher: WorkerActivityDispatcher,
    dev_enabled: bool,
) -> (Arc<dyn ActivityDispatcher>, Option<ActivityMockRegistry>) {
    if dev_enabled {
        let registry = ActivityMockRegistry::new();
        let decorated = DevMockingDispatcher::new(Arc::new(dispatcher), registry.clone());
        (Arc::new(decorated), Some(registry))
    } else {
        (Arc::new(dispatcher), None)
    }
}

/// Validate the WS3 cluster broadcast capacity the server unconditionally mounts
/// (the `cluster` subscription on `/events/stream`). Explicit-no-default with the
/// same non-zero startup guard as the workflow event channel: the lag contract
/// has no buffer to lag against unless sized.
fn required_cluster_broadcast_capacity(
    runtime: &RuntimeConfig,
) -> Result<std::num::NonZeroUsize, ServerError> {
    runtime
        .websocket
        .cluster_broadcast_capacity
        .and_then(std::num::NonZeroUsize::new)
        .ok_or_else(|| ServerError::Config {
            message: crate::config::CLUSTER_BROADCAST_CAPACITY_REQUIRED.to_owned(),
        })
}

/// Build the deployment-wide real-time publishers the server mounts on every
/// boot — the WS3 cluster topology channel and the NOI-5b agent-observability
/// transcript channel — from the validated `websocket.cluster_broadcast_capacity`.
///
/// The transcript sequencer runs over `observability_store` (the durable
/// `O`-keyspace impl on a haematite boot) or an in-memory impl when the backend
/// has none — see [`build_transcript_publisher`].
///
/// # Errors
///
/// Returns [`ServerError`] when `websocket.cluster_broadcast_capacity` is unset
/// or zero (the same explicit-no-default guard the cluster channel already had).
fn build_real_time_publishers(
    runtime: &RuntimeConfig,
    observability_store: Option<Arc<dyn aion_store::ObservabilityStore>>,
) -> Result<
    (
        crate::cluster_publisher::ClusterEventPublisher,
        crate::activity_publisher::ActivityEventPublisher,
    ),
    ServerError,
> {
    let capacity = required_cluster_broadcast_capacity(runtime)?;
    Ok((
        crate::cluster_publisher::ClusterEventPublisher::new(capacity),
        build_transcript_publisher(observability_store, capacity, transcript_bounds(runtime)),
    ))
}

/// The operator-configured transcript retention bounds from `[observability]`.
fn transcript_bounds(runtime: &RuntimeConfig) -> crate::activity_bounds::TranscriptBounds {
    crate::activity_bounds::TranscriptBounds {
        max_event_bytes: runtime.observability.max_event_bytes,
        max_stream_events: runtime.observability.max_stream_events,
    }
}

/// Build the NOI-5b transcript sequencer over `observability_store` (the durable
/// `O`-keyspace impl when the backend has one, an in-memory impl otherwise) with
/// a live-tail buffer of `capacity` and the `[observability]` retention bounds.
///
/// The publisher is ALWAYS constructed (the transcript channel is served on every
/// boot); only the durability of the backing store varies by backend. A backend
/// with no `O` keyspace (libSQL / in-memory) gets the in-memory
/// [`InMemoryObservabilityStore`](aion_store::InMemoryObservabilityStore), so the
/// live-tail + resume path behaves identically and only cross-restart durability
/// differs — exactly the "keep the no-observability path uniform" contract.
fn build_transcript_publisher(
    observability_store: Option<Arc<dyn aion_store::ObservabilityStore>>,
    capacity: std::num::NonZeroUsize,
    bounds: crate::activity_bounds::TranscriptBounds,
) -> crate::activity_publisher::ActivityEventPublisher {
    let store = observability_store
        .unwrap_or_else(|| Arc::new(aion_store::InMemoryObservabilityStore::default()));
    crate::activity_publisher::ActivityEventPublisher::new(store, capacity).with_bounds(bounds)
}

/// The request-routing pieces built from the cluster store + peer config.
#[cfg(feature = "haematite-backend")]
struct RoutingState {
    shard_directory: Option<Arc<crate::routing::StaticShardDirectory>>,
    request_forwarder: Option<Arc<dyn crate::routing::RequestForwarder>>,
}

/// Build the R-2 shard directory and R-3 request forwarder over the cluster
/// store and static peer config, or all-`None` when this is not a distributed
/// boot (no cluster store) so the routing edge is a no-op (default path).
#[cfg(feature = "haematite-backend")]
fn build_routing_state(
    cluster_store: Option<&Arc<aion_store_haematite::HaematiteStore>>,
    directory_peers: Vec<crate::routing::DirectoryPeer>,
    self_node_id: Option<String>,
) -> RoutingState {
    let Some(store) = cluster_store else {
        return RoutingState {
            shard_directory: None,
            request_forwarder: None,
        };
    };
    RoutingState {
        shard_directory: Some(Arc::new(crate::routing::StaticShardDirectory::new(
            Arc::clone(store),
            directory_peers,
            self_node_id,
        ))),
        request_forwarder: Some(Arc::new(crate::routing::GrpcRequestForwarder::new())),
    }
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
    /// The SAME concrete leaf store as `event_store`, captured as a
    /// [`NamespaceStore`] before the decorator chain wraps it (the decorators
    /// are `NamespaceStore`-unaware). The control plane mints and lists through
    /// this handle. Every backend populates it: haematite supplies the
    /// quorum-replicated implementation, libSQL and in-memory the local-only
    /// one.
    namespace_store: Arc<dyn NamespaceStore>,
    /// NOI-5b: the SAME concrete leaf store captured as an
    /// [`ObservabilityStore`](aion_store::ObservabilityStore) when the backend
    /// implements the durable `O` keyspace (haematite). `None` for backends with
    /// no `O` keyspace (libSQL / in-memory), where the transcript sequencer runs
    /// over an in-memory impl instead. Captured before the leaf is wrapped in the
    /// (`ObservabilityStore`-unaware) decorator chain, exactly like
    /// `namespace_store`.
    observability_store: Option<Arc<dyn aion_store::ObservabilityStore>>,
    bootstrap_coordinator: bool,
    #[cfg(feature = "haematite-backend")]
    cluster_responder: Option<aion_store_haematite::ClusterResponder>,
    /// The concrete distributed haematite store (the SAME leaf as `event_store`),
    /// retained for the SS-5b cluster supervisor's peer-liveness polling. `None`
    /// for every non-distributed boot.
    #[cfg(feature = "haematite-backend")]
    cluster_store: Option<Arc<aion_store_haematite::HaematiteStore>>,
    /// The peers the SS-5b supervisor watches, each with the shards this node
    /// adopts on its death. Empty for non-distributed boots.
    #[cfg(feature = "haematite-backend")]
    watched_peers: Vec<crate::cluster::WatchedPeer>,
    /// The static shard-directory peer entries (name + declared shards + gRPC
    /// forward address) used to build the request-routing directory (R-2). Empty
    /// for non-distributed boots.
    #[cfg(feature = "haematite-backend")]
    directory_peers: Vec<crate::routing::DirectoryPeer>,
    /// This node's own distribution name (cluster `node_id`), so the SS-3
    /// directory can resolve a shard-owner record naming THIS node to `Local`.
    /// `None` for non-distributed boots.
    #[cfg(feature = "haematite-backend")]
    self_node_id: Option<String>,
}

impl ConnectedStore {
    /// A non-distributed connected store: owns the coordinator's shard (so it
    /// bootstraps the coordinator) and has no cluster responder.
    ///
    /// `namespace_store` is the SAME concrete leaf as `event_store`, captured as
    /// a [`NamespaceStore`] by the caller (where the concrete type is still
    /// known) before the decorator chain wraps the event store.
    fn local(
        event_store: Arc<dyn EventStore>,
        outbox_store: Option<Arc<dyn OutboxStore>>,
        namespace_store: Arc<dyn NamespaceStore>,
    ) -> Self {
        Self {
            event_store,
            outbox_store,
            namespace_store,
            // A `local` connected store is the memory / libSQL / embedder path,
            // none of which implement the durable `O` keyspace: the transcript
            // sequencer falls back to an in-memory impl (NOI-5b).
            observability_store: None,
            bootstrap_coordinator: true,
            #[cfg(feature = "haematite-backend")]
            cluster_responder: None,
            #[cfg(feature = "haematite-backend")]
            cluster_store: None,
            #[cfg(feature = "haematite-backend")]
            watched_peers: Vec::new(),
            #[cfg(feature = "haematite-backend")]
            directory_peers: Vec::new(),
            #[cfg(feature = "haematite-backend")]
            self_node_id: None,
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
        StoreBackend::Memory => {
            // One leaf store, captured as both the engine's event store and the
            // namespace registry (in-memory backends have no outbox table).
            let leaf = Arc::new(aion_store::InMemoryStore::default());
            let namespace_store: Arc<dyn NamespaceStore> = leaf.clone();
            Ok(ConnectedStore::local(leaf, None, namespace_store))
        }
        StoreBackend::LibSql => {
            #[cfg(feature = "libsql-backend")]
            {
                connect_libsql_store(config).await
            }
            #[cfg(not(feature = "libsql-backend"))]
            {
                let _ = config;
                connect_libsql_store_unavailable()
            }
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

/// Connect the libSQL backend, opening the embedded database at `store.url` and
/// sharing the SAME leaf `Arc<LibSqlStore>` (one `libsql::Connection`) as both the
/// engine's [`EventStore`] and the dispatcher's [`OutboxStore`].
#[cfg(feature = "libsql-backend")]
async fn connect_libsql_store(config: StoreConfig) -> Result<ConnectedStore, ServerError> {
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
    let namespace_store: Arc<dyn NamespaceStore> = leaf.clone();
    let outbox_store: Arc<dyn OutboxStore> = leaf;
    Ok(ConnectedStore::local(
        event_store,
        Some(outbox_store),
        namespace_store,
    ))
}

/// Reject `backend = libsql` cleanly when the optional `libsql-backend` feature
/// is not compiled in, so a default (ablative-stack) build gives a precise
/// operator error instead of a silent fallthrough.
#[cfg(not(feature = "libsql-backend"))]
fn connect_libsql_store_unavailable() -> Result<ConnectedStore, ServerError> {
    Err(ServerError::Config {
        message: "store.backend = libsql requires the aion-server `libsql-backend` feature"
            .to_owned(),
    })
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
    // The peers the SS-5b supervisor watches, captured before `cluster` is moved
    // into the blocking build. A peer with declared `owned_shards` becomes a
    // watch target; peers without are kept out of the watch set (the supervisor
    // would have nothing to adopt for them).
    let watched_peers: Vec<crate::cluster::WatchedPeer> = cluster
        .as_ref()
        .map(|cluster| {
            cluster
                .peers
                .iter()
                .map(|peer| crate::cluster::WatchedPeer {
                    name: peer.name.clone(),
                    owned_shards: peer.owned_shards.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    // The static shard-directory entries (R-2): each peer's declared shards plus
    // its gRPC forward address. Built from the same config the supervisor uses.
    let directory_peers: Vec<crate::routing::DirectoryPeer> = cluster
        .as_ref()
        .map(|cluster| {
            cluster
                .peers
                .iter()
                .map(|peer| crate::routing::DirectoryPeer {
                    name: peer.name.clone(),
                    owned_shards: peer.owned_shards.clone(),
                    grpc_addr: peer.grpc_address,
                })
                .collect()
        })
        .unwrap_or_default();
    // This node's own distribution name, so the SS-3 directory resolves a
    // shard-owner record naming THIS node to `Local`.
    let self_node_id: Option<String> = cluster.as_ref().map(|cluster| cluster.node_id.clone());
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
    let outbox_store: Arc<dyn OutboxStore> = leaf.clone();
    // The namespace registry is the SAME concrete `HaematiteStore` leaf (the
    // quorum-replicated implementation), captured before the leaf is moved into
    // the cluster-store retention below.
    let namespace_store: Arc<dyn NamespaceStore> = leaf.clone();
    // NOI-5b: the SAME concrete leaf captured as the durable `O`-keyspace
    // observability store, so the transcript sequencer persists to haematite and
    // survives restart/failover. Captured here (before the decorator chain wraps
    // the event store) exactly like the namespace registry.
    let observability_store: Arc<dyn aion_store::ObservabilityStore> = leaf.clone();
    // Retain the concrete store ONLY for a distributed boot (responder present),
    // where the SS-5b supervisor will poll it for peer liveness. A single-node
    // boot has no peers, so it carries no cluster store and never supervises.
    let cluster_store = responder.as_ref().map(|_| leaf);
    let (watched_peers, directory_peers, self_node_id) = if cluster_store.is_some() {
        (watched_peers, directory_peers, self_node_id)
    } else {
        (Vec::new(), Vec::new(), None)
    };
    Ok(ConnectedStore {
        event_store,
        outbox_store: Some(outbox_store),
        namespace_store,
        observability_store: Some(observability_store),
        bootstrap_coordinator,
        cluster_responder: responder,
        cluster_store,
        watched_peers,
        directory_peers,
        self_node_id,
    })
}

/// Build the haematite store: the distributed path when a cluster section is
/// present, otherwise the single-node path. Returns the store and (for the
/// distributed path) its inbound-write responder. Restart-safe: an existing
/// on-disk database is reused (its shard count wins) rather than re-created.
///
/// Linux/Android give Haematite a descriptor-authoritative `/proc/self/fd` path.
/// On path-ambient Unix targets such as macOS, startup instead resolves the held
/// descriptor's current path and refuses any ancestor owned by an unprivileged
/// principal other than the server euid or writable by group/world. That policy
/// prevents a second principal from renaming a parent after startup and replacing
/// the old name with a symlink that redirects Haematite's normal reads/commits.
/// Every shard is still eagerly materialized and the capability retained, but on
/// those targets neither action confines later pathname I/O. A descriptor-relative
/// Haematite constructor and backend I/O remain the long-term fix.
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
    build_haematite_store_with_hook(data_dir, shard_count, cluster, || Ok(()))
}

#[cfg(feature = "haematite-backend")]
fn build_haematite_store_with_hook(
    data_dir: &str,
    shard_count: usize,
    cluster: Option<crate::config::ClusterConfig>,
    before_backend_touch: impl FnOnce() -> Result<(), std::io::Error>,
) -> Result<
    (
        aion_store_haematite::HaematiteStore,
        Option<aion_store_haematite::ClusterResponder>,
    ),
    ServerError,
> {
    use aion_store_haematite::{ClusterBootstrap, HaematiteStore};

    // Acquire the data root through the same no-follow component walk used by
    // authoring. New components are 0700 on Unix and a permissive existing root
    // is a loud startup failure.
    let private_root = crate::filesystem::ConfinedDir::open_or_create(std::path::Path::new(
        data_dir,
    ))
    .map_err(|error| ServerError::Config {
        message: format!("unsafe store.data_dir `{data_dir}`: {error}"),
    })?;

    // Haematite 0.5 creates shard directories lazily. Pre-create every configured
    // directory descriptor-relatively, then force the backend's actual shard
    // spawn/recovery path below while this checked-and-hardened window is held.
    for shard in 0..shard_count {
        private_root
            .create_dir_all(std::path::Path::new(&format!("shard-{shard}")))
            .map_err(|error| ServerError::Config {
                message: format!(
                    "failed to materialize shard-{shard} under store.data_dir `{data_dir}`: {error}"
                ),
            })?;
    }
    private_root
        .harden_tree()
        .map_err(|error| private_store_mode_error(data_dir, &error))?;

    // Deterministic regression seam: the capability and shard directories exist,
    // but Haematite has not touched any path yet.
    before_backend_touch().map_err(|error| ServerError::Config {
        message: format!("store.data_dir pre-open hook failed: {error}"),
    })?;

    #[cfg(unix)]
    let backend_path = private_root
        .backend_path()
        .map_err(|error| ServerError::Config {
            message: format!("failed to resolve held store.data_dir `{data_dir}`: {error}"),
        })?;
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    crate::filesystem::validate_ambient_backend_ancestors(&backend_path).map_err(|error| {
        let (component, reason) = error.into_parts();
        ServerError::UnsafeDataRootAncestor {
            data_root: backend_path.clone(),
            component,
            reason,
        }
    })?;
    #[cfg(not(unix))]
    let backend_path = std::path::PathBuf::from(data_dir);

    let Some(cluster) = cluster else {
        let store = if backend_path.join("config.json").exists() {
            HaematiteStore::open(&backend_path).map_err(ServerError::from)?
        } else {
            HaematiteStore::create_with_shard_count(&backend_path, shard_count)
                .map_err(ServerError::from)?
        };
        store.materialize_all_shards().map_err(ServerError::from)?;
        private_root
            .harden_tree()
            .map_err(|error| private_store_mode_error(data_dir, &error))?;
        let store = store.retain_data_root_capability(private_root);
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
        HaematiteStore::open_or_create_distributed(&backend_path, shard_count, boot)
            .map_err(ServerError::from)?;
    store.materialize_all_shards().map_err(ServerError::from)?;
    private_root
        .harden_tree()
        .map_err(|error| private_store_mode_error(data_dir, &error))?;
    let store = store.retain_data_root_capability(private_root);
    Ok((store, Some(responder)))
}

#[cfg(feature = "haematite-backend")]
fn private_store_mode_error(data_dir: &str, error: &std::io::Error) -> ServerError {
    ServerError::Config {
        message: format!(
            "failed to apply private modes under store.data_dir `{data_dir}`: {error}"
        ),
    }
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

/// The NOI-6 intervention transport used when no push transport is compiled in.
///
/// Without the `liminal-transport` feature there is no way to reach a worker's
/// out-of-band connection, so every routed command reports the owning worker
/// unreachable — which the router maps onto the attempt-scoped stale-target no-op.
/// This keeps the intervention endpoint honest on a transport-less build (an
/// operator gets a NACK, never a false "applied") without gating the endpoint on a
/// feature.
#[cfg(not(feature = "liminal-transport"))]
#[derive(Clone, Debug)]
struct NullInterventionTransport;

#[cfg(not(feature = "liminal-transport"))]
#[async_trait::async_trait]
impl crate::worker::InterventionTransport for NullInterventionTransport {
    async fn push(
        &self,
        _worker: &crate::worker::WorkerHandle,
        _command: aion_core::InterventionCommand,
    ) -> Result<aion_core::InterventionOutcome, ServerError> {
        Err(ServerError::worker_connection_lost(
            "intervention",
            "no intervention push transport is compiled in".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use aion_store::InMemoryStore;

    use super::ServerState;
    use crate::config::{
        AuthConfig, AuthoringConfig, DeployConfig, DevConfig, ListenConfig, MetricsConfig,
        NamespaceConfig, NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, OutboxConfig,
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
            ops_console: OpsConsoleConfig {
                source: OpsConsoleAssetSource::Embedded,
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
                cluster_broadcast_capacity: Some(64),
            },
            workflow_packages: Vec::new(),
            deploy: DeployConfig::default(),
            authoring: AuthoringConfig::default(),
            dev: DevConfig::default(),
            outbox: OutboxConfig::default(),
            observability: crate::config::ObservabilityConfig::default(),
            scheduler_threads: 1,
            query_timeout: Some(Duration::from_millis(10_000)),
            default_namespace: "default".to_owned(),
            auto_create: crate::config::AutoCreate::Open,
            max_in_flight_activities: crate::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
            drain_timeout: Duration::from_secs(30),
            metrics: MetricsConfig { enabled: true },
            owned_shards: Vec::new(),
            cors_allowed_origins: Vec::new(),
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
    async fn namespace_store_is_reachable_and_functional_after_default_boot()
    -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::{MintOutcome, NamespaceOrigin};

        // A default single-node (in-memory) boot must expose a real, functional
        // namespace registry through `state.namespace_store()` — the control
        // plane's mint (S5) and `GET /namespaces` (S7) reach the store this way.
        let state =
            ServerState::build_with_store(InMemoryStore::default(), runtime_config()).await?;

        let store = state.namespace_store();

        // Mint a fresh namespace: the first reference creates it.
        let outcome = store
            .register_namespace("orders", NamespaceOrigin::WorkerMint)
            .await?;
        assert_eq!(
            outcome,
            MintOutcome::Created,
            "the first reference to a namespace mints it"
        );

        // Re-referencing is idempotent: the record already exists.
        let again = store
            .register_namespace("orders", NamespaceOrigin::WorkerMint)
            .await?;
        assert_eq!(
            again,
            MintOutcome::AlreadyExisted,
            "a second reference touches the existing record rather than re-creating it"
        );

        // Single lookup returns the durable record.
        let fetched = store.get_namespace("orders").await?;
        let record = fetched.ok_or("registered namespace must be retrievable via get_namespace")?;
        assert_eq!(record.name, "orders");
        assert_eq!(record.origin, NamespaceOrigin::WorkerMint);

        // The live set lists the namespace.
        let listed = store.list_namespaces().await?;
        assert!(
            listed.iter().any(|record| record.name == "orders"),
            "list_namespaces returns the minted namespace"
        );

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

    #[cfg(all(feature = "haematite-backend", unix))]
    #[test]
    fn haematite_root_swap_before_first_backend_touch_cannot_redirect_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let sandbox = tempfile::tempdir()?;
        let configured_root = sandbox.path().join("data");
        let held_root = sandbox.path().join("held-data");
        let outside = sandbox.path().join("outside");
        std::fs::create_dir(&outside)?;
        let configured = configured_root
            .to_str()
            .ok_or("temporary data path was not UTF-8")?;

        let (store, responder) =
            super::build_haematite_store_with_hook(configured, 4, None, || {
                // The server has acquired and hardened `configured_root`, but
                // Haematite has not opened or created anything. Replace the
                // ambient name with an attacker-controlled symlink at exactly
                // the old check/use boundary.
                std::fs::rename(&configured_root, &held_root)?;
                symlink(&outside, &configured_root)?;
                Ok(())
            })?;
        assert!(responder.is_none());

        let outside_entries = std::fs::read_dir(&outside)?.collect::<Result<Vec<_>, _>>()?;
        assert!(
            outside_entries.is_empty(),
            "Haematite followed the replaced ambient root and wrote outside"
        );
        assert!(held_root.join("config.json").is_file());
        for shard in 0..4 {
            let shard_path = held_root.join(format!("shard-{shard}"));
            assert!(shard_path.is_dir(), "shard {shard} was not materialized");
            assert!(
                std::fs::read_dir(&shard_path)?
                    .next()
                    .transpose()?
                    .is_some(),
                "shard {shard} did not run Haematite's materialization path"
            );
        }

        drop(store);
        Ok(())
    }

    #[cfg(all(
        feature = "haematite-backend",
        any(target_os = "linux", target_os = "android")
    ))]
    #[tokio::test]
    async fn proc_fd_backend_path_survives_a_post_startup_root_swap()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        use aion_core::{ContentType, EventEnvelope, PackageVersion, Payload, RunId, WorkflowId};
        use aion_store::{WritableEventStore as _, WriteToken};
        use chrono::Utc;

        let sandbox = tempfile::tempdir()?;
        let configured_root = sandbox.path().join("data");
        let held_root = sandbox.path().join("held-data");
        let capture = sandbox.path().join("capture");
        std::fs::create_dir(&capture)?;
        let configured = configured_root
            .to_str()
            .ok_or("temporary data path was not UTF-8")?;

        let (store, responder) = super::build_haematite_store(configured, 4, None)?;
        assert!(responder.is_none());
        std::fs::rename(&configured_root, &held_root)?;
        symlink(&capture, &configured_root)?;

        let workflow_id = WorkflowId::new_v4();
        let event = aion_core::Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("post-startup-root-swap"),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            run_id: RunId::new_v4(),
            parent_run_id: None,
            package_version: PackageVersion::new("a".repeat(64)),
        };
        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                0,
            )
            .await?;

        let captured = std::fs::read_dir(&capture)?.collect::<Result<Vec<_>, _>>()?;
        assert!(
            captured.is_empty(),
            "post-startup append followed the replacement symlink into capture"
        );
        assert!(held_root.join("config.json").is_file());
        drop(store);
        Ok(())
    }

    #[cfg(all(
        feature = "haematite-backend",
        unix,
        not(any(target_os = "linux", target_os = "android"))
    ))]
    #[test]
    fn path_ambient_haematite_refuses_group_or_world_writable_ancestors()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;

        let sandbox = tempfile::tempdir()?;
        std::fs::set_permissions(sandbox.path(), std::fs::Permissions::from_mode(0o700))?;

        for mode in [0o770, 0o1777] {
            let shared = sandbox.path().join(format!("shared-{mode:o}"));
            let data_root = shared.join("data");
            std::fs::create_dir(&shared)?;
            std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(mode))?;
            std::fs::create_dir(&data_root)?;
            std::fs::set_permissions(&data_root, std::fs::Permissions::from_mode(0o700))?;
            let configured = data_root
                .to_str()
                .ok_or("temporary data path was not UTF-8")?;

            let Err(error) = super::build_haematite_store(configured, 4, None) else {
                return Err(format!("mode {mode:04o} ancestor was accepted").into());
            };
            let message = error.to_string();
            let crate::ServerError::UnsafeDataRootAncestor {
                data_root: resolved_root,
                component,
                reason,
            } = error
            else {
                return Err(format!("expected typed unsafe-ancestor error, got {message}").into());
            };
            assert_eq!(resolved_root, std::fs::canonicalize(&data_root)?);
            assert_eq!(component, std::fs::canonicalize(&shared)?);
            assert!(
                reason.contains(&format!("mode {mode:04o}")),
                "unexpected reason: {reason}"
            );
            if mode & 0o1000 != 0 {
                assert!(reason.contains("sticky bit is not accepted"));
            }
            assert!(message.contains("private Aion home"));
            assert!(
                !data_root.join("config.json").exists(),
                "Haematite touched its ambient path before the refusal"
            );
        }
        Ok(())
    }

    #[cfg(all(feature = "haematite-backend", target_os = "macos"))]
    #[test]
    fn path_ambient_haematite_refuses_mutating_allow_acl_ancestor()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;

        let sandbox = tempfile::tempdir()?;
        std::fs::set_permissions(sandbox.path(), std::fs::Permissions::from_mode(0o700))?;
        let shared = sandbox.path().join("acl-shared");
        let data_root = shared.join("data");
        std::fs::create_dir(&shared)?;
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700))?;
        let acl = "everyone allow list,search,add_file,add_subdirectory,delete_child";
        let status = std::process::Command::new("chmod")
            .arg("+a")
            .arg(acl)
            .arg(&shared)
            .status()?;
        assert!(status.success(), "failed to install Darwin regression ACL");
        let configured = data_root
            .to_str()
            .ok_or("temporary data path was not UTF-8")?;

        let result = super::build_haematite_store(configured, 4, None);
        let cleanup = std::process::Command::new("chmod")
            .arg("-RN")
            .arg(&shared)
            .status()?;
        assert!(cleanup.success(), "failed to clean Darwin regression ACL");

        let Err(error) = result else {
            return Err("mutating non-euid allow ACL ancestor was accepted".into());
        };
        let message = error.to_string();
        let crate::ServerError::UnsafeDataRootAncestor {
            component, reason, ..
        } = error
        else {
            return Err(format!("expected typed unsafe-ancestor error, got {message}").into());
        };
        assert_eq!(component, std::fs::canonicalize(&shared)?);
        assert!(
            reason.contains("allow"),
            "reason did not name the ACE: {reason}"
        );
        assert!(
            reason.contains("everyone"),
            "reason did not name the ACE principal: {reason}"
        );
        assert!(
            !data_root.join("config.json").exists(),
            "Haematite touched its ambient path before the ACL refusal"
        );
        Ok(())
    }

    #[cfg(all(feature = "haematite-backend", target_os = "macos"))]
    #[test]
    fn path_ambient_haematite_accepts_a_deny_only_acl_ancestor()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;

        let sandbox = tempfile::tempdir()?;
        std::fs::set_permissions(sandbox.path(), std::fs::Permissions::from_mode(0o700))?;
        let private_parent = sandbox.path().join("deny-only");
        let data_root = private_parent.join("data");
        std::fs::create_dir(&private_parent)?;
        std::fs::set_permissions(&private_parent, std::fs::Permissions::from_mode(0o700))?;
        let status = std::process::Command::new("chmod")
            .arg("+a")
            .arg("everyone deny delete")
            .arg(&private_parent)
            .status()?;
        assert!(status.success(), "failed to install Darwin deny-only ACL");
        let configured = data_root
            .to_str()
            .ok_or("temporary data path was not UTF-8")?;

        let result = super::build_haematite_store(configured, 4, None);
        let cleanup = std::process::Command::new("chmod")
            .arg("-RN")
            .arg(&private_parent)
            .status()?;
        assert!(cleanup.success(), "failed to clean Darwin deny-only ACL");

        let (store, responder) = result?;
        assert!(responder.is_none());
        assert!(data_root.join("config.json").is_file());
        drop(store);
        Ok(())
    }

    #[cfg(all(feature = "haematite-backend", target_os = "macos"))]
    #[test]
    fn path_ambient_haematite_accepts_the_stock_home_acl_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;
        use users::os::unix::UserExt as _;

        let effective_uid = rustix::process::geteuid().as_raw();
        let effective_user = users::get_user_by_uid(effective_uid)
            .ok_or_else(|| format!("server euid {effective_uid} has no account record"))?;
        let sandbox = tempfile::Builder::new()
            .prefix(".aion-acl-home-proof-")
            .tempdir_in(effective_user.home_dir())?;
        std::fs::set_permissions(sandbox.path(), std::fs::Permissions::from_mode(0o700))?;
        let data_root = sandbox.path().join("data");
        let configured = data_root
            .to_str()
            .ok_or("temporary data path was not UTF-8")?;

        let (store, responder) = super::build_haematite_store(configured, 4, None)?;
        assert!(responder.is_none());
        assert!(data_root.join("config.json").is_file());
        drop(store);
        Ok(())
    }

    #[cfg(all(
        feature = "haematite-backend",
        unix,
        not(any(target_os = "linux", target_os = "android"))
    ))]
    #[test]
    fn path_ambient_haematite_accepts_an_owner_controlled_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;

        let sandbox = tempfile::tempdir()?;
        std::fs::set_permissions(sandbox.path(), std::fs::Permissions::from_mode(0o700))?;
        let private_parent = sandbox.path().join("private");
        let data_root = private_parent.join("data");
        std::fs::create_dir(&private_parent)?;
        std::fs::set_permissions(&private_parent, std::fs::Permissions::from_mode(0o700))?;
        let configured = data_root
            .to_str()
            .ok_or("temporary data path was not UTF-8")?;

        let (store, responder) = super::build_haematite_store(configured, 4, None)?;
        assert!(responder.is_none());
        assert!(data_root.join("config.json").is_file());
        for shard in 0..4 {
            assert!(data_root.join(format!("shard-{shard}")).is_dir());
        }
        drop(store);
        Ok(())
    }

    #[tokio::test]
    async fn connect_store_memory_backend_exposes_no_outbox_store()
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
        Ok(())
    }

    // The libSQL connect path is now an opt-in backend (`libsql-backend`), so this
    // libSQL-specific outbox-sharing assertion compiles and runs only under that
    // feature. The memory case is covered above, unconditionally.
    #[cfg(feature = "libsql-backend")]
    #[tokio::test]
    async fn connect_store_shares_outbox_store_only_for_libsql()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::config::{StoreBackend, StoreConfig};

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
