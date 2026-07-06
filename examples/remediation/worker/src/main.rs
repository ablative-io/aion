//! Composition root for the remediation worker.
//!
//! It serves nine activity types on ONE task queue (`remediation`) across
//! FIVE liminal connections in this single process:
//!
//! - four DRIVEN AGENT connections (`test_author`, `developer`, `verifier`,
//!   `re_auditor`), each with its OWN composed [`ProfiledNornHarness`]: a
//!   distinct `--output-schema`, a `{workflow_id}`-templated `--session-id` /
//!   `--workspace-root`, and the role's profile markdown (loaded once at
//!   startup from `--profiles-dir`, pointing at the yggdrasil checkout's
//!   `docs/design/remediation-flow/profiles/`) assembled with the per-run
//!   context by the role's ONE prompt function. One `AgentHarnessConfig`
//!   carries one fixed schema/profile, so the roles cannot share a
//!   connection; each advertises its one agent type and the server routes to
//!   it.
//! - one SHELL connection serving `provision_workspace`, `gate1`, `gate2`,
//!   `ledger_update`, and `cleanup_workspace` from a typed registry, with no
//!   harness.
//!
//! Per-brief session isolation falls out of the topology: the agent
//! activities run inside CHILD `remediation_brief` workflows, so
//! `{workflow_id}` in their session ids and workspace roots is the CHILD's id
//! — automatically per-brief and stable across that brief's fix cycles
//! (`--resume-if-exists` resumes them).
//!
//! Norn runs with `OPENAI_API_KEY` REMOVED from its child environment so it
//! uses the operator's `ChatGPT` OAuth login, exactly like the pipeline-run
//! worker. No secret is ever read, stored, or passed by this worker.

use std::path::Path;
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

use remediation_worker::handlers::{self, WORKSPACE_BASE};
use remediation_worker::harness::ProfiledNornHarness;
use remediation_worker::profiles::{self, Profiles};
use remediation_worker::prompts;
use remediation_worker::schemas;
use remediation_worker::shell::Shell;

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address`.
const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";
/// The one task queue every remediation activity is dispatched on.
const TASK_QUEUE: &str = "remediation";
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// One driven-agent role: its activity type, output schema, the
/// `--session-id` role suffix, the Norn `--workspace-root` (which may carry
/// the `{workflow_id}` placeholder), the profile doctrine, and the prompt
/// assembly function.
struct Role {
    activity_type: &'static str,
    output_schema: &'static str,
    session_suffix: &'static str,
    workspace_root: String,
    profile: String,
    assemble: prompts::AssembleFn,
}

/// Parsed command-line arguments.
#[derive(Debug)]
struct Args {
    candidates: Vec<String>,
    identity_prefix: String,
    ready_file: Option<String>,
    norn_bin: String,
    /// The repository the fixes land in — the re-auditor's grounding root
    /// (the other roles work in per-brief worktrees derived from
    /// `{workflow_id}`).
    repo_root: String,
    /// The directory the four role profiles are loaded from — REQUIRED: the
    /// yggdrasil checkout's `docs/design/remediation-flow/profiles/`.
    profiles_dir: String,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    let profiles =
        profiles::load(Path::new(&args.profiles_dir)).map_err(|error| anyhow::anyhow!(error))?;
    tracing::info!(
        candidates = ?args.candidates,
        norn_bin = %args.norn_bin,
        repo_root = %args.repo_root,
        profiles_dir = %args.profiles_dir,
        task_queue = TASK_QUEUE,
        "remediation-worker starting: 4 driven agent roles + 5 shell activities across 5 connections"
    );

    let roles = roles(&args.repo_root, profiles);

    // Spawn one connection thread per agent role. Each owns its config, an
    // empty registry, its stop flag, and its composed harness, and blocks in
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

/// The four agent roles. `test_author`/`developer`/`verifier` operate in the
/// per-brief worktree at `<base>/{workflow_id}` (the verifier reads the fixed
/// tree there — static reading, per its profile); the `re_auditor` grounds in
/// the target repo itself (post-wave, landed state). All run on the judgment
/// tier — none of these roles is a fast grounding pass.
fn roles(repo_root: &str, profiles: Profiles) -> Vec<Role> {
    let brief_workspace = format!("{WORKSPACE_BASE}/{{workflow_id}}");
    vec![
        Role {
            activity_type: "test_author",
            output_schema: schemas::TEST_MANIFEST,
            session_suffix: "test-author",
            workspace_root: brief_workspace.clone(),
            profile: profiles.test_author,
            assemble: prompts::test_author,
        },
        Role {
            activity_type: "developer",
            output_schema: schemas::FIX_REPORT,
            session_suffix: "developer",
            workspace_root: brief_workspace.clone(),
            profile: profiles.developer,
            assemble: prompts::developer,
        },
        Role {
            activity_type: "verifier",
            output_schema: schemas::VERDICT,
            session_suffix: "verifier",
            workspace_root: brief_workspace,
            profile: profiles.verifier,
            assemble: prompts::verifier,
        },
        Role {
            activity_type: "re_auditor",
            output_schema: schemas::RE_AUDIT_FINDINGS,
            session_suffix: "re-auditor",
            workspace_root: repo_root.to_owned(),
            profile: profiles.re_auditor,
            assemble: prompts::re_auditor,
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

/// Build the composed harness for one role: the inner [`NornHarness`] carries
/// the driven-mode wiring (`--output-schema`, `{workflow_id}` session
/// identity, `--resume-if-exists`, workspace root, env hygiene); the
/// [`ProfiledNornHarness`] wrapper assembles {profile + context} into the
/// prompt. This is the ONE place a concrete adapter is named per role; the
/// serve path drives it only through the erased [`DynAgentHarness`] trait.
fn composed_agent_harness(norn_bin: &str, role: &Role) -> AgentHarnessConfig {
    let session_id = format!("{{workflow_id}}-{}", role.session_suffix);
    let inner = NornHarness::with_binary(norn_bin)
        .with_arg("--output-schema")
        .with_arg(role.output_schema.trim_start())
        .with_arg("--session-id")
        .with_arg(session_id)
        .with_arg("--resume-if-exists")
        .with_arg("--workspace-root")
        .with_arg(role.workspace_root.clone())
        // Force the ChatGPT OAuth login: a stray ambient API key would take
        // precedence and fail. No secret is ever set here.
        .without_env("OPENAI_API_KEY");
    let harness = ProfiledNornHarness::new(inner, role.profile.clone(), role.assemble);

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

/// Serve the five shell activities from a typed registry on one connection.
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
            .register_activity("gate1", blocking(shell.clone(), handlers::gate1))?
            .register_activity("gate2", blocking(shell.clone(), handlers::gate2))?
            .register_activity(
                "ledger_update",
                blocking(shell.clone(), handlers::ledger_update),
            )?
            .register_activity("cleanup_workspace", blocking(shell, handlers::cleanup))?,
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
            tracing::info!(
                "shell connection registered; serving provision/gate1/gate2/ledger_update/cleanup"
            );
        },
    )?;
    Ok(())
}

/// The shared worker config for one connection: one identity, the remediation
/// task queue, and an effectively unbounded reconnect budget (a long-lived
/// worker must outwait server restarts).
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
/// handler signature — the shell bodies block on git/cargo/python, so each
/// invocation moves to the blocking thread pool.
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
/// `--profiles-dir` is REQUIRED: the roles cannot run without their doctrine,
/// and a silently-defaulted path would mask an operator omission.
fn parse_args_from(
    args: impl IntoIterator<Item = String>,
    default_norn_bin: String,
) -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity_prefix = "remediation-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = default_norn_bin;
    let mut repo_root = ".".to_owned();
    let mut profiles_dir: Option<String> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => candidates.push(next_value(&mut args, "--address")?),
            "--identity" => identity_prefix = next_value(&mut args, "--identity")?,
            "--ready-file" => ready_file = Some(next_value(&mut args, "--ready-file")?),
            "--norn-bin" => norn_bin = next_value(&mut args, "--norn-bin")?,
            "--repo-root" => repo_root = next_value(&mut args, "--repo-root")?,
            "--profiles-dir" => profiles_dir = Some(next_value(&mut args, "--profiles-dir")?),
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push(DEFAULT_ADDRESS.to_owned());
    }
    let profiles_dir = profiles_dir.ok_or_else(|| {
        anyhow::anyhow!(
            "--profiles-dir is required (point it at the yggdrasil checkout's \
             docs/design/remediation-flow/profiles/)"
        )
    })?;
    Ok(Args {
        candidates,
        identity_prefix,
        ready_file,
        norn_bin,
        repo_root,
        profiles_dir,
    })
}

/// Take the value for a value-taking flag, bailing clearly when it is
/// missing — a silent default would mask an operator typo.
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{DEFAULT_ADDRESS, Profiles, parse_args_from, roles};

    fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
        parse_args_from(
            args.iter().map(|arg| (*arg).to_owned()),
            "norn-default".to_owned(),
        )
    }

    fn profiles() -> Profiles {
        Profiles {
            test_author: "ta".to_owned(),
            developer: "dev".to_owned(),
            verifier: "ver".to_owned(),
            re_auditor: "re".to_owned(),
        }
    }

    #[test]
    fn profiles_dir_is_required() {
        let error = parse(&[]).expect_err("must fail without --profiles-dir");
        assert!(
            error.to_string().contains("--profiles-dir"),
            "error: {error}"
        );
    }

    #[test]
    fn minimal_arguments_yield_the_defaults() -> anyhow::Result<()> {
        let args = parse(&["--profiles-dir", "/yg/profiles"])?;
        assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
        assert_eq!(args.identity_prefix, "remediation-worker");
        assert_eq!(args.ready_file, None);
        assert_eq!(args.norn_bin, "norn-default");
        assert_eq!(args.repo_root, ".");
        assert_eq!(args.profiles_dir, "/yg/profiles");
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
            "--profiles-dir",
            "/yg/profiles",
        ])?;
        assert_eq!(
            args.candidates,
            vec!["127.0.0.1:1".to_owned(), "127.0.0.1:2".to_owned()]
        );
        assert_eq!(args.identity_prefix, "w");
        assert_eq!(args.ready_file.as_deref(), Some("/tmp/r"));
        assert_eq!(args.norn_bin, "/opt/norn");
        assert_eq!(args.repo_root, "/repo");
        assert_eq!(args.profiles_dir, "/yg/profiles");
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
            "--profiles-dir",
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

    /// The role wiring: each role carries its own schema, session suffix,
    /// profile, and the agreed workspace roots — worktree-per-brief for the
    /// three brief-scoped roles, the repo itself for the re-auditor.
    #[test]
    fn roles_bind_schema_session_profile_and_workspace() {
        let roles = roles("/repo", profiles());
        let summary: Vec<(&str, &str, &str)> = roles
            .iter()
            .map(|role| {
                (
                    role.activity_type,
                    role.session_suffix,
                    role.workspace_root.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "test_author",
                    "test-author",
                    "/tmp/aion-remediation/ws/{workflow_id}"
                ),
                (
                    "developer",
                    "developer",
                    "/tmp/aion-remediation/ws/{workflow_id}"
                ),
                (
                    "verifier",
                    "verifier",
                    "/tmp/aion-remediation/ws/{workflow_id}"
                ),
                ("re_auditor", "re-auditor", "/repo"),
            ]
        );
        // Every role's schema is a JSON object document and its profile is
        // the loaded doctrine.
        for role in &roles {
            assert!(role.output_schema.trim_start().starts_with('{'));
            assert!(!role.profile.is_empty());
        }
    }
}
