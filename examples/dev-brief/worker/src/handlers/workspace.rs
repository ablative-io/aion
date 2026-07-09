//! The worktree lifecycle handlers: `provision_workspace` creates the brief's
//! isolated worktree and pins the base commit; `cleanup_workspace` removes it
//! at the end of the run (the branch and its commits remain).

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::paths;
use crate::shell::Shell;
use crate::types::{CleanupInput, CleanupOutcome, ProvisionInput, WorkspaceInfo};

use super::support::{clip, require_can_run, require_run};

// --- provision_workspace ------------------------------------------------------

/// `provision_workspace`: create the brief's isolated git worktree at
/// `workspace_path`, checking out `branch` freshly based on `base_branch`,
/// and report the base commit the gate will diff against.
///
/// Idempotent across retries: a pre-existing worktree at the path is removed
/// first, and `-B` resets the branch to `base_branch`, so a re-dispatch after
/// a crash lands in a clean, correctly-based worktree.
///
/// # Errors
///
/// Terminal when `git` cannot run, the repo is missing, or the worktree
/// cannot be created — provisioning is pure infrastructure.
pub fn provision(shell: &Shell, input: ProvisionInput) -> Result<WorkspaceInfo, ActivityFailure> {
    ensure_parent_dir(&input.workspace_path)?;

    // Best-effort removal of any stale worktree at this path (ignore failure:
    // it usually means "nothing was there").
    let _ = shell.run(
        "git",
        &[
            "-C",
            &input.repo_root,
            "worktree",
            "remove",
            "--force",
            &input.workspace_path,
        ],
        &input.repo_root,
    );

    // A PREVIOUS run of this brief may have abandoned a worktree elsewhere
    // (a failed run never reaches cleanup), and git refuses to reset a
    // branch checked out in another worktree — which would block every
    // re-run of the brief forever. Reclaim stale holders of this branch:
    // remove them when CLEAN; refuse loudly (naming the path) when dirty,
    // because uncommitted work must never be destroyed.
    reclaim_branch_worktrees(
        shell,
        &input.repo_root,
        &input.branch,
        &input.workspace_path,
    )?;

    require_run(
        shell,
        "git",
        &[
            "-C",
            &input.repo_root,
            "worktree",
            "add",
            "--force",
            "-B",
            &input.branch,
            &input.workspace_path,
            &input.base_branch,
        ],
        &input.repo_root,
        "git worktree add",
    )?;

    // The exact commit the worktree starts from. Run through the parent repo
    // with -C so the shell's cwd check never couples to the new directory.
    let head = require_run(
        shell,
        "git",
        &["-C", &input.workspace_path, "rev-parse", "HEAD"],
        &input.repo_root,
        "git rev-parse HEAD (provisioned base)",
    )?;

    Ok(WorkspaceInfo {
        workspace_path: input.workspace_path,
        branch: input.branch,
        base_commit: head.stdout.trim().to_owned(),
    })
}

/// Find every OTHER worktree of `repo_root` holding `branch` and remove it
/// when clean; a dirty holder is a terminal error naming the path (operator
/// salvage beats silent destruction).
fn reclaim_branch_worktrees(
    shell: &Shell,
    repo_root: &str,
    branch: &str,
    own_workspace: &str,
) -> Result<(), ActivityFailure> {
    let listing = require_run(
        shell,
        "git",
        &["-C", repo_root, "worktree", "list", "--porcelain"],
        repo_root,
        "git worktree list (stale branch holders)",
    )?;
    let wanted_ref = format!("refs/heads/{branch}");
    let mut current_path: Option<String> = None;
    let mut holders: Vec<String> = Vec::new();
    for line in listing.stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.trim().to_owned());
        } else if let Some(branch_ref) = line.strip_prefix("branch ")
            && branch_ref.trim() == wanted_ref
            && let Some(path) = current_path.clone()
            && path != own_workspace
        {
            holders.push(path);
        }
    }
    for holder in holders {
        let status = require_run(
            shell,
            "git",
            &["-C", &holder, "status", "--porcelain"],
            repo_root,
            "git status (stale holder dirty check)",
        )?;
        if !status.stdout.trim().is_empty() {
            return Err(ActivityFailure::terminal(format!(
                "branch {branch} is held by the stale worktree {holder}, which \
                 has UNCOMMITTED changes — refusing to destroy work; salvage \
                 or remove it, then re-run"
            )));
        }
        require_run(
            shell,
            "git",
            &["-C", repo_root, "worktree", "remove", "--force", &holder],
            repo_root,
            "git worktree remove (stale clean holder)",
        )?;
        tracing::info!(%holder, %branch, "removed a stale clean worktree holding the brief branch");
    }
    Ok(())
}

/// Ensure the PARENT directory of `path` exists (git worktree add needs it).
fn ensure_parent_dir(path: &str) -> Result<(), ActivityFailure> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ActivityFailure::terminal(format!(
                "could not create workspace parent directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    Ok(())
}

// --- cleanup_workspace -----------------------------------------------------------------

/// `cleanup_workspace`: remove the brief's worktree. The branch (and every
/// commit on it) remains in the repository.
///
/// A DIRTY worktree is never removed — `git worktree remove --force` would
/// destroy uncommitted work. It is left in place and reported.
///
/// # Errors
///
/// Terminal only when `git` cannot run at all.
pub fn cleanup(shell: &Shell, input: CleanupInput) -> Result<CleanupOutcome, ActivityFailure> {
    let CleanupInput {
        repo_root,
        workspace_path,
    } = input;
    if !Path::new(&workspace_path).is_dir() {
        return Ok(CleanupOutcome {
            removed: false,
            detail: format!("workspace not present, nothing to remove: {workspace_path}"),
        });
    }

    // Before the `git worktree remove --force` below can touch anything,
    // PROVE the target is strictly under the repo's dev-brief worktree root —
    // the repository itself must be unreachable by this destructive path even
    // under a misconfigured input.
    paths::guard_destructive_path(&repo_root, &workspace_path)?;

    let status = require_run(
        shell,
        "git",
        &["-C", &workspace_path, "status", "--porcelain"],
        &repo_root,
        "git status (cleanup dirty check)",
    )?;
    if !status.stdout.trim().is_empty() {
        return Ok(CleanupOutcome {
            removed: false,
            detail: format!(
                "worktree has uncommitted changes; left in place to avoid \
                 destroying work:\n{}",
                clip(&status.output)
            ),
        });
    }

    let removal = require_can_run(
        shell,
        "git",
        &[
            "-C",
            &repo_root,
            "worktree",
            "remove",
            "--force",
            &workspace_path,
        ],
        &repo_root,
        "git worktree remove",
    )?;
    Ok(CleanupOutcome {
        removed: removal.succeeded(),
        detail: if removal.succeeded() {
            "worktree removed; branch retained".to_owned()
        } else {
            format!(
                "git worktree remove exited {}: {}",
                removal.exit_status,
                clip(removal.output.trim())
            )
        },
    })
}
