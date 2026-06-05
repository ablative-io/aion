//! Thin binary entry point; `anyhow` is confined to this file.

use std::{net::SocketAddr, path::PathBuf};

use aion_server::{ServerConfig, ServerError, ServerState, api};
use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = config_path_from_args()?;
    let config = ServerConfig::load_from_path(config_path)?;
    let state = ServerState::build(config).await?;
    reject_tls_until_supported(&state)?;

    let grpc_address = state.runtime_config().listen.grpc;
    let http_address = state.runtime_config().listen.http;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let grpc = serve_grpc(state.clone(), grpc_address, shutdown_rx.clone());
    let http = serve_http(state.clone(), http_address, shutdown_rx);

    tokio::select! {
        result = grpc => result.context("gRPC transport stopped")?,
        result = http => result.context("HTTP transport stopped")?,
        result = shutdown_signal() => {
            result?;
            let _receiver_count = shutdown_tx.send(true);
        },
    }

    state.shutdown()?;
    Ok(())
}

async fn serve_grpc(
    state: ServerState,
    address: SocketAddr,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let service = api::grpc::workflow_service(state);
    TonicServer::builder()
        .add_service(service)
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
    axum::serve(listener, api::http::workflow_router(state))
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
    tokio::signal::ctrl_c()
        .await
        .context("shutdown signal listener failed")
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

fn config_path_from_args() -> Result<PathBuf> {
    let mut args = std::env::args_os();
    drop(args.next());
    let Some(path) = args.next() else {
        bail!("usage: aion-server <config.json>");
    };

    Ok(PathBuf::from(path))
}
