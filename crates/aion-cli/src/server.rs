//! The `aion server` subcommand: argument surface for running the full Aion
//! workflow server in-process via [`aion_server::run`].

use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use aion_server::config::CliOverrides;
use clap::Args;

/// Arguments for `aion server`, identical to the surface of the former
/// standalone `aion-server` binary.
#[derive(Args, Clone, Debug)]
pub struct ServerArgs {
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

impl From<ServerArgs> for CliOverrides {
    fn from(args: ServerArgs) -> Self {
        Self {
            config_path: args.config,
            listen_address: args.listen_address,
            store_url: args.store_url,
            scheduler_threads: args.scheduler_threads.map(NonZeroUsize::get),
            drain_timeout_seconds: args.drain_timeout_seconds.map(NonZeroU64::get),
            workflow_packages: args.workflow_packages,
        }
    }
}
