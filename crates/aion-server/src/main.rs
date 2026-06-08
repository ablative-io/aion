//! Thin binary entry point; `anyhow` is confined to this file.

use std::{ffi::OsString, net::SocketAddr, path::PathBuf, process::ExitCode};

use aion_server::{
    ServerConfig, ServerError, ServerState, api,
    config::CliOverrides,
    observability,
    shutdown::{self, ShutdownOutcome},
};
use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing::{error, info};

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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    runtime.block_on(run())
}

async fn run() -> Result<ExitCode> {
    observability::tracing::init()?;

    let cli = parse_cli(std::env::args_os().skip(1))?;
    let config = ServerConfig::load(&cli)?;
    reject_auth_without_feature(&config)?;
    let state = ServerState::build(config).await?;
    reject_tls_until_supported(&state)?;

    let grpc_address = state.runtime_config().listen.grpc;
    let http_address = state.runtime_config().listen.http;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        grpc_address = %grpc_address,
        http_address = %http_address,
        "aion-server starting"
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
    let worker = api::worker_grpc::worker_service(state);
    TonicServer::builder()
        .add_service(workflow)
        .add_service(worker)
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

fn parse_cli(args: impl IntoIterator<Item = OsString>) -> Result<CliOverrides, ServerError> {
    let mut overrides = CliOverrides::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let flag = arg.to_string_lossy();
        match flag.as_ref() {
            "--config" => {
                overrides.config_path =
                    Some(PathBuf::from(required_value("--config", args.next())?));
            }
            "--listen-address" => {
                let value = required_value("--listen-address", args.next())?;
                overrides.listen_address = Some(parse_socket_addr("--listen-address", &value)?);
            }
            "--store-url" => overrides.store_url = Some(required_utf8("--store-url", args.next())?),
            "--scheduler-threads" => {
                let value = required_utf8("--scheduler-threads", args.next())?;
                overrides.scheduler_threads =
                    Some(parse_positive_usize("--scheduler-threads", &value)?);
            }
            "--drain-timeout" => {
                let value = required_utf8("--drain-timeout", args.next())?;
                overrides.drain_timeout_seconds =
                    Some(parse_positive_u64("--drain-timeout", &value)?);
            }
            "--help" | "-h" => {
                return Err(ServerError::Config { message: usage() });
            }
            unknown => {
                return Err(ServerError::Config {
                    message: format!("unknown argument `{unknown}`\n{}", usage()),
                });
            }
        }
    }
    Ok(overrides)
}

fn required_value(flag: &str, value: Option<OsString>) -> Result<OsString, ServerError> {
    value.ok_or_else(|| ServerError::Config {
        message: format!("{flag} requires a value"),
    })
}

fn required_utf8(flag: &str, value: Option<OsString>) -> Result<String, ServerError> {
    let value = required_value(flag, value)?;
    value.into_string().map_err(|_| ServerError::Config {
        message: format!("{flag} must be valid UTF-8"),
    })
}

fn parse_socket_addr(flag: &str, value: &OsString) -> Result<SocketAddr, ServerError> {
    let value = value.to_str().ok_or_else(|| ServerError::Config {
        message: format!("{flag} must be valid UTF-8"),
    })?;
    value.parse().map_err(|source| ServerError::Config {
        message: format!("{flag} must be a socket address: {source}"),
    })
}

fn parse_positive_usize(flag: &str, value: &str) -> Result<usize, ServerError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|source| ServerError::Config {
            message: format!("{flag} must be a positive integer: {source}"),
        })?;
    if parsed == 0 {
        return Err(ServerError::Config {
            message: format!("{flag} must be a positive integer"),
        });
    }
    Ok(parsed)
}

fn parse_positive_u64(flag: &str, value: &str) -> Result<u64, ServerError> {
    let parsed = value.parse::<u64>().map_err(|source| ServerError::Config {
        message: format!("{flag} must be a positive integer: {source}"),
    })?;
    if parsed == 0 {
        return Err(ServerError::Config {
            message: format!("{flag} must be a positive integer"),
        });
    }
    Ok(parsed)
}

fn is_config_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ServerError>()
            .is_some_and(ServerError::is_config)
    })
}

fn usage() -> String {
    "usage: aion-server [--config PATH] [--listen-address HOST:PORT] [--store-url URL] [--scheduler-threads N] [--drain-timeout SECONDS]".to_owned()
}
