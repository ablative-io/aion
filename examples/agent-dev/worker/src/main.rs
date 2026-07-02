//! Generic mixed worker for the `agent_dev` proof.
//!
//! Serves two kinds of activity over the liminal server-push transport
//! ([`aion_worker::serve_with_redial`], same wiring as `norn-fan-worker`):
//!
//! - AGENT activities `scout`, `dev`, `review`: routed through the composed
//!   [`NornHarness`] (observable + intervenable). Input is the prompt string,
//!   output the agent's answer string. Each run's Norn session id is
//!   `{workflow_id}-{activity_type}` and its `--workspace-root` is the run's
//!   own clone `<workspace root>/{workflow_id}/repo` — the placeholders are
//!   expanded per run by the adapter, and the workspace root is the SAME
//!   resolved root the `provision` handler clones under (resolved once here,
//!   threaded to both).
//! - PLAIN registry activities `provision`, `gate`, `land`: synchronous
//!   handler bodies in [`agent_dev_worker::handlers`], adapted onto the async
//!   handler signature via `spawn_blocking`.
//!
//! Auth: Norn is invoked with `OPENAI_API_KEY` REMOVED from its child
//! environment (via the adapter's `without_env`) so it uses the operator's
//! `ChatGPT` OAuth login — a stray ambient key would take precedence and fail.
//!
//! Usage:
//!   agent-dev-worker --address 127.0.0.1:PORT [--address 127.0.0.1:PORT2 ...]
//!                    [--identity <id>] [--ready-file <path>] [--norn-bin <path>]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use agent_dev_worker::handlers;
use agent_dev_worker::shell::Shell;
use aion_integration_norn::NornHarness;
use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityFailure, ActivityRegistry, AgentHarnessConfig, HandlerFuture,
    RedialTiming, WorkerConfig,
};

/// The agent activity types routed through the composed harness rather than
/// the typed registry — the three Norn rounds of the `agent_dev` workflow.
const AGENT_ACTIVITY_TYPES: [&str; 3] = ["scout", "dev", "review"];

/// Lower bound on the reconnect backoff between candidate dials.
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
/// Upper bound on the reconnect backoff (a survivor may take a moment to
/// adopt the shard and bring its listener up).
const REDIAL_MAX_BACKOFF: Duration = Duration::from_millis(500);

/// Compose the agent harness at the binary root — the ONE place this worker
/// names a concrete [`AgentHarness`](aion_integrations::AgentHarness) adapter,
/// mirroring `norn-fan-worker` and the `aion` binary's composition root. The
/// serve path drives it only through the erased neutral trait
/// ([`DynAgentHarness`]), so swapping the adapter touches this function alone.
///
/// `workspace_root` is the SAME resolved root the `provision` handler clones
/// under: every agent round works inside the run's own clone
/// (`<root>/{workflow_id}/repo`), and its session id
/// (`{workflow_id}-{activity_type}` + `--resume-if-exists`) is stable per
/// `(run, round)` so a re-dispatch after a failover RESUMES the session
/// rather than starting over. `--fast` selects Norn's fast model tier.
///
/// The advertised capabilities are exactly the neutral primitives the Norn
/// adapter's intervention translation supports today (`InjectMessage` +
/// `Cancel`); advertising more would promise interventions the harness
/// rejects.
fn composed_agent_harness(norn_bin: &str, workspace_root: &Path) -> AgentHarnessConfig {
    let harness: Arc<dyn DynAgentHarness> = Arc::new(
        NornHarness::with_binary(norn_bin)
            .with_arg("--workspace-root")
            .with_arg(format!("{}/{{workflow_id}}/repo", workspace_root.display()))
            .with_arg("--session-id")
            .with_arg("{workflow_id}-{activity_type}")
            .with_arg("--resume-if-exists")
            .with_arg("--fast")
            // Force the `ChatGPT` OAuth login: the project does not use API
            // keys, and a stray ambient key would take precedence and fail.
            .without_env("OPENAI_API_KEY"),
    );
    AgentHarnessConfig::new(
        harness,
        AGENT_ACTIVITY_TYPES,
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]),
    )
}

/// Adapt a synchronous, blocking handler body onto the worker SDK's async
/// handler signature. The bodies block on child processes (git clones and
/// cargo gates can run for minutes), so each invocation moves to the blocking
/// thread pool instead of stalling the worker's async runtime.
fn blocking<Input, Output, Body>(
    body: Body,
) -> impl for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
+ Send
+ Sync
+ 'static
where
    Body: Fn(Input) -> Result<Output, ActivityFailure> + Clone + Send + Sync + 'static,
    Input: Send + 'static,
    Output: Send + 'static,
{
    move |input: Input, _context: &ActivityContext| {
        let body = body.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || body(input))
                .await
                .map_err(|join_error| {
                    ActivityFailure::terminal(format!(
                        "activity handler task did not complete: {join_error}"
                    ))
                })?
        })
    }
}

/// Build the plain activity registry: `provision` (clones under the resolved
/// `workspace_root`), `gate`, and `land`.
fn build_registry(shell: &Shell, workspace_root: &Path) -> anyhow::Result<Arc<ActivityRegistry>> {
    let provision = {
        let shell = shell.clone();
        let root = workspace_root.to_path_buf();
        move |input| handlers::provision(&shell, &root, input)
    };
    let gate = {
        let shell = shell.clone();
        move |input| handlers::gate(&shell, input)
    };
    let land = {
        let shell = shell.clone();
        move |input| handlers::land(&shell, input)
    };
    let registry = ActivityRegistry::new()
        .register_activity("provision", blocking(provision))?
        .register_activity("gate", blocking(gate))?
        .register_activity("land", blocking(land))?;
    Ok(Arc::new(registry))
}

/// Parsed command-line arguments.
struct Args {
    /// One or more candidate liminal listen addresses, in dial-preference order.
    candidates: Vec<String>,
    /// The worker identity announced in-band.
    identity: String,
    /// Optional readiness file written once after the first registration.
    ready_file: Option<String>,
    /// Path to the `norn` binary (default: `NORN_BIN` env, else `norn` on PATH).
    norn_bin: String,
}

/// Parse `--address` (repeatable), `--identity`, `--ready-file`, `--norn-bin`.
fn parse_args() -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity = "agent-dev-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => match args.next() {
                Some(value) => candidates.push(value),
                None => anyhow::bail!("--address requires a value"),
            },
            "--identity" => {
                if let Some(value) = args.next() {
                    identity = value;
                }
            }
            "--ready-file" => {
                ready_file = args.next();
            }
            "--norn-bin" => {
                if let Some(value) = args.next() {
                    norn_bin = value;
                }
            }
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push("127.0.0.1:50061".to_owned());
    }
    Ok(Args {
        candidates,
        identity,
        ready_file,
        norn_bin,
    })
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;

    // Resolve the stable workspace root ONCE and thread it to both consumers:
    // the provision handler (clones under it) and the agent harness (whose
    // --workspace-root template points into the same per-run clone).
    let workspace_root = handlers::resolve_workspace_root()
        .map_err(|failure| anyhow::anyhow!(failure.message().to_owned()))?;

    tracing::info!(
        candidates = ?args.candidates,
        identity = %args.identity,
        norn_bin = %args.norn_bin,
        workspace_root = %workspace_root.display(),
        "agent-dev-worker starting"
    );

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue("default")
        .identity(&args.identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(3)
        .build()?;

    let shell = Shell::inherited();
    let registry = build_registry(&shell, &workspace_root)?;
    let agent = composed_agent_harness(&args.norn_bin, &workspace_root);

    // Never stop on our own; the operator ends the worker with a signal.
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    aion_worker::serve_with_redial(
        args.candidates,
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        Some(&agent),
        move || {
            if let Some(path) = ready_file.as_ref()
                && let Err(error) = std::fs::write(path, b"connected")
            {
                tracing::error!(%error, "failed to write worker readiness file");
            }
            tracing::info!("agent-dev-worker connected and registered; serving dispatches");
        },
    )?;

    Ok(())
}
