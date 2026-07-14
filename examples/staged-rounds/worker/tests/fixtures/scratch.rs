//! Shared hermetic fixtures for the shell-activity integration tests: a
//! scratch git repository built inside each test, plus the item/provision
//! helpers both test binaries drive. Real git, no mocks.

use staged_rounds_worker::shell::Shell;
use staged_rounds_worker::types::{DoneItem, ProvisionItemInput, WorkItem};

/// Run one git command in `dir`, failing the test loudly on any refusal.
pub fn git(dir: &str, args: &[&str]) -> anyhow::Result<String> {
    let run = Shell::inherited()
        .run("git", args, dir)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    anyhow::ensure!(
        run.succeeded(),
        "git {args:?} exited {}: {}",
        run.exit_status,
        run.output
    );
    Ok(run.stdout.trim().to_owned())
}

/// A scratch repo with one commit on `main`; returns (tempdir, `repo_root`,
/// `run_root`) with the run root under `<repo>/.staged-rounds/wf-1`.
pub fn scratch_repo() -> anyhow::Result<(tempfile::TempDir, String, String)> {
    let dir = tempfile::tempdir()?;
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    let root = repo.display().to_string();
    git(&root, &["init", "-b", "main"])?;
    git(&root, &["config", "user.name", "test"])?;
    git(&root, &["config", "user.email", "test@example.com"])?;
    std::fs::write(repo.join("README.md"), "base\n")?;
    git(&root, &["add", "-A"])?;
    git(&root, &["commit", "-m", "base"])?;
    let run_root = format!("{root}/.staged-rounds/wf-1");
    Ok((dir, root, run_root))
}

/// A minimal phase-1 work item with no dependencies.
pub fn item(id: &str) -> WorkItem {
    WorkItem {
        id: id.to_owned(),
        title: format!("item {id}"),
        goal: format!("do {id}"),
        scope_in: vec![format!("{id}.txt")],
        scope_out: vec![],
        phase: 1,
        depends_on: vec![],
        feedback: String::new(),
    }
}

/// The `provision_item` input for one work item against the scratch repo,
/// carrying the run's done records the item's `depends_on` resolves against.
pub fn provision_input(
    repo: &str,
    run_root: &str,
    work: WorkItem,
    done: Vec<DoneItem>,
) -> ProvisionItemInput {
    ProvisionItemInput {
        run_root: run_root.to_owned(),
        repo_root: repo.to_owned(),
        base_branch: "main".to_owned(),
        item: work,
        done,
    }
}

/// Commit one file on an item branch through its provisioned worktree,
/// simulating the machinery's dev commit.
pub fn commit_file_in(
    workspace: &str,
    file: &str,
    content: &str,
    message: &str,
) -> anyhow::Result<()> {
    std::fs::write(std::path::Path::new(workspace).join(file), content)?;
    git(workspace, &["add", "-A"])?;
    git(workspace, &["commit", "-m", message])?;
    Ok(())
}
