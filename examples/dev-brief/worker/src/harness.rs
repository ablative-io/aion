//! [`ProfiledNornHarness`] — the thin harness wrapper that assembles the
//! role prompt before delegating to the real [`NornHarness`].
//!
//! WHY A WRAPPER: the dev-brief per-turn prompt is the activity's per-run
//! context JSON, rendered by the role's assembly function. The role's profile
//! markdown is NOT part of that prompt — it is the role's static system
//! prompt, carried on the inner `NornHarness` as an `--append-system-prompt`
//! argument (composed in `main.rs`), so Norn APPENDS the doctrine to its own
//! system instructions rather than the doctrine overwriting them. `NornHarness`
//! derives its per-turn prompt from the input payload alone, so the wrapper
//! intercepts exactly one seam: it reads the spec's input payload (the context
//! JSON the workflow encoded), runs the role's ONE assembly function
//! (`crate::prompts`), and hands the inner harness the same spec with its input
//! replaced by the assembled context prompt as a JSON string — which
//! `NornHarness` unwraps verbatim. Everything else (driven mode, jsonrpc,
//! `--output-schema`, `--append-system-prompt` doctrine, `{workflow_id}`
//! session identity, env hygiene) stays the inner harness's, untouched.
//!
//! THE MECHANICAL-GIT SEAM (doctrine: agents do not run git — the machinery
//! does): the developer role is built with [`PostRunCommit::DevWork`] and
//! gets a session that intercepts the terminal result too: commit the round's
//! work in the brief workspace under the scoped machinery identity
//! (`crate::commit::commit_dev_work`), then REWRITE the activity result's
//! `commits` to the real branch head — the agent never ran git, so its
//! asserted hashes are fabricated; reality wins, and the reviewers downstream
//! see a hash that exists. The reviewer role writes nothing and commits
//! nothing.

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
    /// Commit the developer round's work and rewrite the result's `commits`
    /// to the real head (the developer role).
    DevWork,
}

/// A per-role harness: the composed inner [`NornHarness`] (which already
/// carries the role's profile as its `--append-system-prompt` doctrine) and
/// the role's per-turn context-assembly function.
#[derive(Clone, Debug)]
pub struct ProfiledNornHarness {
    inner: NornHarness,
    assemble: AssembleFn,
    post_run_commit: Option<PostRunCommit>,
}

impl ProfiledNornHarness {
    /// Wrap a composed inner harness with the role's context-assembly
    /// function. The profile doctrine is not held here — it rides on `inner`
    /// as its `--append-system-prompt` argument.
    #[must_use]
    pub fn new(inner: NornHarness, assemble: AssembleFn) -> Self {
        Self {
            inner,
            assemble,
            post_run_commit: None,
        }
    }

    /// Enable the developer's post-turn commit: after a successful run this
    /// harness's session commits the round's work and rewrites the result's
    /// `commits` to the real branch head. Requires the activity input to
    /// carry `brief.id` and `workspace_path`.
    #[must_use]
    pub fn committing_dev_work(mut self) -> Self {
        self.post_run_commit = Some(PostRunCommit::DevWork);
        self
    }

    /// Assemble the per-turn context prompt this harness would send for
    /// `context_json` — exposed so tests exercise the exact production
    /// assembly path. The profile is not folded in here; it is the inner
    /// harness's `--append-system-prompt` doctrine.
    #[must_use]
    pub fn assembled_prompt(&self, context_json: &str) -> String {
        (self.assemble)(context_json)
    }
}

#[async_trait]
impl AgentHarness for ProfiledNornHarness {
    type Session = ProfiledSession;

    async fn start(&self, mut spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        // The input payload is the workflow-encoded context JSON (an object).
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
/// the developer role, [`AgentSession::wait_result`] additionally performs
/// the mechanical commit after a successful turn (see [`crate::commit`]).
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
            Some((PostRunCommit::DevWork, context)) => commit_dev_work(&context, payload).await,
        }
    }
}

/// The developer's post-turn commit step: commit the round's work, then
/// rewrite the result's `commits` to the real branch head (reality wins — the
/// agent never ran git, so downstream must see a hash that exists). Any
/// refusal is an activity failure.
async fn commit_dev_work(
    context: &CommitContext,
    payload: Payload,
) -> Result<Payload, HarnessError> {
    let workspace_path = context.workspace_path.clone();
    let brief_id = context.brief.id.clone();
    // The git commands block; hop to the blocking pool (the worker drives
    // this session inside its async runtime).
    let outcome = tokio::task::spawn_blocking(move || {
        commit::commit_dev_work(&Shell::inherited(), &workspace_path, &brief_id)
    })
    .await
    .map_err(|join_error| {
        HarnessError::harness(format!(
            "dev-work commit task did not complete: {join_error}"
        ))
    })?
    .map_err(|error| HarnessError::harness(format!("dev-work commit failed: {error}")))?;
    match &outcome {
        commit::FixCommitOutcome::Committed { commit, paths } => {
            tracing::info!(%commit, ?paths, "dev work committed on the brief branch");
        }
        commit::FixCommitOutcome::Skipped { head, reason } => {
            tracing::info!(%head, %reason, "dev-work commit skipped");
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
    fn the_assembled_prompt_is_the_role_function_applied_to_the_context() {
        let harness = ProfiledNornHarness::new(NornHarness::new(), crate::prompts::review_lens);
        let prompt = harness.assembled_prompt("{\"diff\":\"...\"}");
        // The per-turn prompt is context only — the profile doctrine is the
        // inner harness's `--append-system-prompt` text, never folded in here.
        assert!(!prompt.contains("# Reviewer"));
        assert!(prompt.contains("{\"diff\":\"...\"}"));
        assert!(prompt.contains("```json"));
    }
}
