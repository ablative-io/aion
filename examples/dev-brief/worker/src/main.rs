//! Composition root for the dev-brief worker.
//!
//! It serves seven activity types on ONE task queue (`dev_brief`) across
//! THREE liminal connections in this single process, each connection
//! registered on its OWN node (the third routing dimension: namespace ×
//! `task_queue` × node) so the server — which routes by those three
//! dimensions only, never by activity type — can land every dispatched
//! activity on the one connection that holds its handler:
//!
//! - two DRIVEN AGENT connections (`developer`, `reviewer`), each registered
//!   on its role's node, each with its OWN composed [`ProfiledNornHarness`]:
//!   a distinct `--output-schema`, a `{workflow_id}`-templated `--session-id`,
//!   a per-run `--workspace-root` the harness reads from the activity input's
//!   `workspace_path`, and the role's profile markdown (loaded once at
//!   startup from `--profiles-dir`, this package's `worker/profiles/`)
//!   assembled with the per-run context by the role's ONE prompt function.
//! - one SHELL connection, registered on node `shell`, serving
//!   `provision_workspace`, `run_gates`, `reset_workspace`, `verify_gates`,
//!   and `cleanup_workspace` from a typed registry, with no harness.
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

use dev_brief_worker::handlers;
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
/// The exact Norn file-mutating tool names denied to the reviewer role's
/// sessions (a comma-separated `--disallowed-tools` value). A review lens is
/// READ-ONLY: it is rooted at the run's actual worktree so it can grep the
/// code and run `git diff`, but it must never write there — so `write`,
/// `edit`, and `apply_patch` are removed from its tool set. `read`, `search`,
/// `bash`, and `lsp` stay available (a lens needs `bash` for `git diff`). The
/// post-review reset is the belt to this deny-list's braces.
const REVIEWER_DISALLOWED_TOOLS: &str = "write,edit,apply_patch";
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// One driven-agent role: its activity type, the node its connection
/// registers, its output schema, the `--session-id` role suffix, the profile
/// doctrine, the prompt assembly function, its Norn tool deny-list, and its
/// post-turn commit. The Norn `--workspace-root` is NO LONGER a role field: it
/// is now per-run data, read from each activity input's `workspace_path` by
/// the harness (both roles operate in the run's actual worktree — the
/// developer to edit it, the reviewer to read it).
struct Role {
    activity_type: &'static str,
    node: &'static str,
    output_schema: &'static str,
    session_suffix: &'static str,
    profile: String,
    assemble: prompts::AssembleFn,
    /// The comma-separated Norn tool deny-list this role's sessions run with
    /// (`--disallowed-tools`), or `None` for no deny-list. `Some` for the
    /// reviewer (file-mutating tools removed — a lens is read-only); `None`
    /// for the developer (it must write to implement the brief).
    disallowed_tools: Option<&'static str>,
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
    let profiles =
        profiles::load(Path::new(&args.profiles_dir)).map_err(|error| anyhow::anyhow!(error))?;
    tracing::info!(
        candidates = ?args.candidates,
        norn_bin = %args.norn_bin,
        profiles_dir = %args.profiles_dir,
        task_queue = TASK_QUEUE,
        "dev-brief-worker starting: 2 driven agent roles + 5 shell activities across 3 connections"
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

/// The two agent roles. Both root their Norn session at the run's ACTUAL
/// worktree, read from the activity input's `workspace_path` by the harness —
/// for the developer the `dev_brief` run's worktree (it edits the code); for
/// the reviewer the PARENT run's worktree at the exact reviewed state (it
/// reads the code, with file-mutating tools denied). The reviewer runs in the
/// `review_lens` CHILD workflow, so its own `{workflow_id}` keys a per-lens
/// session, but the parent's path can only reach it through `LensInput` — the
/// harness plumbs it, not a template.
fn roles(profiles: Profiles) -> Vec<Role> {
    vec![
        Role {
            activity_type: "developer",
            node: "developer",
            output_schema: schemas::DEV_REPORT,
            session_suffix: "developer",
            profile: profiles.developer,
            assemble: prompts::developer,
            disallowed_tools: None,
            post_run_commit: Some(PostRunCommit::DevWork),
        },
        Role {
            activity_type: "review_lens",
            node: "reviewer",
            output_schema: schemas::LENS_VERDICT,
            session_suffix: "reviewer",
            profile: profiles.reviewer,
            assemble: prompts::review_lens,
            disallowed_tools: Some(REVIEWER_DISALLOWED_TOOLS),
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
/// the driven-mode wiring (`--output-schema`, the role's profile as its
/// `--append-system-prompt` doctrine, `{workflow_id}` session identity,
/// `--resume-if-exists`, workspace root, env hygiene); the
/// [`ProfiledNornHarness`] wrapper assembles the per-turn context into the
/// prompt. This is the ONE place a concrete adapter is named per role; the
/// serve path drives it only through the erased [`DynAgentHarness`] trait.
fn composed_agent_harness(norn_bin: &str, role: &Role) -> AgentHarnessConfig {
    let harness = ProfiledNornHarness::new(inner_norn_harness(norn_bin, role), role.assemble);
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

/// Build the inner [`NornHarness`] for one role: the driven-mode wiring plus
/// the role's profile passed via `--append-system-prompt`, which APPENDS the
/// doctrine to Norn's own system instructions (never `--system-prompt`, which
/// would OVERWRITE them). The profile is a fixed argument value; it carries no
/// `{workflow_id}`/`{activity_type}` placeholder, so the harness's `expand_arg`
/// leaves it byte-identical. `--fast` / `--reasoning-effort high` are the
/// operator's speed/effort settings.
///
/// The reviewer role adds `--disallowed-tools` (its file-mutating tools
/// removed — a lens is read-only). `--workspace-root` is NOT set here: it is
/// per-run data the [`ProfiledNornHarness`] appends from the activity input's
/// `workspace_path` at start time, so both roles root at the run's actual
/// worktree rather than a static template.
fn inner_norn_harness(norn_bin: &str, role: &Role) -> NornHarness {
    let session_id = format!("{{workflow_id}}-{}", role.session_suffix);
    let mut harness = NornHarness::with_binary(norn_bin)
        .with_arg("--fast")
        .with_arg("--reasoning-effort")
        .with_arg("high")
        .with_arg("--append-system-prompt")
        .with_arg(role.profile.clone())
        .with_arg("--output-schema")
        .with_arg(role.output_schema.trim_start())
        .with_arg("--session-id")
        .with_arg(session_id)
        .with_arg("--resume-if-exists");
    if let Some(disallowed) = role.disallowed_tools {
        harness = harness.with_arg("--disallowed-tools").with_arg(disallowed);
    }
    // Force the ChatGPT OAuth login: a stray ambient API key would take
    // precedence and fail. No secret is ever set here.
    harness.without_env("OPENAI_API_KEY")
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
        .register_activity("reset_workspace", blocking(shell.clone(), handlers::reset))?
        .register_activity(
            "verify_gates",
            blocking(shell.clone(), handlers::verify_gates),
        )?
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
            tracing::info!(
                "shell connection registered; serving \
                 provision/run_gates/reset/verify_gates/cleanup"
            );
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
        DEFAULT_ADDRESS, PostRunCommit, Profiles, Role, SHELL_NODE, Shell, inner_norn_harness,
        parse_args_from, prompts, roles, shell_registry,
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
    /// profile, and tool deny-list. The workspace root is NO LONGER a role
    /// field — it is per-run data the harness reads from the activity input —
    /// so the deny-list is the discriminator the reviewer carries and the
    /// developer does not.
    #[test]
    fn roles_bind_schema_session_profile_and_deny_list() {
        let roles = roles(profiles());
        let summary: Vec<(&str, &str, Option<&str>)> = roles
            .iter()
            .map(|role| {
                (
                    role.activity_type,
                    role.session_suffix,
                    role.disallowed_tools,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("developer", "developer", None),
                ("review_lens", "reviewer", Some("write,edit,apply_patch")),
            ]
        );
        for role in &roles {
            assert!(role.output_schema.trim_start().starts_with('{'));
            assert!(!role.profile.is_empty());
        }
    }

    /// The reviewer's read-only guarantee at the process boundary: its
    /// composed Norn command carries `--disallowed-tools` naming exactly the
    /// file-mutating tools (`write`, `edit`, `apply_patch`), and the developer
    /// carries no deny-list at all (it must write to implement the brief).
    #[test]
    fn the_reviewer_denies_file_mutating_tools_and_the_developer_does_not() {
        let roles = roles(profiles());
        for role in &roles {
            let debug = format!("{:?}", inner_norn_harness("norn", role));
            match role.activity_type {
                "review_lens" => {
                    assert!(
                        debug.contains("\"--disallowed-tools\", \"write,edit,apply_patch\""),
                        "the reviewer must deny the file-mutating tools; args were:\n{debug}"
                    );
                }
                "developer" => {
                    assert!(
                        !debug.contains("--disallowed-tools"),
                        "the developer must carry no tool deny-list; args were:\n{debug}"
                    );
                }
                other => panic!("unexpected role {other}"),
            }
            // No role bakes a static --workspace-root: it is per-run input data.
            assert!(
                !debug.contains("--workspace-root"),
                "the workspace root is per-run input, never a static harness arg; \
                 args were:\n{debug}"
            );
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
                "reset_workspace",
                "review_lens",
                "run_gates",
                "verify_gates",
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

    /// The role's profile doctrine reaches Norn as `--append-system-prompt`
    /// (which APPENDS to Norn's own system prompt — never `--system-prompt`,
    /// which would OVERWRITE it), and the profile text follows that flag
    /// byte-identical. The "profile byte-identical in the prompt" contract
    /// moved here from the per-turn prompt assembly.
    #[test]
    fn the_profile_rides_as_append_system_prompt_byte_identical() {
        let role = Role {
            activity_type: "developer",
            node: "developer",
            output_schema: "{}",
            session_suffix: "developer",
            profile: "MARKER_PROFILE_TEXT".to_owned(),
            assemble: prompts::developer,
            disallowed_tools: None,
            post_run_commit: Some(PostRunCommit::DevWork),
        };
        let debug = format!("{:?}", inner_norn_harness("norn", &role));
        assert!(
            debug.contains("\"--append-system-prompt\", \"MARKER_PROFILE_TEXT\""),
            "the profile must ride as the value immediately after \
             --append-system-prompt, byte-identical; args were:\n{debug}"
        );
        assert!(
            !debug.contains("\"--system-prompt\""),
            "the doctrine must APPEND, never OVERWRITE Norn's system prompt"
        );
    }
}
