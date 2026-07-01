//! [`CliHarness`] — the [`AgentHarness`] implementation that drives a plain-stdout CLI agent.
//!
//! [`AgentHarness::start`] spawns the configured command with piped stdout (stdin and stderr are
//! inherited/ignored — there is no structured channel to write, and stderr stays the agent's human
//! logs), and hands the child's stdout to a [`CliSession`] whose pump demuxes it into neutral
//! events. There is **no handshake**: a plain-stdout agent advertises nothing, so the session's
//! capability set is empty by construction (the observability-only tier).
//!
//! The harness is deliberately generic over the command it runs — it names no specific agent — so
//! it drives any line-oriented process. The spec's input is passed to the child as a single
//! argument, so a fake in-crate line-emitter can echo the run identity back for tests.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use aion_integrations::contract::AgentHarness;
use aion_integrations::{AgentRunSpec, HarnessError};
use async_trait::async_trait;
use tokio::process::{ChildStdout, Command};

use crate::demux::EventIdentity;
use crate::session::CliSession;

/// A harness that runs each activity attempt as a plain-stdout CLI child process.
///
/// Holds the harness-specific settings (the command, its fixed arguments, and its environment) so
/// the [`AgentRunSpec`] stays harness-blind — the spec never carries CLI configuration. Because the
/// agent has no control channel, the produced session's capability set is always empty.
#[derive(Clone, Debug)]
pub struct CliHarness {
    program: PathBuf,
    args: Vec<OsString>,
    env: Vec<(String, String)>,
    /// Whether to pass the spec's input payload to the child as a trailing argument.
    pass_input_as_arg: bool,
}

impl CliHarness {
    /// A harness that runs `program` with no fixed arguments and does not pass the input as an arg.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            pass_input_as_arg: false,
        }
    }

    /// Adds a fixed argument passed to every spawned child.
    #[must_use]
    pub fn with_arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Sets an environment variable on every spawned child (applied to the CHILD only).
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Passes the spec's input payload (decoded as UTF-8) to the child as a trailing argument.
    ///
    /// A plain-stdout agent has no `run/execute` request to carry the prompt, so the input is
    /// handed on argv. Off by default (some CLIs take input on stdin or a file); enabling it lets a
    /// fake line-emitter echo the run input.
    #[must_use]
    pub fn passing_input_as_arg(mut self) -> Self {
        self.pass_input_as_arg = true;
        self
    }

    /// Spawns the child with piped stdout and inherited stderr.
    fn spawn(&self, spec: &AgentRunSpec) -> Result<tokio::process::Child, HarnessError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if self.pass_input_as_arg {
            command.arg(input_arg(spec)?);
        }
        command.spawn().map_err(|source| {
            HarnessError::transport(format!(
                "failed to spawn `{}`: {source}",
                self.program.display()
            ))
        })
    }
}

/// Reads the spec's input payload as a UTF-8 argument string.
fn input_arg(spec: &AgentRunSpec) -> Result<String, HarnessError> {
    std::str::from_utf8(spec.input.bytes())
        .map(str::to_owned)
        .map_err(|source| HarnessError::protocol(format!("run input is not valid UTF-8: {source}")))
}

/// Takes the child's piped stdout, or a transport error if it was not piped.
fn take_stdout(child: &mut tokio::process::Child) -> Result<ChildStdout, HarnessError> {
    child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::transport("spawned cli child has no piped stdout"))
}

#[async_trait]
impl AgentHarness for CliHarness {
    type Session = CliSession;

    async fn start(&self, spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        let mut child = self.spawn(&spec)?;
        let stdout = take_stdout(&mut child)?;
        let identity = EventIdentity {
            workflow_id: spec.workflow_id,
            activity_id: spec.activity_id,
            attempt: spec.attempt,
        };
        Ok(CliSession::start(stdout, identity, Some(child)))
    }
}
