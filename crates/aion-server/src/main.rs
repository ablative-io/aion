//! Thin binary entry point; `anyhow` is confined to this file.

use std::path::PathBuf;

use aion_server::{ServerConfig, ServerState};
use anyhow::{Context, Result, bail};

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = config_path_from_args()?;
    let config = ServerConfig::load_from_path(config_path)?;
    let _state = ServerState::build(config).await?;

    tokio::signal::ctrl_c()
        .await
        .context("shutdown signal listener failed")?;

    Ok(())
}

fn config_path_from_args() -> Result<PathBuf> {
    let mut args = std::env::args_os();
    drop(args.next());
    let Some(path) = args.next() else {
        bail!("usage: aion-server <config.json>");
    };

    Ok(PathBuf::from(path))
}
