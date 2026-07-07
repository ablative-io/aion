//! Mechanical post-turn git for the developer role: commit the round's work
//! in the brief workspace on the brief branch.
//!
//! DOCTRINE: agents do not run git — the worker machinery does the mechanical
//! git around them. Without this step the brief branch retains nothing the
//! developer did, the gate diffs an empty change, and the final cleanup
//! refuses a dirty worktree.
//!
//! The developer commit stages EVERYTHING in the worktree (`git add -A`):
//! unlike the remediation flow's narrow claims-based staging, a dev brief
//! legitimately creates arbitrary new files, and the worktree is WHOLLY OWNED
//! by this brief (nothing else writes there — the driven harness's
//! `--workspace-root` is this exact directory). Honesty is preserved
//! downstream, not at staging: the gate battery runs on the full tree and the
//! adversarial lenses read the full diff, so smuggled junk is reviewable
//! junk. The staged set is recorded in the commit body.
//!
//! - the commit runs under a scoped machinery identity via `-c`, so it is
//!   attributable to the machinery without touching the workspace's git
//!   config;
//! - nothing staged (a round that changed nothing, or an activity retry after
//!   a successful commit) skips green and idempotently — never an
//!   empty-commit failure.
//!
//! REALITY WINS on the dev report's `commits`: the schema carries the field
//! but the agent never ran git, so whatever it asserts is fabricated. After
//! the mechanical commit the harness rewrites the activity RESULT's `commits`
//! to the branch head that actually embodies the reported work
//! ([`rewrite_report_commits`]), so the reviewers and the operator's handoff
//! see a real hash.

use serde::Deserialize;

use crate::shell::{CliRun, Shell};

/// The developer commit's scoped committer name — machinery identity, not an
/// agent, not the operator.
pub const DEVELOPER_COMMIT_NAME: &str = "dev-brief-developer";

/// The developer commit's scoped committer email.
pub const DEVELOPER_COMMIT_EMAIL: &str = "developer@dev-brief.local";

/// The gate-normalization commit's scoped committer name (write-mode
/// formatter output committed by [`commit_gate_normalization`]).
pub const GATES_COMMIT_NAME: &str = "dev-brief-gates";

/// The gate-normalization commit's scoped committer email.
pub const GATES_COMMIT_EMAIL: &str = "gates@dev-brief.local";

/// The slice of an agent activity INPUT the commit step needs: the brief
/// identity (for the commit message) and the provisioned workspace the work
/// lives in. Deserialized tolerantly from the workflow's context JSON
/// (`codecs.developer_input_codec` is the authoritative wire shape; extra
/// fields are ignored).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CommitContext {
    /// The brief this turn serves.
    pub brief: BriefIdentity,
    /// The brief's provisioned worktree (absolute path).
    pub workspace_path: String,
}

/// The one brief field the commit step reads.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BriefIdentity {
    /// The brief id (rides in the commit message).
    pub id: String,
}

/// What a mechanical commit step did. BOTH variants carry the branch head
/// afterwards: that hash is what the activity result's `commits` is rewritten
/// to (reality wins — on a skip, the head already embodies the reported work:
/// a previous attempt's commit, or the unchanged base when the round changed
/// nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FixCommitOutcome {
    /// The work was committed.
    Committed {
        /// The created commit's hash (the branch head afterwards).
        commit: String,
        /// The staged paths the commit carries (from git, not from claims).
        paths: Vec<String>,
    },
    /// Nothing was staged; the branch head is unchanged.
    Skipped {
        /// The branch head at skip time.
        head: String,
        /// Why nothing was committed.
        reason: String,
    },
}

impl FixCommitOutcome {
    /// The branch head that embodies the reported work after the step.
    #[must_use]
    pub fn head(&self) -> &str {
        match self {
            Self::Committed { commit, .. } => commit,
            Self::Skipped { head, .. } => head,
        }
    }
}

/// Parse an agent activity input into the commit-relevant context.
///
/// # Errors
///
/// Returns a message naming what was missing/malformed — the input codec is
/// supposed to always carry `brief.id` and `workspace_path`, so a failure
/// here is a wiring fault that must surface loudly.
pub fn context_from_input(context_json: &str) -> Result<CommitContext, String> {
    serde_json::from_str(context_json).map_err(|error| {
        format!(
            "agent activity input does not carry the commit context \
             (brief.id + workspace_path): {error}"
        )
    })
}

/// Rewrite the dev report's `commits` field to the ONE real branch head the
/// mechanical commit step produced/verified, preserving the output's envelope
/// form (plain object stays an object; a double-encoded string stays a
/// string). Reality wins: the agent never ran git, so its asserted hashes are
/// fabricated by construction and downstream (reviewers, the operator's
/// handoff) must see the real one.
///
/// # Errors
///
/// Returns a message when the output is not a JSON object document.
pub fn rewrite_report_commits(bytes: &[u8], head: &str) -> Result<Vec<u8>, String> {
    let (mut value, double_encoded) = output_json(bytes, "developer")?;
    let serde_json::Value::Object(ref mut object) = value else {
        return Err("developer output is not a JSON object; cannot rewrite `commits`".to_owned());
    };
    object.insert(
        "commits".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(head.to_owned())]),
    );
    let rewritten = if double_encoded {
        let inner = serde_json::to_string(&value)
            .map_err(|error| format!("rewritten dev report is not encodable: {error}"))?;
        serde_json::to_vec(&serde_json::Value::String(inner))
    } else {
        serde_json::to_vec(&value)
    };
    rewritten.map_err(|error| format!("rewritten dev report is not encodable: {error}"))
}

/// Decode an agent output payload to its JSON value, unwrapping the
/// double-encoded string form when present. Returns the value and whether it
/// was double-encoded (so a rewrite can preserve the envelope).
fn output_json(bytes: &[u8], role: &str) -> Result<(serde_json::Value, bool), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("{role} output is not JSON: {error}"))?;
    match value {
        serde_json::Value::String(inner) => {
            let unwrapped = serde_json::from_str(&inner).map_err(|error| {
                format!("{role} output string does not hold a JSON document: {error}")
            })?;
            Ok((unwrapped, true))
        }
        other => Ok((other, false)),
    }
}

/// Commit the developer round's work in `workspace_path` on the brief branch:
/// `git add -A` (the worktree is wholly owned by this brief; the full tree is
/// what the gate and the lenses judge) then one commit under the scoped
/// machinery identity, message `dev(<brief_id>): implementation round`.
/// Nothing staged skips green with the unchanged head — idempotent across
/// activity retries and honest for a round that changed nothing.
///
/// # Errors
///
/// `git` unable to run, or any staging/commit command exiting non-zero.
pub fn commit_dev_work(
    shell: &Shell,
    workspace_path: &str,
    brief_id: &str,
) -> Result<FixCommitOutcome, String> {
    stage_all_and_commit(
        shell,
        workspace_path,
        DEVELOPER_COMMIT_NAME,
        DEVELOPER_COMMIT_EMAIL,
        &format!("dev({brief_id}): implementation round"),
        "no changes in the worktree; nothing to commit (already committed, \
         or the round changed nothing)",
    )
}

/// Commit whatever the gate battery left dirty (write-mode formatter output
/// is the expected case) so the branch stays complete and cleanup sees a
/// clean worktree. A clean tree skips green.
///
/// # Errors
///
/// `git` unable to run, or any staging/commit command exiting non-zero.
pub fn commit_gate_normalization(
    shell: &Shell,
    workspace_path: &str,
) -> Result<FixCommitOutcome, String> {
    stage_all_and_commit(
        shell,
        workspace_path,
        GATES_COMMIT_NAME,
        GATES_COMMIT_EMAIL,
        "chore(gates): mechanical normalization from gate commands",
        "gate commands left the worktree clean; nothing to commit",
    )
}

fn stage_all_and_commit(
    shell: &Shell,
    workspace_path: &str,
    name: &str,
    email: &str,
    message: &str,
    skip_reason: &str,
) -> Result<FixCommitOutcome, String> {
    require_git_ok(shell, workspace_path, &["add", "-A"], "git add -A")?;

    let staged = staged_paths(shell, workspace_path)?;
    if staged.is_empty() {
        return Ok(FixCommitOutcome::Skipped {
            head: rev_parse_head(shell, workspace_path)?,
            reason: skip_reason.to_owned(),
        });
    }

    let full_message = format!(
        "{message}\n\nStaged paths:\n{}",
        staged
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    commit_as(shell, workspace_path, name, email, &full_message)?;

    Ok(FixCommitOutcome::Committed {
        commit: rev_parse_head(shell, workspace_path)?,
        paths: staged,
    })
}

/// The currently staged paths (index vs HEAD).
fn staged_paths(shell: &Shell, workspace_path: &str) -> Result<Vec<String>, String> {
    let staged = require_git_ok(
        shell,
        workspace_path,
        &["diff", "--cached", "--name-only"],
        "git diff --cached (staged set)",
    )?;
    Ok(staged
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// One commit under a scoped machinery identity (repo config untouched).
fn commit_as(
    shell: &Shell,
    workspace_path: &str,
    name: &str,
    email: &str,
    message: &str,
) -> Result<(), String> {
    let user_name = format!("user.name={name}");
    let user_email = format!("user.email={email}");
    require_git_ok(
        shell,
        workspace_path,
        &["-c", &user_name, "-c", &user_email, "commit", "-m", message],
        "git commit (mechanical)",
    )?;
    Ok(())
}

fn rev_parse_head(shell: &Shell, workspace_path: &str) -> Result<String, String> {
    let head = require_git_ok(
        shell,
        workspace_path,
        &["rev-parse", "HEAD"],
        "git rev-parse HEAD",
    )?;
    Ok(head.stdout.trim().to_owned())
}

/// Run one git command in the workspace; unrunnable OR non-zero is an error
/// carrying the context and captured output (the commit steps have no
/// recorded-data exits — every red is a fault).
fn require_git_ok(
    shell: &Shell,
    workspace_path: &str,
    args: &[&str],
    context: &str,
) -> Result<CliRun, String> {
    let run = shell
        .run("git", args, workspace_path)
        .map_err(|failure| format!("{context}: {}", failure.message()))?;
    if run.succeeded() {
        Ok(run)
    } else {
        Err(format!(
            "{context} exited {}: {}",
            run.exit_status,
            run.output.trim()
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{context_from_input, rewrite_report_commits};

    #[test]
    fn context_parses_the_agent_input_wire_shape() {
        // The exact codec wire shape (extra fields present, as on the wire).
        let context = context_from_input(
            "{\"brief\":{\"id\":\"DB-1\",\"objective\":\"o\"},\
             \"gate\":null,\"verdicts\":[],\"workspace_path\":\"/tmp/ws/wf-1\"}",
        )
        .expect("parses");
        assert_eq!(context.brief.id, "DB-1");
        assert_eq!(context.workspace_path, "/tmp/ws/wf-1");
    }

    #[test]
    fn rewrite_replaces_commits_with_the_real_head_in_a_plain_object() {
        let rewritten = rewrite_report_commits(
            b"{\"brief_id\":\"DB-1\",\"commits\":[\"fabricated\"],\"summary\":\"s\"}",
            "realhead",
        )
        .expect("rewrites");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");
        assert_eq!(value["commits"], serde_json::json!(["realhead"]));
        assert_eq!(value["summary"], "s");
    }

    #[test]
    fn rewrite_preserves_the_double_encoded_envelope() {
        let inner = "{\"brief_id\":\"DB-1\",\"commits\":[],\"summary\":\"s\"}";
        let outer = serde_json::to_vec(&serde_json::Value::String(inner.to_owned())).unwrap();
        let rewritten = rewrite_report_commits(&outer, "realhead").expect("rewrites");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");
        let serde_json::Value::String(inner_rewritten) = value else {
            panic!("the double-encoded envelope must be preserved");
        };
        let inner_value: serde_json::Value = serde_json::from_str(&inner_rewritten).expect("json");
        assert_eq!(inner_value["commits"], serde_json::json!(["realhead"]));
    }

    #[test]
    fn a_missing_commit_context_is_a_loud_wiring_fault() {
        let error = context_from_input("{\"gate\":null}").expect_err("must fail");
        assert!(error.contains("brief.id"), "error was: {error}");
    }
}
