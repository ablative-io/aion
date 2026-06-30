//! The `aion server` subcommand: argument surface for running the full Aion
//! workflow server in-process via [`aion_server::run`].

use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use aion_server::config::{CliOverrides, ServerConfig};
use clap::Args;

/// Arguments for `aion server`, identical to the surface of the former
/// standalone `aion-server` binary.
#[derive(Args, Clone, Debug)]
pub struct ServerArgs {
    /// Path to a TOML server configuration file. Optional when using local defaults.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Override the HTTP/JSON and ops-console listener address.
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
    /// Path to the external `gleam` binary that commissions the server-side
    /// authoring loop. Setting it mounts the `/authoring/*` endpoints (and
    /// requires `--authoring-project-root`); absent, the server compiles no
    /// Gleam and deploys pre-built `.aion` files only.
    #[arg(long = "gleam-path")]
    gleam_path: Option<PathBuf>,
    /// Built Gleam workflow project root that submitted authoring source is
    /// written into and packaged from. Required when `--gleam-path` is set.
    #[arg(long = "authoring-project-root")]
    authoring_project_root: Option<PathBuf>,
    /// Open the ops console in the default browser once the HTTP listener is up.
    /// Resolves the served URL from the effective config (the same
    /// `--listen-address`/config/default precedence the server uses).
    #[arg(long)]
    open: bool,
}

impl ServerArgs {
    /// Whether `--open` was passed.
    #[must_use]
    pub fn open(&self) -> bool {
        self.open
    }
}

/// Resolve the served HTTP URL from the effective config and, once the listener
/// accepts a connection, open it in the default browser. Best-effort: any
/// failure (config load, port never comes up, no opener) is logged and dropped
/// so it never affects the server run.
pub fn spawn_browser_open(overrides: &CliOverrides) {
    let address = match ServerConfig::load(overrides) {
        Ok(config) => {
            let (_store, runtime) = config.into_parts();
            runtime.listen.http
        }
        Err(error) => {
            eprintln!("aion server --open: could not resolve listen address: {error}");
            return;
        }
    };
    tokio::spawn(async move {
        if wait_for_listener(address).await {
            open_browser(&served_url(address));
        } else {
            eprintln!(
                "aion server --open: listener at {address} did not come up; not opening browser"
            );
        }
    });
}

/// Poll the address until a TCP connection succeeds, up to a short budget.
async fn wait_for_listener(address: SocketAddr) -> bool {
    const ATTEMPTS: u32 = 100;
    const INTERVAL: Duration = Duration::from_millis(100);
    for _ in 0..ATTEMPTS {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return true;
        }
        tokio::time::sleep(INTERVAL).await;
    }
    false
}

/// The browsable URL. A wildcard bind (`0.0.0.0`/`::`) is not browsable as-is, so
/// rewrite it to loopback.
fn served_url(address: SocketAddr) -> String {
    let host = if address.ip().is_unspecified() {
        if address.is_ipv6() {
            "[::1]".to_owned()
        } else {
            "127.0.0.1".to_owned()
        }
    } else if address.is_ipv6() {
        format!("[{}]", address.ip())
    } else {
        address.ip().to_string()
    };
    format!("http://{host}:{}/", address.port())
}

/// Launch the platform browser-opener. Best-effort; errors are reported, not propagated.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";

    match std::process::Command::new(program).arg(url).spawn() {
        Ok(_) => eprintln!("aion server --open: opening {url}"),
        Err(error) => eprintln!("aion server --open: could not open {url}: {error}"),
    }
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
            gleam_path: args.gleam_path,
            authoring_project_root: args.authoring_project_root,
        }
    }
}
