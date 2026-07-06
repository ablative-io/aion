//! [`ProfiledNornHarness`] — the thin harness wrapper that assembles the
//! role prompt before delegating to the real [`NornHarness`].
//!
//! WHY A WRAPPER: the remediation prompt is {profile markdown, loaded at
//! startup} + {the activity's per-run context JSON}. The profile cannot ride
//! in the activity input (the workflow does not have it — it is a worker-side
//! `--profiles-dir` concern), and `NornHarness` derives its prompt from the
//! input payload alone. So the wrapper intercepts exactly one seam: it reads
//! the spec's input payload (the context JSON the workflow encoded), runs the
//! role's ONE assembly function (`crate::prompts`), and hands the inner
//! harness the same spec with its input replaced by the assembled prompt as a
//! JSON string — which `NornHarness` unwraps verbatim. Everything else
//! (driven mode, jsonrpc, `--output-schema`, `{workflow_id}` session
//! identity, env hygiene) stays the inner harness's, untouched.
//!
//! THE MECHANICAL-GIT SEAM (doctrine: agents do not run git — the machinery
//! does): a role built with a [`PostRunCommit`] mode gets a session that
//! intercepts the terminal result too:
//!
//! - [`PostRunCommit::AuthoredTests`] (the test-author): parse the manifest,
//!   commit the claimed test files in the brief workspace under the scoped
//!   machinery identity (`crate::commit::commit_authored_tests`).
//! - [`PostRunCommit::FixWork`] (the developer): commit the tracked delta
//!   plus report-named new test files (`crate::commit::commit_fix_work`),
//!   then REWRITE the activity result's `commits` to the real branch head —
//!   the agent never ran git, so its asserted hashes are fabricated;
//!   reality wins, and the ledger/verdict downstream see a hash that exists.
//!
//! A report claiming a file the agent never wrote fails the ACTIVITY loudly,
//! naming the path.

use aion_integration_norn::NornHarness;
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::{
    ActivityEvent, AgentRunSpec, ContentType, HarnessError, InterventionCapabilities,
    InterventionCommand, Payload,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::commit::{self, CommitContext};
use crate::prompts::AssembleFn;
use crate::shell::Shell;

/// Which mechanical commit a role's session performs after a successful turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostRunCommit {
    /// Commit the manifest's authored test files (the test-author role).
    AuthoredTests,
    /// Commit the tracked fix delta + report-named new test files, and
    /// rewrite the result's `commits` to the real head (the developer role).
    FixWork,
}

/// A per-role harness: the composed inner [`NornHarness`], the role's profile
/// markdown (loaded once at startup), and the role's prompt assembly
/// function.
#[derive(Clone, Debug)]
pub struct ProfiledNornHarness {
    inner: NornHarness,
    profile: String,
    assemble: AssembleFn,
    post_run_commit: Option<PostRunCommit>,
}

impl ProfiledNornHarness {
    /// Wrap a composed inner harness with a role profile and its assembly
    /// function.
    #[must_use]
    pub fn new(inner: NornHarness, profile: String, assemble: AssembleFn) -> Self {
        Self {
            inner,
            profile,
            assemble,
            post_run_commit: None,
        }
    }

    /// Enable the test-author's post-turn commit: after a successful run this
    /// harness's session commits the manifest's test files in the brief
    /// workspace. Requires the activity input to carry `brief.id` and
    /// `workspace_path`.
    #[must_use]
    pub fn committing_authored_tests(mut self) -> Self {
        self.post_run_commit = Some(PostRunCommit::AuthoredTests);
        self
    }

    /// Enable the developer's post-turn commit: after a successful run this
    /// harness's session commits the fix work (tracked delta + report-named
    /// new test files) and rewrites the result's `commits` to the real branch
    /// head. Requires the activity input to carry `brief.id` and
    /// `workspace_path`.
    #[must_use]
    pub fn committing_fix_work(mut self) -> Self {
        self.post_run_commit = Some(PostRunCommit::FixWork);
        self
    }

    /// Assemble the prompt this harness would send for `context_json` —
    /// exposed so tests exercise the exact production assembly path.
    #[must_use]
    pub fn assembled_prompt(&self, context_json: &str) -> String {
        (self.assemble)(&self.profile, context_json)
    }
}

#[async_trait]
impl AgentHarness for ProfiledNornHarness {
    type Session = ProfiledSession;

    async fn start(&self, mut spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        // The input payload is the workflow-encoded context JSON (an object;
        // for the test-author, already recommendation-free by construction).
        let context_json = std::str::from_utf8(spec.input.bytes())
            .map_err(|source| {
                HarnessError::protocol(format!("run input is not valid UTF-8: {source}"))
            })?
            .to_owned();
        // Resolve the commit plan BEFORE the run: an input that cannot name
        // the workspace must fail here, not after an expensive agent turn.
        let commit = match self.post_run_commit {
            Some(kind) => Some((
                kind,
                commit::context_from_input(&context_json).map_err(HarnessError::protocol)?,
            )),
            None => None,
        };
        let prompt = self.assembled_prompt(&context_json);
        // Re-encode as a JSON string so the inner harness's prompt derivation
        // unwraps it to the exact assembled text.
        spec.input = Payload::from_json(&Value::String(prompt)).map_err(|source| {
            HarnessError::protocol(format!("could not encode the assembled prompt: {source}"))
        })?;
        let inner = self.inner.start(spec).await?;
        Ok(ProfiledSession { inner, commit })
    }
}

/// The wrapper session: everything delegates to the inner Norn session; for
/// the committing roles, [`AgentSession::wait_result`] additionally performs
/// the role's mechanical commit after a successful turn (see
/// [`crate::commit`]).
pub struct ProfiledSession {
    inner: <NornHarness as AgentHarness>::Session,
    commit: Option<(PostRunCommit, CommitContext)>,
}

#[async_trait]
impl AgentSession for ProfiledSession {
    fn capabilities(&self) -> &InterventionCapabilities {
        self.inner.capabilities()
    }

    fn events(&mut self) -> BoxStream<'static, ActivityEvent> {
        self.inner.events()
    }

    async fn intervene(&self, cmd: InterventionCommand) -> Result<(), HarnessError> {
        self.inner.intervene(cmd).await
    }

    async fn wait_result(self) -> Result<Payload, HarnessError> {
        let Self { inner, commit } = self;
        let payload = inner.wait_result().await?;
        match commit {
            None => Ok(payload),
            Some((PostRunCommit::AuthoredTests, context)) => {
                commit_authored_tests(&context, payload.bytes()).await?;
                Ok(payload)
            }
            Some((PostRunCommit::FixWork, context)) => commit_fix_work(&context, payload).await,
        }
    }
}

/// The test-author's post-turn commit step: parse the manifest slice out of
/// the terminal output and commit the claimed test files. Any refusal is an
/// activity failure — a manifest the workspace cannot honor must surface,
/// never be swallowed into a green turn.
async fn commit_authored_tests(context: &CommitContext, output: &[u8]) -> Result<(), HarnessError> {
    let manifest = commit::manifest_from_output(output).map_err(HarnessError::harness)?;
    let workspace_path = context.workspace_path.clone();
    let brief_id = context.brief.id.clone();
    // The git commands block; hop to the blocking pool (the worker drives
    // this session inside its async runtime).
    let outcome = tokio::task::spawn_blocking(move || {
        commit::commit_authored_tests(&Shell::inherited(), &workspace_path, &brief_id, &manifest)
    })
    .await
    .map_err(|join_error| {
        HarnessError::harness(format!(
            "authored-test commit task did not complete: {join_error}"
        ))
    })?
    .map_err(|error| HarnessError::harness(format!("authored-test commit failed: {error}")))?;
    match outcome {
        commit::CommitOutcome::Committed { commit, paths } => {
            tracing::info!(%commit, ?paths, "authored tests committed on the brief branch");
        }
        commit::CommitOutcome::Skipped { reason } => {
            tracing::info!(%reason, "authored-test commit skipped");
        }
    }
    Ok(())
}

/// The developer's post-turn commit step: commit the fix work, then rewrite
/// the result's `commits` to the real branch head (reality wins — the agent
/// never ran git, so downstream must see a hash that exists). Any refusal is
/// an activity failure.
async fn commit_fix_work(
    context: &CommitContext,
    payload: Payload,
) -> Result<Payload, HarnessError> {
    let report = commit::fix_report_from_output(payload.bytes()).map_err(HarnessError::harness)?;
    let workspace_path = context.workspace_path.clone();
    let brief_id = context.brief.id.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        commit::commit_fix_work(&Shell::inherited(), &workspace_path, &brief_id, &report)
    })
    .await
    .map_err(|join_error| {
        HarnessError::harness(format!(
            "fix-work commit task did not complete: {join_error}"
        ))
    })?
    .map_err(|error| HarnessError::harness(format!("fix-work commit failed: {error}")))?;
    match &outcome {
        commit::FixCommitOutcome::Committed { commit, paths } => {
            tracing::info!(%commit, ?paths, "fix work committed on the brief branch");
        }
        commit::FixCommitOutcome::Skipped { head, reason } => {
            tracing::info!(%head, %reason, "fix-work commit skipped");
        }
    }
    let rewritten = commit::rewrite_report_commits(payload.bytes(), outcome.head())
        .map_err(HarnessError::harness)?;
    Ok(Payload::new(ContentType::Json, rewritten))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::ProfiledNornHarness;
    use aion_integration_norn::NornHarness;

    #[test]
    fn the_assembled_prompt_is_the_role_function_applied_to_the_loaded_profile() {
        let harness = ProfiledNornHarness::new(
            NornHarness::new(),
            "# Verifier\nrefute with evidence".to_owned(),
            crate::prompts::verifier,
        );
        let prompt = harness.assembled_prompt("{\"diff\":\"...\"}");
        assert!(prompt.starts_with("# Verifier"));
        assert!(prompt.contains("refute with evidence"));
        assert!(prompt.contains("{\"diff\":\"...\"}"));
    }
}
