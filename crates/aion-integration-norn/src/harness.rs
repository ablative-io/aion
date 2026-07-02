//! [`NornHarness`] — the [`AgentHarness`] implementation that drives a real Norn process.
//!
//! [`AgentHarness::start`] spawns `norn --protocol jsonrpc` with piped stdin/stdout (stderr is
//! inherited so Norn's human logs go straight to the parent's stderr, never into the structured
//! channel), performs the `initialize` handshake to read Norn's advertised
//! [`InterventionCapabilities`], issues the `run/execute` request carrying the spec's input as the
//! prompt, and hands the outstanding `run/execute` id to a [`NornSession`] whose reader pump
//! captures its Response as the terminal result.
//!
//! The harness names the `norn` binary by path but takes **no** cargo dependency on Norn: the wire
//! contract it speaks lives in [`crate::protocol`] / [`crate::translate`], mapped from Norn's
//! documented `--protocol jsonrpc` behaviour.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use aion_integrations::contract::AgentHarness;
use aion_integrations::jsonrpc::{IncomingMessage, JsonRpcConnection, JsonRpcId, JsonRpcRequest};
use aion_integrations::{AgentRunSpec, HarnessError};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::protocol;
use crate::session::NornSession;
use crate::translate::{self, EventIdentity};

/// The default binary name used when no explicit path is configured: `norn` on `PATH`.
const DEFAULT_NORN_BINARY: &str = "norn";

/// A harness that runs each activity attempt as a `norn --protocol jsonrpc` child process.
///
/// Holds the harness-specific settings (the binary location and any fixed extra arguments) so the
/// [`AgentRunSpec`] stays harness-blind — the spec never carries Norn configuration.
#[derive(Clone, Debug)]
pub struct NornHarness {
    binary: PathBuf,
    extra_args: Vec<String>,
    env: Vec<(String, String)>,
}

impl Default for NornHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl NornHarness {
    /// A harness that invokes `norn` from `PATH` with no extra arguments.
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from(DEFAULT_NORN_BINARY),
            extra_args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// A harness that invokes the `norn` binary at an explicit path.
    #[must_use]
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            extra_args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// Adds a fixed extra argument passed to every spawned `norn` process.
    ///
    /// Used to configure the harness (e.g. a provider or model flag) without leaking Norn config
    /// into the neutral [`AgentRunSpec`].
    ///
    /// The argument may carry run-identity placeholders, expanded from the [`AgentRunSpec`] at
    /// spawn time (see [`expand_arg`]): `{workflow_id}` becomes the spec's workflow id and
    /// `{activity_type}` becomes the dispatched activity-type name. Exactly those two are
    /// recognised; any other `{...}` text passes through literally.
    #[must_use]
    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.extra_args.push(arg.into());
        self
    }

    /// Sets an environment variable on every spawned `norn` process.
    ///
    /// Applied to the CHILD only (never the parent process), so a harness can supply Norn's
    /// configuration environment (e.g. a provider API-key variable) without mutating shared
    /// process state.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Builds the `norn --protocol jsonrpc` command for one run: the fixed protocol arguments
    /// followed by the configured extra arguments with their run-identity placeholders expanded
    /// from `spec` (see [`expand_arg`]), with piped stdin/stdout and inherited stderr.
    fn command(&self, spec: &AgentRunSpec) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg("--protocol")
            .arg("jsonrpc")
            .args(self.extra_args.iter().map(|arg| expand_arg(arg, spec)))
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        command
    }

    /// Spawns the `norn` child for one run, its arguments expanded from `spec`.
    fn spawn(&self, spec: &AgentRunSpec) -> Result<tokio::process::Child, HarnessError> {
        self.command(spec).spawn().map_err(|source| {
            HarnessError::transport(format!(
                "failed to spawn `{}`: {source}",
                self.binary.display()
            ))
        })
    }
}

/// Expands the run-identity placeholders in one configured extra argument.
///
/// Exactly two placeholders are recognised: `{workflow_id}` (the spec's workflow id, rendered as
/// its canonical string form) and `{activity_type}` (the activity-type name the engine
/// dispatched). Any other `{...}` text is **not** a placeholder and passes through literally, so
/// arguments containing unrelated braces (e.g. a JSON snippet) survive unchanged.
fn expand_arg(template: &str, spec: &AgentRunSpec) -> String {
    template
        .replace("{workflow_id}", &spec.workflow_id.to_string())
        .replace("{activity_type}", &spec.activity_type)
}

/// Reads the prompt string an `AgentRunSpec` carries: the input payload decoded as UTF-8 text.
///
/// The spec's input is the activity input; Norn's `run/execute` prompt is a string, so the bytes
/// are decoded as UTF-8. A non-UTF-8 input is a protocol mismatch surfaced honestly.
fn prompt_from_spec(spec: &AgentRunSpec) -> Result<String, HarnessError> {
    std::str::from_utf8(spec.input.bytes())
        .map(str::to_owned)
        .map_err(|source| HarnessError::protocol(format!("run input is not valid UTF-8: {source}")))
}

/// Takes the child's piped stdout + stdin, or a transport error if either was not piped.
fn take_child_io(
    child: &mut tokio::process::Child,
) -> Result<(ChildStdout, ChildStdin), HarnessError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::transport("spawned norn child has no piped stdout"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HarnessError::transport("spawned norn child has no piped stdin"))?;
    Ok((stdout, stdin))
}

/// Sends `initialize` and reads its Response, returning the negotiated capabilities.
///
/// The handshake is synchronous request/response before the run starts, so the pre-run reads here
/// consume only the `initialize` Response — event notifications only begin after `run/execute`.
async fn handshake<R, W>(
    connection: &JsonRpcConnection<R, W>,
) -> Result<aion_core::InterventionCapabilities, HarnessError>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    let init_id = connection.next_request_id();
    let request = JsonRpcRequest::new(init_id.clone(), protocol::METHOD_INITIALIZE, None);
    connection.send_request(&request).await?;
    let result = read_response_for(connection, &init_id).await?;
    Ok(translate::parse_capabilities(&result))
}

/// Reads inbound frames until the Response for `expected_id` arrives, returning its `result`.
///
/// Any notification arriving during the handshake (none is expected before `run/execute`, but the
/// read is defensive) is skipped; a non-matching Response or an error Response is a protocol
/// violation.
async fn read_response_for<R, W>(
    connection: &JsonRpcConnection<R, W>,
    expected_id: &JsonRpcId,
) -> Result<Value, HarnessError>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    loop {
        let message = connection
            .recv()
            .await?
            .ok_or_else(|| HarnessError::transport("norn closed the channel during handshake"))?;
        match message {
            IncomingMessage::Response(response) if &response.id == expected_id => {
                if let Some(error) = response.error {
                    return Err(HarnessError::harness(format!(
                        "initialize failed (code {}): {}",
                        error.code, error.message
                    )));
                }
                return response.result.ok_or_else(|| {
                    HarnessError::protocol("initialize response carried no result")
                });
            }
            IncomingMessage::Response(other) => {
                return Err(HarnessError::protocol(format!(
                    "unexpected response id during handshake: {:?}",
                    other.id
                )));
            }
            IncomingMessage::Notification(_) => {}
            IncomingMessage::Request(request) => {
                return Err(HarnessError::protocol(format!(
                    "unexpected child request during handshake: {}",
                    request.method
                )));
            }
        }
    }
}

#[async_trait]
impl AgentHarness for NornHarness {
    type Session = NornSession<ChildStdout, ChildStdin>;

    async fn start(&self, spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        let prompt = prompt_from_spec(&spec)?;
        let mut child = self.spawn(&spec)?;
        let (stdout, stdin) = take_child_io(&mut child)?;
        let connection = Arc::new(JsonRpcConnection::new(stdout, stdin));

        // 1. initialize handshake → capabilities.
        let capabilities = handshake(connection.as_ref()).await?;

        // 2. run/execute request. Its id is the ONLY id the terminal result is captured under; the
        //    session's reader pump routes that Response to `wait_result`.
        let run_id = connection.next_request_id();
        let run_request = JsonRpcRequest::new(
            run_id.clone(),
            protocol::METHOD_RUN_EXECUTE,
            Some(json!({ protocol::PARAM_PROMPT: prompt })),
        );
        connection.send_request(&run_request).await?;

        let identity = EventIdentity {
            workflow_id: spec.workflow_id,
            activity_id: spec.activity_id,
            attempt: spec.attempt,
        };
        Ok(NornSession::start(
            connection,
            capabilities,
            run_id,
            identity,
            Some(child),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    //! Fast unit tests of the spec-aware argument construction — no process is spawned; the built
    //! [`Command`]'s argv is inspected directly, and that argv is exactly what a spawn would exec.

    use aion_core::{ActivityId, ContentType, Payload, WorkflowId};

    use super::{AgentRunSpec, NornHarness, expand_arg};

    fn spec() -> AgentRunSpec {
        AgentRunSpec::new(
            WorkflowId::new_v4(),
            ActivityId::from_sequence_position(1),
            1,
            "dev",
            Payload::new(ContentType::Json, b"run".to_vec()),
        )
    }

    /// The argv the built command would exec, decoded as UTF-8 strings.
    fn argv(harness: &NornHarness, spec: &AgentRunSpec) -> Vec<String> {
        harness
            .command(spec)
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().expect("argv is UTF-8").to_owned())
            .collect()
    }

    #[test]
    fn expands_both_placeholders_amid_literal_text() {
        let spec = spec();
        let expanded = expand_arg("run={workflow_id}:{activity_type}!", &spec);
        assert_eq!(expanded, format!("run={}:dev!", spec.workflow_id));
    }

    #[test]
    fn argument_without_placeholders_passes_through_unchanged() {
        let spec = spec();
        assert_eq!(expand_arg("--verbose", &spec), "--verbose");
    }

    #[test]
    fn unrecognised_braced_text_is_literal() {
        // Only `{workflow_id}` / `{activity_type}` are placeholders; every other `{...}` (an
        // unknown name, a JSON snippet) is plain text and must survive verbatim.
        let spec = spec();
        assert_eq!(
            expand_arg("{attempt} {\"json\":true} {workflow-id}", &spec),
            "{attempt} {\"json\":true} {workflow-id}"
        );
    }

    #[test]
    fn spec_values_land_in_the_spawned_command_args() {
        let spec = spec();
        let harness = NornHarness::with_binary("/bin/norn")
            .with_arg("--label")
            .with_arg("{activity_type}/{workflow_id}")
            .with_arg("--model")
            .with_arg("mock-model");

        assert_eq!(
            argv(&harness, &spec),
            vec![
                "--protocol".to_owned(),
                "jsonrpc".to_owned(),
                "--label".to_owned(),
                format!("dev/{}", spec.workflow_id),
                "--model".to_owned(),
                "mock-model".to_owned(),
            ]
        );
    }
}
