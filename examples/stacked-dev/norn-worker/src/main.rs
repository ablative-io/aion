//! Standalone activity worker for the stacked-dev workflow family.
//!
//! Serves all eleven activity names the three `workflow.toml` entries declare
//! (`provision_workspace`, `warm_build`, `dev`, `scoped_checks`,
//! `dev_resume`, `full_checks`, `request_review`, `land`, `scout`,
//! `dev_review`, `enrich_brief` — `await_verdict` is a signal, not an
//! activity) by shelling to the real CLIs that own each step. The handler
//! bodies live in [`handlers`] and mirror the example's local implementations
//! (`../src/stacked_dev/locals.gleam`) exactly; `warm_build` and `dev` share
//! the tagged `StartupTask`/`StartupResult` envelope because both flow through
//! one homogeneous `workflow.all` fan-out.
//!
//! Usage: `stacked-dev-worker --endpoint http://127.0.0.1:50051`
//! The endpoint is the aion server's `[server] grpc_address` and is the only
//! configuration; everything else the activities need (repo root, workspace
//! paths) arrives in the activity inputs.

use std::time::Duration;

use aion_worker::{ActivityContext, ActivityFailure, HandlerFuture, Worker, WorkerConfig};
use anyhow::{Context, bail};
use stacked_dev_worker_norn::handlers;
use stacked_dev_worker_norn::shell::Shell;
use stacked_dev_worker_norn::types::ReviewRequest;

/// Parsed CLI arguments.
struct Args {
    /// gRPC server endpoint.
    endpoint: String,
    /// Maximum concurrent activity executions.
    concurrency: usize,
    /// Task queue (namespace) to register on.
    task_queue: String,
}

/// Parse CLI flags.
fn parse_args() -> anyhow::Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut endpoint = None;
    let mut concurrency = None;
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
            "--task-queue" => {
                let value = args.next().context("--task-queue requires a value")?;
                task_queue = Some(value);
            }
            other => {
                bail!(
                    "unknown argument `{other}`\nusage: stacked-dev-worker \
                     --endpoint <grpc-url> [--concurrency <n>] [--task-queue <name>]"
                )
            }
        }
    }
    Ok(Args {
        endpoint: endpoint.context(
            "missing required --endpoint <grpc-url> (the server's [server] grpc_address)",
        )?,
        concurrency: concurrency.unwrap_or(4),
        task_queue: task_queue.unwrap_or_else(|| "default".to_owned()),
    })
}

/// Adapt a synchronous, blocking handler body onto the worker SDK's async
/// handler signature. The bodies block on child processes (norn rounds and
/// cargo builds can run for minutes), so each invocation moves to the
/// blocking thread pool instead of stalling the worker's async runtime.
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
const SERVED_ACTIVITIES: [&str; 13] = [
    "provision_workspace",
    "warm_build",
    "dev",
    "scoped_checks",
    "dev_resume",
    "full_checks",
    "request_review",
    "land",
    "scout",
    "dev_review",
    "enrich_brief",
    "assemble_wave",
    "teardown_workspace",
];

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
        concurrency = cli.concurrency,
        activities = ?SERVED_ACTIVITIES,
        "stacked-dev-worker starting; connection failures will be logged \
         with reconnect backoff — a quiet worker is a connected worker"
    );

    // Everything but the endpoint and the reconnect budget mirrors the saga
    // template's worker. The budget is deliberately effectively infinite: a
    // long-lived worker must outwait server restarts (kill -9 the server and
    // bring it back — the worker should still be there), so it probes every
    // 5s for as long as it runs. The published SDK cannot express "unbounded"
    // yet; usize::MAX is the honest spelling of that intent.
    let config = WorkerConfig::builder()
        .endpoint(cli.endpoint)
        .namespace(&cli.task_queue)
        .task_queue(&cli.task_queue)
        .identity("stacked-dev-worker-1")
        .max_concurrency(cli.concurrency)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(usize::MAX)
        .build()?;

    Worker::builder(config)
        .register_activity(
            "provision_workspace",
            blocking(shell.clone(), handlers::provision_workspace),
        )?
        // warm_build and dev BOTH serve the tagged StartupTask envelope; the
        // engine routes each name only its own variant.
        .register_activity(
            "warm_build",
            blocking(shell.clone(), handlers::startup_task),
        )?
        .register_activity("dev", blocking(shell.clone(), handlers::startup_task))?
        .register_activity(
            "scoped_checks",
            blocking(shell.clone(), handlers::scoped_checks),
        )?
        .register_activity("dev_resume", blocking(shell.clone(), handlers::dev_resume))?
        .register_activity(
            "full_checks",
            blocking(shell.clone(), handlers::full_checks),
        )?
        .register_activity(
            "request_review",
            blocking(shell.clone(), |shell, input: ReviewRequest| {
                handlers::request_review(shell, &input)
            }),
        )?
        .register_activity("land", blocking(shell.clone(), handlers::land))?
        .register_activity("scout", blocking(shell.clone(), handlers::scout))?
        .register_activity("dev_review", blocking(shell.clone(), handlers::dev_review))?
        .register_activity(
            "enrich_brief",
            blocking(shell.clone(), handlers::enrich_brief),
        )?
        // assemble_wave is the dispatcher activity (BD-006); it serves the
        // dispatch entry's [["assemble_wave"]] list and reads ledgers itself.
        .register_activity(
            "assemble_wave",
            blocking(shell.clone(), handlers::assemble_wave),
        )?
        .register_activity(
            "teardown_workspace",
            blocking(shell, handlers::teardown_workspace),
        )?
        .build()?
        .run()
        .await?;

    Ok(())
}
