//! [`ProfiledNornHarness`] — the thin harness wrapper that assembles the
//! role prompt and derives the per-run session identity before delegating to
//! the real [`NornHarness`].
//!
//! WHY A WRAPPER: the per-turn prompt is the activity's per-run context
//! JSON, rendered by the role's assembly function; the role's profile
//! markdown is the session's `--append-system-prompt` doctrine on the inner
//! harness (composed in `main.rs`). The wrapper intercepts exactly one seam:
//! it reads the spec's input payload, runs the role's ONE assembly function
//! (`crate::prompts`), and hands the inner harness the same spec with its
//! input replaced by the assembled prompt as a JSON string.
//!
//! PER-RUN SESSION IDENTITY (the mechanic dev-brief's template could not
//! express): the norn arg template expands only `{workflow_id}` and
//! `{activity_type}`, so a per-ITEM session id cannot ride the template.
//! The wrapper therefore appends `--session-id <workflow_id>-<suffix>` and
//! `--resume-if-exists` per run in `start()`, deriving the suffix from the
//! role's extractor over the input JSON — `dev-<item id>` and
//! `review-<item id>` for the per-item roles, and the constant `planner`
//! for BOTH the planner and the remediator: the remediator RESUMES the
//! planner's session, so the persistent coordinator judges its own plan's
//! merge conflicts. `--workspace-root` is appended the same way (dev-brief's
//! proven per-run-append seam).
//!
//! THE MECHANICAL-GIT SEAM (doctrine: agents do not run git — the machinery
//! does): the developer role commits the round's work in the item worktree
//! and the activity result is ASSEMBLED around the agent's report with the
//! real coordinates and the real head ([`crate::commit`]); the remediator
//! role's session concludes the in-progress merge it resolved. The planner
//! and reviewer write nothing and commit nothing.

use aion_integration_norn::NornHarness;
use aion_integrations::contract::{AgentHarness, AgentSession};
use aion_integrations::{
    ActivityEvent, AgentRunSpec, ContentType, HarnessError, InterventionCapabilities,
    InterventionCommand, Payload,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::Value;

use crate::commit;
use crate::prompts::AssembleFn;
use crate::shell::Shell;
use crate::types::ProvisionedItem;

/// Which mechanical git step a role's session performs after a successful
/// turn, with the context it resolved from the activity input BEFORE the
/// run (an input that cannot name its workspace must fail before an
/// expensive agent turn, not after).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostRunPlan {
    /// Commit the dev round's work in the item worktree and assemble the
    /// `DevItemResult` payload around the agent's report (developer role).
    DevWork {
        /// The provisioned item coordinates from the activity input
        /// (boxed: the item rides its whole `WorkItem`, which dwarfs the
        /// other variant).
        work: Box<ProvisionedItem>,
    },
    /// Conclude the in-progress merge in the integration worktree
    /// (remediator role).
    ConcludeMerge {
        /// The integration worktree from the activity input.
        workspace_path: String,
    },
}

/// What a role's extractor derives from one activity input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleContext {
    /// The worktree the session is rooted at (`--workspace-root`).
    pub workspace_root: String,
    /// The per-run session suffix (`--session-id <workflow_id>-<suffix>`).
    pub session_suffix: String,
    /// The mechanical git step after a successful turn, when the role has
    /// one.
    pub plan: Option<PostRunPlan>,
}

/// The signature every role's context extractor shares.
pub type ExtractFn = fn(&str) -> Result<RoleContext, String>;

/// A per-role harness: the composed inner [`NornHarness`] (which already
/// carries the role's profile as its `--append-system-prompt` doctrine),
/// the role's per-turn context-assembly function, and the role's context
/// extractor.
#[derive(Clone, Debug)]
pub struct ProfiledNornHarness {
    inner: NornHarness,
    assemble: AssembleFn,
    extract: ExtractFn,
}

impl ProfiledNornHarness {
    /// Wrap a composed inner harness with the role's context-assembly and
    /// context-extraction functions.
    #[must_use]
    pub fn new(inner: NornHarness, assemble: AssembleFn, extract: ExtractFn) -> Self {
        Self {
            inner,
            assemble,
            extract,
        }
    }

    /// Assemble the per-turn context prompt this harness would send for
    /// `context_json` — exposed so tests exercise the exact production
    /// assembly path.
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
        // Resolve the workspace root, session suffix, and commit plan BEFORE
        // the run — loudly, never with a fallback that would unconfine the
        // session or lose a mechanical commit.
        let context = (self.extract)(&context_json).map_err(HarnessError::protocol)?;
        let session_id = format!("{}-{}", spec.workflow_id, context.session_suffix);
        let prompt = self.assembled_prompt(&context_json);
        // Re-encode as a JSON string so the inner harness's prompt
        // derivation unwraps it to the exact assembled text.
        spec.input = Payload::from_json(&Value::String(prompt)).map_err(|source| {
            HarnessError::protocol(format!("could not encode the assembled prompt: {source}"))
        })?;
        // Root THIS run's session at the input's worktree and key it to the
        // derived per-run session id. The inner harness is a template; a
        // per-run clone appends the resolved values.
        let inner = self
            .inner
            .clone()
            .with_arg("--workspace-root")
            .with_arg(context.workspace_root)
            .with_arg("--session-id")
            .with_arg(session_id)
            .with_arg("--resume-if-exists")
            .start(spec)
            .await?;
        Ok(ProfiledSession {
            inner,
            plan: context.plan,
        })
    }
}

// --- the per-role context extractors -----------------------------------------

/// Planner input slice: `workspace_path` at the top level (the repo root —
/// the planner reads the actual tree).
#[derive(Debug, Deserialize)]
struct TopLevelWorkspace {
    #[serde(default)]
    workspace_path: String,
}

/// Dev-item input slice: the provisioned item under `work`.
#[derive(Debug, Deserialize)]
struct DevItemInput {
    work: ProvisionedItem,
}

/// Review-item input slice: item id + worktree under `work` (extra
/// `DevItemResult` fields are ignored).
#[derive(Debug, Deserialize)]
struct ReviewItemInput {
    work: ReviewWorkSlice,
}

#[derive(Debug, Deserialize)]
struct ReviewWorkSlice {
    item: ReviewItemSlice,
    #[serde(default)]
    workspace_path: String,
}

#[derive(Debug, Deserialize)]
struct ReviewItemSlice {
    #[serde(default)]
    id: String,
}

fn require_non_blank(value: &str, what: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!(
            "agent activity input carries an empty {what} — the harness \
             cannot root or key the session on an unnamed value"
        ));
    }
    Ok(())
}

/// The planner's extractor: rooted at the input's `workspace_path` (the
/// repo), session suffix `planner`.
///
/// # Errors
///
/// A message when the input carries no non-empty `workspace_path`.
pub fn planner_context(context_json: &str) -> Result<RoleContext, String> {
    let parsed: TopLevelWorkspace = serde_json::from_str(context_json)
        .map_err(|error| format!("planner input does not parse: {error}"))?;
    require_non_blank(&parsed.workspace_path, "workspace_path")?;
    Ok(RoleContext {
        workspace_root: parsed.workspace_path,
        session_suffix: "planner".to_owned(),
        plan: None,
    })
}

/// The developer's extractor: rooted at `work.workspace_path`, session
/// suffix `dev-<item id>`, with the [`PostRunPlan::DevWork`] commit plan.
///
/// # Errors
///
/// A message when the input carries no provisioned item, or blank
/// coordinates.
pub fn dev_item_context(context_json: &str) -> Result<RoleContext, String> {
    let parsed: DevItemInput = serde_json::from_str(context_json)
        .map_err(|error| format!("dev_item input does not carry a provisioned item: {error}"))?;
    require_non_blank(&parsed.work.workspace_path, "work.workspace_path")?;
    require_non_blank(&parsed.work.item.id, "work.item.id")?;
    Ok(RoleContext {
        workspace_root: parsed.work.workspace_path.clone(),
        session_suffix: format!("dev-{}", parsed.work.item.id),
        plan: Some(PostRunPlan::DevWork {
            work: Box::new(parsed.work),
        }),
    })
}

/// The reviewer's extractor: rooted at `work.workspace_path`, session
/// suffix `review-<item id>`, no commit plan (the reviewer writes nothing).
///
/// # Errors
///
/// A message when the input carries no item id or worktree.
pub fn review_item_context(context_json: &str) -> Result<RoleContext, String> {
    let parsed: ReviewItemInput = serde_json::from_str(context_json)
        .map_err(|error| format!("review_item input does not carry the reviewed work: {error}"))?;
    require_non_blank(&parsed.work.workspace_path, "work.workspace_path")?;
    require_non_blank(&parsed.work.item.id, "work.item.id")?;
    Ok(RoleContext {
        workspace_root: parsed.work.workspace_path,
        session_suffix: format!("review-{}", parsed.work.item.id),
        plan: None,
    })
}

/// The remediator's extractor: rooted at the input's top-level
/// `workspace_path` (the integration worktree), session suffix `planner` —
/// THE mechanic: the remediator resumes the planner's session — with the
/// [`PostRunPlan::ConcludeMerge`] plan.
///
/// # Errors
///
/// A message when the input carries no non-empty `workspace_path`.
pub fn remediate_context(context_json: &str) -> Result<RoleContext, String> {
    let parsed: TopLevelWorkspace = serde_json::from_str(context_json)
        .map_err(|error| format!("remediate input does not parse: {error}"))?;
    require_non_blank(&parsed.workspace_path, "workspace_path")?;
    Ok(RoleContext {
        workspace_root: parsed.workspace_path.clone(),
        session_suffix: "planner".to_owned(),
        plan: Some(PostRunPlan::ConcludeMerge {
            workspace_path: parsed.workspace_path,
        }),
    })
}

// --- the wrapper session ------------------------------------------------------

/// The wrapper session: everything delegates to the inner Norn session; for
/// the developer and remediator roles, [`AgentSession::wait_result`]
/// additionally performs the mechanical git step (see [`crate::commit`]).
pub struct ProfiledSession {
    inner: <NornHarness as AgentHarness>::Session,
    plan: Option<PostRunPlan>,
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
        let Self { inner, plan } = self;
        let payload = inner.wait_result().await?;
        match plan {
            None => Ok(payload),
            Some(PostRunPlan::DevWork { work }) => commit_and_assemble(*work, payload).await,
            Some(PostRunPlan::ConcludeMerge { workspace_path }) => {
                conclude(workspace_path, payload).await
            }
        }
    }
}

/// The developer's post-turn step: commit the round's work, then assemble
/// the `DevItemResult` payload around the agent's report with the real head
/// (reality wins — the agent never ran git). Any refusal is an activity
/// failure.
async fn commit_and_assemble(
    work: ProvisionedItem,
    payload: Payload,
) -> Result<Payload, HarnessError> {
    let workspace_path = work.workspace_path.clone();
    let item_id = work.item.id.clone();
    // The git commands block; hop to the blocking pool (the worker drives
    // this session inside its async runtime).
    let outcome = tokio::task::spawn_blocking(move || {
        commit::commit_dev_work(&Shell::inherited(), &workspace_path, &item_id)
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
            tracing::info!(%commit, ?paths, "dev work committed on the item branch");
        }
        commit::FixCommitOutcome::Skipped { head, reason } => {
            tracing::info!(%head, %reason, "dev-work commit skipped");
        }
    }
    let assembled = commit::assemble_dev_item_result(payload.bytes(), &work, outcome.head())
        .map_err(HarnessError::harness)?;
    Ok(Payload::new(ContentType::Json, assembled))
}

/// The remediator's post-turn step: conclude the in-progress merge (or
/// commit a dirty tree, or record a clean skip). The agent's report payload
/// passes through unchanged. Any refusal is an activity failure.
async fn conclude(workspace_path: String, payload: Payload) -> Result<Payload, HarnessError> {
    let outcome = tokio::task::spawn_blocking(move || {
        commit::conclude_merge(&Shell::inherited(), &workspace_path)
    })
    .await
    .map_err(|join_error| {
        HarnessError::harness(format!(
            "merge-conclusion task did not complete: {join_error}"
        ))
    })?
    .map_err(|error| HarnessError::harness(format!("merge conclusion failed: {error}")))?;
    match &outcome {
        commit::ConcludeOutcome::Concluded { commit } => {
            tracing::info!(%commit, "in-progress merge concluded with the remediator's resolutions");
        }
        commit::ConcludeOutcome::Committed { commit } => {
            tracing::info!(%commit, "remediation fix committed (no merge was in progress)");
        }
        commit::ConcludeOutcome::Skipped { head } => {
            tracing::info!(%head, "remediation left the tree clean; nothing to conclude");
        }
    }
    Ok(payload)
}

#[cfg(test)]
#[path = "harness/tests.rs"]
mod tests;
