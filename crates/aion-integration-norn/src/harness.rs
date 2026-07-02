//! [`NornHarness`] — the [`AgentHarness`] implementation that drives a real Norn process.
//!
//! [`AgentHarness::start`] spawns `norn --protocol jsonrpc` with piped stdin/stdout (stderr is
//! inherited so Norn's human logs go straight to the parent's stderr, never into the structured
//! channel), performs the `initialize` handshake — gating on the advertised
//! `protocol: "norn-driven/1"` version and reading Norn's advertised
//! [`InterventionCapabilities`] — issues the `run/execute` request carrying the spec's input as
//! the prompt, and hands the outstanding `run/execute` id to a [`NornSession`] whose reader pump
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
    env_removed: Vec<String>,
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
            env_removed: Vec::new(),
        }
    }

    /// A harness that invokes the `norn` binary at an explicit path.
    #[must_use]
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            extra_args: Vec::new(),
            env: Vec::new(),
            env_removed: Vec::new(),
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

    /// Removes an environment variable from every spawned `norn` process.
    ///
    /// Applied to the CHILD only (never the parent process), so a harness can strip ambient
    /// configuration a spawned Norn must not inherit — e.g. removing `OPENAI_API_KEY` forces
    /// Norn onto the operator's `ChatGPT` OAuth login when a stray ambient key would otherwise
    /// take precedence and fail. Removals are applied AFTER the explicit [`Self::with_env`]
    /// sets, so removing a key wins over setting the same key.
    #[must_use]
    pub fn without_env(mut self, key: impl Into<String>) -> Self {
        self.env_removed.push(key.into());
        self
    }

    /// Builds the `norn --protocol jsonrpc` command for one run: the fixed protocol arguments
    /// followed by the configured extra arguments with their run-identity placeholders expanded
    /// from `spec` (see [`expand_arg`]), the configured child-environment sets and removals,
    /// with piped stdin/stdout and inherited stderr.
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
        // Removals AFTER the sets, so `without_env` wins over `with_env` for the same key.
        for key in &self.env_removed {
            command.env_remove(key);
        }
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

/// Reads the prompt string an `AgentRunSpec` carries from its input payload.
///
/// Deliberate decoding semantics — the activity input is a serialized [`Payload`], not raw prompt
/// text, so a `String`-typed activity input arrives as its JSON encoding (surrounding quotes plus
/// `\n`/`\"` escapes):
///
/// - JSON content type, bytes parse as a JSON **string** → unwrap to the inner string, so the
///   agent receives the exact text the workflow passed (multi-line prompts survive verbatim).
/// - JSON content type, bytes parse as any **other** JSON value (object/array/number/…) → the raw
///   JSON text passes through unchanged; a structured payload is a legitimate prompt for some
///   harnesses, and inventing a projection here would lose information.
/// - JSON content type, bytes are **not** valid JSON → the raw UTF-8 text passes through
///   unchanged ([`Payload`] is a dumb carrier that does not validate on construction).
/// - Non-JSON content types → the bytes decoded as UTF-8 text, today's behaviour.
///
/// A non-UTF-8 input is a protocol mismatch surfaced honestly in every case.
fn prompt_from_spec(spec: &AgentRunSpec) -> Result<String, HarnessError> {
    let text = std::str::from_utf8(spec.input.bytes())
        .map(str::to_owned)
        .map_err(|source| {
            HarnessError::protocol(format!("run input is not valid UTF-8: {source}"))
        })?;
    if !matches!(spec.input.content_type(), aion_core::ContentType::Json) {
        return Ok(text);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::String(inner)) => Ok(inner),
        _ => Ok(text),
    }
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
/// The result is gated on `protocol: "norn-driven/1"` (inside
/// [`translate::parse_capabilities`]): a missing or different version is a
/// [`HarnessError::Protocol`] naming the expected and received values — the honest "your norn
/// binary is stale" signal, raised before any run is issued.
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
    translate::parse_capabilities(&result)
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
    //! Fast unit tests of the spec-aware argument construction and prompt decoding — no process
    //! is spawned; the built [`Command`]'s argv is inspected directly (that argv is exactly what
    //! a spawn would exec) and [`prompt_from_spec`] is exercised on payloads directly.

    use aion_core::{ActivityId, ContentType, Payload, WorkflowId};

    use super::{AgentRunSpec, NornHarness, expand_arg, prompt_from_spec};

    fn spec() -> AgentRunSpec {
        spec_with_input(Payload::new(ContentType::Json, b"run".to_vec()))
    }

    fn spec_with_input(input: Payload) -> AgentRunSpec {
        AgentRunSpec::new(
            WorkflowId::new_v4(),
            ActivityId::from_sequence_position(1),
            1,
            "dev",
            input,
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
    fn removed_env_vars_are_expressed_as_child_removals() {
        let spec = spec();
        let harness = NornHarness::with_binary("/bin/norn")
            .with_env("OPENAI_API_KEY", "stray-ambient-key")
            .without_env("OPENAI_API_KEY");
        let command = harness.command(&spec);

        // On std's Command, an env REMOVAL is expressed as a `(key, None)` entry — the child
        // will not inherit the variable even when the parent environment carries it, and the
        // removal wins over an explicit set of the same key.
        let removal_expressed = command
            .as_std()
            .get_envs()
            .any(|(key, value)| key == "OPENAI_API_KEY" && value.is_none());
        assert!(
            removal_expressed,
            "without_env must express a child-side env removal on the spawned command"
        );
    }

    #[test]
    fn json_string_input_unwraps_to_the_exact_inner_text() {
        // A String-typed activity input arrives as its JSON encoding: surrounding quotes plus
        // \n / \" escapes. The prompt must be the exact multi-line text, not the encoding.
        let encoded = serde_json::to_vec(&serde_json::json!("line one\nsay \"hi\"\nline three"))
            .expect("json string encodes");
        let spec = spec_with_input(Payload::new(ContentType::Json, encoded));
        assert_eq!(
            prompt_from_spec(&spec).expect("valid UTF-8 JSON string input"),
            "line one\nsay \"hi\"\nline three"
        );
    }

    #[test]
    fn json_object_input_passes_through_as_its_json_text() {
        // A structured payload is a legitimate prompt for some harnesses: a non-string JSON
        // value must reach the agent as its raw JSON text, not be rejected or reshaped.
        let raw = r#"{"task":"build","steps":[1,2]}"#;
        let spec = spec_with_input(Payload::new(ContentType::Json, raw.as_bytes().to_vec()));
        assert_eq!(prompt_from_spec(&spec).expect("valid UTF-8 input"), raw);
    }

    #[test]
    fn json_tagged_plain_text_input_passes_through_unchanged() {
        // Payload is a dumb carrier that does not validate on construction, so JSON-tagged
        // bytes that are NOT valid JSON keep today's UTF-8 pass-through behaviour. (This is
        // also the behaviour every non-JSON content type keeps; ContentType has only `Json`
        // today, so the non-JSON arm cannot be constructed in a test yet.)
        let spec = spec_with_input(Payload::new(
            ContentType::Json,
            b"plain prompt, not json".to_vec(),
        ));
        assert_eq!(
            prompt_from_spec(&spec).expect("valid UTF-8 input"),
            "plain prompt, not json"
        );
    }

    #[test]
    fn invalid_utf8_input_still_errors() {
        let spec = spec_with_input(Payload::new(ContentType::Json, vec![0xff, 0xfe, 0xfd]));
        let error = prompt_from_spec(&spec).expect_err("non-UTF-8 input must error");
        assert!(
            error.to_string().contains("not valid UTF-8"),
            "error must name the UTF-8 mismatch, got: {error}"
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
