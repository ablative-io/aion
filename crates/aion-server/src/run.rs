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

/// Short TTL for the dispatcher's per-namespace placement cache (Control-Plane
/// Phase 2, P2-P3). Kept small so an operator's `PUT /namespaces/{name}/placement`
/// takes effect on the hot claim loop within a couple of seconds, while still
/// collapsing a per-sweep quorum `get_namespace` into a cheap in-process lookup.
/// A stale entry under `Prefer` only mis-prefers a worker for at most one window
/// and self-corrects — it never affects correctness or replay.
const PLACEMENT_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// Short TTL for the dispatcher's per-namespace quota cache (Control-Plane Phase 2,
/// P2-Q2). Kept small so an operator raising/lowering a tenant's
/// `max_in_flight_activities` takes effect on the hot claim loop within a couple of
/// seconds, while still collapsing a per-sweep quorum `get_namespace` into a cheap
/// in-process lookup. A stale entry only over- or under-admits slightly for one
/// window and self-corrects — backpressure never drops a row, so it cannot affect
/// correctness or replay.
const QUOTA_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// Cadence of the ops-console quota-state broadcaster (Control-Plane Phase 2,
/// P2-Q3). Each tick samples every registry namespace's durable Claimed-row count
/// and cluster-wide ceiling, then pushes one `NamespaceQuotaState` per namespace
/// onto the cluster channel, so the console badge tracks live load. Kept at 1s:
/// brisk enough that the badge visibly ticks as work flows, throttled enough that
/// it is never a per-row firehose (in-flight changes on every claim/settle). It is
/// a server-side push on a timer, NOT a client poll — the dashboard rule bans the
/// latter, not a throttled server snapshot of REAL durable state.
const QUOTA_BROADCAST_CADENCE: std::time::Duration = std::time::Duration::from_secs(1);

/// Resolved keyed-backpressure inputs for the outbox dispatcher (Control-Plane
/// Phase 2, P2-Q2): the generous platform-default ceiling and this node's
/// owned-shard fraction of the cluster shard space.
#[derive(Clone, Copy, Debug)]
struct BackpressureSettings {
    /// The `[namespaces] max_in_flight_activities` platform default, applied to any
    /// namespace carrying no explicit per-tenant override.
    platform_default: u32,
    /// This node's owned-shard fraction of the cluster's virtual shard space,
    /// derived from `[store] owned_shards` and `[store] shard_count`.
    fraction: crate::worker::OwnedShardFraction,
}

impl BackpressureSettings {
    /// Derive the backpressure inputs from the merged server config.
    ///
    /// An empty `[store] owned_shards` means own-all (the single-node default), so
    /// the fraction is 1 and per-node ceilings equal the cluster-wide quota. A
    /// declared owned set enforces the proportional per-node slice
    /// `|owned| / shard_count` (CP-Phase-2 §3.6).
    fn from_config(config: &ServerConfig) -> Self {
        let total = u32::try_from(config.store.shard_count).unwrap_or(u32::MAX);
        let fraction = if config.store.owned_shards.is_empty() {
            crate::worker::OwnedShardFraction::own_all()
        } else {
            let owned = u32::try_from(config.store.owned_shards.len()).unwrap_or(u32::MAX);
            crate::worker::OwnedShardFraction::new(owned, total)
        };
        Self {
            platform_default: config.namespaces.max_in_flight_activities,
            fraction,
        }
    }
}

/// Owns the liminal worker listener for the server's lifetime when the outbox is
/// commissioned over the liminal transport.
///
/// The aion-server HOSTS the liminal listener that remote workers connect IN to;
/// its inner [`ServerListener`](liminal_server::server::listener::ServerListener)
/// owns the accept worker. Held as a local in [`run_server`] across the whole
/// serve `select!`, so it is dropped exactly at server shutdown — and the
/// listener's own `Drop` stops the accept worker cleanly (no leaked thread, no
/// orphaned listener). Every non-liminal boot (the default) carries the `None`
/// guard, which holds nothing and drops to a no-op, so behaviour is unchanged.
#[derive(Debug, Default)]
struct OutboxWorkerListener {
    /// Held purely for its `Drop` side-effect (stopping the accept worker on
    /// server shutdown); never read after construction, hence the leading
    /// underscore.
    #[cfg(feature = "liminal-transport")]
    _inner: Option<liminal_server::server::listener::ServerListener>,
}

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
    // Control-Plane Phase 2 (P2-Q2): capture the keyed-backpressure inputs — the
    // generous platform-default ceiling and this node's owned-shard fraction —
    // before `build` consumes `config`. On a single-node / own-all boot the fraction
    // is 1, so per-node ceilings equal the cluster-wide quota and, with the generous
    // default and no tenant override, the ceiling never engages (byte-identical claim).
    let backpressure_settings = BackpressureSettings::from_config(&config);
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
    // Hold the liminal worker listener (if any) for the server's lifetime: it is
    // dropped at the end of `run_server`, after the serve `select!` completes, so
    // its accept worker stops cleanly on shutdown via the listener's own `Drop`.
    // #204/#253: rebuild the pause dispatch-hold and settle terminal
    // workflows' stranded outbox rows BEFORE the dispatcher's first claim.
    rebuild_outbox_boot_state(&state, &outbox_config).await;
    let _outbox_worker_listener = maybe_spawn_outbox_dispatcher(
        &state,
        &outbox_config,
        outbox_clustered,
        backpressure_settings,
        &shutdown_rx,
    )?;
    // SS-5b: a distributed boot whose peers declare owned shards runs the cluster
    // supervisor — automatic failover detection. A single-node boot spawns
    // nothing here (the method returns `false`), so default behaviour is
    // unchanged.
    #[cfg(feature = "haematite-backend")]
    maybe_spawn_cluster_supervisor(&state, cluster_config.as_ref(), &shutdown_rx)?;
    // #176: the worker heartbeat expiry sweeper is ALWAYS commissioned —
    // dead-worker detection is a liveness correctness property, not an opt-in
    // feature. It is the production caller of `fail_expired_workers`: a worker
    // whose stream stays open while its process wedges (stops heartbeating
    // without disconnecting) is expired, deregistered with the provable Timeout
    // reason, and its in-flight tasks surface as retryable lost-worker failures.
    // Cadence derives from `worker.heartbeat_window` (quarter-window, clamped to
    // [1s, window]; the default 30s window sweeps every 7.5s) — deliberately no
    // separate config knob. It drains on the same shutdown watch as the
    // transports; dropping the JoinHandle only detaches the task.
    drop(state.spawn_heartbeat_sweeper(shutdown_rx.clone()));
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

/// Rebuild the outbox-related boot state BEFORE the dispatcher's first claim,
/// when (and only when) the outbox is commissioned:
///
/// - #204: repopulate the durable pause dispatch-hold from `list_paused`, so a
///   run paused before a restart keeps its outbox rows held (never claimed)
///   after recovery. A run projecting `Paused` is excluded from `list_active`
///   respawn for free; this repopulates the hold that would otherwise be empty
///   in memory after a crash.
/// - #253: settle terminal workflows' stranded outbox rows. A workflow that
///   reached a durable terminal without its rows being settled (a settle-hook
///   failure, or a crash between the terminal append and the settle) must not
///   have those rows re-armed and redelivered after restart — that is the
///   zombie-round incident. A sweep error is loud but non-fatal: the
///   settle-at-terminal hook and the reconciler's liveness gate remain as
///   repair paths, and the residual window is one bounded dispatch whose
///   completion drops unmatched, never a re-arm loop.
async fn rebuild_outbox_boot_state(state: &ServerState, outbox_config: &OutboxConfig) {
    if !outbox_config.enabled {
        return;
    }
    let Ok(engine) = state.engine() else {
        return;
    };
    if let Err(error) = engine.rebuild_paused_runs().await {
        warn!(%error, "failed to rebuild paused-runs dispatch hold at startup");
    }
    let Some(outbox_store) = state.outbox_store() else {
        return;
    };
    match crate::worker::settle_terminal_outbox_rows(engine.store().as_ref(), outbox_store.as_ref())
        .await
    {
        Ok(settled) if settled.is_empty() => {}
        Ok(settled) => {
            info!(
                settled = settled.len(),
                "boot sweep settled stranded outbox rows for terminal workflows"
            );
        }
        Err(error) => {
            error!(
                %error,
                "boot sweep failed to settle terminal workflows' outbox rows; \
                 the reconciler liveness gate remains the backstop"
            );
        }
    }
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
    backpressure_settings: BackpressureSettings,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Result<OutboxWorkerListener, ServerError> {
    if !outbox_config.enabled {
        return Ok(OutboxWorkerListener::default());
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
    let (row_dispatch, worker_listener) = select_outbox_row_dispatch(state, outbox_config)?;
    // LSUB-2: share the engine's advisory wake so the stage seam pulses this
    // dispatcher the instant a fan-out row commits, dispatching in ~RTT instead of
    // up to one poll interval. The wake is always-on and free; the interval poll is
    // untouched, so it remains the correctness backstop for any lost wake.
    // Control-Plane Phase 2 (P2-Q2): attach per-tenant keyed backpressure so each
    // sweep claims per-namespace, round-robin, capped at each tenant's CLAIMED-only
    // headroom (`per_node_ceiling − claimed`). The quota cache front-runs a per-sweep
    // quorum `get_namespace`. With the generous platform default and no tenant
    // override the ceiling never engages, so a default deployment's claim behaviour is
    // byte-identical to the pre-Phase-2 single unscoped claim.
    let quota_cache = crate::worker::QuotaCache::new(
        Arc::clone(state.namespace_store()),
        backpressure_settings.platform_default,
        QUOTA_CACHE_TTL,
    );
    let backpressure =
        crate::worker::Backpressure::new(quota_cache.clone(), backpressure_settings.fraction);
    let mut dispatcher =
        OutboxDispatcher::new(Arc::clone(&outbox_store), row_dispatch, dispatcher_config)
            .with_wake(state.outbox_wake())
            .with_backpressure(backpressure);
    // #204: attach the engine's durable pause dispatch-hold so a held (paused)
    // run's rows are never claimed. The hold set is rebuilt from `list_paused`
    // BEFORE this spawn (see `run_server`), so the dispatcher's first claim
    // already excludes pre-pause rows after a restart.
    if let Ok(engine) = state.engine() {
        dispatcher = dispatcher.with_paused_runs(engine.paused_runs());
    }
    tokio::spawn(dispatcher.run(shutdown_rx.clone()));
    // Control-Plane Phase 2 (P2-Q3): commission the ops-console quota-state
    // broadcaster on the SAME durable stores + quota cache the dispatcher enforces
    // against, so the console badge is a faithful window onto the live per-tenant
    // in-flight/ceiling the backpressure caps. It shares the shutdown watch, so it
    // drains with the dispatcher. Only spawned alongside the (default-off)
    // dispatcher: quota state is meaningless without the outbox fan-out path, and
    // `in_flight` is the durable Claimed outbox count that path produces.
    let quota_broadcaster = crate::worker::QuotaBroadcaster::new(
        Arc::clone(state.namespace_store()),
        Arc::clone(&outbox_store),
        quota_cache,
        state.cluster_publisher().clone(),
        QUOTA_BROADCAST_CADENCE,
    );
    tokio::spawn(quota_broadcaster.run(shutdown_rx.clone()));
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
        // #253: the reconciler's liveness gate projects each stale candidate's
        // workflow status from the engine's event store before any re-arm, so
        // a terminal workflow's stranded row settles instead of redelivering.
        let event_store = state.engine()?.store();
        let reconciler = OutboxReconciler::new(outbox_store, event_store, reconciler_config);
        tokio::spawn(reconciler.run(shutdown_rx.clone()));
        info!("outbox reconciler commissioned (terminal-workflow liveness gate active)");
    } else if clustered {
        warn!(
            "outbox reconciler is UNCONFIGURED on a clustered boot (outbox.reconcile_interval_ms \
             and outbox.reconcile_stale_after_ms are both unset): in-flight recovery after an \
             owner is killed is then bounded only by re-residency replay on the adopting node, \
             not by a stale-claim backstop; set both knobs to bound stale-claim recovery latency"
        );
    }
    Ok(worker_listener)
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

/// Select the outbox row-dispatch sink by the configured `outbox.transport`,
/// returning the sink plus the worker listener whose lifetime the caller must
/// hold.
///
/// `grpc` (the default) builds the unchanged [`WorkerOutboxDispatch`] over the
/// connected-worker registry and carries the empty [`OutboxWorkerListener`], so a
/// default server is byte-identical. `liminal` builds the cross-node
/// [`RegistryLiminalDispatch`](crate::worker::RegistryLiminalDispatch) AND stands
/// up the liminal worker listener the aion-server hosts (returned in the guard);
/// it is only reachable when the `liminal-transport` feature is compiled in, and
/// selecting it without that feature is a configuration error rather than a
/// silent fall-through to gRPC.
fn select_outbox_row_dispatch(
    state: &ServerState,
    outbox_config: &OutboxConfig,
) -> Result<(Arc<dyn OutboxRowDispatch>, OutboxWorkerListener), ServerError> {
    match outbox_config.transport {
        OutboxTransport::Grpc => {
            let push_dispatcher = ActivityDispatcher::new(state.worker_registry().clone())
                .with_drain_state(state.drain_state().clone());
            // Control-Plane Phase 2 (P2-P3): attach the short-TTL placement cache
            // so an unpinned row in a `Prefer{L}` namespace prefers an L-labelled
            // worker (spilling to any live worker). The cache front-runs a per-row
            // quorum `get_namespace` on the hot claim loop; a default-`Unplaced`
            // deployment is byte-identical (every row falls through to any-worker).
            let placement_cache = crate::worker::PlacementCache::new(
                Arc::clone(state.namespace_store()),
                PLACEMENT_CACHE_TTL,
            );
            let dispatch: Arc<dyn OutboxRowDispatch> = Arc::new(
                WorkerOutboxDispatch::new(push_dispatcher).with_placement_cache(placement_cache),
            );
            Ok((dispatch, OutboxWorkerListener::default()))
        }
        OutboxTransport::Liminal => build_liminal_row_dispatch(state, outbox_config),
    }
}

/// Build the production liminal row-dispatch sink and host the worker listener, or
/// fail with the missing-feature error.
///
/// This lifts the tested cross-node wiring (the `lsub1`/`lsub5` e2e blueprint)
/// into the production boot. The aion-server HOSTS the liminal listener that
/// remote workers connect IN to, so its
/// [`ConnectionSupervisor`](liminal_server::server::connection::ConnectionSupervisor)
/// owns each worker's connection and can push a dispatch out on it. The
/// constructor cycle resolves the notifier <-> supervisor dependency:
///
/// 1. Reuse the registry already in [`ServerState`] — gRPC and liminal workers
///    share ONE registry and the same `select_worker`, so routing is identical.
/// 2. Build the [`LiminalConnectionNotifier`] over that registry (no supervisor
///    yet).
/// 3. Build the [`LiminalConnectionServices`] from the liminal listen config.
/// 4. Build the [`ConnectionSupervisor`] WITH the services + notifier.
/// 5. Bind the supervisor back into the notifier (must succeed).
/// 6. Bind the [`ServerListener`] on the configured listen address — workers
///    connect IN here.
/// 7. Reuse the SAME completion callback the gRPC completion path installs
///    ([`ServerOutboxDeliveryCallback`] over the live engine), so a liminal
///    completion re-enters aion through the identical terminal-recording seam.
/// 8. Build the [`RegistryLiminalDispatch`] over the registry + callback (it
///    constructs the [`LiminalCompletionSource`] internally).
///
/// The returned listener is held by the caller for the server's lifetime; its
/// `Drop` stops the accept worker on shutdown.
///
/// [`LiminalConnectionServices`]: liminal_server::server::connection::LiminalConnectionServices
/// [`ServerListener`]: liminal_server::server::listener::ServerListener
/// [`ServerOutboxDeliveryCallback`]: crate::worker::ServerOutboxDeliveryCallback
/// [`LiminalCompletionSource`]: crate::worker::LiminalCompletionSource
/// [`LiminalConnectionNotifier`]: crate::worker::LiminalConnectionNotifier
#[cfg(feature = "liminal-transport")]
fn build_liminal_row_dispatch(
    state: &ServerState,
    outbox_config: &OutboxConfig,
) -> Result<(Arc<dyn OutboxRowDispatch>, OutboxWorkerListener), ServerError> {
    use liminal_server::config::ServerConfig as LiminalServerConfig;
    use liminal_server::config::{LimitsConfig, ServicesConfig};
    use liminal_server::server::connection::{ConnectionSupervisor, LiminalConnectionServices};
    use liminal_server::server::listener::ServerListener;

    use crate::worker::{
        LiminalConnectionNotifier, RegistryLiminalDispatch, ServerOutboxDeliveryCallback,
    };

    let listen_address = outbox_config
        .liminal_listen_address
        .as_ref()
        .ok_or_else(|| ServerError::Config {
            message: "outbox.transport=liminal requires outbox.liminal_listen_address \
                      (host:port the aion-server listens on for inbound liminal worker \
                      connections)"
                .to_owned(),
        })?;
    let listen_address: SocketAddr =
        listen_address
            .parse()
            .map_err(|error| ServerError::Config {
                message: format!(
                    "outbox.liminal_listen_address must be a host:port socket address: {error}"
                ),
            })?;

    // The liminal listener is the worker-connection front door only: it binds the
    // wire listen address and serves the connection supervisor. `from_config` and
    // `ServerListener::bind` read neither `health_listen_address` nor `channels`
    // (the health probe is bound only by the standalone liminal server's full
    // boot, not this embedded path), so no separate health port is bound here;
    // it is set structurally to the listen address and never used.
    let liminal_config = LiminalServerConfig {
        listen_address,
        health_listen_address: listen_address,
        drain_timeout_ms: 30_000,
        channels: Vec::new(),
        routing_rules: Vec::new(),
        persistence_path: None,
        cluster: None,
        // liminal 0.2.3 (H4) added an optional shared-token Connect gate. `None`
        // keeps this embedded worker front door open at the liminal layer —
        // identical to the pre-0.2.3 wire behavior; worker identity/authorization
        // stays aion's job (x-aion-* registration metadata). Threading an
        // operator-configured token through aion's outbox config is a separate
        // feature decision, not part of the dependency alignment.
        auth: None,
        // liminal 0.2.4 (D2/§5): service profile + operational bounds. Defaults =
        // full profile + the certifying-pair-signed caps — byte-equivalent to the
        // 0.2.3 behaviour this embedded front door always had. A worker-front-door
        // profile election here is a future feature decision, not this migration.
        services: ServicesConfig::default(),
        limits: LimitsConfig::default(),
    };

    // (1) Reuse the registry already in ServerState: gRPC + liminal workers share
    // ONE registry and the same `select_worker`.
    let registry = state.worker_registry().clone();
    // (2) Notifier over that registry (supervisor bound after it is built), with the
    // NOI-5b transcript tap: a worker's observability publishes on the reserved
    // channel drain into the SAME transcript sequencer the transcript socket serves,
    // so a live agent's transcript is persisted + fanned out. (Captures the current
    // runtime handle to bridge the sync connection callback onto the async append.)
    let notifier = Arc::new(
        LiminalConnectionNotifier::new(registry.clone())
            .with_transcript_publisher(state.transcript_publisher().clone())
            // The SAME per-task liveness tracker the engine-seam bridge tracks
            // into: a liminal worker's automatic liveness beats refresh it, so
            // the #176 expiry sweeper never falsely expires a healthy liminal
            // worker running an activity longer than the heartbeat window.
            .with_heartbeat_tracker(state.heartbeat_tracker().clone()),
    );
    // (3) Connection services from the liminal listen config.
    let services = Arc::new(
        LiminalConnectionServices::from_config(&liminal_config).map_err(|error| {
            ServerError::Config {
                message: format!("liminal connection services build failed: {error}"),
            }
        })?,
    );
    // (4) Supervisor WITH the services + notifier (the cycle's forward edge).
    let supervisor = ConnectionSupervisor::with_services_and_notifier(services, notifier.clone())
        .map_err(|error| ServerError::Config {
        message: format!("liminal connection supervisor build failed: {error}"),
    })?;
    // (5) Bind the supervisor back into the notifier (the cycle's back edge); a
    // failure here is a wiring bug, surfaced rather than silently ignored.
    if !notifier.bind_supervisor(supervisor.clone()) {
        return Err(ServerError::Config {
            message: "liminal notifier supervisor handle was already bound during boot".to_owned(),
        });
    }
    // (6) Bind the listener on the configured address — workers connect IN here.
    let listener =
        ServerListener::bind(&liminal_config, supervisor).map_err(|error| ServerError::Config {
            message: format!("liminal worker listener failed to bind {listen_address}: {error}"),
        })?;
    // (7) Reuse the SAME completion callback the gRPC completion path uses, over
    // the live engine, so a liminal completion re-enters aion through the
    // identical terminal-recording seam (`record_fan_out_completion`).
    let engine = state.engine()?;
    let callback: Arc<dyn crate::worker::OutboxDeliveryCallback> =
        Arc::new(ServerOutboxDeliveryCallback::new(engine));
    // (8) The registry-backed dispatch builds its LiminalCompletionSource from the
    // shared callback internally. Attach the SAME short-TTL placement cache the
    // gRPC arm installs (Control-Plane Phase 2, P2-P3), so an unpinned row in a
    // `Prefer{L}` namespace prefers an L-labelled worker (spilling to any live
    // worker) on the cross-node liminal transport too — the cluster-failover
    // demo behaviour. A default-`Unplaced` deployment is byte-identical.
    let placement_cache = crate::worker::PlacementCache::new(
        Arc::clone(state.namespace_store()),
        PLACEMENT_CACHE_TTL,
    );
    // NOI-6: install the SAME attempt-owner back-index the server's intervention
    // router resolves through, so each dispatched agent attempt binds its owning
    // worker and a pushed command reaches the worker this dispatcher sent it to.
    let dispatch: Arc<dyn OutboxRowDispatch> = Arc::new(
        RegistryLiminalDispatch::new(registry, callback)
            .with_placement_cache(placement_cache)
            .with_attempt_owners(state.attempt_owners().clone()),
    );

    info!(
        listen_address = %listen_address,
        "liminal outbox worker listener commissioned (remote workers connect in and self-register)"
    );
    Ok((
        dispatch,
        OutboxWorkerListener {
            _inner: Some(listener),
        },
    ))
}

/// Feature-off stub: selecting the liminal transport without the
/// `liminal-transport` feature is a configuration error, never a silent
/// fall-through to gRPC.
#[cfg(not(feature = "liminal-transport"))]
fn build_liminal_row_dispatch(
    _state: &ServerState,
    _outbox_config: &OutboxConfig,
) -> Result<(Arc<dyn OutboxRowDispatch>, OutboxWorkerListener), ServerError> {
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
        BackpressureSettings, OutboxConfig, OutboxTransport, maybe_spawn_outbox_dispatcher,
        resolve_outbox_reconciler_config,
    };
    use crate::ServerState;
    use crate::config::RuntimeConfig;
    use aion_store::InMemoryStore;
    use std::net::SocketAddr;
    use std::time::Duration;

    /// Own-all, generous-default backpressure settings for the gate tests (the
    /// single-node default: fraction 1, so the ceiling never engages).
    fn test_backpressure_settings() -> BackpressureSettings {
        BackpressureSettings {
            platform_default: crate::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
            fraction: crate::worker::OwnedShardFraction::own_all(),
        }
    }

    /// A minimal `RuntimeConfig` for building an in-memory `ServerState` in unit
    /// tests (mirrors `state.rs`'s test `runtime_config`).
    fn runtime_config() -> RuntimeConfig {
        use crate::config::{
            AuthConfig, AuthoringConfig, DeployConfig, DevConfig, ListenConfig, MetricsConfig,
            NamespaceConfig, NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig,
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
            liminal_listen_address: None,
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
        let error = maybe_spawn_outbox_dispatcher(
            &state,
            &enabled_outbox_config(),
            false,
            test_backpressure_settings(),
            &rx,
        )
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
        maybe_spawn_outbox_dispatcher(
            &state,
            &OutboxConfig::default(),
            false,
            test_backpressure_settings(),
            &rx,
        )
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

    /// LSUB-PROD (13-6): the liminal transport requires `liminal_listen_address`.
    /// Commissioning the dispatcher with `transport = liminal` but no listen
    /// address is a configuration error naming the missing knob, rather than a
    /// panic or a silent fall-through to gRPC. Built over the libSQL backend (so
    /// the outbox-store gate passes and the missing-address check is actually
    /// reached). (Feature-gated: the liminal arm of `build_liminal_row_dispatch`
    /// only exists with `liminal-transport` on; in a feature-off build the same
    /// selection is the missing-feature error instead, covered by the type system
    /// rather than this test.)
    // Also gated on `libsql-backend`: it boots a real libSQL-backed `ServerState`
    // to obtain an outbox-bearing store, and the libSQL connect path is now an
    // opt-in feature. The listen-address guard itself is backend-agnostic.
    #[cfg(all(feature = "liminal-transport", feature = "libsql-backend"))]
    #[tokio::test]
    async fn liminal_transport_requires_listen_address() {
        use crate::config::{
            RuntimeSection, ServerConfig, StoreBackend, StoreConfig, WebSocketConfig,
        };

        let db_path = std::env::temp_dir().join(format!(
            "aion-lsub-prod-listen-guard-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let mut outbox = enabled_outbox_config();
        outbox.transport = OutboxTransport::Liminal;
        outbox.liminal_listen_address = None;
        let config = ServerConfig {
            store: StoreConfig {
                backend: StoreBackend::LibSql,
                url: Some(db_path.to_string_lossy().into_owned()),
                ..StoreConfig::default()
            },
            runtime: RuntimeSection {
                scheduler_threads: 1,
                query_timeout_ms: Some(10_000),
            },
            websocket: WebSocketConfig {
                outbound_buffer_bound: 32,
                event_broadcast_capacity: Some(64),
                cluster_broadcast_capacity: Some(64),
            },
            outbox: outbox.clone(),
            ..ServerConfig::default()
        };
        let state = ServerState::build(config)
            .await
            .expect("build libsql state");
        let (_tx, rx) = tokio::sync::watch::channel(false);

        let error = maybe_spawn_outbox_dispatcher(
            &state,
            &outbox,
            false,
            test_backpressure_settings(),
            &rx,
        )
        .expect_err("liminal transport without a listen address must be a config error");
        assert!(
            error.is_config(),
            "missing-listen-address error must be Config"
        );
        assert!(
            error.to_string().contains("liminal_listen_address"),
            "error must name the missing knob, got: {error}"
        );
    }
}

/// LSUB-PROD (13-6): production-boot cross-node round-trip over the REAL wiring.
///
/// This is the proof that the production boot now does the full round-trip the
/// retired stub could not. It drives the EXACT production commissioning function
/// `run_server` calls — [`maybe_spawn_outbox_dispatcher`] — over a real
/// [`ServerState`] built with `outbox.enabled`, `transport = liminal`, and a
/// `liminal_listen_address`. That function lifts the full push wiring
/// (`build_liminal_row_dispatch`): it hosts the liminal worker listener, builds
/// [`RegistryLiminalDispatch`](crate::worker::RegistryLiminalDispatch) over the
/// SAME registry the gRPC path uses and the SAME
/// [`ServerOutboxDeliveryCallback`](crate::worker::ServerOutboxDeliveryCallback)
/// (over the live engine), and spawns the real [`OutboxDispatcher`].
///
/// A REAL remote [`LiminalActivityWorker`](aion_worker::LiminalActivityWorker)
/// connects IN to the listener and self-registers in-band. A `collect_four`
/// fan-out is started over the REAL HTTP transport, which stages four pending
/// outbox rows; the production-wired dispatcher claims and pushes each to the
/// worker, the worker executes it, and its completion re-enters aion through the
/// production engine callback — `record_fan_out_completion` — driving the
/// workflow to a recorded terminal. The proof asserts BOTH: the worker observably
/// executed the activities, AND the terminals were recorded in history (four
/// `ActivityCompleted` + one `WorkflowCompleted`), which the stub's
/// publish-and-mark-done path never achieved.
// Also gated on `libsql-backend`: this production-boot round-trip stands up a
// real libSQL-backed server (the durable outbox path it exercises), and the
// libSQL connect path is now an opt-in feature.
#[cfg(all(test, feature = "liminal-transport", feature = "libsql-backend"))]
mod lsub_prod_xnode_e2e {
    #![allow(clippy::expect_used)]

    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use aion_core::Event;
    use aion_package::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
        PackageBuilder,
    };
    use aion_store::ReadableEventStore;
    use aion_store_libsql::LibSqlStore;
    use aion_worker::{ActivityRegistry, LiminalActivityWorker, WorkerConfig};
    use axum::body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use super::{BackpressureSettings, maybe_spawn_outbox_dispatcher};
    use crate::ServerState;
    use crate::api::http::http_router;
    use crate::config::{
        OutboxConfig, OutboxTransport, RuntimeSection, ServerConfig, StoreBackend, StoreConfig,
        WebSocketConfig,
    };

    type TestError = Box<dyn std::error::Error + Send + Sync>;

    /// The `collect_four` fixture passes each member the JSON string `"in"` as
    /// activity input, so the worker handler decodes a [`String`], not a struct.
    type FanInput = String;

    const NAMESPACE: &str = "default";
    const TASK_QUEUE: &str = "default";
    const OUTBOX_MODULE: &str = "aion_outbox_fixture";
    const OUTBOX_BEAM: &[u8] = include_bytes!("../tests/fixtures/aion_outbox_fixture.beam");
    const OUTBOX_SOURCE: &[u8] = include_bytes!("../tests/fixtures/aion_outbox_fixture.erl");
    const FAN_OUT: usize = 4;
    const FAN_ACTIVITY_TYPES: [&str; FAN_OUT] = ["fan:0", "fan:1", "fan:2", "fan:3"];
    const POLL_DEADLINE: Duration = Duration::from_secs(20);

    fn test_error(message: impl std::fmt::Display) -> TestError {
        message.to_string().into()
    }

    /// Reserve a loopback port and return it: the liminal listener binds this exact
    /// address (the production path binds the configured `liminal_listen_address`,
    /// so the test must commit to a concrete port the worker can also dial).
    fn reserve_loopback_port() -> Result<SocketAddr, TestError> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(test_error)?;
        let address = listener.local_addr().map_err(test_error)?;
        drop(listener);
        Ok(address)
    }

    /// Build the `collect_four` package on disk so the production state-build path
    /// loads it exactly as it loads operator-supplied `workflow_packages`.
    fn write_package_archive(dir: &std::path::Path) -> Result<PathBuf, TestError> {
        let beams =
            BeamSet::new(vec![BeamModule::new(OUTBOX_MODULE, OUTBOX_BEAM)]).map_err(test_error)?;
        let manifest = Manifest {
            entry_module: OUTBOX_MODULE.to_owned(),
            entry_function: "collect_four".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({}),
            timeout: Duration::from_secs(30),
            activities: vec![DeclaredActivity {
                activity_type: "fixture_activity".to_owned(),
            }],
            version: ManifestVersion::new("stamped-by-builder"),
            format_version: CURRENT_FORMAT_VERSION,
            additional_workflows: Vec::new(),
        };
        let archive =
            PackageBuilder::with_source(manifest, beams, [(OUTBOX_MODULE, OUTBOX_SOURCE.to_vec())])
                .write_to_bytes()
                .map_err(test_error)?;
        let path = dir.join("collect_four.aion");
        std::fs::write(&path, archive).map_err(test_error)?;
        Ok(path)
    }

    /// A production-shaped `ServerConfig`: the libSQL backend (so the boot store
    /// path shares the leaf as the dispatcher's outbox store, exactly as
    /// `ServerState::build` does in production), `outbox.enabled`,
    /// `transport = liminal`, the reserved `liminal_listen_address`, and the
    /// `collect_four` package. Built through `ServerState::build` (not
    /// `build_with_store`), so this is the real boot store seam, not a test stand-in.
    fn server_config(
        db_path: &std::path::Path,
        package_path: PathBuf,
        listen_address: SocketAddr,
    ) -> ServerConfig {
        ServerConfig {
            store: StoreConfig {
                backend: StoreBackend::LibSql,
                url: Some(db_path.to_string_lossy().into_owned()),
                ..StoreConfig::default()
            },
            runtime: RuntimeSection {
                scheduler_threads: 1,
                query_timeout_ms: Some(10_000),
            },
            websocket: WebSocketConfig {
                outbound_buffer_bound: 32,
                event_broadcast_capacity: Some(64),
                cluster_broadcast_capacity: Some(64),
            },
            workflow_packages: vec![package_path],
            outbox: OutboxConfig {
                enabled: true,
                poll_interval_ms: Some(20),
                batch_size: Some(16),
                max_attempts: Some(5),
                backoff_base_ms: Some(50),
                backoff_multiplier: Some(2),
                backoff_max_ms: Some(1_000),
                reconcile_interval_ms: None,
                reconcile_stale_after_ms: None,
                transport: OutboxTransport::Liminal,
                liminal_listen_address: Some(listen_address.to_string()),
            },
            ..ServerConfig::default()
        }
    }

    /// The remote worker self-describes for the fixture's pool `(default, default)`
    /// and registers a handler for every `fan:N` activity type, counting executions
    /// so the test proves it genuinely ran the pushed dispatches.
    fn worker_config() -> Result<WorkerConfig, TestError> {
        WorkerConfig::builder()
            .endpoint("unused-direct-address")
            .namespace(NAMESPACE)
            .task_queue(TASK_QUEUE)
            .identity("lsub-prod-worker")
            .max_concurrency(4)
            .reconnect_initial_backoff(Duration::from_millis(5))
            .reconnect_max_backoff(Duration::from_millis(20))
            .reconnect_max_attempts(3)
            .build()
            .map_err(test_error)
    }

    fn worker_registry(executions: &Arc<AtomicUsize>) -> Result<Arc<ActivityRegistry>, TestError> {
        let mut registry = ActivityRegistry::new();
        for activity_type in FAN_ACTIVITY_TYPES {
            let executions = Arc::clone(executions);
            registry = registry
                .register_activity(activity_type, move |_input: FanInput, _context| {
                    let executions = Arc::clone(&executions);
                    Box::pin(async move {
                        executions.fetch_add(1, Ordering::SeqCst);
                        Ok(activity_type.to_owned())
                    })
                })
                .map_err(test_error)?;
        }
        Ok(Arc::new(registry))
    }

    /// Spawns the remote worker on its own OS thread with a current-thread runtime
    /// (the push receive is blocking), connecting IN to the production listener.
    struct WorkerThread {
        stop: Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl WorkerThread {
        fn spawn(address: String, config: WorkerConfig, registry: Arc<ActivityRegistry>) -> Self {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("worker runtime build failed: {error}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let worker = match LiminalActivityWorker::connect(&address, &config, registry) {
                        Ok(worker) => worker,
                        Err(error) => {
                            eprintln!("worker connect failed: {error}");
                            return;
                        }
                    };
                    if let Err(error) = worker
                        .serve_until(|| thread_stop.load(Ordering::SeqCst))
                        .await
                    {
                        eprintln!("worker serve loop ended with error: {error}");
                    }
                });
            });
            Self {
                stop,
                handle: Some(handle),
            }
        }

        fn stop(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                handle.join().ok();
            }
        }
    }

    fn count_completed(history: &[Event]) -> usize {
        history
            .iter()
            .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
            .count()
    }

    fn count_workflow_completed(history: &[Event]) -> usize {
        history
            .iter()
            .filter(|event| matches!(event, Event::WorkflowCompleted { .. }))
            .count()
    }

    async fn wait_for_history<F>(
        store: &LibSqlStore,
        workflow_id: &aion_core::WorkflowId,
        description: &str,
        predicate: F,
    ) -> Result<Vec<Event>, TestError>
    where
        F: Fn(&[Event]) -> bool,
    {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let history = store.read_history(workflow_id).await.map_err(test_error)?;
            if predicate(&history) {
                return Ok(history);
            }
            if Instant::now() > deadline {
                return Err(test_error(format!(
                    "timed out waiting for {description}: {history:#?}"
                )));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Start the loaded `collect_four` workflow over the REAL HTTP transport.
    async fn start_over_http(router: &axum::Router) -> Result<aion_core::WorkflowId, TestError> {
        let build_request = || -> Result<Request<body::Body>, TestError> {
            Request::builder()
                .uri("/workflows/start")
                .method("POST")
                .header("content-type", "application/json")
                .header("x-aion-subject", "ci")
                .header("x-aion-namespaces", NAMESPACE)
                .body(body::Body::from(
                    serde_json::to_vec(&json!({
                        "namespace": NAMESPACE,
                        "workflow_type": OUTBOX_MODULE,
                        "input": { "fixture": "input" },
                    }))
                    .map_err(test_error)?,
                ))
                .map_err(test_error)
        };
        let response = router
            .clone()
            .oneshot(build_request()?)
            .await
            .map_err(test_error)?;
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .map_err(test_error)?
            .to_vec();
        if status != StatusCode::OK {
            return Err(test_error(format!(
                "workflow start over HTTP must succeed, got {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(test_error)?;
        // The HTTP wire contract (`clean_dtos::StartWorkflowResponse`) serializes
        // `workflow_id` as a plain UUID string, not a nested `{ uuid }` object.
        let workflow_id = body["workflow_id"]
            .as_str()
            .ok_or_else(|| test_error("start response missing workflow id"))?
            .parse::<uuid::Uuid>()
            .map_err(test_error)?;
        Ok(aion_core::WorkflowId::new(workflow_id))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn production_boot_dispatches_executes_and_records_over_liminal() -> Result<(), TestError>
    {
        let dir = tempfile::tempdir().map_err(test_error)?;
        let db_path = dir.path().join("aion.db");
        let package_path = write_package_archive(dir.path())?;
        // The production path binds the CONFIGURED listen address, so commit to a
        // concrete reserved loopback port the worker can also dial.
        let listen_address = reserve_loopback_port()?;

        // (A) Build a real ServerState through the production boot path
        // (ServerState::build over a libSQL ServerConfig): outbox enabled,
        // transport = liminal, the listen address set, collect_four loaded. This
        // shares the libSQL leaf as the dispatcher's outbox store (the real boot
        // store seam) and installs the production ServerOutboxDeliveryCallback over
        // the live engine (gated on outbox.enabled).
        let config = server_config(&db_path, package_path, listen_address);
        let outbox_config = config.outbox.clone();
        let state = ServerState::build(config).await.map_err(test_error)?;

        // (B) Drive the EXACT production commissioning function run_server calls:
        // it hosts the liminal listener, builds RegistryLiminalDispatch over the
        // shared registry + engine callback, and spawns the real OutboxDispatcher.
        // Hold the returned listener guard for the test's lifetime, exactly as
        // run_server holds it.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        // Own-all, generous-default backpressure (single-node e2e): fraction 1 and
        // the platform default, so the ceiling never engages — the claim behaves
        // exactly as before, proving the production path is byte-identical on default.
        let backpressure_settings = BackpressureSettings {
            platform_default: crate::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
            fraction: crate::worker::OwnedShardFraction::own_all(),
        };
        let listener_guard = maybe_spawn_outbox_dispatcher(
            &state,
            &outbox_config,
            false,
            backpressure_settings,
            &shutdown_rx,
        )
        .map_err(test_error)?;

        // (C) A REAL remote worker connects IN to the production listener and
        // self-registers in-band for the fixture's pool.
        let executions = Arc::new(AtomicUsize::new(0));
        let worker = WorkerThread::spawn(
            listen_address.to_string(),
            worker_config()?,
            worker_registry(&executions)?,
        );

        // Wait until the in-band registration landed in the SAME registry the
        // dispatch path selects from (every fan-out activity type is eligible).
        let registry = state.worker_registry().clone();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ready = FAN_ACTIVITY_TYPES.iter().all(|activity_type| {
                registry
                    .select_worker(NAMESPACE, TASK_QUEUE, activity_type, None)
                    .ok()
                    .flatten()
                    .is_some()
            });
            if ready {
                break;
            }
            if Instant::now() > deadline {
                worker.stop();
                return Err(test_error("worker never registered in-band for the pool"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // (D) Start collect_four over the REAL HTTP transport: the engine stages
        // four pending outbox rows; the production-wired dispatcher claims and
        // pushes each to the worker.
        let router = http_router(state.clone()).map_err(test_error)?;
        let workflow_id = start_over_http(&router).await?;

        // (E) THE PROOF: the worker executed all four activities AND every terminal
        // was recorded through the production engine callback (record_fan_out_completion)
        // — four ActivityCompleted + one WorkflowCompleted in durable history. This
        // is the full round-trip the retired stub never achieved.
        let reader = LibSqlStore::open(db_path.clone())
            .await
            .map_err(test_error)?;
        let settled = wait_for_history(&reader, &workflow_id, "fan-out settled", |events| {
            count_completed(events) == FAN_OUT && count_workflow_completed(events) == 1
        })
        .await?;
        assert_eq!(
            count_completed(&settled),
            FAN_OUT,
            "every fan-out member must record a terminal through the production callback"
        );
        assert_eq!(
            count_workflow_completed(&settled),
            1,
            "the workflow must complete exactly once"
        );
        assert_eq!(
            executions.load(Ordering::SeqCst),
            FAN_OUT,
            "the remote worker must have executed every pushed dispatch exactly once"
        );

        // Teardown: stop the dispatcher + worker, drop the listener guard (its Drop
        // stops the accept worker), shut the engine down so durable appends finish.
        shutdown_tx.send(true).ok();
        worker.stop();
        drop(listener_guard);
        state.shutdown().map_err(test_error)?;
        Ok(())
    }
}
