//! `merge_branches`: the idempotent, continue-style integration merge.
//!
//! One integration worktree per run (created once, NEVER reset — remediation
//! commits live there). Each pass merges the accepted item branches in order,
//! skipping branches already merged. On a conflict the merge is LEFT IN
//! PROGRESS in the integration worktree — the remediator resolves it in place
//! and the machinery concludes it — and the pass returns that one conflict.
//! An unconcluded merge found at entry (remediation did not finish) is
//! re-read and returned unchanged rather than touched.

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::shell::Shell;
use crate::types::{DoneItem, MergeBranchesInput, MergeConflict, MergeState};

use super::provision::{ensure_parent_dir, reclaim_branch_worktrees, run_basename};
use super::support::{clip, require_can_run, require_run};

/// The integration merge commit's scoped committer name — machinery
/// identity, not an agent, not the operator.
pub const MERGE_COMMIT_NAME: &str = "staged-rounds-merge";

/// The integration merge commit's scoped committer email.
pub const MERGE_COMMIT_EMAIL: &str = "merge@staged-rounds.local";

/// `merge_branches`: ensure the run's integration worktree exists, then
/// merge every accepted item branch in `done` order. Returns the whole
/// [`MergeState`] the AWL merge loop threads.
///
/// # Errors
///
/// Terminal when `git` cannot run or the integration worktree cannot be
/// created. A CONFLICT is never an error — it is recorded data on the
/// returned state, with the merge left in progress for the remediator.
pub fn merge_branches(
    shell: &Shell,
    input: MergeBranchesInput,
) -> Result<MergeState, ActivityFailure> {
    let MergeBranchesInput {
        run_root,
        repo_root,
        base_branch,
        done,
        prior_evidence,
        remediation,
    } = input;
    let run_id = run_basename(&run_root)?;
    let integration_branch = format!("staged/{run_id}/integration");
    let workspace_path = format!("{run_root}/integration");

    ensure_integration_worktree(
        shell,
        &repo_root,
        &base_branch,
        &integration_branch,
        &workspace_path,
    )?;

    let mut lines: Vec<String> = Vec::new();
    if !remediation.trim().is_empty() {
        lines.push(format!("remediation: {}", remediation.trim()));
    }

    // An unconcluded merge at entry means remediation did not finish its
    // job: report the same conflict set again, untouched.
    if merge_in_progress(shell, &repo_root, &workspace_path)? {
        return unconcluded_merge_state(
            shell,
            &repo_root,
            &done,
            integration_branch,
            workspace_path,
            &prior_evidence,
            lines,
        );
    }

    let mut merged: Vec<String> = Vec::new();
    for entry in &done {
        if is_ancestor(shell, &repo_root, &workspace_path, &entry.branch)? {
            merged.push(entry.branch.clone());
            lines.push(format!("already merged {}", entry.branch));
            continue;
        }
        let message = format!("merge(staged): {}", entry.item_id);
        let user_name = format!("user.name={MERGE_COMMIT_NAME}");
        let user_email = format!("user.email={MERGE_COMMIT_EMAIL}");
        let run = require_can_run(
            shell,
            "git",
            &[
                "-C",
                &workspace_path,
                "-c",
                &user_name,
                "-c",
                &user_email,
                "merge",
                "--no-ff",
                &entry.branch,
                "-m",
                &message,
            ],
            &repo_root,
            "git merge (integration)",
        )?;
        if run.succeeded() {
            merged.push(entry.branch.clone());
            lines.push(format!("merged {}", entry.branch));
            continue;
        }
        // Conflict: capture it, LEAVE the merge in progress for the
        // remediator, and stop this pass. Branches after this one stay
        // pending and are picked up by the next pass.
        let files = conflicted_files(shell, &repo_root, &workspace_path)?;
        lines.push(format!(
            "conflict on {} ({} file(s))",
            entry.branch,
            files.len()
        ));
        return Ok(MergeState {
            integration_branch,
            workspace_path,
            merged,
            conflicts: vec![MergeConflict {
                item_id: entry.item_id.clone(),
                branch: entry.branch.clone(),
                files,
                detail: clip(run.output.trim()),
            }],
            evidence: joined_evidence(&prior_evidence, &lines),
        });
    }

    Ok(MergeState {
        integration_branch,
        workspace_path,
        merged,
        conflicts: Vec::new(),
        evidence: joined_evidence(&prior_evidence, &lines),
    })
}

/// The state returned when an UNCONCLUDED merge is found at entry
/// (remediation ran but did not finish): the same conflict set is re-read
/// from the worktree and reported again, untouched.
fn unconcluded_merge_state(
    shell: &Shell,
    repo_root: &str,
    done: &[DoneItem],
    integration_branch: String,
    workspace_path: String,
    prior_evidence: &str,
    mut lines: Vec<String>,
) -> Result<MergeState, ActivityFailure> {
    let files = conflicted_files(shell, repo_root, &workspace_path)?;
    let (item_id, branch) = merge_head_identity(shell, repo_root, done, &workspace_path)
        .unwrap_or_else(|| ("(unknown)".to_owned(), "(unknown)".to_owned()));
    lines.push(format!(
        "remediation did not conclude the in-progress merge of {branch}; \
         the same conflicts stand"
    ));
    let merged = already_merged(shell, repo_root, done, &workspace_path)?;
    Ok(MergeState {
        integration_branch,
        workspace_path,
        merged,
        conflicts: vec![MergeConflict {
            item_id,
            branch,
            files,
            detail: "the merge is still in progress in the integration \
                     worktree; conclude it by resolving every conflict"
                .to_owned(),
        }],
        evidence: joined_evidence(prior_evidence, &lines),
    })
}

/// Ensure the integration worktree exists — created from the base branch
/// ONLY when absent; an existing worktree is never reset (remediation
/// commits live there).
fn ensure_integration_worktree(
    shell: &Shell,
    repo_root: &str,
    base_branch: &str,
    integration_branch: &str,
    workspace_path: &str,
) -> Result<(), ActivityFailure> {
    if Path::new(workspace_path).is_dir() {
        return Ok(());
    }
    ensure_parent_dir(workspace_path)?;
    reclaim_branch_worktrees(shell, repo_root, integration_branch, workspace_path)?;
    require_run(
        shell,
        "git",
        &[
            "-C",
            repo_root,
            "worktree",
            "add",
            "--force",
            "-B",
            integration_branch,
            workspace_path,
            base_branch,
        ],
        repo_root,
        "git worktree add (integration)",
    )?;
    Ok(())
}

/// Whether a merge is in progress in the worktree (`MERGE_HEAD` resolves).
fn merge_in_progress(
    shell: &Shell,
    repo_root: &str,
    workspace_path: &str,
) -> Result<bool, ActivityFailure> {
    let run = require_can_run(
        shell,
        "git",
        &[
            "-C",
            workspace_path,
            "rev-parse",
            "-q",
            "--verify",
            "MERGE_HEAD",
        ],
        repo_root,
        "git rev-parse MERGE_HEAD",
    )?;
    Ok(run.succeeded())
}

/// The currently conflicted files (`git diff --name-only --diff-filter=U`).
fn conflicted_files(
    shell: &Shell,
    repo_root: &str,
    workspace_path: &str,
) -> Result<Vec<String>, ActivityFailure> {
    let run = require_run(
        shell,
        "git",
        &[
            "-C",
            workspace_path,
            "diff",
            "--name-only",
            "--diff-filter=U",
        ],
        repo_root,
        "git diff --diff-filter=U (conflicted files)",
    )?;
    Ok(run
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Identify which done item's branch the in-progress merge belongs to by
/// matching `MERGE_HEAD` against the branch heads. `None` when nothing
/// matches (reported as unknown, never a guess).
fn merge_head_identity(
    shell: &Shell,
    repo_root: &str,
    done: &[DoneItem],
    workspace_path: &str,
) -> Option<(String, String)> {
    let merge_head = shell
        .run(
            "git",
            &["-C", workspace_path, "rev-parse", "MERGE_HEAD"],
            repo_root,
        )
        .ok()
        .filter(crate::shell::CliRun::succeeded)?
        .stdout
        .trim()
        .to_owned();
    for entry in done {
        let head = shell
            .run(
                "git",
                &["-C", repo_root, "rev-parse", &entry.branch],
                repo_root,
            )
            .ok()
            .filter(crate::shell::CliRun::succeeded)?
            .stdout
            .trim()
            .to_owned();
        if head == merge_head {
            return Some((entry.item_id.clone(), entry.branch.clone()));
        }
    }
    None
}

/// The subset of done branches already reachable from the integration head.
fn already_merged(
    shell: &Shell,
    repo_root: &str,
    done: &[DoneItem],
    workspace_path: &str,
) -> Result<Vec<String>, ActivityFailure> {
    let mut merged = Vec::new();
    for entry in done {
        if is_ancestor(shell, repo_root, workspace_path, &entry.branch)? {
            merged.push(entry.branch.clone());
        }
    }
    Ok(merged)
}

/// Whether `branch` is already an ancestor of the integration worktree's
/// HEAD. Exit 0 = yes, exit 1 = no; anything else is a terminal fault.
fn is_ancestor(
    shell: &Shell,
    repo_root: &str,
    workspace_path: &str,
    branch: &str,
) -> Result<bool, ActivityFailure> {
    let run = require_can_run(
        shell,
        "git",
        &[
            "-C",
            workspace_path,
            "merge-base",
            "--is-ancestor",
            branch,
            "HEAD",
        ],
        repo_root,
        "git merge-base --is-ancestor",
    )?;
    match run.exit_status {
        0 => Ok(true),
        1 => Ok(false),
        other => Err(ActivityFailure::terminal(format!(
            "git merge-base --is-ancestor {branch} HEAD exited {other}: {}",
            clip(run.output.trim())
        ))),
    }
}

fn joined_evidence(prior: &str, lines: &[String]) -> String {
    let round = lines.join("; ");
    match (prior.is_empty(), round.is_empty()) {
        (_, true) => prior.to_owned(),
        (true, false) => round,
        (false, false) => format!("{prior}; {round}"),
    }
}
