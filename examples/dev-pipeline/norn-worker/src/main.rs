//! Standalone MIXED activity worker for the dev-pipeline workflows, served
//! over the liminal server-push transport ([`aion_worker::serve_with_redial`],
//! the `norn-fan-worker`/`agent-dev` wiring).
//!
//! Two kinds of activity:
//!
//! - AGENT activities `scout`/`design`/`refute` (`brief_forge`): routed through
//!   the composed [`NornHarness`] in DRIVEN MODE (`norn --protocol jsonrpc`)
//!   — every `event/*` notification lands as a durable [`ActivityEvent`]
//!   (live transcript in the ops console) and the session accepts
//!   InjectMessage/Cancel interventions. The activity input IS the projected
//!   prompt text (a JSON string payload built by the Gleam workflow); the
//!   terminal stop envelope's schema-validated `output` object is the
//!   activity result the Gleam codecs decode.
//! - PLAIN registry activities `provision_workspace`/`run_gate`/
//!   `teardown_workspace` (workspace-bound command steps) plus the SHELL-path
//!   implementer rounds `implement`/`implement_resume`: synchronous handler
//!   bodies in [`handlers`], adapted onto the async signature via
//!   `spawn_blocking`.
//!
//! # Why `implement`/`implement_resume` stay on the shell path (for now)
//!
//! Harness spawn arguments are fixed PER WORKER PROCESS; the only per-run
//! interpolations are the `{workflow_id}`/`{activity_type}` placeholders.
//! The implementer rounds need two things that cannot ride those templates:
//!
//! 1. a per-run `--workspace-root` — the isolated workspace path is minted at
//!    runtime by `provision_workspace` under the RUN INPUT's `repo_root`
//!    (with collision `-attempt-N` suffixes), so no spawn-time template can
//!    name it;
//! 2. one session SHARED ACROSS TWO activity types — `implement_resume` must
//!    resume `implement`'s session, but `{activity_type}` expands to two
//!    different names.
//!
//! Until the seam grows per-run spawn parameters, those two rounds keep the
//! `norn --print` shell-out (still with deterministic `--session-id` +
//! `--resume-if-exists`). A partial, honest conversion of the three
//! brief-forge rounds beats a broken full one.
//!
//! # Driven-mode session identity + schemas
//!
//! Each driven run's norn session id is `{workflow_id}-{activity_type}` with
//! `--resume-if-exists`: `design` resumes ITS OWN session across refute-loop
//! rounds (the intended designer-keeps-context behaviour), retries of a
//! stage resume rather than restart, and distinct workflow runs never share
//! sessions (an improvement over the shell path's `task_ref`-derived ids).
//! Deliberate deviation: `refute` rounds within one run now RESUME one
//! session instead of getting a fresh one per round — no per-round
//! placeholder exists yet (see the report; the refuter still never sees the
//! designer's reasoning, which the prompt projection owns).
//!
//! Per-activity output schemas ride the `{activity_type}` placeholder:
//! startup materializes the embedded stage schemas as activity-type-named
//! files (`<schemas-dir>/scout.json`/`design.json`/`refute.json`) and the
//! harness passes `--output-schema <schemas-dir>/{activity_type}.json`.
//!
//! One worker process serves ONE task queue. The Gleam side pins agent
//! rounds to `agents` (this binary's default `--task-queue`) and the
//! workspace-bound command steps to `workspaces`, so a full deployment runs
//! TWO instances of this binary — the harness config and every handler are
//! wired in both; only the queue subscription (and therefore which
//! dispatches arrive) differs.
//!
//! Auth: norn children spawned by the harness get `OPENAI_API_KEY` REMOVED
//! (via the adapter's `without_env`) so they use the operator's `ChatGPT`
//! OAuth login — a stray ambient key would take precedence and fail.
//!
//! Usage:
//!   dev-pipeline-worker-norn --address 127.0.0.1:50061 \
//!       [--address 127.0.0.1:PORT2 ...] [--namespace <name>] \
//!       [--task-queue agents|workspaces] [--concurrency <n>] \
//!       [--identity <id>] [--norn-bin <path>] [--workspace-root <dir>] \
//!       [--schemas-dir <dir>] [--reasoning-effort <level>] \
//!       [--norn-timeout <duration>]
//!
//! `--address` is the server's `[outbox] liminal_listen_address` (driven
//! mode needs the liminal transport — the gRPC worker path has no agent
//! seam). `--workspace-root` should point at the target repository for the
//! `agents` instance: it becomes the driven runs' `--workspace-root` (file
//! tool confinement) and `-C` (tool-execution cwd); when omitted the runs
//! are unconfined and execute in this worker's own cwd, with only the
//! prompt's "Repository root:" line steering the agent.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_integration_norn::NornHarness;
use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityFailure, ActivityRegistry, AgentHarnessConfig, HandlerFuture,
    RedialTiming, WorkerConfig,
};
use anyhow::{Context, bail};
use dev_pipeline_worker_norn::handlers;
use dev_pipeline_worker_norn::schemas;
use dev_pipeline_worker_norn::shell::Shell;

/// The agent activity types routed through the composed driven-mode harness
/// rather than the typed registry — the three brief-forge rounds. The
/// implementer rounds stay on the shell path (see the module doc).
const AGENT_ACTIVITY_TYPES: [&str; 3] = ["scout", "design", "refute"];

/// The embedded stage schemas materialized at startup, named EXACTLY after
/// their activity types so `--output-schema <dir>/{activity_type}.json`
/// resolves per dispatch.
const STAGE_SCHEMAS: [(&str, &str); 3] = [
    ("scout", schemas::SCOUT_OUTPUT_SCHEMA),
    ("design", schemas::BRIEF_OUTPUT_SCHEMA),
    ("refute", schemas::REFUTATION_OUTPUT_SCHEMA),
];

/// Lower bound on the reconnect backoff between candidate dials.
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
/// Upper bound on the reconnect backoff — a long-lived worker must outwait
/// server restarts, so it keeps probing at this cadence for as long as it
/// runs (the redial loop has no attempt ceiling).
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Default `--reasoning-effort` for the driven stages. Harness arguments are
/// fixed per process, so the shell path's per-stage efforts (scout `medium`;
/// designer/refuter `high`) collapse to ONE knob here; `high` preserves the
/// two quality-bearing rounds and the operator can lower it per instance.
const DEFAULT_REASONING_EFFORT: &str = "high";

/// Default norn `--timeout` (a norn duration string) for one driven step, so
/// a wedged run ends in norn's TYPED `timed_out` stop envelope — an honest,
/// diagnosable activity failure — rather than pinning an activity slot
/// forever (the engine imposes no activity timeout of its own).
const DEFAULT_NORN_TIMEOUT: &str = "30m";

/// Compose the agent harness at the binary root — the ONE place this worker
/// names a concrete [`AgentHarness`](aion_integrations::AgentHarness)
/// adapter, mirroring `norn-fan-worker`/`agent-dev` and the `aion` binary's
/// composition root. The serve path drives it only through the erased
/// neutral trait ([`DynAgentHarness`]).
///
/// The argument set mirrors the shell path's flag set minus `--print`/
/// `--output-format` (driven mode IS the transport): the pilot `--model` pin
/// with its armed context window (same constants as the shell handlers), the
/// per-process reasoning effort, the `{workflow_id}-{activity_type}` session
/// template with `--resume-if-exists`, the step `--timeout`, and the
/// materialized per-activity-type `--output-schema` file. When the operator
/// named a `--workspace-root`, it rides as both `--workspace-root` (file-tool
/// confinement) and `-C` (tool-execution cwd) — the same pairing the shell
/// path got from running norn IN `repo_root`.
///
/// The advertised capabilities are exactly the neutral primitives the norn
/// adapter's intervention translation supports today (`InjectMessage` +
/// `Cancel`); advertising more would promise interventions the harness
/// rejects.
fn composed_agent_harness(
    norn_bin: &str,
    schemas_dir: &std::path::Path,
    workspace_root: Option<&str>,
    reasoning_effort: &str,
    norn_timeout: &str,
) -> AgentHarnessConfig {
    let mut harness = NornHarness::with_binary(norn_bin)
        .with_arg("--fast")
        .with_arg("--model")
        .with_arg(handlers::PILOT_MODEL)
        .with_arg("-c")
        .with_arg(handlers::PILOT_MODEL_CONTEXT_WINDOW)
        .with_arg("--reasoning-effort")
        .with_arg(reasoning_effort)
        .with_arg("--session-id")
        .with_arg("{workflow_id}-{activity_type}")
        .with_arg("--resume-if-exists")
        .with_arg("--timeout")
        .with_arg(norn_timeout)
        .with_arg("--output-schema")
        .with_arg(format!("{}/{{activity_type}}.json", schemas_dir.display()))
        // Force the `ChatGPT` OAuth login: the project does not use API
        // keys, and a stray ambient key would take precedence and fail.
        .without_env("OPENAI_API_KEY");
    if let Some(root) = workspace_root {
        harness = harness
            .with_arg("--workspace-root")
            .with_arg(root)
            .with_arg("-C")
            .with_arg(root);
    }
    let harness: Arc<dyn DynAgentHarness> = Arc::new(harness);
    AgentHarnessConfig::new(
        harness,
        AGENT_ACTIVITY_TYPES,
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]),
    )
}

/// Write the embedded stage schemas into `schemas_dir` as activity-type-named
/// files (creating the directory), so the harness's
/// `--output-schema <dir>/{activity_type}.json` template resolves for every
/// driven dispatch. The binary remains the single schema source; the files
/// are a spawn-time projection of it, refreshed on every start.
fn materialize_stage_schemas(schemas_dir: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(schemas_dir).with_context(|| {
        format!(
            "cannot create the stage-schema directory {}",
            schemas_dir.display()
        )
    })?;
    for (activity_type, schema) in STAGE_SCHEMAS {
        let path = schemas_dir.join(format!("{activity_type}.json"));
        std::fs::write(&path, schema)
            .with_context(|| format!("cannot write stage schema {}", path.display()))?;
    }
    Ok(())
}

/// Parsed CLI arguments.
struct Args {
    /// One or more candidate liminal listen addresses, in dial-preference
    /// order (the server's `[outbox] liminal_listen_address`).
    candidates: Vec<String>,
    /// Maximum concurrent activity executions.
    concurrency: usize,
    /// Namespace (correctness/isolation boundary) to register into.
    namespace: String,
    /// Task queue (pool/flavour selector within the namespace) to serve.
    task_queue: String,
    /// Worker identity announced in-band; defaults to
    /// `dev-pipeline-worker-<task_queue>` so the two-instance deployment
    /// gets distinct identities without extra flags.
    identity: Option<String>,
    /// Path to the `norn` binary (default: `NORN_BIN` env, else `norn`).
    norn_bin: String,
    /// Repository root the driven agent rounds are confined to (their
    /// `--workspace-root` and `-C`); unconfined when absent.
    workspace_root: Option<String>,
    /// Directory the stage schemas are materialized into (default: a
    /// per-process directory under the system temp dir).
    schemas_dir: Option<PathBuf>,
    /// `--reasoning-effort` for the driven stages.
    reasoning_effort: String,
    /// norn `--timeout` duration string for one driven step.
    norn_timeout: String,
}

/// Parse CLI flags.
fn parse_args() -> anyhow::Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut candidates: Vec<String> = Vec::new();
    let mut concurrency = None;
    let mut namespace = None;
    let mut task_queue = None;
    let mut identity = None;
    let mut norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    let mut workspace_root = None;
    let mut schemas_dir = None;
    let mut reasoning_effort = DEFAULT_REASONING_EFFORT.to_owned();
    let mut norn_timeout = DEFAULT_NORN_TIMEOUT.to_owned();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--address" => {
                let value = args.next().context(
                    "--address requires a value — the server's \
                     [outbox] liminal_listen_address, e.g. 127.0.0.1:50061",
                )?;
                candidates.push(value);
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
            "--identity" => {
                let value = args.next().context("--identity requires a value")?;
                identity = Some(value);
            }
            "--norn-bin" => {
                let value = args.next().context("--norn-bin requires a value")?;
                norn_bin = value;
            }
            "--workspace-root" => {
                let value = args.next().context(
                    "--workspace-root requires a directory — the target \
                     repository the driven agent rounds are confined to",
                )?;
                workspace_root = Some(value);
            }
            "--schemas-dir" => {
                let value = args.next().context("--schemas-dir requires a directory")?;
                schemas_dir = Some(PathBuf::from(value));
            }
            "--reasoning-effort" => {
                let value = args
                    .next()
                    .context("--reasoning-effort requires a level (none|low|medium|high|x-high)")?;
                reasoning_effort = value;
            }
            "--norn-timeout" => {
                let value = args
                    .next()
                    .context("--norn-timeout requires a norn duration string, e.g. 30m")?;
                norn_timeout = value;
            }
            other => {
                bail!(
                    "unknown argument `{other}`\nusage: dev-pipeline-worker-norn \
                     --address <host:port> [--address <host:port> ...] \
                     [--namespace <name>] [--task-queue <name>] \
                     [--concurrency <n>] [--identity <id>] [--norn-bin <path>] \
                     [--workspace-root <dir>] [--schemas-dir <dir>] \
                     [--reasoning-effort <level>] [--norn-timeout <duration>]"
                )
            }
        }
    }
    if candidates.is_empty() {
        candidates.push("127.0.0.1:50061".to_owned());
    }
    Ok(Args {
        candidates,
        concurrency: concurrency.unwrap_or(4),
        namespace: namespace.unwrap_or_else(|| "default".to_owned()),
        // The Gleam activities pin task_queue("agents"); a worker left on
        // "default" would silently serve nothing.
        task_queue: task_queue.unwrap_or_else(|| "agents".to_owned()),
        identity,
        norn_bin,
        workspace_root,
        schemas_dir,
        reasoning_effort,
        norn_timeout,
    })
}

/// Adapt a synchronous, blocking handler body onto the worker SDK's async
/// handler signature. The bodies block on norn/git/gate child processes that
/// can run for minutes, so each invocation moves to the blocking thread pool
/// instead of stalling the worker's async runtime.
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

/// Build the plain typed registry: the workspace-bound command steps plus the
/// shell-path implementer rounds. The three driven agent types are NOT here —
/// the serve path routes them through the composed harness.
fn build_registry(shell: &Shell) -> anyhow::Result<Arc<ActivityRegistry>> {
    let registry = ActivityRegistry::new()
        .register_activity(
            "provision_workspace",
            blocking(shell.clone(), handlers::provision_workspace),
        )?
        .register_activity("implement", blocking(shell.clone(), handlers::implement))?
        .register_activity("run_gate", blocking(shell.clone(), handlers::run_gate))?
        .register_activity(
            "implement_resume",
            blocking(shell.clone(), handlers::implement_resume),
        )?
        .register_activity(
            "teardown_workspace",
            blocking(shell.clone(), handlers::teardown_workspace),
        )?;
    Ok(Arc::new(registry))
}

fn main() -> anyhow::Result<()> {
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
    let identity = cli
        .identity
        .clone()
        .unwrap_or_else(|| format!("dev-pipeline-worker-{}", cli.task_queue));
    let schemas_dir = cli.schemas_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("dev-pipeline-stage-schemas-{}", std::process::id()))
    });
    materialize_stage_schemas(&schemas_dir)?;

    tracing::info!(
        candidates = ?cli.candidates,
        namespace = %cli.namespace,
        task_queue = %cli.task_queue,
        identity = %identity,
        concurrency = cli.concurrency,
        norn_bin = %cli.norn_bin,
        workspace_root = ?cli.workspace_root,
        schemas_dir = %schemas_dir.display(),
        agent_activities = ?AGENT_ACTIVITY_TYPES,
        "dev-pipeline-worker starting; connection failures will be logged \
         with reconnect backoff — a quiet worker is a connected worker"
    );

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace(&cli.namespace)
        .task_queue(&cli.task_queue)
        .identity(&identity)
        .max_concurrency(cli.concurrency)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        // Effectively unbounded: a long-lived worker must outwait server
        // restarts; the SDK cannot express "unbounded" yet and usize::MAX is
        // the honest spelling of that intent (stacked-dev convention).
        .reconnect_max_attempts(usize::MAX)
        .build()?;

    let registry = build_registry(&shell)?;
    let agent = composed_agent_harness(
        &cli.norn_bin,
        &schemas_dir,
        cli.workspace_root.as_deref(),
        &cli.reasoning_effort,
        &cli.norn_timeout,
    );

    // Never stop on our own; the operator ends the worker with a signal. The
    // redial loop keeps probing the candidates for as long as the process
    // runs, so a long-lived worker outwaits server restarts.
    let stop = AtomicBool::new(false);
    aion_worker::serve_with_redial(
        cli.candidates,
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        Some(&agent),
        || {
            tracing::info!("dev-pipeline-worker connected and registered; serving dispatches");
        },
    )?;

    Ok(())
}
