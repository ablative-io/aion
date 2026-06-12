//! Thin binary entry point; `anyhow` is confined to this file.

use std::{
    net::SocketAddr,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    process::ExitCode,
};

use aion_server::{
    ServerConfig, ServerError, ServerState, api,
    config::{CliOverrides, NamespaceMode, StoreBackend},
    observability,
    shutdown::{self, ShutdownOutcome},
};
use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing::{error, info};

/// Aion workflow server.
#[derive(Debug, Parser)]
#[command(name = "aion-server", version, about = "Run the Aion workflow server")]
struct Cli {
    /// Path to a TOML server configuration file. Optional when using local defaults.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Override the HTTP/JSON and dashboard listener address.
    #[arg(long)]
    listen_address: Option<SocketAddr>,
    /// Override the event-store URL and select the libSQL backend when the default is memory.
    #[arg(long)]
    store_url: Option<String>,
    /// Number of engine scheduler worker threads.
    #[arg(long)]
    scheduler_threads: Option<NonZeroUsize>,
    /// Maximum graceful drain duration in seconds.
    #[arg(long = "drain-timeout")]
    drain_timeout_seconds: Option<NonZeroU64>,
    /// Workflow package archive to load at startup. Repeat to load multiple packages.
    #[arg(long = "workflow-package")]
    workflow_packages: Vec<PathBuf>,
}

impl From<Cli> for CliOverrides {
    fn from(cli: Cli) -> Self {
        Self {
            config_path: cli.config,
            listen_address: cli.listen_address,
            store_url: cli.store_url,
            scheduler_threads: cli.scheduler_threads.map(NonZeroUsize::get),
            drain_timeout_seconds: cli.drain_timeout_seconds.map(NonZeroU64::get),
            workflow_packages: cli.workflow_packages,
        }
    }
}

fn main() -> ExitCode {
    match run_main() {
        Ok(code) => code,
        Err(error) => {
            error!(%error, "aion-server failed");
            if is_config_error(&error) {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn run_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    runtime.block_on(run(cli.into()))
}

async fn run(cli: CliOverrides) -> Result<ExitCode> {
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
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    result
        .with_context(|| format!("{transport} transport task failed"))?
        .with_context(|| format!("{transport} transport stopped"))
}

async fn serve_grpc(
    state: ServerState,
    address: SocketAddr,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
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
) -> Result<()> {
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

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).context("SIGTERM listener failed")?;
        let mut interrupt = signal(SignalKind::interrupt()).context("SIGINT listener failed")?;
        tokio::select! {
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("shutdown signal listener failed")
    }
}

fn reject_auth_without_feature(config: &ServerConfig) -> Result<()> {
    if cfg!(not(feature = "auth")) && config.auth.enabled {
        return Err(ServerError::Config {
            message: "auth.enabled=true but binary compiled without auth feature".to_owned(),
        }
        .into());
    }
    Ok(())
}

fn reject_tls_until_supported(state: &ServerState) -> Result<()> {
    if state.runtime_config().tls.is_some() {
        return Err(ServerError::Config {
            message: "configured TLS material cannot be served until transport TLS is wired"
                .to_owned(),
        }
        .into());
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

fn is_config_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ServerError>()
            .is_some_and(ServerError::is_config)
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::{Cli, CliOverrides};

    #[test]
    fn workflow_package_flag_is_repeatable() -> Result<(), Box<dyn std::error::Error>> {
        let overrides = CliOverrides::from(Cli::try_parse_from([
            "aion-server",
            "--workflow-package",
            "examples/hello-world/hello-world.aion",
            "--workflow-package",
            "local.aion",
        ])?);

        assert_eq!(
            overrides.workflow_packages,
            vec![
                PathBuf::from("examples/hello-world/hello-world.aion"),
                PathBuf::from("local.aion"),
            ]
        );
        Ok(())
    }

    #[test]
    fn help_is_handled_by_clap() -> Result<(), Box<dyn std::error::Error>> {
        let error = Cli::try_parse_from(["aion-server", "--help"])
            .err()
            .ok_or("help should exit early")?;

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("Run the Aion workflow server"));
        assert!(help.contains("--workflow-package"));
        assert!(!help.contains("{\"timestamp\""));
        assert!(!help.contains("ERROR"));
        Ok(())
    }

    #[test]
    fn cli_converts_all_overrides() -> Result<(), Box<dyn std::error::Error>> {
        let overrides = CliOverrides::from(Cli::try_parse_from([
            "aion-server",
            "--config",
            "dev-config.toml",
            "--listen-address",
            "127.0.0.1:18080",
            "--store-url",
            "aion.db",
            "--scheduler-threads",
            "2",
            "--drain-timeout",
            "45",
        ])?);

        assert_eq!(
            overrides.config_path,
            Some(PathBuf::from("dev-config.toml"))
        );
        assert_eq!(
            overrides.listen_address.map(|address| address.port()),
            Some(18080)
        );
        assert_eq!(overrides.store_url.as_deref(), Some("aion.db"));
        assert_eq!(overrides.scheduler_threads, Some(2));
        assert_eq!(overrides.drain_timeout_seconds, Some(45));
        Ok(())
    }
}
