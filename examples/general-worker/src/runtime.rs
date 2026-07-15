//! Binary runtime: two liminal connections across one task queue.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use aion_worker::{RedialTiming, serve_with_redial};
use anyhow::{Context, Result, anyhow};
use tracing_subscriber::EnvFilter;

use crate::args::{Args, parse_args};
use crate::composition::{
    AGENT_NODE, REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF, SHELL_NODE, TASK_QUEUE, agent_config,
    agent_registry, build_worker_config, shell_registry,
};
use crate::shell::Shell;

/// Parse configuration and serve the agent and shell nodes until termination.
///
/// # Errors
///
/// Returns an error for invalid logging or command-line configuration, thread
/// creation failure, worker configuration failure, transport termination, or an
/// agent thread failure observed after the shell serve loop returns.
pub fn run() -> Result<()> {
    initialize_tracing()?;
    let args = parse_args()?;
    tracing::info!(
        addresses = ?args.addresses,
        norn_bin = %args.norn_bin,
        task_queue = TASK_QUEUE,
        "general-worker starting with agent and shell connections"
    );

    let agent_addresses = args.addresses.clone();
    let agent_norn_bin = args.norn_bin.clone();
    let agent_identity = format!("{}-agent", args.identity);
    let agent_thread = thread::Builder::new()
        .name("general-worker-agent".to_owned())
        .spawn(move || serve_agent(&agent_addresses, &agent_identity, &agent_norn_bin))
        .context("failed to spawn the general-worker agent thread")?;

    serve_shell(&args)?;

    match agent_thread.join() {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "general-worker agent thread terminated unexpectedly"
        )),
    }
}

fn initialize_tracing() -> Result<()> {
    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(value).context("RUST_LOG is invalid")?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new("info"),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(anyhow!("RUST_LOG must contain valid Unicode"));
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|source| anyhow!("failed to initialize tracing: {source}"))
}

fn serve_agent(addresses: &[String], identity: &str, norn_bin: &str) -> Result<()> {
    let config = build_worker_config(identity, AGENT_NODE)?;
    let registry = Arc::new(agent_registry());
    let harness = agent_config(norn_bin);
    let stop = AtomicBool::new(false);
    serve_with_redial(
        addresses.to_vec(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        Some(&harness),
        || tracing::info!("agent connection registered; serving run_agent"),
    )
    .context("agent connection ended")
}

fn serve_shell(args: &Args) -> Result<()> {
    let identity = format!("{}-shell", args.identity);
    let config = build_worker_config(&identity, SHELL_NODE)?;
    let registry = Arc::new(shell_registry(Shell::inherited())?);
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    serve_with_redial(
        args.addresses.clone(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        None,
        move || {
            if let Some(path) = ready_file.as_deref() {
                write_readiness(path);
            }
            tracing::info!("shell connection registered; serving run_command and parse_output");
        },
    )
    .context("shell connection ended")
}

fn write_readiness(path: &Path) {
    if let Err(source) = std::fs::write(path, b"connected") {
        tracing::error!(path = %path.display(), %source, "failed to write readiness file");
    }
}
