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
use tracing::{error, info};

use crate::{
    ServerConfig, ServerError, ServerState, api,
    config::{CliOverrides, NamespaceMode, StoreBackend},
    observability,
    shutdown::{self, ShutdownOutcome},
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
        "aion-server startup banner"
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
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
