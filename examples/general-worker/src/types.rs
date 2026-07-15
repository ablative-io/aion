//! Wire contracts for the three `general` task-queue activities.

use serde::{Deserialize, Serialize};

/// Input accepted by the `run_agent` activity.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RunAgentInput {
    /// System instructions appended for this run.
    pub instructions: String,
    /// User prompt delivered to Norn's `run/execute` request.
    pub prompt: String,
    /// Inline JSON Schema passed to Norn.
    pub output_schema: String,
    /// Stable Norn session key; defaults to the workflow ID plus `-agent`.
    pub session_key: Option<String>,
    /// Workspace root made available to Norn.
    pub workspace_path: String,
    /// Optional comma-separated Norn deny-list.
    pub disallowed_tools: Option<String>,
}

/// Input accepted by the `run_command` activity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CommandInput {
    /// Working directory for the child process.
    pub workspace_path: String,
    /// Human-readable command name preserved in the result.
    pub name: String,
    /// Executable followed by its arguments.
    pub argv: Vec<String>,
}

/// Output returned by the `run_command` activity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CommandOutput {
    /// Human-readable command name copied from the input.
    pub name: String,
    /// Full executable-and-arguments vector copied from the input.
    pub argv: Vec<String>,
    /// Process exit code, including shell-style signal codes on Unix.
    pub exit_code: i32,
    /// Whether `exit_code` is zero.
    pub passed: bool,
    /// Independently clipped stdout text.
    pub stdout: String,
    /// Independently clipped stdout-then-stderr text.
    pub output: String,
    /// Wall-clock process duration in milliseconds.
    pub duration_ms: u64,
}

/// Input accepted by the `parse_output` activity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ParseInput {
    /// Text to parse.
    pub text: String,
    /// Parser mode: `json_path`, `regex`, or `lines`.
    pub mode: String,
    /// Mode-specific query.
    pub query: String,
}

/// Data-level parse result returned by `parse_output`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ParseOutput {
    /// Whether the requested value was found and rendered.
    pub ok: bool,
    /// Rendered value on success, otherwise an empty string.
    pub value: String,
    /// Exact diagnostic on failure, otherwise an empty string.
    pub error: String,
}

impl ParseOutput {
    /// Construct a successful parse result.
    #[must_use]
    pub fn success(value: String) -> Self {
        Self {
            ok: true,
            value,
            error: String::new(),
        }
    }

    /// Construct a data-level parse failure.
    #[must_use]
    pub fn failure(error: String) -> Self {
        Self {
            ok: false,
            value: String::new(),
            error,
        }
    }
}
