//! Composition root for the pipeline-run worker.
//!
//! It serves eight activity types on ONE task queue (`pipeline_run`) across FIVE
//! liminal connections in this single process:
//!
//! - four DRIVEN AGENT connections (`scout`, `plan`, `dev`, `review`), each with
//!   its OWN composed [`NornHarness`] — a distinct `--output-schema`,
//!   `--append-system-prompt`, and `{workflow_id}`-templated `--session-id` /
//!   `--workspace-root`. One `AgentHarnessConfig` carries one fixed
//!   schema/system-prompt, so the four roles cannot share a connection; each
//!   advertises its one agent type and the server routes to it.
//! - one SHELL connection serving `provision_workspace`, `gate`, `land`, and
//!   `notify` from a typed registry, with no harness.
//!
//! Per-unit session isolation falls out of the topology: `dev`/`review` run
//! inside CHILD `pipeline_unit` workflows, so `{workflow_id}` in their session
//! ids and workspace roots is the CHILD's id — automatically per-unit and stable
//! across that unit's rounds (`--resume-if-exists` resumes it).
//!
//! Norn runs with `OPENAI_API_KEY` REMOVED from its child environment so it uses
//! the operator's `ChatGPT` OAuth login, exactly like the incident-triage worker.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use aion_integration_norn::NornHarness;
use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityRegistry, AgentHarnessConfig, HandlerFuture, RedialTiming,
    WorkerConfig, WorkerConfigBuildError,
};

use pipeline_run_worker::handlers::{self, WORKSPACE_BASE};
use pipeline_run_worker::prompts;
use pipeline_run_worker::schemas;
use pipeline_run_worker::shell::Shell;

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address`.
const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";
/// The one task queue every pipeline-run activity is dispatched on.
const TASK_QUEUE: &str = "pipeline_run";
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// One driven-agent role: its activity type, system prompt, output schema, the
/// `--session-id` role suffix, and the Norn `--workspace-root` (which may carry
/// the `{workflow_id}` placeholder), plus whether it runs on the fast tier.
struct Role {
    activity_type: &'static str,
    system_prompt: &'static str,
    output_schema: &'static str,
    session_suffix: &'static str,
    workspace_root: String,
    fast: bool,
}

/// Parsed command-line arguments.
struct Args {
    candidates: Vec<String>,
    identity_prefix: String,
    ready_file: Option<String>,
    norn_bin: String,
    /// The repository `scout` and `plan` ground in (their Norn `--workspace-root`).
    /// The dev/review workspace is per-unit and derived from `{workflow_id}`, so
    /// this only sizes the read-only grounding passes.
    repo_root: String,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    tracing::info!(
        candidates = ?args.candidates,
        norn_bin = %args.norn_bin,
        repo_root = %args.repo_root,
        task_queue = TASK_QUEUE,
        "pipeline-run-worker starting: 4 driven agents + 4 shell activities across 5 connections"
    );

    let roles = roles(&args.repo_root);

    // Spawn one connection thread per agent role. Each owns its config, an empty
    // registry, its stop flag, and its composed harness, and blocks in
    // serve_with_redial (redialling internally) until the process is killed.
    let mut threads = Vec::new();
    for role in roles {
        let candidates = args.candidates.clone();
        let norn_bin = args.norn_bin.clone();
        let identity = format!("{}-{}", args.identity_prefix, role.activity_type);
        threads.push(thread::spawn(move || {
            serve_agent_role(&candidates, &identity, &norn_bin, &role);
        }));
    }

    // Serve the shell activities on the main thread; it writes the readiness
    // file once connected (a signal the whole worker is up enough to accept
    // work — the agent connections come up alongside it).
    serve_shell(&args)?;

    for handle in threads {
        let _ = handle.join();
    }
    Ok(())
}

/// The four agent roles. `scout`/`plan` ground in the target repo (fast tier);
/// `dev`/`review` operate in the per-unit worktree at `<base>/{workflow_id}`
/// (judgment tier).
fn roles(repo_root: &str) -> Vec<Role> {
    let unit_workspace = format!("{WORKSPACE_BASE}/{{workflow_id}}");
    vec![
        Role {
            activity_type: "scout",
            system_prompt: prompts::SCOUT_SYSTEM,
            output_schema: schemas::SCOUT_OUTPUT,
            session_suffix: "scout",
            workspace_root: repo_root.to_owned(),
            fast: true,
        },
        Role {
            activity_type: "plan",
            system_prompt: prompts::PLAN_SYSTEM,
            output_schema: schemas::STACK_PLAN,
            session_suffix: "plan",
            workspace_root: repo_root.to_owned(),
            fast: true,
        },
        Role {
            activity_type: "dev",
            system_prompt: prompts::DEV_SYSTEM,
            output_schema: schemas::DEV_OUTPUT,
            session_suffix: "dev",
            workspace_root: unit_workspace.clone(),
            fast: false,
        },
        Role {
            activity_type: "review",
            system_prompt: prompts::REVIEW_SYSTEM,
            output_schema: schemas::REVIEW_OUTPUT,
            session_suffix: "review",
            workspace_root: unit_workspace,
            fast: false,
        },
    ]
}

/// Compose one role's harness and serve it on its own liminal connection.
fn serve_agent_role(candidates: &[String], identity: &str, norn_bin: &str, role: &Role) {
    let config = match worker_config(identity) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%identity, %error, "could not build agent worker config");
            return;
        }
    };
    let registry = Arc::new(ActivityRegistry::new());
    let agent = composed_agent_harness(norn_bin, role);
    let stop = AtomicBool::new(false);
    let role_name = role.activity_type;
    if let Err(error) = aion_worker::serve_with_redial(
        candidates.to_vec(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        Some(&agent),
        move || tracing::info!(role = role_name, "agent connection registered"),
    ) {
        tracing::error!(role = role_name, %error, "agent connection ended");
    }
}

/// Build the composed [`NornHarness`] for one role. This is the ONE place a
/// concrete adapter is named per role; the serve path drives it only through the
/// erased [`DynAgentHarness`] trait.
fn composed_agent_harness(norn_bin: &str, role: &Role) -> AgentHarnessConfig {
    let session_id = format!("{{workflow_id}}-{}", role.session_suffix);
    let mut harness = NornHarness::with_binary(norn_bin)
        .with_arg("--append-system-prompt")
        .with_arg(role.system_prompt)
        .with_arg("--output-schema")
        .with_arg(role.output_schema.trim_start())
        .with_arg("--session-id")
        .with_arg(session_id)
        .with_arg("--resume-if-exists")
        .with_arg("--workspace-root")
        .with_arg(role.workspace_root.clone());
    if role.fast {
        harness = harness.with_arg("--fast");
    }
    // Force the ChatGPT OAuth login: a stray ambient API key would take
    // precedence and fail.
    let harness = harness.without_env("OPENAI_API_KEY");

    let erased: Arc<dyn DynAgentHarness> = Arc::new(harness);
    AgentHarnessConfig::new(
        erased,
        [role.activity_type],
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]),
    )
}

/// Serve the four shell activities from a typed registry on one connection.
fn serve_shell(args: &Args) -> anyhow::Result<()> {
    let identity = format!("{}-shell", args.identity_prefix);
    let config = worker_config(&identity)?;
    let shell = Shell::inherited();
    let registry = Arc::new(
        ActivityRegistry::new()
            .register_activity(
                "provision_workspace",
                blocking(shell.clone(), handlers::provision),
            )?
            .register_activity("gate", blocking(shell.clone(), handlers::gate))?
            .register_activity("land", blocking(shell.clone(), handlers::land))?
            .register_activity("notify", blocking(shell, handlers::notify))?,
    );
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    aion_worker::serve_with_redial(
        args.candidates.clone(),
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
            tracing::info!("shell connection registered; serving provision/gate/land/notify");
        },
    )?;
    Ok(())
}

/// The shared worker config for one connection: one identity, the pipeline task
/// queue, and an effectively unbounded reconnect budget (a long-lived worker
/// must outwait server restarts).
fn worker_config(identity: &str) -> Result<WorkerConfig, WorkerConfigBuildError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue(TASK_QUEUE)
        .identity(identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(usize::MAX)
        .build()
}

/// Adapt a synchronous, blocking handler body onto the worker SDK's async
/// handler signature — the shell bodies block on git/cargo, so each invocation
/// moves to the blocking thread pool.
fn blocking<Input, Output>(
    shell: Shell,
    body: fn(&Shell, Input) -> Result<Output, aion_worker::ActivityFailure>,
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
                    aion_worker::ActivityFailure::terminal(format!(
                        "activity handler task did not complete: {join_error}"
                    ))
                })?
        })
    }
}

fn parse_args() -> anyhow::Result<Args> {
    let default_norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    parse_args_from(std::env::args().skip(1), default_norn_bin)
}

/// The argument-parsing core, fed an explicit iterator and defaults so tests
/// exercise the exact production logic without touching process globals.
fn parse_args_from(
    args: impl IntoIterator<Item = String>,
    default_norn_bin: String,
) -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity_prefix = "pipeline-run-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = default_norn_bin;
    let mut repo_root = ".".to_owned();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => candidates.push(next_value(&mut args, "--address")?),
            "--identity" => identity_prefix = next_value(&mut args, "--identity")?,
            "--ready-file" => ready_file = Some(next_value(&mut args, "--ready-file")?),
            "--norn-bin" => norn_bin = next_value(&mut args, "--norn-bin")?,
            "--repo-root" => repo_root = next_value(&mut args, "--repo-root")?,
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push(DEFAULT_ADDRESS.to_owned());
    }
    Ok(Args {
        candidates,
        identity_prefix,
        ready_file,
        norn_bin,
        repo_root,
    })
}

/// Take the value for a value-taking flag, bailing clearly when it is missing —
/// a silent default would mask an operator typo.
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_ADDRESS, parse_args_from};

    fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
        parse_args_from(
            args.iter().map(|arg| (*arg).to_owned()),
            "norn-default".to_owned(),
        )
    }

    #[test]
    fn no_arguments_yields_the_defaults() -> anyhow::Result<()> {
        let args = parse(&[])?;
        assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
        assert_eq!(args.identity_prefix, "pipeline-run-worker");
        assert_eq!(args.ready_file, None);
        assert_eq!(args.norn_bin, "norn-default");
        assert_eq!(args.repo_root, ".");
        Ok(())
    }

    #[test]
    fn flags_parse_and_addresses_repeat() -> anyhow::Result<()> {
        let args = parse(&[
            "--address",
            "127.0.0.1:1",
            "--address",
            "127.0.0.1:2",
            "--identity",
            "w",
            "--ready-file",
            "/tmp/r",
            "--norn-bin",
            "/opt/norn",
            "--repo-root",
            "/repo",
        ])?;
        assert_eq!(
            args.candidates,
            vec!["127.0.0.1:1".to_owned(), "127.0.0.1:2".to_owned()]
        );
        assert_eq!(args.identity_prefix, "w");
        assert_eq!(args.ready_file.as_deref(), Some("/tmp/r"));
        assert_eq!(args.norn_bin, "/opt/norn");
        assert_eq!(args.repo_root, "/repo");
        Ok(())
    }

    #[test]
    fn every_value_taking_flag_bails_when_missing() {
        for flag in [
            "--address",
            "--identity",
            "--ready-file",
            "--norn-bin",
            "--repo-root",
        ] {
            assert_eq!(
                parse(&[flag]).err().map(|error| error.to_string()),
                Some(format!("{flag} requires a value")),
            );
        }
    }

    #[test]
    fn unknown_argument_bails() {
        assert_eq!(
            parse(&["--bogus"]).err().map(|error| error.to_string()),
            Some("unknown argument `--bogus`".to_owned()),
        );
    }
}
