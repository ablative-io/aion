//! Composition root for the dev-brief worker.
//!
//! It serves five activity types on ONE task queue (`dev_brief`) across
//! THREE liminal connections in this single process, each connection
//! registered on its OWN node (the third routing dimension: namespace ×
//! `task_queue` × node) so the server — which routes by those three
//! dimensions only, never by activity type — can land every dispatched
//! activity on the one connection that holds its handler:
//!
//! - two DRIVEN AGENT connections (`developer`, `reviewer`), each registered
//!   on its role's node, each with its OWN composed [`ProfiledNornHarness`]:
//!   a distinct `--output-schema`, a `{workflow_id}`-templated `--session-id`
//!   / `--workspace-root`, and the role's profile markdown (loaded once at
//!   startup from `--profiles-dir`, this package's `worker/profiles/`)
//!   assembled with the per-run context by the role's ONE prompt function.
//! - one SHELL connection, registered on node `shell`, serving
//!   `provision_workspace`, `run_gates`, and `cleanup_workspace` from a typed
//!   registry, with no harness.
//!
//! Session isolation falls out of the topology: the developer runs inside
//! the `dev_brief` workflow (`{workflow_id}-developer` — per brief, resumed
//! across that brief's fix cycles via `--resume-if-exists`); each review
//! lens runs inside its OWN `review_lens` CHILD workflow, so
//! `{workflow_id}-reviewer` is automatically per-lens-run — concurrent
//! lenses never share a session.
//!
//! Norn runs with `OPENAI_API_KEY` REMOVED from its child environment so it
//! uses the operator's `ChatGPT` OAuth login, exactly like the remediation
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

use dev_brief_worker::handlers::{self, WORKSPACE_BASE};
use dev_brief_worker::harness::{PostRunCommit, ProfiledNornHarness};
use dev_brief_worker::profiles::{self, Profiles};
use dev_brief_worker::prompts;
use dev_brief_worker::schemas;
use dev_brief_worker::shell::Shell;

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address`.
const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";
/// The one task queue every dev-brief activity is dispatched on.
const TASK_QUEUE: &str = "dev_brief";
/// The node id the SHELL connection registers. The server routes a pushed
/// activity by (namespace, `task_queue`, node) ONLY — never by activity type —
/// so each of this worker's three connections on the one task queue MUST
/// register a distinct node, and every activity the workflow builds pins the
/// node of the one connection that serves it. This string MUST equal
/// `shell_node` in `../src/dev_brief/activities.gleam` (the authoritative
/// node table); the agent connections' node ids mirror that table's
/// `developer_node`/`reviewer_node` constants.
const SHELL_NODE: &str = "shell";
/// The reviewer role's Norn workspace root: a STATIC scratch directory the
/// worker creates at startup. Lens children read the brief/diff/report from
/// their prompt context — the workspace is just Norn's working home, and it
/// must EXIST before a session starts (Norn refuses a missing
/// `--workspace-root`; the developer's per-brief root exists because
/// `provision_workspace` creates that exact worktree first — the live proof
/// run 1ac905ed failed on exactly this).
const REVIEW_SCRATCH: &str = "/tmp/aion-dev/review-scratch";
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// One driven-agent role: its activity type, the node its connection
/// registers, its output schema, the `--session-id` role suffix, the Norn
/// `--workspace-root` (carrying the `{workflow_id}` placeholder), the profile
/// doctrine, and the prompt assembly function.
struct Role {
    activity_type: &'static str,
    node: &'static str,
    output_schema: &'static str,
    session_suffix: &'static str,
    workspace_root: String,
    profile: String,
    assemble: prompts::AssembleFn,
    /// The mechanical commit this role's harness performs in the brief
    /// workspace after a successful turn (the doctrine: agents never run git
    /// — the machinery does). `DevWork` for the developer (the report's
    /// `commits` is rewritten to the real head); `None` for the reviewer,
    /// which writes nothing.
    post_run_commit: Option<PostRunCommit>,
}

/// Parsed command-line arguments.
#[derive(Debug)]
struct Args {
    candidates: Vec<String>,
    identity_prefix: String,
    ready_file: Option<String>,
    norn_bin: String,
    /// The directory the two role profiles are loaded from — REQUIRED: this
    /// package's `worker/profiles/`.
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
    std::fs::create_dir_all(REVIEW_SCRATCH)
        .map_err(|error| anyhow::anyhow!("could not create {REVIEW_SCRATCH}: {error}"))?;
    let profiles =
        profiles::load(Path::new(&args.profiles_dir)).map_err(|error| anyhow::anyhow!(error))?;
    tracing::info!(
        candidates = ?args.candidates,
        norn_bin = %args.norn_bin,
        profiles_dir = %args.profiles_dir,
        task_queue = TASK_QUEUE,
        "dev-brief-worker starting: 2 driven agent roles + 3 shell activities across 3 connections"
    );

    let roles = roles(profiles);

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

/// The two agent roles. Both operate in the per-brief worktree at
/// `<base>/{workflow_id}` — for the developer that is the `dev_brief` run's
/// worktree; for the reviewer the placeholder resolves to the `review_lens`
/// CHILD's id, so its Norn session is per-lens-run by construction (the lens
/// reads the diff and report from its prompt context; the workspace root
/// gives it a benign scratch home, not the developer's tree).
fn roles(profiles: Profiles) -> Vec<Role> {
    let brief_workspace = format!("{WORKSPACE_BASE}/{{workflow_id}}");
    vec![
        Role {
            activity_type: "developer",
            node: "developer",
            output_schema: schemas::DEV_REPORT,
            session_suffix: "developer",
            workspace_root: brief_workspace,
            profile: profiles.developer,
            assemble: prompts::developer,
            post_run_commit: Some(PostRunCommit::DevWork),
        },
        Role {
            activity_type: "review_lens",
            node: "reviewer",
            output_schema: schemas::LENS_VERDICT,
            session_suffix: "reviewer",
            workspace_root: REVIEW_SCRATCH.to_owned(),
            profile: profiles.reviewer,
            assemble: prompts::review_lens,
            post_run_commit: None,
        },
    ]
}

/// Compose one role's harness and serve it on its own liminal connection,
/// registered on the role's node so only this role's activity is routed here.
fn serve_agent_role(candidates: &[String], identity: &str, norn_bin: &str, role: &Role) {
    let config = match worker_config(identity, role.node) {
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
    let harness = match role.post_run_commit {
        Some(PostRunCommit::DevWork) => harness.committing_dev_work(),
        None => harness,
    };

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

/// The three shell activities as a typed registry — the ONE definition of
/// what the shell connection (node [`SHELL_NODE`]) serves; the routing tests
/// read the served set from here so it can never drift from production.
fn shell_registry(shell: Shell) -> Result<ActivityRegistry, aion_worker::WorkerError> {
    ActivityRegistry::new()
        .register_activity(
            "provision_workspace",
            blocking(shell.clone(), handlers::provision),
        )?
        .register_activity("run_gates", blocking(shell.clone(), handlers::run_gates))?
        .register_activity("cleanup_workspace", blocking(shell, handlers::cleanup))
}

/// Serve the three shell activities from a typed registry on one connection,
/// registered on the `shell` node so only shell activities are routed here.
fn serve_shell(args: &Args) -> anyhow::Result<()> {
    let identity = format!("{}-shell", args.identity_prefix);
    let config = worker_config(&identity, SHELL_NODE)?;
    let registry = Arc::new(shell_registry(Shell::inherited())?);
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
            tracing::info!("shell connection registered; serving provision/run_gates/cleanup");
        },
    )?;
    Ok(())
}

/// The shared worker config for one connection: one identity, the dev-brief
/// task queue, the connection's DISTINCT node (the routing key that separates
/// this process's three same-queue connections — see [`SHELL_NODE`]), and an
/// effectively unbounded reconnect budget (a long-lived worker must outwait
/// server restarts).
fn worker_config(identity: &str, node: &str) -> Result<WorkerConfig, WorkerConfigBuildError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue(TASK_QUEUE)
        .node(node)
        .identity(identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(usize::MAX)
        .build()
}

/// Adapt a synchronous, blocking handler body onto the worker SDK's async
/// handler signature — the shell bodies block on git and the gate commands,
/// so each invocation moves to the blocking thread pool.
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
    let mut identity_prefix = "dev-brief-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = default_norn_bin;
    let mut profiles_dir: Option<String> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => candidates.push(next_value(&mut args, "--address")?),
            "--identity" => identity_prefix = next_value(&mut args, "--identity")?,
            "--ready-file" => ready_file = Some(next_value(&mut args, "--ready-file")?),
            "--norn-bin" => norn_bin = next_value(&mut args, "--norn-bin")?,
            "--profiles-dir" => profiles_dir = Some(next_value(&mut args, "--profiles-dir")?),
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push(DEFAULT_ADDRESS.to_owned());
    }
    let profiles_dir = profiles_dir.ok_or_else(|| {
        anyhow::anyhow!("--profiles-dir is required (point it at this package's worker/profiles/)")
    })?;
    Ok(Args {
        candidates,
        identity_prefix,
        ready_file,
        norn_bin,
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
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        DEFAULT_ADDRESS, PostRunCommit, Profiles, SHELL_NODE, Shell, parse_args_from, roles,
        shell_registry,
    };

    fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
        parse_args_from(
            args.iter().map(|arg| (*arg).to_owned()),
            "norn-default".to_owned(),
        )
    }

    fn profiles() -> Profiles {
        Profiles {
            developer: "dev".to_owned(),
            reviewer: "rev".to_owned(),
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
        let args = parse(&["--profiles-dir", "/pkg/profiles"])?;
        assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
        assert_eq!(args.identity_prefix, "dev-brief-worker");
        assert_eq!(args.ready_file, None);
        assert_eq!(args.norn_bin, "norn-default");
        assert_eq!(args.profiles_dir, "/pkg/profiles");
        Ok(())
    }

    #[test]
    fn every_value_taking_flag_bails_when_missing() {
        for flag in [
            "--address",
            "--identity",
            "--ready-file",
            "--norn-bin",
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
    /// profile, and workspace root — the brief worktree for the developer,
    /// a per-lens scratch root for the reviewer.
    #[test]
    fn roles_bind_schema_session_profile_and_workspace() {
        let roles = roles(profiles());
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
                ("developer", "developer", "/tmp/aion-dev/ws/{workflow_id}"),
                ("review_lens", "reviewer", "/tmp/aion-dev/review-scratch"),
            ]
        );
        for role in &roles {
            assert!(role.output_schema.trim_start().starts_with('{'));
            assert!(!role.profile.is_empty());
        }
    }

    /// The mechanical-git doctrine's wiring, the FULL table: the developer
    /// commits its round's work (the report's `commits` is rewritten to the
    /// real head), and no other role's harness may grow a silent git side
    /// effect.
    #[test]
    fn post_run_commits_are_wired_per_role_exactly() {
        let table: Vec<(&str, Option<PostRunCommit>)> = roles(profiles())
            .iter()
            .map(|role| (role.activity_type, role.post_run_commit))
            .collect();
        assert_eq!(
            table,
            vec![
                ("developer", Some(PostRunCommit::DevWork)),
                ("review_lens", None),
            ]
        );
    }

    /// The routing contract this worker's three connections uphold: the
    /// server routes by (namespace, `task_queue`, node) only, so the node
    /// table must be INJECTIVE (no two connections share a node) and
    /// EXHAUSTIVE (every served activity type maps to exactly one node).
    /// Reads the served sets from the production `roles`/`shell_registry`
    /// definitions so the guard cannot drift from what actually registers.
    #[test]
    fn node_mapping_is_exhaustive_and_injective() {
        let roles = roles(profiles());

        let mut nodes: BTreeSet<&str> = roles.iter().map(|role| role.node).collect();
        assert_eq!(nodes.len(), roles.len(), "two agent roles share a node");
        assert!(
            nodes.insert(SHELL_NODE),
            "an agent role reuses the shell connection's node"
        );

        let registry = shell_registry(Shell::inherited()).expect("the shell registry builds");
        let mut activity_to_node: BTreeMap<String, &str> = BTreeMap::new();
        for activity_type in registry.activity_types() {
            assert!(
                activity_to_node
                    .insert(activity_type.clone(), SHELL_NODE)
                    .is_none(),
                "shell activity `{activity_type}` registered twice"
            );
        }
        for role in &roles {
            assert!(
                activity_to_node
                    .insert(role.activity_type.to_owned(), role.node)
                    .is_none(),
                "activity `{}` is served on two nodes",
                role.activity_type
            );
        }
        assert_eq!(
            activity_to_node
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "cleanup_workspace",
                "developer",
                "provision_workspace",
                "review_lens",
                "run_gates",
            ],
            "the served activity-type set changed; update the node table \
             (worker AND src/dev_brief/activities.gleam) together"
        );
    }

    /// Pin the exact node-id strings to the workflow-side source of truth
    /// (`shell_node`/`developer_node`/`reviewer_node` in
    /// `src/dev_brief/activities.gleam`). The server matches these strings
    /// blindly; a drift on either side strands activities on handlerless
    /// connections.
    #[test]
    fn node_ids_mirror_the_workflow_constants() {
        assert_eq!(SHELL_NODE, "shell");
        let nodes: Vec<&str> = roles(profiles()).iter().map(|role| role.node).collect();
        assert_eq!(nodes, vec!["developer", "reviewer"]);
    }
}
