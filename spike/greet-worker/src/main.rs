//! Minimal remote activity worker for the multi-process cluster spike.
//!
//! Connects to an aion server's gRPC endpoint, registers the activity handlers
//! the spike's workflows need, and serves until killed.
//!
//! Activities served:
//!   * `greet` (hello-world, Phase A): input `{"name": String}` -> output
//!     `{"greeting": String}`.
//!   * `publish_document` / `archive_document` (approval-gate, Phase B): input
//!     `{"document_id": String, "reason": String}` -> output
//!     `{"action_taken": String}`. These mirror the Gleam example's
//!     `local_publish_document` / `local_archive_document` bodies and the e2e
//!     test's RecordingDispatcher, so the approval workflow's terminal activity
//!     runs cross-process exactly as in production.
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

/// approval-gate document activity input (`publish_document` / `archive_document`).
#[derive(Deserialize, Serialize)]
struct DocumentInput {
    /// The document under decision.
    document_id: String,
    /// Why this action is being taken (for the recorded action message).
    reason: String,
}

/// approval-gate document activity output.
#[derive(Deserialize, Serialize)]
struct DocumentOutput {
    /// Human-readable description of the action taken.
    action_taken: String,
}

/// The `publish_document` activity handler (approval-gate's approved branch).
/// Mirrors the Gleam `local_publish_document`: `published <document_id>`.
fn publish_document(
    input: DocumentInput,
    _context: &ActivityContext,
) -> HandlerFuture<'_, DocumentOutput> {
    Box::pin(async move {
        let identity = std::env::var("GREET_WORKER_IDENTITY").unwrap_or_else(|_| "?".to_owned());
        tracing::info!(document = %input.document_id, worker = %identity, "serving publish_document activity");
        Ok(DocumentOutput {
            action_taken: format!("published {}", input.document_id),
        })
    })
}

/// The `archive_document` activity handler (approval-gate's rejected/timeout
/// branch). Mirrors the Gleam `local_archive_document`.
fn archive_document(
    input: DocumentInput,
    _context: &ActivityContext,
) -> HandlerFuture<'_, DocumentOutput> {
    Box::pin(async move {
        let identity = std::env::var("GREET_WORKER_IDENTITY").unwrap_or_else(|_| "?".to_owned());
        tracing::info!(document = %input.document_id, worker = %identity, "serving archive_document activity");
        Ok(DocumentOutput {
            action_taken: format!(
                "archived {} because {}",
                input.document_id, input.reason
            ),
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
        .register_activity("publish_document", publish_document)?
        .register_activity("archive_document", archive_document)?
        .build()?
        .run()
        .await?;

    Ok(())
}
