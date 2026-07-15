//! Harness-seam tests: per-role workspace-root + session-suffix extraction
//! (incl. loud failures), the assembled-prompt path, and the
//! `ConcludeMerge` mechanic proven BY EXECUTION against a real conflicted
//! repo in a tempdir.

use super::{
    PostRunPlan, ProfiledNornHarness, dev_item_context, planner_context, remediate_context,
    review_item_context,
};
use crate::commit::{ConcludeOutcome, conclude_merge};
use crate::shell::Shell;
use aion_integration_norn::NornHarness;

const DEV_INPUT: &str = r#"{
    "work": {
        "item": {
            "id": "it-1", "title": "t", "goal": "g",
            "scope_in": [], "scope_out": [],
            "phase": 1, "depends_on": [], "feedback": ""
        },
        "workspace_path": "/repo/.staged-rounds/wf/items/it-1",
        "branch": "staged/wf/it-1",
        "base_commit": "abc"
    },
    "gates": []
}"#;

#[test]
fn the_planner_roots_at_the_repo_and_keys_the_planner_session() -> anyhow::Result<()> {
    let context =
        planner_context("{\"material\":{},\"repo_root\":\"/repo\",\"workspace_path\":\"/repo\"}")
            .map_err(anyhow::Error::msg)?;
    assert_eq!(context.workspace_root, "/repo");
    assert_eq!(context.session_suffix, "planner");
    assert_eq!(context.plan, None);
    Ok(())
}

#[test]
fn the_developer_roots_at_the_item_worktree_with_a_per_item_session() -> anyhow::Result<()> {
    let context = dev_item_context(DEV_INPUT).map_err(anyhow::Error::msg)?;
    assert_eq!(context.workspace_root, "/repo/.staged-rounds/wf/items/it-1");
    assert_eq!(context.session_suffix, "dev-it-1");
    let Some(PostRunPlan::DevWork { work }) = context.plan else {
        anyhow::bail!("the developer must carry the DevWork plan");
    };
    assert_eq!(work.branch, "staged/wf/it-1");
    Ok(())
}

#[test]
fn the_reviewer_roots_at_the_item_worktree_with_a_per_item_session() -> anyhow::Result<()> {
    let context = review_item_context(
        "{\"work\":{\"item\":{\"id\":\"it-2\",\"title\":\"t\",\"goal\":\"g\",\"phase\":1,\
         \"feedback\":\"\"},\"workspace_path\":\"/ws/it-2\",\"branch\":\"b\",\
         \"base_commit\":\"c\",\"report\":{\"item_id\":\"it-2\",\"summary\":\"s\"}}}",
    )
    .map_err(anyhow::Error::msg)?;
    assert_eq!(context.workspace_root, "/ws/it-2");
    assert_eq!(context.session_suffix, "review-it-2");
    assert_eq!(context.plan, None);
    Ok(())
}

#[test]
fn the_remediator_resumes_the_planner_session_in_the_integration_worktree() -> anyhow::Result<()> {
    let context = remediate_context(
        "{\"merge\":{\"integration_branch\":\"b\",\"workspace_path\":\"/run/integration\"},\
         \"plan\":{},\"workspace_path\":\"/run/integration\"}",
    )
    .map_err(anyhow::Error::msg)?;
    assert_eq!(context.workspace_root, "/run/integration");
    assert_eq!(
        context.session_suffix, "planner",
        "the remediator MUST key the planner's session — the resumed \
         coordinator judges its own plan's conflicts"
    );
    assert_eq!(
        context.plan,
        Some(PostRunPlan::ConcludeMerge {
            workspace_path: "/run/integration".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_missing_item_id_is_a_loud_error() -> anyhow::Result<()> {
    let input = DEV_INPUT.replace("\"it-1\"", "\"  \"");
    let Err(error) = dev_item_context(&input) else {
        anyhow::bail!("a blank item id unexpectedly resolved");
    };
    assert!(error.contains("work.item.id"), "error was: {error}");
    Ok(())
}

#[test]
fn a_missing_workspace_path_is_a_loud_error() -> anyhow::Result<()> {
    for (extract, json) in [
        (planner_context as super::ExtractFn, "{\"material\":{}}"),
        (remediate_context as super::ExtractFn, "{\"merge\":{}}"),
    ] {
        let Err(error) = extract(json) else {
            anyhow::bail!("a missing workspace_path unexpectedly resolved");
        };
        assert!(error.contains("workspace_path"), "error was: {error}");
    }
    Ok(())
}

#[test]
fn the_assembled_prompt_is_the_role_function_applied_to_the_context() {
    let harness = ProfiledNornHarness::new(
        NornHarness::new(),
        crate::prompts::review_item,
        review_item_context,
    );
    let prompt = harness.assembled_prompt("{\"work\":{\"base_commit\":\"x\"}}");
    // The per-turn prompt is context only — the profile doctrine is the
    // inner harness's `--append-system-prompt` text, never folded in here.
    assert!(!prompt.contains("# Adversarial"));
    assert!(prompt.contains("```json"));
    assert!(prompt.contains("\"base_commit\":\"x\""));
}

// --- ConcludeMerge, proven by execution --------------------------------------

fn git(dir: &str, args: &[&str]) -> anyhow::Result<()> {
    let run = Shell::inherited()
        .run("git", args, dir)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    anyhow::ensure!(
        run.succeeded(),
        "git {args:?} exited {}: {}",
        run.exit_status,
        run.output
    );
    Ok(())
}

/// A real repo with a conflicted merge in progress: `main` and `side` both
/// rewrite the same line of every file in `files`, and `git merge side` is
/// left mid-conflict.
fn conflicted_repo_over(files: &[&str]) -> anyhow::Result<(tempfile::TempDir, String)> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().display().to_string();
    git(&root, &["init", "-b", "main"])?;
    git(&root, &["config", "user.name", "test"])?;
    git(&root, &["config", "user.email", "test@example.com"])?;
    // Pin git's DEFAULT pathname quoting on, so these repos exhibit the
    // C-quoted non-ASCII listings the marker scan must survive even when a
    // host gitconfig turns quoting off.
    git(&root, &["config", "core.quotepath", "true"])?;
    for (content, commit_message, branch_cmd) in [
        ("base\n", "base", Some(["checkout", "-b", "side"])),
        ("side\n", "side", Some(["checkout", "main", "--"])),
        ("main\n", "main", None),
    ] {
        for file in files {
            std::fs::write(dir.path().join(file), content)?;
        }
        git(&root, &["add", "-A"])?;
        git(&root, &["commit", "-m", commit_message])?;
        if let Some(args) = branch_cmd {
            git(&root, &[args[0], args[1], args[2]])?;
        }
    }
    let merge = Shell::inherited()
        .run("git", &["merge", "side"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    anyhow::ensure!(!merge.succeeded(), "the merge should have conflicted");
    Ok((dir, root))
}

/// The one-file conflicted repo most conclusion tests drive.
fn conflicted_repo() -> anyhow::Result<(tempfile::TempDir, String)> {
    conflicted_repo_over(&["file.txt"])
}

#[test]
fn conclude_merge_concludes_a_resolved_in_progress_merge() -> anyhow::Result<()> {
    let (dir, root) = conflicted_repo()?;
    // The "remediator" resolves the conflict in place (no git).
    std::fs::write(dir.path().join("file.txt"), "resolved\n")?;

    let outcome = conclude_merge(&Shell::inherited(), &root).map_err(anyhow::Error::msg)?;
    let ConcludeOutcome::Concluded { commit } = outcome else {
        anyhow::bail!("expected the in-progress merge to be concluded, got {outcome:?}");
    };
    assert!(!commit.is_empty());

    // Proven by execution: MERGE_HEAD is gone, the tree is clean, the head
    // is a merge commit carrying the resolution.
    let shell = Shell::inherited();
    let probe = shell
        .run("git", &["rev-parse", "-q", "--verify", "MERGE_HEAD"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert!(
        !probe.succeeded(),
        "MERGE_HEAD must be gone after conclusion"
    );
    let status = shell
        .run("git", &["status", "--porcelain"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert!(status.stdout.trim().is_empty(), "the tree must be clean");
    let parents = shell
        .run("git", &["rev-list", "--parents", "-n", "1", "HEAD"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert_eq!(
        parents.stdout.split_whitespace().count(),
        3,
        "the conclusion must be a two-parent merge commit"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file.txt"))?,
        "resolved\n"
    );
    Ok(())
}

#[test]
fn conclude_merge_refuses_unresolved_conflict_markers() -> anyhow::Result<()> {
    let (dir, root) = conflicted_repo()?;
    // The "remediator" resolved NOTHING: the conflict markers are still in
    // the tree exactly as git left them.
    let Err(error) = conclude_merge(&Shell::inherited(), &root) else {
        anyhow::bail!("an unresolved merge was concluded — markers would be committed");
    };
    assert!(error.contains("conflict markers"), "error was: {error}");
    assert!(error.contains("file.txt"), "error was: {error}");

    // Proven by execution: the merge is STILL in progress (MERGE_HEAD
    // resolves) and nothing was committed — the next remediation attempt
    // finds the conflict exactly where it was.
    let shell = Shell::inherited();
    let probe = shell
        .run("git", &["rev-parse", "-q", "--verify", "MERGE_HEAD"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert!(probe.succeeded(), "MERGE_HEAD must survive the refusal");
    let committed = shell
        .run("git", &["show", "HEAD:file.txt"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert!(
        !committed.stdout.contains("<<<<<<<"),
        "HEAD must not carry conflict markers: {}",
        committed.stdout
    );
    drop(dir);
    Ok(())
}

#[test]
fn conclude_merge_refuses_a_partial_resolution_naming_the_unresolved_file() -> anyhow::Result<()> {
    let (dir, root) = conflicted_repo_over(&["a.txt", "b.txt"])?;
    // The "remediator" resolves only a.txt; b.txt keeps its markers.
    std::fs::write(dir.path().join("a.txt"), "resolved\n")?;

    let Err(error) = conclude_merge(&Shell::inherited(), &root) else {
        anyhow::bail!("a partial resolution was concluded — b.txt's markers would be committed");
    };
    assert!(error.contains("b.txt"), "error was: {error}");
    assert!(!error.contains("a.txt"), "error was: {error}");

    // Resolving the remaining file makes the SAME conclusion succeed.
    std::fs::write(dir.path().join("b.txt"), "resolved too\n")?;
    let outcome = conclude_merge(&Shell::inherited(), &root).map_err(anyhow::Error::msg)?;
    let ConcludeOutcome::Concluded { commit } = outcome else {
        anyhow::bail!("expected the fully-resolved merge to conclude, got {outcome:?}");
    };
    assert!(!commit.is_empty());
    Ok(())
}

#[test]
fn conclude_merge_refuses_markers_in_a_git_quoted_non_ascii_filename() -> anyhow::Result<()> {
    // Under default core.quotepath=true, `git diff --name-only` prints this
    // path as the C-quoted literal "caf\303\251.txt" — a string that does
    // not exist on disk. A scan parsing that output as a literal path would
    // misread the file as resolved-by-deletion and COMMIT its markers.
    let (dir, root) = conflicted_repo_over(&["café.txt"])?;

    let Err(error) = conclude_merge(&Shell::inherited(), &root) else {
        anyhow::bail!("a quoted-pathname conflict was concluded — markers would be committed");
    };
    assert!(error.contains("café.txt"), "error was: {error}");

    // Proven by execution: the merge is still in progress and HEAD carries
    // no markers.
    let shell = Shell::inherited();
    let probe = shell
        .run("git", &["rev-parse", "-q", "--verify", "MERGE_HEAD"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert!(probe.succeeded(), "MERGE_HEAD must survive the refusal");
    let committed = shell
        .run("git", &["show", "HEAD:café.txt"], &root)
        .map_err(|failure| anyhow::anyhow!(failure.message()))?;
    assert!(
        !committed.stdout.contains("<<<<<<<"),
        "HEAD must not carry conflict markers: {}",
        committed.stdout
    );

    // Resolving the file makes the SAME conclusion succeed — the exists()
    // deletion check still sees the real on-disk path through -z output.
    std::fs::write(dir.path().join("café.txt"), "resolved\n")?;
    let outcome = conclude_merge(&Shell::inherited(), &root).map_err(anyhow::Error::msg)?;
    let ConcludeOutcome::Concluded { commit } = outcome else {
        anyhow::bail!("expected the resolved merge to conclude, got {outcome:?}");
    };
    assert!(!commit.is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("café.txt"))?,
        "resolved\n"
    );
    Ok(())
}

#[test]
fn conclude_merge_commits_a_dirty_tree_when_no_merge_is_in_progress() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().display().to_string();
    git(&root, &["init", "-b", "main"])?;
    git(&root, &["config", "user.name", "test"])?;
    git(&root, &["config", "user.email", "test@example.com"])?;
    std::fs::write(dir.path().join("file.txt"), "base\n")?;
    git(&root, &["add", "-A"])?;
    git(&root, &["commit", "-m", "base"])?;
    std::fs::write(dir.path().join("file.txt"), "fixed\n")?;

    let outcome = conclude_merge(&Shell::inherited(), &root).map_err(anyhow::Error::msg)?;
    let ConcludeOutcome::Committed { commit } = outcome else {
        anyhow::bail!("expected a remediation fix commit, got {outcome:?}");
    };
    assert!(!commit.is_empty());
    Ok(())
}

#[test]
fn conclude_merge_skips_green_on_a_clean_tree() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().display().to_string();
    git(&root, &["init", "-b", "main"])?;
    git(&root, &["config", "user.name", "test"])?;
    git(&root, &["config", "user.email", "test@example.com"])?;
    std::fs::write(dir.path().join("file.txt"), "base\n")?;
    git(&root, &["add", "-A"])?;
    git(&root, &["commit", "-m", "base"])?;

    let outcome = conclude_merge(&Shell::inherited(), &root).map_err(anyhow::Error::msg)?;
    let ConcludeOutcome::Skipped { head } = outcome else {
        anyhow::bail!("expected a clean skip, got {outcome:?}");
    };
    assert!(!head.is_empty());
    Ok(())
}
