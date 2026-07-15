//! Shared worker topology and typed activity registry composition.

use std::sync::Arc;
use std::time::Duration;

use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityRegistry, AgentHarnessConfig, HandlerFuture, WorkerConfig,
    WorkerConfigBuildError, WorkerError,
};

use crate::agent::GeneralNornHarness;
use crate::handlers::{parse_output, run_command};
use crate::shell::Shell;
use crate::types::{CommandInput, CommandOutput, ParseInput, ParseOutput};

/// Task queue served by both worker connections.
pub const TASK_QUEUE: &str = "general";
/// Node serving the driven agent activity.
pub const AGENT_NODE: &str = "agent";
/// Node serving the two deterministic shell activities.
pub const SHELL_NODE: &str = "shell";
/// Driven agent activity name.
pub const RUN_AGENT: &str = "run_agent";
/// Arbitrary command activity name.
pub const RUN_COMMAND: &str = "run_command";
/// Deterministic parser activity name.
pub const PARSE_OUTPUT: &str = "parse_output";
/// Authoritative activity-to-node routing table.
pub const ACTIVITY_NODE_MAP: [(&str, &str); 3] = [
    (RUN_AGENT, AGENT_NODE),
    (RUN_COMMAND, SHELL_NODE),
    (PARSE_OUTPUT, SHELL_NODE),
];

/// Initial reconnect delay for both long-lived connections.
pub const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
/// Maximum reconnect delay for both long-lived connections.
pub const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Build the empty typed registry used by the agent connection.
#[must_use]
pub fn agent_registry() -> ActivityRegistry {
    ActivityRegistry::new()
}

/// Return the exact intervention capability set advertised by `run_agent`.
#[must_use]
pub fn agent_capabilities() -> InterventionCapabilities {
    InterventionCapabilities::from_primitives([
        InterventionPrimitive::InjectMessage,
        InterventionPrimitive::Cancel,
    ])
}

/// Build the agent harness advertisement for exactly `run_agent`.
#[must_use]
pub fn agent_config(norn_bin: &str) -> AgentHarnessConfig {
    let harness: Arc<dyn DynAgentHarness> = Arc::new(GeneralNornHarness::new(norn_bin));
    AgentHarnessConfig::new(harness, [RUN_AGENT], agent_capabilities())
}

/// Build the shell registry containing exactly `run_command` and `parse_output`.
///
/// # Errors
///
/// Returns [`WorkerError`] if an activity type is registered more than once.
pub fn shell_registry(shell: Shell) -> Result<ActivityRegistry, WorkerError> {
    ActivityRegistry::new()
        .register_activity(RUN_COMMAND, blocking_command(shell))?
        .register_activity(PARSE_OUTPUT, immediate_parse)
}

/// Build one node-specific long-lived worker configuration.
///
/// # Errors
///
/// Returns [`WorkerConfigBuildError`] if a required SDK configuration value is
/// rejected.
pub fn build_worker_config(
    identity: &str,
    node: &str,
) -> Result<WorkerConfig, WorkerConfigBuildError> {
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

fn blocking_command(
    shell: Shell,
) -> impl for<'context> Fn(
    CommandInput,
    &'context ActivityContext,
) -> HandlerFuture<'context, CommandOutput>
+ Send
+ Sync
+ 'static {
    move |input, context| {
        let _ = context;
        let shell = shell.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || run_command(&shell, input))
                .await
                .map_err(|source| {
                    aion_worker::ActivityFailure::terminal(format!(
                        "run_command handler task did not complete: {source}"
                    ))
                })?
        })
    }
}

fn immediate_parse(input: ParseInput, context: &ActivityContext) -> HandlerFuture<'_, ParseOutput> {
    let _ = context;
    Box::pin(async move { Ok(parse_output(input)) })
}
