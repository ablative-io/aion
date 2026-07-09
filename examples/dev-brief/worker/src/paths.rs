//! The destructive-path guard: the single choke point every worktree-
//! destroying git operation in this worker passes through before it runs.
//!
//! DOCTRINE: the repository itself must be UNREACHABLE by any destructive
//! path in this worker, even under a misconfigured input. A `git clean -fd`,
//! a `git checkout -- .`, or a `git worktree remove --force` aimed at the
//! repo root (or anywhere outside the brief worktree tree) would shred an
//! operator's real checkout. So before ANY of them, the target is
//! canonicalized (symlink-resolved) and PROVEN to be a strict descendant of
//! `<repo_root>/.yggdrasil-worktrees/dev-brief/`. Anything else — the repo
//! root, the worktrees root, a sibling path, a symlink escape — is refused
//! loudly as a terminal activity failure.

use std::path::{Path, PathBuf};

use aion_worker::ActivityFailure;

/// The repo-relative directory brief worktrees live under. MUST match
/// `dev_brief.worktrees_subdir` in the Gleam workflow (which derives every
/// `workspace_path` as `<repo_root>/<this>/<workflow_id>`).
pub const WORKTREES_SUBDIR: &str = ".yggdrasil-worktrees/dev-brief";

/// Assert `workspace_path` is safe to destroy: strictly under
/// `<repo_root>/.yggdrasil-worktrees/dev-brief/`, and never the repo root
/// itself. Both paths are canonicalized first so a symlink cannot smuggle a
/// destructive operation out of the guarded tree.
///
/// # Errors
///
/// Terminal when either path cannot be canonicalized (a destructive op on a
/// path that does not resolve is refused, not attempted), when the target is
/// the repo root, or when the target is not a strict descendant of the
/// dev-brief worktree root.
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
    let guarded_root = canonical_repo.join(WORKTREES_SUBDIR);
    if !canonical_target.starts_with(&guarded_root) || canonical_target == guarded_root {
        return Err(ActivityFailure::terminal(format!(
            "refusing a destructive git operation: the target {} is not \
             strictly under the dev-brief worktree root {} — a misconfigured \
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{WORKTREES_SUBDIR, guard_destructive_path};

    /// A repo with a real dev-brief worktree tree; returns (`repo_root`,
    /// a valid workspace under it). Both are real directories so
    /// canonicalization resolves.
    fn repo_with_worktree() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let workspace = repo.join(WORKTREES_SUBDIR).join("wf-1");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        (
            dir,
            repo.display().to_string(),
            workspace.display().to_string(),
        )
    }

    #[test]
    fn a_workspace_strictly_under_the_worktree_root_is_allowed() {
        let (_dir, repo, workspace) = repo_with_worktree();
        guard_destructive_path(&repo, &workspace).expect("a valid worktree is allowed");
    }

    #[test]
    fn the_repo_root_itself_is_refused() {
        let (_dir, repo, _workspace) = repo_with_worktree();
        let error =
            guard_destructive_path(&repo, &repo).expect_err("the repo root must never be a target");
        assert!(
            error.message().contains("IS the repo root"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn the_worktrees_root_itself_is_refused() {
        // The dev-brief root is not a per-run worktree; only strict
        // descendants are.
        let (_dir, repo, _workspace) = repo_with_worktree();
        let worktrees_root = std::path::Path::new(&repo)
            .join(WORKTREES_SUBDIR)
            .display()
            .to_string();
        let error = guard_destructive_path(&repo, &worktrees_root)
            .expect_err("the worktrees root is not a per-run worktree");
        assert!(
            error.message().contains("strictly under"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_path_outside_the_repo_is_refused() {
        let (dir, repo, _workspace) = repo_with_worktree();
        // A sibling directory of the repo, real so it canonicalizes, but
        // nowhere near the guarded tree.
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        let error = guard_destructive_path(&repo, &outside.display().to_string())
            .expect_err("a path outside the repo must be refused");
        assert!(
            error.message().contains("strictly under"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_non_existent_target_is_refused_not_attempted() {
        let (_dir, repo, _workspace) = repo_with_worktree();
        let missing = std::path::Path::new(&repo)
            .join(WORKTREES_SUBDIR)
            .join("does-not-exist")
            .display()
            .to_string();
        let error =
            guard_destructive_path(&repo, &missing).expect_err("an unresolvable target is refused");
        assert!(
            error.message().contains("could not be canonicalized"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_symlink_escaping_the_guarded_tree_is_refused() {
        // A symlink INSIDE the worktree root pointing OUT of the repo must not
        // let a destructive op escape: canonicalization resolves the link, and
        // the resolved target fails the prefix check.
        let (dir, repo, _workspace) = repo_with_worktree();
        let outside = dir.path().join("secret-real-dir");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        let link = std::path::Path::new(&repo)
            .join(WORKTREES_SUBDIR)
            .join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        #[cfg(not(unix))]
        std::os::windows::fs::symlink_dir(&outside, &link).expect("symlink");
        let error = guard_destructive_path(&repo, &link.display().to_string())
            .expect_err("a symlink escaping the guarded tree must be refused");
        assert!(
            error.message().contains("strictly under"),
            "{}",
            error.message()
        );
    }
}
