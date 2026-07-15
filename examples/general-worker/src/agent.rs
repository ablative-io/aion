//! Input-validating `run_agent` adapter around [`NornHarness`].

use std::path::PathBuf;

use aion_integration_norn::{NornHarness, NornSession};
use aion_integrations::{AgentHarness, AgentRunSpec, HarnessError, Payload};
use async_trait::async_trait;
use serde_json::Value;
use tokio::process::{ChildStdin, ChildStdout};

use crate::types::RunAgentInput;

const OPENAI_API_KEY: &str = "OPENAI_API_KEY";

/// A fully validated and prepared Norn invocation.
///
/// This public test seam exposes the exact child arguments, removed environment
/// variables, and rewritten neutral run specification that production passes to
/// the inner harness.
#[derive(Debug)]
pub struct PreparedAgentRun {
    /// Norn arguments, including the protocol pair added by [`NornHarness`].
    pub argv: Vec<String>,
    /// Child environment variables removed before spawn.
    pub removed_environment: Vec<String>,
    /// Run specification whose input is the prompt encoded as a JSON string.
    pub spec: AgentRunSpec,
    harness: NornHarness,
}

/// Cloneable `run_agent` adapter that adds general-worker Norn policy per run.
#[derive(Clone, Debug)]
pub struct GeneralNornHarness {
    base: NornHarness,
    base_arguments: Vec<String>,
    removed_environment: Vec<String>,
}

impl GeneralNornHarness {
    /// Construct the adapter for an explicit Norn binary.
    #[must_use]
    pub fn new(norn_binary: impl Into<PathBuf>) -> Self {
        let base_arguments = vec![
            "--fast".to_owned(),
            "--reasoning-effort".to_owned(),
            "high".to_owned(),
        ];
        let base = NornHarness::with_binary(norn_binary)
            .with_literal_arg("--fast")
            .with_literal_arg("--reasoning-effort")
            .with_literal_arg("high")
            .without_env(OPENAI_API_KEY);
        Self {
            base,
            base_arguments,
            removed_environment: vec![OPENAI_API_KEY.to_owned()],
        }
    }

    /// Validate one activity input and build the exact per-run Norn invocation.
    ///
    /// # Errors
    ///
    /// Returns a protocol error for non-UTF-8 input, malformed activity JSON,
    /// missing required fields, blank required values, or a blank supplied
    /// `session_key`.
    pub fn prepare_run(&self, mut spec: AgentRunSpec) -> Result<PreparedAgentRun, HarnessError> {
        let input_text = std::str::from_utf8(spec.input.bytes()).map_err(|source| {
            HarnessError::protocol(format!("run_agent input is not valid UTF-8: {source}"))
        })?;
        let input = serde_json::from_str::<RunAgentInput>(input_text).map_err(|source| {
            HarnessError::protocol(format!("run_agent input is invalid JSON: {source}"))
        })?;
        validate_required("instructions", &input.instructions)?;
        validate_required("prompt", &input.prompt)?;
        validate_required("output_schema", &input.output_schema)?;
        validate_required("workspace_path", &input.workspace_path)?;

        let session_key = match input.session_key {
            Some(value) => {
                if value.trim().is_empty() {
                    return Err(HarnessError::protocol(
                        "run_agent field `session_key` must be nonblank when supplied",
                    ));
                }
                value
            }
            None => format!("{}-agent", spec.workflow_id),
        };
        let output_schema = input.output_schema.trim_start().to_owned();

        let mut per_run_arguments = vec![
            "--append-system-prompt".to_owned(),
            input.instructions,
            "--output-schema".to_owned(),
            output_schema,
            "--session-id".to_owned(),
            session_key,
            "--resume-if-exists".to_owned(),
            "--workspace-root".to_owned(),
            input.workspace_path,
        ];
        if let Some(disallowed_tools) = input.disallowed_tools
            && !disallowed_tools.trim().is_empty()
        {
            per_run_arguments.push("--disallowed-tools".to_owned());
            per_run_arguments.push(disallowed_tools);
        }

        let mut harness = self.base.clone();
        for argument in &per_run_arguments {
            harness = harness.with_literal_arg(argument);
        }
        let prompt_value = Value::String(input.prompt);
        spec.input = Payload::from_json(&prompt_value).map_err(|source| {
            HarnessError::protocol(format!(
                "run_agent prompt could not be encoded as a JSON string: {source}"
            ))
        })?;

        let mut argv = vec!["--protocol".to_owned(), "jsonrpc".to_owned()];
        argv.extend(self.base_arguments.iter().cloned());
        argv.extend(per_run_arguments);

        Ok(PreparedAgentRun {
            argv,
            removed_environment: self.removed_environment.clone(),
            spec,
            harness,
        })
    }
}

#[async_trait]
impl AgentHarness for GeneralNornHarness {
    type Session = NornSession<ChildStdout, ChildStdin>;

    async fn start(&self, spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        let prepared = self.prepare_run(spec)?;
        prepared.harness.start(prepared.spec).await
    }
}

fn validate_required(field: &str, value: &str) -> Result<(), HarnessError> {
    if value.trim().is_empty() {
        return Err(HarnessError::protocol(format!(
            "run_agent field `{field}` must be a nonblank string"
        )));
    }
    Ok(())
}
