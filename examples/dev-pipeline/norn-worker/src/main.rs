//! Standalone activity worker for the brief-forge workflow (dev-pipeline
//! slice 1).
//!
//! Serves the three activity names the `brief_forge` entry of
//! `workflow.toml` declares (`scout`, `design`, `refute`) by shelling to the
//! `norn` CLI; the handler bodies live in [`handlers`]. All three activities
//! dispatch on the `agents` task queue (the Gleam side pins
//! `activity.task_queue("agents")`), so this worker's `--task-queue` default
//! is `agents` — NOT `default` — to match; the namespace stays an explicit
//! choice.
//!
//! Usage: `dev-pipeline-worker-norn --endpoint http://127.0.0.1:50051 \
//!         --namespace dev-pipeline`
//! The endpoint is the aion server's `[server] grpc_address`; everything
//! else the activities need (repo root, session ids, prompts) arrives in the
//! activity inputs.

use std::time::Duration;

use aion_worker::{ActivityContext, ActivityFailure, HandlerFuture, Worker, WorkerConfig};
use anyhow::{Context, bail};
use dev_pipeline_worker_norn::handlers;
use dev_pipeline_worker_norn::shell::Shell;

/// Parsed CLI arguments.
struct Args {
    /// gRPC server endpoint.
    endpoint: String,
    /// Maximum concurrent activity executions.
    concurrency: usize,
    /// Namespace (correctness/isolation boundary) to register into.
    namespace: String,
    /// Task queue (pool/flavour selector within the namespace) to serve.
    task_queue: String,
}

/// Parse CLI flags.
fn parse_args() -> anyhow::Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut endpoint = None;
    let mut concurrency = None;
    let mut namespace = None;
    let mut task_queue = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--endpoint" => {
                let value = args
                    .next()
                    .context("--endpoint requires a value, e.g. http://127.0.0.1:50051")?;
                endpoint = Some(value);
            }
            "--concurrency" => {
                let value = args.next().context("--concurrency requires a number")?;
                concurrency = Some(
                    value
                        .parse::<usize>()
                        .context("--concurrency must be a positive integer")?,
                );
            }
            "--namespace" => {
                let value = args.next().context("--namespace requires a value")?;
                namespace = Some(value);
            }
            "--task-queue" => {
                let value = args.next().context("--task-queue requires a value")?;
                task_queue = Some(value);
            }
            other => {
                bail!(
                    "unknown argument `{other}`\nusage: dev-pipeline-worker-norn \
                     --endpoint <grpc-url> [--concurrency <n>] [--namespace <name>] \
                     [--task-queue <name>]"
                )
            }
        }
    }
    Ok(Args {
        endpoint: endpoint.context(
            "missing required --endpoint <grpc-url> (the server's [server] grpc_address)",
        )?,
        concurrency: concurrency.unwrap_or(4),
        namespace: namespace.unwrap_or_else(|| "default".to_owned()),
        // The Gleam activities pin task_queue("agents"); a worker left on
        // "default" would silently serve nothing.
        task_queue: task_queue.unwrap_or_else(|| "agents".to_owned()),
    })
}

/// Adapt a synchronous, blocking handler body onto the worker SDK's async
/// handler signature. The bodies block on norn child processes that can run
/// for minutes, so each invocation moves to the blocking thread pool instead
/// of stalling the worker's async runtime.
fn blocking<Input, Output>(
    shell: Shell,
    body: fn(&Shell, Input) -> Result<Output, ActivityFailure>,
) -> impl for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
+ Send
+ Sync
+ 'static
where
    Input: Send + 'static,
    Output: Send + 'static,
{
    move |input: Input, _context: &ActivityContext| {
        let shell = shell.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || body(&shell, input))
                .await
                .map_err(|join_error| {
                    ActivityFailure::terminal(format!(
                        "activity handler task did not complete: {join_error}"
                    ))
                })?
        })
    }
}

/// Every activity name this worker serves, in registration order.
const SERVED_ACTIVITIES: [&str; 3] = ["scout", "design", "refute"];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Surface the worker SDK's own tracing (task receipt at info, session
    // drops and reconnect backoff at warn) — without a subscriber the worker
    // is silent even while serving. Default to info; RUST_LOG overrides.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = parse_args()?;
    let shell = Shell::inherited();

    tracing::info!(
        endpoint = %cli.endpoint,
        namespace = %cli.namespace,
        task_queue = %cli.task_queue,
        concurrency = cli.concurrency,
        activities = ?SERVED_ACTIVITIES,
        "dev-pipeline-worker starting; connection failures will be logged \
         with reconnect backoff — a quiet worker is a connected worker"
    );

    // The reconnect budget is deliberately effectively infinite: a
    // long-lived worker must outwait server restarts, so it probes every 5s
    // for as long as it runs. The published SDK cannot express "unbounded"
    // yet; usize::MAX is the honest spelling of that intent (stacked-dev
    // convention).
    let config = WorkerConfig::builder()
        .endpoint(cli.endpoint)
        .namespace(&cli.namespace)
        .task_queue(&cli.task_queue)
        .identity("dev-pipeline-worker-1")
        .max_concurrency(cli.concurrency)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(usize::MAX)
        .build()?;

    Worker::builder(config)
        .register_activity("scout", blocking(shell.clone(), handlers::scout))?
        .register_activity("design", blocking(shell.clone(), handlers::design))?
        .register_activity("refute", blocking(shell, handlers::refute))?
        .build()?
        .run()
        .await?;

    Ok(())
}
