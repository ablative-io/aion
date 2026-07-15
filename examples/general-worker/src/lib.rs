//! Standalone general-purpose Aion worker.
//!
//! The library exposes the worker's typed contracts, Norn adapter, shell
//! boundary, activity handlers, composition helpers, and command-line parser so
//! production and hermetic tests exercise the same code paths.

pub mod agent;
pub mod args;
pub mod clip;
pub mod composition;
pub mod handlers;
pub mod runtime;
pub mod shell;
pub mod types;

pub use agent::{GeneralNornHarness, PreparedAgentRun};
pub use args::{Args, ArgsError, parse_args_from};
pub use composition::{
    ACTIVITY_NODE_MAP, AGENT_NODE, PARSE_OUTPUT, RUN_AGENT, RUN_COMMAND, SHELL_NODE, TASK_QUEUE,
    agent_capabilities, agent_config, agent_registry, build_worker_config, shell_registry,
};
pub use handlers::{parse_output, run_command};
pub use runtime::run;
pub use shell::{CliFailure, CliRun, Shell};
pub use types::{CommandInput, CommandOutput, ParseInput, ParseOutput, RunAgentInput};
