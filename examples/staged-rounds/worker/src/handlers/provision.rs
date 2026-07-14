//! `provision_item`: create (or REUSE) the item's isolated git worktree on
//! its own branch and report the base commit the reviewer diffs against.
//!
//! REUSE, DON'T RESET — the one deliberate semantic change from dev-brief's
//! provisioning: round-2 re-provisioning of a rejected item MUST preserve the
//! committed round-1 work (the dev session resumes and continues), so an
//! existing worktree that is on the item branch and clean is returned as-is
//! with `base_commit` re-derived as the merge base against the base branch.
//! dev-brief's `-B` reset semantics would destroy the prior round's commits.

use std::path::Path;

use aion_worker::ActivityFailure;

use crate::paths;
use crate::shell::Shell;
use crate::types::{ProvisionItemInput, ProvisionedItem};

use super::support::require_run;

/// `provision_item`: ensure the item's worktree exists at
/// `<run_root>/items/<item id>` on branch
/// `staged/<run id>/<item id>` (run id = the basename of `run_root`, the
/// workflow id — so branches never collide across runs).
///
/// - Existing worktree, on the item branch, clean → REUSED as-is;
///   `base_commit` = `git merge-base <branch> <base_branch>`.
/// - Existing worktree with uncommitted changes → terminal refusal
///   (uncommitted work must never be destroyed).
/// - Existing worktree on the WRONG branch (clean) → removed (guarded),
///   then recreated fresh.
/// - Absent → stale clean holders of the branch are reclaimed (dirty
///   holders refuse loudly), then `git worktree add --force -B` creates it
///   from the base branch.
///
/// # Errors
///
/// Terminal when the item id is not a git-ref-safe slug, when `git` cannot
/// run, when a dirty worktree/holder blocks provisioning, or when the
/// worktree cannot be created.
pub fn provision_item(
    shell: &Shell,
    input: ProvisionItemInput,
) -> Result<ProvisionedItem, ActivityFailure> {
    require_slug(&input.item.id)?;
    let run_id = run_basename(&input.run_root)?;
    let workspace_path = format!("{}/items/{}", input.run_root, input.item.id);
    let branch = format!("staged/{run_id}/{}", input.item.id);

    if Path::new(&workspace_path).is_dir()
        && let Some(reused) = try_reuse(shell, &input, &workspace_path, &branch)?
    {
        return Ok(reused);
    }

    ensure_parent_dir(&workspace_path)?;
    reclaim_branch_worktrees(shell, &input.repo_root, &branch, &workspace_path)?;

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
            &branch,
            &workspace_path,
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
        &["-C", &workspace_path, "rev-parse", "HEAD"],
        &input.repo_root,
        "git rev-parse HEAD (provisioned base)",
    )?;

    Ok(ProvisionedItem {
        item: input.item,
        workspace_path,
        branch,
        base_commit: head.stdout.trim().to_owned(),
    })
}

/// Attempt the reuse path for an existing directory at `workspace_path`.
/// Returns `Ok(Some(...))` when reused, `Ok(None)` when the (clean,
/// wrong-branch) worktree was removed and provisioning should recreate it,
/// and a terminal error when the worktree is dirty.
fn try_reuse(
    shell: &Shell,
    input: &ProvisionItemInput,
    workspace_path: &str,
    branch: &str,
) -> Result<Option<ProvisionedItem>, ActivityFailure> {
    let status = require_run(
        shell,
        "git",
        &["-C", workspace_path, "status", "--porcelain"],
        &input.repo_root,
        "git status (existing item worktree)",
    )?;
    if !status.stdout.trim().is_empty() {
        return Err(ActivityFailure::terminal(format!(
            "item {} has an existing worktree at {workspace_path} with \
             UNCOMMITTED changes — refusing to destroy work; salvage or \
             remove it, then re-run",
            input.item.id
        )));
    }
    let current = require_run(
        shell,
        "git",
        &["-C", workspace_path, "rev-parse", "--abbrev-ref", "HEAD"],
        &input.repo_root,
        "git rev-parse --abbrev-ref HEAD (existing item worktree)",
    )?;
    if current.stdout.trim() != branch {
        // A clean worktree on the wrong branch is stale infrastructure:
        // remove it (guarded) and recreate fresh.
        paths::guard_destructive_path(&input.repo_root, workspace_path)?;
        require_run(
            shell,
            "git",
            &[
                "-C",
                &input.repo_root,
                "worktree",
                "remove",
                "--force",
                workspace_path,
            ],
            &input.repo_root,
            "git worktree remove (stale wrong-branch item worktree)",
        )?;
        return Ok(None);
    }
    // Reuse: the committed prior-round work is preserved; the base the
    // reviewer diffs against is the merge base with the base branch.
    let merge_base = require_run(
        shell,
        "git",
        &[
            "-C",
            &input.repo_root,
            "merge-base",
            branch,
            &input.base_branch,
        ],
        &input.repo_root,
        "git merge-base (reused item worktree)",
    )?;
    Ok(Some(ProvisionedItem {
        item: input.item.clone(),
        workspace_path: workspace_path.to_owned(),
        branch: branch.to_owned(),
        base_commit: merge_base.stdout.trim().to_owned(),
    }))
}

/// Find every OTHER worktree of `repo_root` holding `branch` and remove it
/// when clean; a dirty holder is a terminal error naming the path (operator
/// salvage beats silent destruction).
pub(super) fn reclaim_branch_worktrees(
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
        tracing::info!(%holder, %branch, "removed a stale clean worktree holding the item branch");
    }
    Ok(())
}

/// Ensure the PARENT directory of `path` exists (git worktree add needs it).
pub(super) fn ensure_parent_dir(path: &str) -> Result<(), ActivityFailure> {
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

/// Require a git-ref-safe slug: non-empty, lowercase `[a-z0-9-]`, and no
/// leading/trailing hyphen. Item ids name branches; anything else is refused
/// terminally before it can reach a git ref.
fn require_slug(id: &str) -> Result<(), ActivityFailure> {
    let valid = !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(ActivityFailure::terminal(format!(
            "item id {id:?} is not a git-ref-safe slug (lowercase [a-z0-9-], \
             no leading/trailing hyphen) — it names a branch, so it is refused"
        )))
    }
}

/// The final path component of `run_root` — the workflow id the AWL document
/// derived it from. Refused when it cannot be extracted (branch names would
/// collide across runs otherwise).
pub(super) fn run_basename(run_root: &str) -> Result<String, ActivityFailure> {
    Path::new(run_root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            ActivityFailure::terminal(format!(
                "run_root {run_root:?} has no usable final path component — \
                 the run id names branches, so it is required"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::{require_slug, run_basename};

    #[test]
    fn slugs_are_validated_strictly() {
        for good in ["a", "item-1", "split-core-2"] {
            assert!(require_slug(good).is_ok(), "{good} should be a slug");
        }
        for bad in ["", "-a", "a-", "A", "a_b", "a b", "a/b", "a..b"] {
            assert!(require_slug(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn run_basename_takes_the_final_component() -> anyhow::Result<()> {
        assert_eq!(
            run_basename("/repo/.staged-rounds/wf-42")
                .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?,
            "wf-42"
        );
        assert!(run_basename("/").is_err());
        Ok(())
    }
}
