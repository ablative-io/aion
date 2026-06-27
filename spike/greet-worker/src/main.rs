//! Minimal remote activity worker serving the hello-world `greet` activity.
//!
//! Connects to an aion server's gRPC endpoint, registers the single `greet`
//! handler, and serves until killed. Mirrors the hello-world worker.py and the
//! ss5b test's GreetDispatcher: input `{"name": String}` -> output
//! `{"greeting": String}`.
//!
//! Usage: `greet-worker --endpoint http://127.0.0.1:50051 [--identity <id>]`
//!
//! The reconnect budget is effectively unbounded so the worker outwaits a
//! server `kill -9` and reconnects to the survivor that adopts the shard.

use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

/// `greet` activity input.
#[derive(Deserialize, Serialize)]
struct GreetInput {
    /// Who to greet.
    name: String,
}

/// `greet` activity output.
#[derive(Deserialize, Serialize)]
struct GreetOutput {
    /// The composed greeting.
    greeting: String,
}

/// The `greet` activity handler.
fn greet(input: GreetInput, _context: &ActivityContext) -> HandlerFuture<'_, GreetOutput> {
    Box::pin(async move {
        let identity = std::env::var("GREET_WORKER_IDENTITY").unwrap_or_else(|_| "?".to_owned());
        tracing::info!(name = %input.name, worker = %identity, "serving greet activity");
        Ok(GreetOutput {
            greeting: format!("Hello, {}! Welcome to Aion.", input.name),
        })
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut endpoint = "http://127.0.0.1:50051".to_owned();
    let mut identity = "greet-worker-1".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => {
                if let Some(value) = args.next() {
                    endpoint = value;
                }
            }
            "--identity" => {
                if let Some(value) = args.next() {
                    identity = value;
                }
            }
            other => {
                anyhow::bail!("unknown argument `{other}`");
            }
        }
    }

    // The handler reads its own identity label from the environment for logging.
    // SAFETY: set once before any worker thread spawns.
    unsafe {
        std::env::set_var("GREET_WORKER_IDENTITY", &identity);
    }

    tracing::info!(%endpoint, %identity, "greet-worker starting");

    let config = WorkerConfig::builder()
        .endpoint(endpoint)
        .namespace("default")
        .task_queue("default")
        .identity(&identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(2))
        .reconnect_max_attempts(usize::MAX)
        .build()?;

    Worker::builder(config)
        .register_activity("greet", greet)?
        .build()?
        .run()
        .await?;

    Ok(())
}
