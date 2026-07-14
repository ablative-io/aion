//! Hermetic execution proofs for `depends_on`'s MERGED-OUTPUT semantics:
//! a released dependent's worktree is provisioned from the base branch WITH
//! its dependencies' accepted branches merged in, its review diff base
//! excludes that dependency work (recorded base ref, reused across rounds),
//! and every protocol/plan violation refuses loudly. Real git in scratch
//! repositories; results are read back from disk, never assumed.

use staged_rounds_worker::handlers::provision_item;
use staged_rounds_worker::shell::Shell;
use staged_rounds_worker::types::{DoneItem, WorkItem};

mod fixtures;
use fixtures::{commit_file_in, git, item, provision_input, scratch_repo};

/// A phase-2 item depending on `depends_on`.
fn dependent(id: &str, depends_on: &[&str]) -> WorkItem {
    WorkItem {
        phase: 2,
        depends_on: depends_on.iter().map(|dep| (*dep).to_owned()).collect(),
        ..item(id)
    }
}

/// Provision `id` fresh, commit one file on its branch (simulating the
/// machinery's dev commit), and return its accepted `DoneItem`.
fn accept_item(
    shell: &Shell,
    repo: &str,
    run_root: &str,
    id: &str,
    file: &str,
    content: &str,
) -> anyhow::Result<DoneItem> {
    let provisioned = provision_item(shell, provision_input(repo, run_root, item(id), vec![]))
        .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    commit_file_in(&provisioned.workspace_path, file, content, id)?;
    Ok(DoneItem {
        item_id: id.to_owned(),
        branch: provisioned.branch,
        base_commit: provisioned.base_commit,
        summary: format!("did {id}"),
    })
}

#[test]
fn a_dependent_starts_from_its_dependencies_merged_output() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    let done = vec![
        accept_item(&shell, &repo, &run_root, "it-a", "a.txt", "alpha\n")?,
        accept_item(&shell, &repo, &run_root, "it-c", "c.txt", "gamma\n")?,
    ];

    let provisioned = provision_item(
        &shell,
        provision_input(&repo, &run_root, dependent("it-b", &["it-a", "it-c"]), done),
    )
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    // Proven by execution: BOTH dependencies' work is in the dependent's
    // worktree before its dev agent ever runs.
    let ws = std::path::Path::new(&provisioned.workspace_path);
    assert_eq!(std::fs::read_to_string(ws.join("a.txt"))?, "alpha\n");
    assert_eq!(std::fs::read_to_string(ws.join("c.txt"))?, "gamma\n");
    // The reported base is the post-merge head, so the reviewer's diff
    // against it is EMPTY until the dependent's own work lands.
    let head = git(&provisioned.workspace_path, &["rev-parse", "HEAD"])?;
    assert_eq!(provisioned.base_commit, head);
    let diff = git(
        &provisioned.workspace_path,
        &["diff", "--name-only", &provisioned.base_commit],
    )?;
    assert!(
        diff.is_empty(),
        "dependency work leaked into the diff: {diff}"
    );
    Ok(())
}

#[test]
fn a_dependent_with_no_done_record_is_refused_loudly() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let Err(error) = provision_item(
        &Shell::inherited(),
        provision_input(&repo, &run_root, dependent("it-b", &["it-a"]), vec![]),
    ) else {
        anyhow::bail!("a dependent was provisioned without its dependency's done record");
    };
    assert!(error.message().contains("it-a"), "{}", error.message());
    assert!(
        error.message().contains("no done record"),
        "{}",
        error.message()
    );
    Ok(())
}

#[test]
fn conflicting_dependency_branches_are_a_loud_plan_violation() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    // Two "done" items that violated the plan's disjointness: both rewrote
    // shared.txt, so merging both into one dependent MUST conflict.
    let done = vec![
        accept_item(&shell, &repo, &run_root, "it-a", "shared.txt", "alpha\n")?,
        accept_item(&shell, &repo, &run_root, "it-c", "shared.txt", "gamma\n")?,
    ];

    let Err(error) = provision_item(
        &shell,
        provision_input(&repo, &run_root, dependent("it-b", &["it-a", "it-c"]), done),
    ) else {
        anyhow::bail!("conflicting dependency branches were merged silently");
    };
    assert!(
        error.message().contains("plan violation"),
        "{}",
        error.message()
    );
    // The half-provisioned worktree is gone, so a retry starts fresh
    // instead of refusing a mid-merge leftover.
    let workspace = format!("{run_root}/items/it-b");
    assert!(
        !std::path::Path::new(&workspace).exists(),
        "the conflicted worktree must be removed"
    );
    Ok(())
}

#[test]
fn a_reused_dependent_keeps_the_recorded_base_excluding_dependency_work() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    let done = vec![accept_item(
        &shell, &repo, &run_root, "it-a", "a.txt", "alpha\n",
    )?];
    let first = provision_item(
        &shell,
        provision_input(&repo, &run_root, dependent("it-b", &["it-a"]), done.clone()),
    )
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    // The dependent's own round-1 work lands on its branch.
    commit_file_in(&first.workspace_path, "b.txt", "beta\n", "round 1")?;

    let second = provision_item(
        &shell,
        provision_input(&repo, &run_root, dependent("it-b", &["it-a"]), done),
    )
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;

    assert_eq!(second.workspace_path, first.workspace_path);
    // The reused base is the RECORDED post-dependency-merge head — not the
    // merge base with main, which would blame it-b for it-a's entire diff.
    assert_eq!(second.base_commit, first.base_commit);
    let merge_base = git(&repo, &["merge-base", &second.branch, "main"])?;
    assert_ne!(
        second.base_commit, merge_base,
        "the recorded base must differ from the main merge base once \
         dependencies are merged in"
    );
    let diff = git(
        &second.workspace_path,
        &["diff", "--name-only", &second.base_commit],
    )?;
    assert_eq!(
        diff, "b.txt",
        "the review diff must carry ONLY the dependent's own work"
    );
    Ok(())
}

#[test]
fn a_reused_dependent_without_its_recorded_base_is_refused() -> anyhow::Result<()> {
    let (_dir, repo, run_root) = scratch_repo()?;
    let shell = Shell::inherited();
    let done = vec![accept_item(
        &shell, &repo, &run_root, "it-a", "a.txt", "alpha\n",
    )?];
    let first = provision_item(
        &shell,
        provision_input(&repo, &run_root, dependent("it-b", &["it-a"]), done.clone()),
    )
    .map_err(|error| anyhow::anyhow!(error.message().to_owned()))?;
    // Sabotage: the recorded base ref disappears (a worktree created
    // outside this provisioning path would equally have none).
    git(
        &repo,
        &["update-ref", "-d", "refs/staged-rounds/wf-1/base/it-b"],
    )?;

    let Err(error) = provision_item(
        &shell,
        provision_input(&repo, &run_root, dependent("it-b", &["it-a"]), done),
    ) else {
        anyhow::bail!("a dependent was reused with a merge-base fallback that misattributes work");
    };
    assert!(
        error.message().contains("recorded base ref"),
        "{}",
        error.message()
    );
    // The worktree (and its committed history) is untouched by the refusal.
    assert!(std::path::Path::new(&first.workspace_path).is_dir());
    Ok(())
}
