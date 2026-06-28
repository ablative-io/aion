//! Run loop for the Aion workflow server: tracing initialization,
//! configuration load, transport startup, and signal-driven graceful
//! shutdown.
//!
//! This is the library entry point behind the `aion server` command. It
//! preserves the operational contract of the former standalone
//! `aion-server` binary: exit code 2 for configuration errors, the drain
//! outcome's exit code on shutdown, and 130 when a second termination
//! signal forces immediate exit.

use std::{net::SocketAddr, process::ExitCode};

use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing::{error, info, warn};

use std::sync::Arc;

use crate::{
    ServerConfig, ServerError, ServerState, api,
    config::{CliOverrides, NamespaceMode, OutboxConfig, OutboxTransport, StoreBackend},
    observability,
    shutdown::{self, ShutdownOutcome},
    worker::{
        ActivityDispatcher, OutboxDispatcher, OutboxDispatcherConfig, OutboxReconciler,
        OutboxReconcilerConfig, OutboxRowDispatch, WorkerOutboxDispatch,
    },
};

/// Run the Aion workflow server until it shuts down, returning the process
/// exit code.
///
/// Initializes the JSON tracing subscriber, loads and validates the merged
/// configuration (file, environment, then `overrides`), serves the gRPC and
/// HTTP transports, and drains gracefully after the first termination
/// signal. Every failure is logged through tracing and mapped to the exit
/// code contract above; the caller only has to exit with the returned code.
pub async fn run(overrides: CliOverrides) -> ExitCode {
    match run_server(overrides).await {
        Ok(code) => code,
        Err(error) => {
            error!(%error, "aion-server failed");
            if error.is_config() {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

async fn run_server(cli: CliOverrides) -> Result<ExitCode, ServerError> {
    observability::tracing::init()?;

    let config = ServerConfig::load(&cli)?;
    reject_auth_without_feature(&config)?;
    let store_backend = config.store.backend;
    // Static shard assignment (SS-1): read the operator's pinned shard set from
    // `[store] owned_shards`. Empty means own ALL shards (single-node default).
    // The set is carried into `RuntimeConfig` by `into_parts` and applied to the
    // `EngineBuilder` during state construction; surface it here so the boot
    // banner records which shards this node serves. No election is performed.
    let owned_shards = config.store.owned_shards.clone();
    // Capture the outbox settings before `build` consumes `config`, so the
    // (default-off) outbox dispatcher can be wired after state is up. The
    // dispatcher shares the engine's already-opened libSQL store (one
    // connection) via `state.outbox_store()`, so no store settings are needed.
    let outbox_config = config.outbox.clone();
    // Capture the SS-5b failover supervisor knobs before `build` consumes config.
    // Only a distributed haematite boot carries a `[store.cluster]` section; this
    // is `None` for every single-node boot, so no supervisor is ever spawned.
    #[cfg(feature = "haematite-backend")]
    let cluster_config = config.store.cluster.clone();
    let state = ServerState::build(config).await?;
    reject_tls_until_supported(&state)?;

    let runtime = state.runtime_config();
    let grpc_address = runtime.listen.grpc;
    let http_address = runtime.listen.http;
    let workflow_packages: Vec<String> = runtime
        .workflow_packages
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        grpc_address = %grpc_address,
        http_address = %http_address,
        default_namespace = %runtime.default_namespace,
        namespace_mode = namespace_mode_label(&runtime.namespace.mode),
        store_backend = store_backend_label(store_backend),
        auth_enabled = runtime.auth.enabled,
        deploy_enabled = runtime.deploy.enabled,
        metrics_enabled = runtime.metrics.enabled,
        workflow_package_count = workflow_packages.len(),
        workflow_packages = ?workflow_packages,
        owned_shards = ?owned_shards,
        owns_all_shards = owned_shards.is_empty(),
        "aion-server startup banner"
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // LSUB-4-1: a distributed haematite boot carries a `[store.cluster]` section.
    // The single outbox dispatcher task is spawned in BOTH modes; the difference
    // is only how ownership is enforced. Single-node (`None`) owns all shards by
    // construction (`owned_shard_scope() == None`), so its claim sweeps see every
    // row. Clustered (`Some`) relies on `claim_outbox_rows`' `owned_shard_scope()`
    // filter — already seeded by `set_owned_shards` during `ServerState::build`,
    // which runs before this point — so each node only ever claims rows on the
    // shards it owns. Compute the flag here where the (feature-gated) cluster
    // section is in scope; pass it to the gate so the boot banner records the mode.
    #[cfg(feature = "haematite-backend")]
    let outbox_clustered = cluster_config.is_some();
    #[cfg(not(feature = "haematite-backend"))]
    let outbox_clustered = false;
    // Dormant by default: only when `outbox.enabled` is set does the
    // non-replayed outbox dispatcher task start. With the flag off (the
    // default) nothing here runs and server behaviour is unchanged.
    maybe_spawn_outbox_dispatcher(&state, &outbox_config, outbox_clustered, &shutdown_rx)?;
    // SS-5b: a distributed boot whose peers declare owned shards runs the cluster
    // supervisor — automatic failover detection. A single-node boot spawns
    // nothing here (the method returns `false`), so default behaviour is
    // unchanged.
    #[cfg(feature = "haematite-backend")]
    maybe_spawn_cluster_supervisor(&state, cluster_config.as_ref(), &shutdown_rx)?;
    let mut grpc = tokio::spawn(serve_grpc(state.clone(), grpc_address, shutdown_rx.clone()));
    let mut http = tokio::spawn(serve_http(state.clone(), http_address, shutdown_rx));

    let outcome = tokio::select! {
        result = &mut grpc => {
            transport_result("gRPC", result)?;
            state.shutdown()?;
            ShutdownOutcome::Clean
        },
        result = &mut http => {
            transport_result("HTTP", result)?;
            state.shutdown()?;
            ShutdownOutcome::Clean
        },
        result = shutdown_signal() => {
            result?;
            let _receiver_count = shutdown_tx.send(true);
            let outcome = shutdown::drain_after_first_signal(state.clone(), async {
                let _ = shutdown_signal().await;
            }).await?;
            if !matches!(outcome, ShutdownOutcome::Forced) {
                transport_result("gRPC", grpc.await)?;
                transport_result("HTTP", http.await)?;
            }
            outcome
        },
    };

    Ok(outcome.exit_code())
}

fn transport_result(
    transport: &'static str,
    result: Result<Result<(), ServerError>, tokio::task::JoinError>,
) -> Result<(), ServerError> {
    match result {
        Ok(transport_outcome) => transport_outcome,
        Err(join_error) => Err(ServerError::Transport {
            transport,
            message: join_error.to_string(),
        }),
    }
}

async fn serve_grpc(
    state: ServerState,
    address: SocketAddr,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let workflow = api::grpc::workflow_service(state.clone());
    let worker = api::worker_grpc::worker_service(state.clone());
    let mut router = TonicServer::builder()
        .add_service(workflow)
        .add_service(worker);
    // Dark by default: the deploy service joins the listener only when the
    // operator commissioned it; otherwise the surface answers Unimplemented.
    if state.runtime_config().deploy.enabled {
        router = router.add_service(api::deploy_grpc::deploy_service(state)?);
    }
    router
        .serve_with_shutdown(address, shutdown_requested(shutdown))
        .await
        .map_err(|source| transport_bind("grpc", address, source))?;
    Ok(())
}

async fn serve_http(
    state: ServerState,
    address: SocketAddr,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let listener = TcpListener::bind(address)
        .await
        .map_err(|source| transport_bind("http", address, source))?;
    axum::serve(listener, api::http::http_router(state)?)
        .with_graceful_shutdown(shutdown_requested(shutdown))
        .await
        .map_err(|source| transport_bind("http", address, source))?;
    Ok(())
}

async fn shutdown_requested(mut shutdown: tokio::sync::watch::Receiver<bool>) {
    while !*shutdown.borrow_and_update() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

async fn shutdown_signal() -> Result<(), ServerError> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())
            .map_err(|source| signal_listener("SIGTERM", &source))?;
        let mut interrupt =
            signal(SignalKind::interrupt()).map_err(|source| signal_listener("SIGINT", &source))?;
        tokio::select! {
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|source| signal_listener("shutdown signal", &source))
    }
}

fn signal_listener(listener: &'static str, source: &std::io::Error) -> ServerError {
    ServerError::SignalListener {
        listener,
        message: source.to_string(),
    }
}

fn reject_auth_without_feature(config: &ServerConfig) -> Result<(), ServerError> {
    if cfg!(not(feature = "auth")) && config.auth.enabled {
        return Err(ServerError::Config {
            message: "auth.enabled=true but binary compiled without auth feature".to_owned(),
        });
    }
    Ok(())
}

/// Spawn the durable-outbox fan-out dispatcher when, and only when, the
/// operator commissioned it (`outbox.enabled = true`).
///
/// This is the single gate that keeps Phase 2 dormant: with the flag off (the
/// default) the function returns immediately without spawning a task, so
/// default server behaviour — and the live workflow dispatch path — is entirely
/// unchanged. When commissioned, the dispatcher claims rows through the engine's
/// own shared `Arc<LibSqlStore>` (one `libsql::Connection`), so its writes
/// serialize with the engine's rather than contending across a second
/// connection. The dispatcher shares the server's shutdown watch, so it drains
/// on the same signal as the transports.
///
/// NOTE (Phase boundary): the spawned dispatcher dispatches claimed rows and
/// records each row's terminal outbox state (done / retry / failed). Routing the
/// worker completion back into workflow history through the Recorder is Phase 3
/// and is not wired here.
fn maybe_spawn_outbox_dispatcher(
    state: &ServerState,
    outbox_config: &OutboxConfig,
    clustered: bool,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Result<(), ServerError> {
    if !outbox_config.enabled {
        return Ok(());
    }
    let dispatcher_config = resolve_outbox_config(outbox_config)?;
    // Share the engine's already-opened store: one backing connection. The
    // dispatcher's `claim_outbox_rows` writes then serialize against the engine's
    // `append_with_outbox` on that single connection instead of contending across
    // a second one. Both the libSQL and the haematite backends provide an
    // `OutboxStore` (the haematite leaf is wired as the outbox store at boot); the
    // in-memory backend has no outbox table, so `outbox_store()` is `None` and
    // commissioning the dispatcher against it is a configuration error (LSUB-4-2).
    let outbox_store = state.outbox_store().ok_or_else(|| ServerError::Config {
        message: "outbox.enabled=true requires store.backend=libsql or store.backend=haematite: \
                  the durable outbox dispatcher claims rows from the store's outbox table, which \
                  the in-memory store does not provide"
            .to_owned(),
    })?;
    let row_dispatch = select_outbox_row_dispatch(state, outbox_config)?;
    // LSUB-2: share the engine's advisory wake so the stage seam pulses this
    // dispatcher the instant a fan-out row commits, dispatching in ~RTT instead of
    // up to one poll interval. The wake is always-on and free; the interval poll is
    // untouched, so it remains the correctness backstop for any lost wake.
    let dispatcher =
        OutboxDispatcher::new(Arc::clone(&outbox_store), row_dispatch, dispatcher_config)
            .with_wake(state.outbox_wake());
    tokio::spawn(dispatcher.run(shutdown_rx.clone()));
    // LSUB-4-1: the single dispatcher task is spawned in both modes. In a
    // single-node boot it owns all shards by construction; in an active-active
    // clustered boot it claims ONLY the shards this node owns, enforced by
    // `claim_outbox_rows`' owned-shard scope (already seeded before this point).
    info!(
        clustered,
        "outbox dispatcher commissioned (active-active per-shard ownership enforced by claim scope \
         when clustered; single-node owns all shards)"
    );
    // LSUB-4-4: the stale-claim reconciler is the in-flight recovery backstop. It
    // is only configured when BOTH reconcile knobs are set, so on a clustered boot
    // that left them unset, owner-kill in-flight recovery latency is bounded only
    // by re-residency replay (a survivor adopting the shard re-residents from
    // history and re-arms via `rearm_outbox_pending`), NOT by `stale_after`. Warn
    // so the operator knows the backstop is absent.
    if let Some(reconciler_config) = resolve_outbox_reconciler_config(outbox_config)? {
        let reconciler = OutboxReconciler::new(outbox_store, reconciler_config);
        tokio::spawn(reconciler.run(shutdown_rx.clone()));
        info!("outbox reconciler commissioned");
    } else if clustered {
        warn!(
            "outbox reconciler is UNCONFIGURED on a clustered boot (outbox.reconcile_interval_ms \
             and outbox.reconcile_stale_after_ms are both unset): in-flight recovery after an \
             owner is killed is then bounded only by re-residency replay on the adopting node, \
             not by a stale-claim backstop; set both knobs to bound stale-claim recovery latency"
        );
    }
    Ok(())
}

/// Spawn the SS-5b cluster supervisor when, and only when, this is a distributed
/// haematite boot whose `[store.cluster]` declared peers with owned shards.
///
/// Reads the failover cadence + debounce from the cluster config (or the
/// documented defaults), then asks the state to spawn the supervisor over its
/// retained concrete store and live engine. With no `[store.cluster]` section —
/// or with no peer declaring `owned_shards` — nothing is spawned and behaviour
/// is unchanged.
#[cfg(feature = "haematite-backend")]
fn maybe_spawn_cluster_supervisor(
    state: &ServerState,
    cluster_config: Option<&crate::config::ClusterConfig>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let Some(cluster) = cluster_config else {
        return Ok(());
    };
    let poll_interval = std::time::Duration::from_millis(
        cluster
            .failover_poll_interval_ms
            .unwrap_or(crate::config::DEFAULT_FAILOVER_POLL_INTERVAL_MS),
    );
    let confirmations = cluster
        .failover_confirmations
        .unwrap_or(crate::config::DEFAULT_FAILOVER_CONFIRMATIONS);
    let supervisor_config = crate::cluster::SupervisorConfig {
        poll_interval,
        confirmations,
    };
    let spawned = state.spawn_cluster_supervisor(supervisor_config, shutdown_rx.clone())?;
    if spawned {
        info!(
            poll_interval_ms = %poll_interval.as_millis(),
            confirmations,
            "SS-5b cluster supervisor commissioned (automatic peer-down failover)"
        );
    }
    Ok(())
}

/// Select the outbox row-dispatch sink by the configured `outbox.transport`.
///
/// `grpc` (the default) builds the unchanged [`WorkerOutboxDispatch`] over the
/// connected-worker registry, so a default server is byte-identical. `liminal`
/// builds the cross-node [`LiminalOutboxDispatch`](crate::worker::LiminalOutboxDispatch);
/// it is only reachable when the `liminal-transport` feature is compiled in, and
/// selecting it without that feature is a configuration error rather than a
/// silent fall-through to gRPC.
fn select_outbox_row_dispatch(
    state: &ServerState,
    outbox_config: &OutboxConfig,
) -> Result<Arc<dyn OutboxRowDispatch>, ServerError> {
    match outbox_config.transport {
        OutboxTransport::Grpc => {
            let push_dispatcher = ActivityDispatcher::new(state.worker_registry().clone())
                .with_drain_state(state.drain_state().clone());
            Ok(Arc::new(WorkerOutboxDispatch::new(push_dispatcher)))
        }
        OutboxTransport::Liminal => build_liminal_row_dispatch(outbox_config),
    }
}

/// Build the liminal row-dispatch sink, or fail with the missing-feature error.
///
/// Split out so the feature-gated arm never grows the selector function past the
/// length lint and so the feature-off build has one clear error site.
#[cfg(feature = "liminal-transport")]
fn build_liminal_row_dispatch(
    outbox_config: &OutboxConfig,
) -> Result<Arc<dyn OutboxRowDispatch>, ServerError> {
    let address = outbox_config
        .liminal_server_address
        .as_ref()
        .ok_or_else(|| ServerError::Config {
            message: "outbox.transport=liminal requires outbox.liminal_server_address \
                      (host:port of the liminal server)"
                .to_owned(),
        })?;
    // The dispatch channel is no longer a fixed config value: it is derived
    // per-row from the row's durable (namespace, task_queue) via
    // `LiminalOutboxDispatch`'s `dispatch_channel_name` (NSTQ-5), so a single
    // dispatcher fans different pools out to different channels.
    Ok(Arc::new(crate::worker::LiminalOutboxDispatch::new(
        address.clone(),
    )))
}

/// Feature-off stub: selecting the liminal transport without the
/// `liminal-transport` feature is a configuration error, never a silent
/// fall-through to gRPC.
#[cfg(not(feature = "liminal-transport"))]
fn build_liminal_row_dispatch(
    _outbox_config: &OutboxConfig,
) -> Result<Arc<dyn OutboxRowDispatch>, ServerError> {
    Err(ServerError::Config {
        message: "outbox.transport=liminal requires the aion-server `liminal-transport` \
                  Cargo feature, which is not enabled in this build"
            .to_owned(),
    })
}

/// Resolve the validated, all-present outbox knobs into the dispatcher's
/// non-optional config. Validation already guaranteed each value is set and in
/// range when `outbox.enabled` is true, so an absent value here is a defensive
/// configuration error, not a default to invent.
fn resolve_outbox_config(outbox: &OutboxConfig) -> Result<OutboxDispatcherConfig, ServerError> {
    let poll_interval_ms = outbox.poll_interval_ms.ok_or_else(|| ServerError::Config {
        message: crate::config::OUTBOX_POLL_INTERVAL_REQUIRED.to_owned(),
    })?;
    let batch_size = outbox.batch_size.ok_or_else(|| ServerError::Config {
        message: crate::config::OUTBOX_BATCH_SIZE_REQUIRED.to_owned(),
    })?;
    let max_attempts = outbox.max_attempts.ok_or_else(|| ServerError::Config {
        message: crate::config::OUTBOX_MAX_ATTEMPTS_REQUIRED.to_owned(),
    })?;
    let backoff_base_ms = outbox.backoff_base_ms.ok_or_else(|| ServerError::Config {
        message: crate::config::OUTBOX_BACKOFF_BASE_REQUIRED.to_owned(),
    })?;
    let backoff_multiplier = outbox
        .backoff_multiplier
        .ok_or_else(|| ServerError::Config {
            message: crate::config::OUTBOX_BACKOFF_MULTIPLIER_REQUIRED.to_owned(),
        })?;
    let backoff_max_ms = outbox.backoff_max_ms.ok_or_else(|| ServerError::Config {
        message: crate::config::OUTBOX_BACKOFF_MAX_REQUIRED.to_owned(),
    })?;
    Ok(OutboxDispatcherConfig {
        poll_interval: std::time::Duration::from_millis(poll_interval_ms),
        batch_size,
        max_attempts,
        backoff_base: std::time::Duration::from_millis(backoff_base_ms),
        backoff_multiplier,
        backoff_max: std::time::Duration::from_millis(backoff_max_ms),
    })
}

fn resolve_outbox_reconciler_config(
    outbox: &OutboxConfig,
) -> Result<Option<OutboxReconcilerConfig>, ServerError> {
    let (Some(interval_ms), Some(stale_after_ms)) = (
        outbox.reconcile_interval_ms,
        outbox.reconcile_stale_after_ms,
    ) else {
        return Ok(None);
    };
    let batch_size = outbox.batch_size.ok_or_else(|| ServerError::Config {
        message: crate::config::OUTBOX_BATCH_SIZE_REQUIRED.to_owned(),
    })?;
    Ok(Some(OutboxReconcilerConfig {
        interval: std::time::Duration::from_millis(interval_ms),
        stale_after: std::time::Duration::from_millis(stale_after_ms),
        batch_size,
    }))
}

fn reject_tls_until_supported(state: &ServerState) -> Result<(), ServerError> {
    if state.runtime_config().tls.is_some() {
        return Err(ServerError::Config {
            message: "configured TLS material cannot be served until transport TLS is wired"
                .to_owned(),
        });
    }
    Ok(())
}

fn store_backend_label(backend: StoreBackend) -> &'static str {
    match backend {
        StoreBackend::Memory => "memory",
        StoreBackend::LibSql => "libsql",
        StoreBackend::Haematite => "haematite",
    }
}

fn namespace_mode_label(mode: &NamespaceMode) -> &'static str {
    match mode {
        NamespaceMode::SharedEngine => "SharedEngine",
        NamespaceMode::SingleTenant { .. } => "SingleTenant",
    }
}

fn transport_bind<E>(transport: &'static str, address: SocketAddr, source: E) -> ServerError
where
    E: std::error::Error,
{
    ServerError::TransportBind {
        transport,
        address,
        message: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        OutboxConfig, OutboxTransport, maybe_spawn_outbox_dispatcher,
        resolve_outbox_reconciler_config,
    };
    use crate::ServerState;
    use crate::config::RuntimeConfig;
    use aion_store::InMemoryStore;
    use std::net::SocketAddr;
    use std::time::Duration;

    /// A minimal `RuntimeConfig` for building an in-memory `ServerState` in unit
    /// tests (mirrors `state.rs`'s test `runtime_config`).
    fn runtime_config() -> RuntimeConfig {
        use crate::config::{
            AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig,
            DevConfig, ListenConfig, MetricsConfig, NamespaceConfig, NamespaceMode,
            WebSocketConfig, WorkerConfig,
        };
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

    /// An `OutboxConfig` with `enabled = true` and every required knob present, so
    /// the only remaining gate is the store-backend / outbox-table availability.
    fn enabled_outbox_config() -> OutboxConfig {
        OutboxConfig {
            enabled: true,
            poll_interval_ms: Some(250),
            batch_size: Some(64),
            max_attempts: Some(5),
            backoff_base_ms: Some(100),
            backoff_multiplier: Some(2),
            backoff_max_ms: Some(30_000),
            reconcile_interval_ms: None,
            reconcile_stale_after_ms: None,
            transport: OutboxTransport::Grpc,
            liminal_server_address: None,
        }
    }

    /// LSUB-4-2 / LSUB-4-6 (Memory-backend guard): commissioning the outbox
    /// dispatcher against the in-memory backend (which has no outbox table, so
    /// `outbox_store()` is `None`) is a configuration error, and the message names
    /// BOTH supported backends (libsql / haematite), not just libsql.
    #[tokio::test]
    async fn outbox_enabled_on_memory_backend_is_a_config_error() {
        let state = ServerState::build_with_store(InMemoryStore::default(), runtime_config())
            .await
            .expect("build in-memory state");
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let error = maybe_spawn_outbox_dispatcher(&state, &enabled_outbox_config(), false, &rx)
            .expect_err("outbox.enabled on the memory backend must be a config error");
        assert!(
            error.is_config(),
            "memory-backend outbox error must be Config"
        );
        let message = error.to_string();
        assert!(
            message.contains("libsql") && message.contains("haematite"),
            "corrected message must name both supported backends, got: {message}"
        );
    }

    /// LSUB-4-1 (Fork-B fast path): with the outbox disabled (the default), the
    /// gate is a no-op even on a memory backend — nothing is spawned and no error
    /// is produced, so a default single-node boot is unchanged.
    #[tokio::test]
    async fn disabled_outbox_is_a_noop_on_any_backend() {
        let state = ServerState::build_with_store(InMemoryStore::default(), runtime_config())
            .await
            .expect("build in-memory state");
        let (_tx, rx) = tokio::sync::watch::channel(false);
        maybe_spawn_outbox_dispatcher(&state, &OutboxConfig::default(), false, &rx)
            .expect("disabled outbox gate must be an infallible no-op");
    }

    /// LSUB-4-4: the reconciler config resolves to `None` unless BOTH knobs are
    /// set — the condition under which the clustered-boot WARN fires.
    #[test]
    fn reconciler_config_absent_unless_both_knobs_set() {
        let mut config = enabled_outbox_config();
        // Neither knob: absent.
        assert!(
            resolve_outbox_reconciler_config(&config)
                .expect("resolve")
                .is_none()
        );
        // Only interval: still absent (the silent-backstop-absent default).
        config.reconcile_interval_ms = Some(1_000);
        assert!(
            resolve_outbox_reconciler_config(&config)
                .expect("resolve")
                .is_none()
        );
        // Both set: present.
        config.reconcile_stale_after_ms = Some(60_000);
        assert!(
            resolve_outbox_reconciler_config(&config)
                .expect("resolve")
                .is_some()
        );
    }
}
