//! Composition root for the dev-brief worker.
//!
//! It serves nine activity types on ONE task queue (`dev_brief`) across
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
//!   `cleanup_workspace`, `format_verdict_evidence`, and `fold_round` from a
//!   typed registry, with no harness.
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
    ActivityRegistry, AgentHarnessConfig, RedialTiming, WorkerConfig, WorkerConfigBuildError,
};

use dev_brief_worker::harness::{PostRunCommit, ProfiledNornHarness};
use dev_brief_worker::profiles::{self, Profiles};
use dev_brief_worker::prompts;
use dev_brief_worker::schemas;
use dev_brief_worker::shell::Shell;

#[path = "main_args.rs"]
mod args;
#[path = "main_shell_node.rs"]
mod shell_node;

use args::{Args, parse_args};
#[cfg(test)]
use args::{DEFAULT_ADDRESS, parse_args_from};
use shell_node::shell_registry;

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
        "dev-brief-worker starting: 2 driven agent roles + 7 shell activities across 3 connections"
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

/// Serve the seven shell-node activities from a typed registry on one connection,
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
                "shell connection registered; serving provision/run_gates/reset/\
                 verify_gates/cleanup/format_verdict_evidence/fold_round"
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

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
