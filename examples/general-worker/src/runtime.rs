//! Binary runtime: two liminal connections across one task queue.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

enum AgentCompletion {
    Finished(Result<()>),
    UnexpectedTermination,
}

/// Parse configuration and serve the agent and shell nodes until termination.
///
/// # Errors
///
/// Returns an error for invalid logging or command-line configuration, thread
/// creation failure, worker configuration failure, transport termination, or an
/// unexpected agent thread termination. When both connection loops fail, the
/// returned diagnostic includes both errors.
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
    coordinate_connections(
        move |stop| serve_agent(&agent_addresses, &agent_identity, &agent_norn_bin, stop),
        |stop| serve_shell(&args, stop),
    )
}

fn coordinate_connections<AgentServe, ShellServe>(
    agent_serve: AgentServe,
    shell_serve: ShellServe,
) -> Result<()>
where
    AgentServe: FnOnce(&AtomicBool) -> Result<()> + Send + 'static,
    ShellServe: FnOnce(&AtomicBool) -> Result<()>,
{
    let stop = Arc::new(AtomicBool::new(false));
    let agent_stop = Arc::clone(&stop);
    let agent_thread = thread::Builder::new()
        .name("general-worker-agent".to_owned())
        .spawn(move || run_agent_connection(agent_stop.as_ref(), agent_serve))
        .context("failed to spawn the general-worker agent thread")?;

    let shell_result = shell_serve(stop.as_ref());
    stop.store(true, Ordering::SeqCst);
    let agent_result = match agent_thread.join() {
        Ok(result) => result,
        Err(_) => AgentCompletion::UnexpectedTermination,
    };
    combine_connection_results(shell_result, agent_result)
}

fn run_agent_connection<AgentServe>(stop: &AtomicBool, agent_serve: AgentServe) -> AgentCompletion
where
    AgentServe: FnOnce(&AtomicBool) -> Result<()>,
{
    match catch_unwind(AssertUnwindSafe(|| agent_serve(stop))) {
        Ok(Ok(())) if stop.swap(true, Ordering::SeqCst) => AgentCompletion::Finished(Ok(())),
        Ok(Ok(())) => {
            tracing::error!("agent connection terminated unexpectedly without a stop request");
            AgentCompletion::UnexpectedTermination
        }
        Ok(Err(source)) => {
            stop.store(true, Ordering::SeqCst);
            tracing::error!(%source, "agent connection terminated with an error");
            AgentCompletion::Finished(Err(source))
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            tracing::error!("general-worker agent thread terminated unexpectedly");
            AgentCompletion::UnexpectedTermination
        }
    }
}

fn combine_connection_results(
    shell_result: Result<()>,
    agent_result: AgentCompletion,
) -> Result<()> {
    match (shell_result, agent_result) {
        (Ok(()), AgentCompletion::Finished(agent_result)) => agent_result,
        (Err(shell_error), AgentCompletion::Finished(Ok(()))) => Err(shell_error),
        (Err(shell_error), AgentCompletion::Finished(Err(agent_error))) => Err(anyhow!(
            "shell connection failed: {shell_error:#}; agent connection failed: {agent_error:#}"
        )),
        (Ok(()), AgentCompletion::UnexpectedTermination) => Err(anyhow!(
            "general-worker agent thread terminated unexpectedly"
        )),
        (Err(shell_error), AgentCompletion::UnexpectedTermination) => Err(anyhow!(
            "shell connection failed: {shell_error:#}; general-worker agent thread terminated unexpectedly"
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

fn serve_agent(
    addresses: &[String],
    identity: &str,
    norn_bin: &str,
    stop: &AtomicBool,
) -> Result<()> {
    let config = build_worker_config(identity, AGENT_NODE)?;
    let registry = Arc::new(agent_registry());
    let harness = agent_config(norn_bin);
    serve_with_redial(
        addresses.to_vec(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        stop,
        Some(&harness),
        || tracing::info!("agent connection registered; serving run_agent"),
    )
    .context("agent connection ended")
}

fn serve_shell(args: &Args, stop: &AtomicBool) -> Result<()> {
    let identity = format!("{}-shell", args.identity);
    let config = build_worker_config(&identity, SHELL_NODE)?;
    let registry = Arc::new(shell_registry(Shell::inherited())?);
    let ready_file = args.ready_file.clone();
    serve_with_redial(
        args.addresses.clone(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        stop,
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

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn agent_terminal_error_sets_shared_stop_immediately() -> TestResult {
        let stop = AtomicBool::new(false);
        let completion = run_agent_connection(&stop, |_| Err(anyhow!("agent terminal failure")));

        assert!(stop.load(Ordering::SeqCst));
        let AgentCompletion::Finished(result) = completion else {
            return Err("agent error was classified as an unexpected termination".into());
        };
        let error = result.err().ok_or("agent failure must be preserved")?;
        assert_eq!(error.to_string(), "agent terminal failure");
        Ok(())
    }

    #[test]
    fn shell_failure_joins_agent_and_preserves_both_diagnostics() -> TestResult {
        let agent_finished = Arc::new(AtomicBool::new(false));
        let agent_finished_in_thread = Arc::clone(&agent_finished);
        let result = coordinate_connections(
            move |stop| {
                while !stop.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                agent_finished_in_thread.store(true, Ordering::SeqCst);
                Err(anyhow!("agent failure after shell stop"))
            },
            |_| Err(anyhow!("shell terminal failure")),
        );

        assert!(agent_finished.load(Ordering::SeqCst));
        let error = result.err().ok_or("both connection failures must fail")?;
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("shell terminal failure"));
        assert!(diagnostic.contains("agent failure after shell stop"));
        Ok(())
    }

    #[test]
    fn unexpected_agent_return_stops_shell_and_is_reported() -> TestResult {
        let result = coordinate_connections(
            |_| Ok(()),
            |stop| {
                while !stop.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                Ok(())
            },
        );

        let error = result
            .err()
            .ok_or("an unsolicited agent return must fail the runtime")?;
        assert!(error.to_string().contains("terminated unexpectedly"));
        Ok(())
    }
}
