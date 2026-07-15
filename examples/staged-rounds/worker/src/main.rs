//! Composition root for the staged-rounds worker.
//!
//! It serves seven activity types on ONE task queue (`staged_rounds`) across
//! FIVE liminal connections in this single process, each connection
//! registered on its OWN node (the third routing dimension: namespace ×
//! `task_queue` × node) so the server — which routes by those three
//! dimensions only, never by activity type — can land every dispatched
//! activity on the one connection that holds its handler. The node strings
//! here MUST equal the `node` lines in `../awl/staged_rounds.awl` — the
//! document itself is the authoritative node table:
//!
//! - four DRIVEN AGENT connections (`planner`, `developer`, `reviewer`,
//!   `remediation`), each registered on its role's node, each with its OWN
//!   composed [`ProfiledNornHarness`]: a distinct `--output-schema`, a
//!   per-run DERIVED `--session-id` (`<workflow_id>-<suffix>` — the arg
//!   template cannot express per-item ids, so the wrapper appends it at
//!   start), a per-run `--workspace-root` read from the activity input, and
//!   the role's profile markdown (loaded once at startup from
//!   `--profiles-dir`) assembled with the per-run context by the role's ONE
//!   prompt function.
//! - one SHELL connection, registered on node `shell`, serving
//!   `provision_item`, `merge_branches`, and the pure `fold_phase` from a
//!   typed registry, with no harness.
//!
//! PARALLELISM: the engine fans items out via `workflow.map`; genuine
//! N-wide execution is this worker's `max_concurrency` on the developer and
//! reviewer connections (`--max-parallel`, default 4) — the SDK holds a
//! semaphore permit per dispatched activity.
//!
//! SESSION TOPOLOGY: the planner runs once per workflow
//! (`{workflow_id}-planner`); each item's dev agent is `-dev-<item id>`
//! (resumed across that item's feedback rounds); each item's reviewer is
//! `-review-<item id>`; and the remediator keys `{workflow_id}-planner` —
//! it RESUMES the planner's session, so the persistent coordinator judges
//! its own plan's merge conflicts.
//!
//! Norn runs with `OPENAI_API_KEY` REMOVED from its child environment so it
//! uses the operator's `ChatGPT` OAuth login, exactly like the dev-brief
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

use staged_rounds_worker::harness;
use staged_rounds_worker::harness::{ExtractFn, ProfiledNornHarness};
use staged_rounds_worker::profiles::{self, Profiles};
use staged_rounds_worker::prompts;
use staged_rounds_worker::schemas;
use staged_rounds_worker::shell::Shell;

#[path = "main_args.rs"]
mod args;
#[path = "main_shell_node.rs"]
mod shell_node;

use args::{Args, parse_args};
#[cfg(test)]
use args::{DEFAULT_ADDRESS, parse_args_from};
use shell_node::shell_registry;

/// The one task queue every staged-rounds activity is dispatched on. MUST
/// equal the document's first `worker` name (`compile.rs` makes it the
/// deploy task queue).
const TASK_QUEUE: &str = "staged_rounds";
/// The node id the SHELL connection registers (see the module doc: the node
/// strings mirror the `node` lines in `../awl/staged_rounds.awl`).
const SHELL_NODE: &str = "shell";
/// The exact Norn file-mutating tool names denied to the read-only roles'
/// sessions (a comma-separated `--disallowed-tools` value). The planner and
/// the reviewer are READ-ONLY: rooted at real trees they must inspect but
/// never write. `read`, `search`, `bash`, and `lsp` stay available.
const READ_ONLY_DISALLOWED_TOOLS: &str = "write,edit,apply_patch";
/// Concurrency for the planner and remediation connections (single-flight
/// roles; 2 leaves headroom for a retry overlapping a redial).
const COORDINATOR_CONCURRENCY: usize = 2;
/// Concurrency for the shell connection (cheap git/pure handlers).
const SHELL_CONCURRENCY: usize = 8;
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// One driven-agent role: its activity type, the node its connection
/// registers, its output schema, the profile doctrine, the prompt assembly
/// function, the context extractor (workspace root, per-run session suffix,
/// and mechanical-git plan), its Norn tool deny-list, and its connection
/// concurrency.
struct Role {
    activity_type: &'static str,
    node: &'static str,
    output_schema: &'static str,
    profile: String,
    assemble: prompts::AssembleFn,
    /// Derives the per-run workspace root, session suffix, and post-run
    /// mechanical-git plan from each activity input (the template cannot
    /// express per-item values).
    extract: ExtractFn,
    /// The comma-separated Norn tool deny-list this role's sessions run
    /// with (`--disallowed-tools`), or `None` for no deny-list. `Some` for
    /// the planner and reviewer (read-only roles); `None` for the developer
    /// and remediator (they must write).
    disallowed_tools: Option<&'static str>,
    /// This role's connection `max_concurrency` — the developer and
    /// reviewer take the operator's `--max-parallel` (the genuine
    /// parallelism knob); the coordinator roles stay narrow.
    max_concurrency: usize,
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
        max_parallel = args.max_parallel,
        task_queue = TASK_QUEUE,
        "staged-rounds-worker starting: 4 driven agent roles + 3 shell activities across 5 connections"
    );

    let roles = roles(profiles, args.max_parallel);

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

/// The four agent roles. Each roots its Norn session at the worktree its
/// extractor reads from the activity input: the planner and remediator at
/// the top-level `workspace_path` (the repo / the integration worktree),
/// the developer and reviewer at `work.workspace_path` (the item's own
/// worktree).
fn roles(profiles: Profiles, max_parallel: usize) -> Vec<Role> {
    vec![
        Role {
            activity_type: "planner",
            node: "planner",
            output_schema: schemas::PLAN,
            profile: profiles.planner,
            assemble: prompts::planner,
            extract: harness::planner_context,
            disallowed_tools: Some(READ_ONLY_DISALLOWED_TOOLS),
            max_concurrency: COORDINATOR_CONCURRENCY,
        },
        Role {
            activity_type: "dev_item",
            node: "developer",
            output_schema: schemas::ITEM_REPORT,
            profile: profiles.developer,
            assemble: prompts::dev_item,
            extract: harness::dev_item_context,
            disallowed_tools: None,
            max_concurrency: max_parallel,
        },
        Role {
            activity_type: "review_item",
            node: "reviewer",
            output_schema: schemas::ITEM_VERDICT,
            profile: profiles.reviewer,
            assemble: prompts::review_item,
            extract: harness::review_item_context,
            disallowed_tools: Some(READ_ONLY_DISALLOWED_TOOLS),
            max_concurrency: max_parallel,
        },
        Role {
            activity_type: "remediate",
            node: "remediation",
            output_schema: schemas::REMEDIATION,
            profile: profiles.remediator,
            assemble: prompts::remediate,
            extract: harness::remediate_context,
            disallowed_tools: None,
            max_concurrency: COORDINATOR_CONCURRENCY,
        },
    ]
}

/// Compose one role's harness and serve it on its own liminal connection,
/// registered on the role's node so only this role's activity is routed here.
fn serve_agent_role(candidates: &[String], identity: &str, norn_bin: &str, role: &Role) {
    let config = match worker_config(identity, role.node, role.max_concurrency) {
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

/// Build the composed harness for one role: the inner [`NornHarness`]
/// carries the driven-mode wiring (`--output-schema`, the role's profile as
/// its `--append-system-prompt` doctrine, env hygiene); the
/// [`ProfiledNornHarness`] wrapper assembles the per-turn context into the
/// prompt and appends the per-run `--workspace-root` and derived
/// `--session-id` at start. This is the ONE place a concrete adapter is
/// named per role; the serve path drives it only through the erased
/// [`DynAgentHarness`] trait.
fn composed_agent_harness(norn_bin: &str, role: &Role) -> AgentHarnessConfig {
    let harness = ProfiledNornHarness::new(
        inner_norn_harness(norn_bin, role),
        role.assemble,
        role.extract,
    );
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
/// doctrine to Norn's own system instructions (never `--system-prompt`,
/// which would OVERWRITE them). `--fast` / `--reasoning-effort high` are the
/// operator's speed/effort settings.
///
/// NEITHER `--session-id` NOR `--workspace-root` is set here: both are
/// per-run data the [`ProfiledNornHarness`] derives from each activity input
/// and appends at start time (the arg template expands only `{workflow_id}`
/// and `{activity_type}`, which cannot express a per-item session id).
fn inner_norn_harness(norn_bin: &str, role: &Role) -> NornHarness {
    let mut harness = NornHarness::with_binary(norn_bin)
        .with_arg("--fast")
        .with_arg("--reasoning-effort")
        .with_arg("high")
        .with_arg("--append-system-prompt")
        .with_arg(role.profile.clone())
        .with_arg("--output-schema")
        .with_arg(role.output_schema.trim_start());
    if let Some(disallowed) = role.disallowed_tools {
        harness = harness.with_arg("--disallowed-tools").with_arg(disallowed);
    }
    // Force the ChatGPT OAuth login: a stray ambient API key would take
    // precedence and fail. No secret is ever set here.
    harness.without_env("OPENAI_API_KEY")
}

/// Serve the three shell-node activities from a typed registry on one
/// connection, registered on the `shell` node so only shell activities are
/// routed here.
fn serve_shell(args: &Args) -> anyhow::Result<()> {
    let identity = format!("{}-shell", args.identity_prefix);
    let config = worker_config(&identity, SHELL_NODE, SHELL_CONCURRENCY)?;
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
                "shell connection registered; serving provision_item/merge_branches/fold_phase"
            );
        },
    )?;
    Ok(())
}

/// The shared worker config for one connection: one identity, the
/// staged-rounds task queue, the connection's DISTINCT node (the routing key
/// that separates this process's five same-queue connections), the
/// connection's concurrency (the developer/reviewer connections take
/// `--max-parallel` — N permits on the SDK's dispatch semaphore = N item
/// agents genuinely in flight), and an effectively unbounded reconnect
/// budget (a long-lived worker must outwait server restarts).
fn worker_config(
    identity: &str,
    node: &str,
    max_concurrency: usize,
) -> Result<WorkerConfig, WorkerConfigBuildError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue(TASK_QUEUE)
        .node(node)
        .identity(identity)
        .max_concurrency(max_concurrency)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(usize::MAX)
        .build()
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
