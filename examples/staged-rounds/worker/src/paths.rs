//! The destructive-path guard: the single choke point every worktree-
//! destroying git operation in this worker passes through before it runs.
//!
//! DOCTRINE: the repository itself must be UNREACHABLE by any destructive
//! path in this worker, even under a misconfigured input. A `git worktree
//! remove --force` or a forced branch reset aimed at the repo root (or
//! anywhere outside the run's worktree tree) would shred an operator's real
//! checkout. So before ANY of them, the target is canonicalized
//! (symlink-resolved) and PROVEN to be a strict descendant of
//! `<repo_root>/.staged-rounds/`. Anything else — the repo root, the
//! staged-rounds root, a sibling path, a symlink escape — is refused loudly
//! as a terminal activity failure.

use std::path::{Path, PathBuf};

use aion_worker::ActivityFailure;

/// The repo-relative directory staged-rounds run worktrees live under. MUST
/// match the `run_root` derivation in `awl/staged_rounds.awl` (step `plan`
/// binds `config.repo_root + "/.staged-rounds/" + workflow.id`).
pub const RUN_ROOT_SUBDIR: &str = ".staged-rounds";

/// Assert `workspace_path` is safe to destroy: strictly under
/// `<repo_root>/.staged-rounds/`, and never the repo root itself. Both paths
/// are canonicalized first so a symlink cannot smuggle a destructive
/// operation out of the guarded tree.
///
/// # Errors
///
/// Terminal when either path cannot be canonicalized (a destructive op on a
/// path that does not resolve is refused, not attempted), when the target is
/// the repo root, or when the target is not a strict descendant of the
/// staged-rounds root.
pub fn guard_destructive_path(
    repo_root: &str,
    workspace_path: &str,
) -> Result<(), ActivityFailure> {
    let canonical_repo = canonicalize(repo_root, "repo_root")?;
    let canonical_target = canonicalize(workspace_path, "workspace_path")?;

    if canonical_target == canonical_repo {
        return Err(ActivityFailure::terminal(format!(
            "refusing a destructive git operation: the target {} IS the repo \
             root {} — the repository must never be reachable by a destructive \
             path",
            canonical_target.display(),
            canonical_repo.display()
        )));
    }

    // The guarded tree, resolved from the canonical repo root so the prefix
    // check compares like with like (a repo behind a symlink resolves once,
    // here and for the target).
    let guarded_root = canonical_repo.join(RUN_ROOT_SUBDIR);
    if !canonical_target.starts_with(&guarded_root) || canonical_target == guarded_root {
        return Err(ActivityFailure::terminal(format!(
            "refusing a destructive git operation: the target {} is not \
             strictly under the staged-rounds root {} — a misconfigured \
             workspace_path must never let this worker destroy anything else",
            canonical_target.display(),
            guarded_root.display()
        )));
    }
    Ok(())
}

/// Canonicalize a path (symlink-resolving), turning an I/O failure into a
/// terminal activity failure naming which input could not be resolved.
fn canonicalize(path: &str, label: &str) -> Result<PathBuf, ActivityFailure> {
    Path::new(path).canonicalize().map_err(|error| {
        ActivityFailure::terminal(format!(
            "refusing a destructive git operation: {label} {path} could not be \
             canonicalized ({error}); a destructive op on an unresolvable path \
             is refused, never attempted"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{RUN_ROOT_SUBDIR, guard_destructive_path};

    /// A repo with a real staged-rounds run tree; returns (`repo_root`,
    /// a valid workspace under it). Both are real directories so
    /// canonicalization resolves.
    fn repo_with_run_tree() -> anyhow::Result<(tempfile::TempDir, String, String)> {
        let dir = tempfile::tempdir()?;
        let repo = dir.path().join("repo");
        let workspace = repo.join(RUN_ROOT_SUBDIR).join("wf-1").join("items/it-1");
        std::fs::create_dir_all(&workspace)?;
        Ok((
            dir,
            repo.display().to_string(),
            workspace.display().to_string(),
        ))
    }

    #[test]
    fn a_workspace_strictly_under_the_run_root_is_allowed() -> anyhow::Result<()> {
        let (_dir, repo, workspace) = repo_with_run_tree()?;
        guard_destructive_path(&repo, &workspace)
            .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
        Ok(())
    }

    #[test]
    fn the_repo_root_itself_is_refused() -> anyhow::Result<()> {
        let (_dir, repo, _workspace) = repo_with_run_tree()?;
        let Err(error) = guard_destructive_path(&repo, &repo) else {
            anyhow::bail!("the repo root was accepted as a destructive target");
        };
        assert!(
            error.message().contains("IS the repo root"),
            "{}",
            error.message()
        );
        Ok(())
    }

    #[test]
    fn the_staged_rounds_root_itself_is_refused() -> anyhow::Result<()> {
        // The staged-rounds root is not a per-run worktree; only strict
        // descendants are.
        let (_dir, repo, _workspace) = repo_with_run_tree()?;
        let run_root = std::path::Path::new(&repo)
            .join(RUN_ROOT_SUBDIR)
            .display()
            .to_string();
        let Err(error) = guard_destructive_path(&repo, &run_root) else {
            anyhow::bail!("the staged-rounds root was accepted as a per-run worktree");
        };
        assert!(
            error.message().contains("strictly under"),
            "{}",
            error.message()
        );
        Ok(())
    }

    #[test]
    fn a_path_outside_the_repo_is_refused() -> anyhow::Result<()> {
        let (dir, repo, _workspace) = repo_with_run_tree()?;
        // A sibling directory of the repo, real so it canonicalizes, but
        // nowhere near the guarded tree.
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside)?;
        let Err(error) = guard_destructive_path(&repo, &outside.display().to_string()) else {
            anyhow::bail!("a path outside the repo was accepted");
        };
        assert!(
            error.message().contains("strictly under"),
            "{}",
            error.message()
        );
        Ok(())
    }

    #[test]
    fn a_non_existent_target_is_refused_not_attempted() -> anyhow::Result<()> {
        let (_dir, repo, _workspace) = repo_with_run_tree()?;
        let missing = std::path::Path::new(&repo)
            .join(RUN_ROOT_SUBDIR)
            .join("does-not-exist")
            .display()
            .to_string();
        let Err(error) = guard_destructive_path(&repo, &missing) else {
            anyhow::bail!("an unresolvable target was accepted");
        };
        assert!(
            error.message().contains("could not be canonicalized"),
            "{}",
            error.message()
        );
        Ok(())
    }

    #[test]
    fn a_symlink_escaping_the_guarded_tree_is_refused() -> anyhow::Result<()> {
        // A symlink INSIDE the run root pointing OUT of the repo must not
        // let a destructive op escape: canonicalization resolves the link, and
        // the resolved target fails the prefix check.
        let (dir, repo, _workspace) = repo_with_run_tree()?;
        let outside = dir.path().join("secret-real-dir");
        std::fs::create_dir_all(&outside)?;
        let link = std::path::Path::new(&repo)
            .join(RUN_ROOT_SUBDIR)
            .join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link)?;
        #[cfg(not(unix))]
        std::os::windows::fs::symlink_dir(&outside, &link)?;
        let Err(error) = guard_destructive_path(&repo, &link.display().to_string()) else {
            anyhow::bail!("a symlink escaping the guarded tree was accepted");
        };
        assert!(
            error.message().contains("strictly under"),
            "{}",
            error.message()
        );
        Ok(())
    }
}
