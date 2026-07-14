//! Hermetic shell-activity tests for the integration side: `merge_branches`
//! drives REAL git against scratch repositories built inside each test (no
//! server, no norn, no mocks). Runtime behavior is proven by execution —
//! merge results are read from the merged files, `MERGE_HEAD` is probed on
//! disk.

use staged_rounds_worker::handlers::{merge_branches, provision_item};
use staged_rounds_worker::shell::Shell;
use staged_rounds_worker::types::{DoneItem, MergeBranchesInput};

mod fixtures;
use fixtures::{commit_file_in, git, item, provision_input, scratch_repo};

// --- local fixtures -------------------------------------------------------

/// Provision two items, commit disjoint (or colliding) files on their
/// branches, and return their `DoneItem` records.
fn two_done_items(repo: &str, run_root: &str, disjoint: bool) -> anyhow::Result<Vec<DoneItem>> {
    let shell = Shell::inherited();
    let mut done = Vec::new();
    for (id, content) in [("it-a", "alpha\n"), ("it-b", "beta\n")] {
        let provisioned = provision_item(&shell, provision_input(repo, run_root, item(id)))
            .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
        let file = if disjoint {
            format!("{id}.txt")
        } else {
            "shared.txt".to_owned()
        };
        commit_file_in(&provisioned.workspace_path, &file, content, id)?;
        done.push(DoneItem {
            item_id: id.to_owned(),
            branch: provisioned.branch,
            base_commit: provisioned.base_commit,
            summary: format!("did {id}"),
        });
    }
    Ok(done)
}

fn merge_input(
    repo: &str,
    run_root: &str,
    done: Vec<DoneItem>,
    remediation: &str,
) -> MergeBranchesInput {
    MergeBranchesInput {
        run_root: run_root.to_owned(),
        repo_root: repo.to_owned(),
        base_branch: "main".to_owned(),
        done,
        prior_evidence: String::new(),
        remediation: remediation.to_owned(),
    }
}

// --- merge_branches -------------------------------------------------------

#[test]
fn merge_branches_merges_disjoint_item_branches_clean() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let done = two_done_items(&repo, &run_root, true)?;
    let state = merge_branches(&Shell::inherited(), merge_input(&repo, &run_root, done, ""))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert!(state.conflicts.is_empty(), "{:?}", state.conflicts);
    assert_eq!(
        state.merged,
        vec!["staged/wf-1/it-a".to_owned(), "staged/wf-1/it-b".to_owned()]
    );
    assert_eq!(state.integration_branch, "staged/wf-1/integration");
    // Proven by execution: both items' files exist in the integration tree.
    let ws = std::path::Path::new(&state.workspace_path);
    assert_eq!(std::fs::read_to_string(ws.join("it-a.txt"))?, "alpha\n");
    assert_eq!(std::fs::read_to_string(ws.join("it-b.txt"))?, "beta\n");
    Ok(())
}

#[test]
fn merge_branches_reports_a_conflict_and_leaves_the_merge_in_progress() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let done = two_done_items(&repo, &run_root, false)?;
    let state = merge_branches(&Shell::inherited(), merge_input(&repo, &run_root, done, ""))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert_eq!(state.merged, vec!["staged/wf-1/it-a".to_owned()]);
    assert_eq!(state.conflicts.len(), 1);
    assert_eq!(state.conflicts[0].item_id, "it-b");
    assert_eq!(state.conflicts[0].files, vec!["shared.txt".to_owned()]);
    // Proven by execution: MERGE_HEAD exists — the merge is in progress.
    let merge_head = std::path::Path::new(&state.workspace_path)
        .join(".git")
        .exists()
        && Shell::inherited()
            .run(
                "git",
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
                &state.workspace_path,
            )
            .map(|run| run.succeeded())
            .unwrap_or(false);
    assert!(
        merge_head,
        "MERGE_HEAD must exist in the integration worktree"
    );
    Ok(())
}

#[test]
fn merge_branches_resumes_after_a_concluded_resolution() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let done = two_done_items(&repo, &run_root, false)?;
    let shell = Shell::inherited();
    let first = merge_branches(&shell, merge_input(&repo, &run_root, done.clone(), ""))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    assert_eq!(first.conflicts.len(), 1);

    // Resolve + conclude by hand (simulating remediate + ConcludeMerge).
    std::fs::write(
        std::path::Path::new(&first.workspace_path).join("shared.txt"),
        "resolved\n",
    )?;
    git(&first.workspace_path, &["add", "-A"])?;
    git(&first.workspace_path, &["commit", "--no-edit"])?;

    let second = merge_branches(
        &shell,
        merge_input(&repo, &run_root, done, "kept both intents"),
    )
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    assert!(second.conflicts.is_empty(), "{:?}", second.conflicts);
    assert_eq!(
        second.merged,
        vec!["staged/wf-1/it-a".to_owned(), "staged/wf-1/it-b".to_owned()]
    );
    assert!(
        second.evidence.contains("remediation: kept both intents"),
        "{}",
        second.evidence
    );
    Ok(())
}

#[test]
fn merge_branches_reports_an_unconcluded_merge_without_touching_it() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let done = two_done_items(&repo, &run_root, false)?;
    let shell = Shell::inherited();
    let first = merge_branches(&shell, merge_input(&repo, &run_root, done.clone(), ""))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    assert_eq!(first.conflicts.len(), 1);

    // Remediation "ran" but did NOT conclude: call merge_branches again.
    let second = merge_branches(&shell, merge_input(&repo, &run_root, done, "half-done"))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    assert_eq!(second.conflicts.len(), 1);
    assert_eq!(second.conflicts[0].item_id, "it-b");
    assert_eq!(second.conflicts[0].files, vec!["shared.txt".to_owned()]);
    assert!(
        second
            .evidence
            .contains("did not conclude the in-progress merge"),
        "{}",
        second.evidence
    );
    Ok(())
}

#[test]
fn merge_branches_is_idempotent_over_already_merged_branches() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let done = two_done_items(&repo, &run_root, true)?;
    let shell = Shell::inherited();
    let first = merge_branches(&shell, merge_input(&repo, &run_root, done.clone(), ""))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    let head_after_first = git(&first.workspace_path, &["rev-parse", "HEAD"])?;

    let second = merge_branches(&shell, merge_input(&repo, &run_root, done, ""))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    assert!(second.conflicts.is_empty());
    assert_eq!(second.merged.len(), 2);
    assert!(
        second.evidence.contains("already merged"),
        "{}",
        second.evidence
    );
    let head_after_second = git(&second.workspace_path, &["rev-parse", "HEAD"])?;
    assert_eq!(
        head_after_first, head_after_second,
        "a re-run must not create new commits"
    );
    Ok(())
}
