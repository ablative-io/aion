//! Rust activity worker for the prospekt -> Aion incident-triage bridge demo.
//!
//! This is the smallest remote-worker shape: build a [`WorkerConfig`], register
//! one typed handler for the `triage` activity, and `run()` on the default gRPC
//! poll transport. No AI, no network beyond the engine, no liminal push path.
//!
//! The `triage` handler receives the typed incident (the same effective
//! prospekt document the workflow decoded and re-encoded across the activity
//! boundary), maps its severity to a next action with plain string logic, and
//! returns a structured [`TriageSummary`]. The workflow decodes that summary
//! with its output codec and completes returning it.
//!
//! Config is env-overridable for parity with the other examples but defaults to
//! the repo-root `dev-config.toml` server on `127.0.0.1:50051`, task queue and
//! namespace `default`:
//!
//! | Variable                  | Default                     |
//! |---------------------------|-----------------------------|
//! | `AION_WORKER_ENDPOINT`    | `http://127.0.0.1:50051`    |
//! | `AION_TASK_QUEUE`         | `default`                   |
//! | `AION_WORKER_IDENTITY`    | `incident-triage-worker`    |
//! | `AION_WORKER_CONCURRENCY` | `4`                         |

use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

/// Where the incident happened. Mirrors the incident schema's `environment`
/// object; `model` (the LLM in play, if any) is optional and defaults absent.
#[derive(Debug, Deserialize, Serialize)]
struct Environment {
    binary: String,
    #[serde(default)]
    model: Option<String>,
    invocation: String,
}

/// The typed incident the workflow sends across the activity boundary. Extra
/// prospekt-injected fields (`model`, `model_version`, `forensics`) are ignored
/// by serde since they are not modelled here.
#[derive(Debug, Deserialize, Serialize)]
struct Incident {
    id: String,
    title: String,
    severity: String,
    observed: String,
    expected: String,
    environment: Environment,
    state: String,
}

/// The structured triage result returned to the workflow.
#[derive(Debug, Serialize)]
struct TriageSummary {
    incident_id: String,
    severity: String,
    headline: String,
    next_action: String,
}

/// Severity -> next action. Plain, deterministic string logic; identical to the
/// workflow's in-VM fallback so remote and local paths agree.
fn next_action_for(severity: &str) -> &'static str {
    match severity {
        "sev1" => "page on-call and open a war room now",
        "sev2" => "assign an owner and fix within the working day",
        "sev3" => "triage into the backlog for the next sprint",
        _ => "clarify severity before routing",
    }
}

/// The `triage` activity handler.
fn triage(incident: Incident, _context: &ActivityContext) -> HandlerFuture<'_, TriageSummary> {
    Box::pin(async move {
        let next_action = next_action_for(&incident.severity).to_owned();
        let headline = format!("[{}] {}", incident.severity, incident.title);
        tracing::info!(incident = %incident.id, severity = %incident.severity, "serving triage");
        Ok(TriageSummary {
            incident_id: incident.id,
            severity: incident.severity,
            headline,
            next_action,
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let endpoint = std::env::var("AION_WORKER_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:50051".to_owned());
    let task_queue = std::env::var("AION_TASK_QUEUE").unwrap_or_else(|_| "default".to_owned());
    let identity = std::env::var("AION_WORKER_IDENTITY")
        .unwrap_or_else(|_| "incident-triage-worker".to_owned());
    let concurrency: usize = std::env::var("AION_WORKER_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);

    let config = WorkerConfig::builder()
        .endpoint(&endpoint)
        .namespace("default")
        .task_queue(&task_queue)
        .identity(&identity)
        .max_concurrency(concurrency)
        .reconnect_initial_backoff(Duration::from_millis(500))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(10)
        .build()?;

    tracing::info!(%endpoint, %task_queue, %identity, "incident-triage-worker starting");

    Worker::builder(config)
        .register_activity("triage", triage)?
        .build()?
        .run()
        .await?;

    Ok(())
}
