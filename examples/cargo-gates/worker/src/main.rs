//! Composition root for the cargo-gates activity worker.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_worker::{
    ActivityContext, ActivityFailure, ActivityRegistry, HandlerFuture, RedialTiming, WorkerConfig,
    WorkerConfigBuildError,
};
use cargo_gates_worker::activities;

const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";
const TASK_QUEUE: &str = "cargo_gates";
const NODE: &str = "shell";
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

struct Args {
    candidates: Vec<String>,
    identity: String,
    ready_file: Option<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    let config = worker_config(&args.identity)?;
    let registry = Arc::new(registry()?);
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    tracing::info!(
        task_queue = TASK_QUEUE,
        node = NODE,
        "cargo-gates worker starting"
    );
    aion_worker::serve_with_redial(
        args.candidates,
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        None,
        move || {
            if let Some(path) = ready_file.as_ref()
                && let Err(error) = std::fs::write(path, b"connected")
            {
                tracing::error!(%error, "failed to write worker readiness file");
            }
            tracing::info!("connection registered; serving all four Cargo gates");
        },
    )?;
    Ok(())
}

fn registry() -> Result<ActivityRegistry, aion_worker::WorkerError> {
    ActivityRegistry::new()
        .register_activity("run_check", blocking(activities::run_check))?
        .register_activity("run_clippy", blocking(activities::run_clippy))?
        .register_activity("run_tests", blocking(activities::run_tests))?
        .register_activity("run_fmt_check", blocking(activities::run_fmt_check))
}

fn worker_config(identity: &str) -> Result<WorkerConfig, WorkerConfigBuildError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue(TASK_QUEUE)
        .node(NODE)
        .identity(identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(usize::MAX)
        .build()
}

fn blocking<Input, Output>(
    body: fn(Input) -> Result<Output, ActivityFailure>,
) -> impl for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
+ Send
+ Sync
+ 'static
where
    Input: Send + 'static,
    Output: Send + 'static,
{
    move |input: Input, context: &ActivityContext| {
        drop(context.cancelled());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || body(input))
                .await
                .map_err(|error| {
                    ActivityFailure::terminal(format!(
                        "activity handler task did not complete: {error}"
                    ))
                })?
        })
    }
}

fn parse_args() -> anyhow::Result<Args> {
    let mut candidates = Vec::new();
    let mut identity = "cargo-gates-worker".to_owned();
    let mut ready_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => candidates.push(next_value(&mut args, "--address")?),
            "--identity" => identity = next_value(&mut args, "--identity")?,
            "--ready-file" => ready_file = Some(next_value(&mut args, "--ready-file")?),
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push(DEFAULT_ADDRESS.to_owned());
    }
    Ok(Args {
        candidates,
        identity,
        ready_file,
    })
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}
