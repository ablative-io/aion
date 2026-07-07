//! Hermetic tests for the three shell activity bodies and the mechanical
//! commit path, against a REAL temporary git repository — the exact
//! production functions, no shims.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use dev_brief_worker::commit::{self, FixCommitOutcome};
use dev_brief_worker::handlers;
use dev_brief_worker::shell::Shell;
use dev_brief_worker::types::{CleanupInput, GateCommand, GateInput, ProvisionInput};

/// A fresh git repo with one commit on `main`, plus a workspace parent dir.
fn repo_with_initial_commit() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root_str = root.display().to_string();
    let shell = Shell::inherited();
    let git = |args: &[&str]| {
        let run = shell.run("git", args, &root_str).expect("git runs");
        assert!(
            run.succeeded(),
            "git {args:?} exited {}: {}",
            run.exit_status,
            run.output
        );
    };
    git(&["init", "--initial-branch=main", "."]);
    std::fs::write(root.join("lib.txt"), "original\n").expect("seed file");
    git(&["add", "lib.txt"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-m",
        "seed",
    ]);
    (dir, root_str)
}

fn provision(root: &str, workspace: &str) -> dev_brief_worker::types::WorkspaceInfo {
    handlers::provision(
        &Shell::inherited(),
        ProvisionInput {
            repo_root: root.to_owned(),
            base_branch: "main".to_owned(),
            branch: "dev/DB-T".to_owned(),
            workspace_path: workspace.to_owned(),
        },
    )
    .expect("provision succeeds")
}

#[test]
fn provision_creates_a_worktree_on_the_brief_branch_with_the_base_commit() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    let info = provision(&root, &workspace);
    assert_eq!(info.branch, "dev/DB-T");
    assert!(std::path::Path::new(&workspace).join("lib.txt").is_file());
    assert_eq!(info.base_commit.len(), 40, "a full commit hash");
    // Idempotent: provisioning again over the same path succeeds cleanly.
    let again = provision(&root, &workspace);
    assert_eq!(again.base_commit, info.base_commit);
}

#[test]
fn commit_dev_work_commits_tracked_edits_and_new_files_then_skips_when_clean() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    let info = provision(&root, &workspace);
    let shell = Shell::inherited();

    // The developer edits a tracked file AND creates a brand-new one.
    std::fs::write(
        std::path::Path::new(&workspace).join("lib.txt"),
        "changed\n",
    )
    .expect("edit");
    std::fs::write(
        std::path::Path::new(&workspace).join("new_module.txt"),
        "new\n",
    )
    .expect("create");

    let outcome = commit::commit_dev_work(&shell, &workspace, "DB-T").expect("commits");
    let FixCommitOutcome::Committed { commit, paths } = &outcome else {
        panic!("expected a commit, got {outcome:?}");
    };
    assert_ne!(commit, &info.base_commit);
    assert!(paths.contains(&"lib.txt".to_owned()));
    assert!(paths.contains(&"new_module.txt".to_owned()));

    // Retry idempotence: a second run with a clean tree skips green with the
    // same head.
    let again = commit::commit_dev_work(&shell, &workspace, "DB-T").expect("skips");
    let FixCommitOutcome::Skipped { head, .. } = &again else {
        panic!("expected a skip, got {again:?}");
    };
    assert_eq!(head, commit);
}

#[test]
fn run_gates_records_green_and_red_commands_and_the_diff() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    let info = provision(&root, &workspace);
    let shell = Shell::inherited();

    std::fs::write(
        std::path::Path::new(&workspace).join("lib.txt"),
        "changed\n",
    )
    .expect("edit");
    commit::commit_dev_work(&shell, &workspace, "DB-T").expect("commit work");

    let outcome = handlers::run_gates(
        &shell,
        GateInput {
            workspace_path: workspace.clone(),
            base_commit: info.base_commit.clone(),
            gates: vec![
                GateCommand {
                    name: "green".to_owned(),
                    argv: vec!["true".to_owned()],
                },
                GateCommand {
                    name: "red".to_owned(),
                    argv: vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "echo boom; exit 3".to_owned(),
                    ],
                },
            ],
        },
    )
    .expect("run_gates returns recorded data");

    assert!(!outcome.pass, "one red command means the battery is red");
    assert_eq!(outcome.runs.len(), 2);
    assert!(outcome.runs[0].passed);
    assert_eq!(outcome.runs[1].exit_code, 3);
    assert!(!outcome.runs[1].passed);
    assert!(outcome.runs[1].output_tail.contains("boom"));
    assert!(outcome.diagnostics.contains("gate `red` exited 3"));
    assert!(
        outcome.diff.contains("-original") && outcome.diff.contains("+changed"),
        "the reviewers' diff carries the developer's change; diff was:\n{}",
        outcome.diff
    );
}

#[test]
fn run_gates_commits_normalization_a_mutating_command_leaves_behind() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    let info = provision(&root, &workspace);
    let shell = Shell::inherited();

    // A "formatter" gate that rewrites a tracked file (write mode, exit 0).
    let outcome = handlers::run_gates(
        &shell,
        GateInput {
            workspace_path: workspace.clone(),
            base_commit: info.base_commit.clone(),
            gates: vec![GateCommand {
                name: "fmt".to_owned(),
                argv: vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "printf 'normalized\\n' > lib.txt".to_owned(),
                ],
            }],
        },
    )
    .expect("run_gates succeeds");
    assert!(outcome.pass);
    assert!(
        outcome.diagnostics.contains("normalized the tree"),
        "the mechanical normalization commit is recorded; diagnostics were:\n{}",
        outcome.diagnostics
    );

    // The worktree is clean afterwards, so cleanup can remove it.
    let cleaned = handlers::cleanup(
        &shell,
        CleanupInput {
            repo_root: root,
            workspace_path: workspace,
        },
    )
    .expect("cleanup runs");
    assert!(cleaned.removed, "detail: {}", cleaned.detail);
}

#[test]
fn an_empty_gate_battery_is_a_recorded_vacuous_pass() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    let info = provision(&root, &workspace);

    let outcome = handlers::run_gates(
        &Shell::inherited(),
        GateInput {
            workspace_path: workspace,
            base_commit: info.base_commit,
            gates: vec![],
        },
    )
    .expect("run_gates succeeds");
    assert!(outcome.pass);
    assert!(outcome.diagnostics.contains("no gates configured"));
    let _ = root;
}

#[test]
fn a_gate_with_an_empty_argv_is_a_loud_configuration_fault() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    let info = provision(&root, &workspace);

    let error = handlers::run_gates(
        &Shell::inherited(),
        GateInput {
            workspace_path: workspace,
            base_commit: info.base_commit,
            gates: vec![GateCommand {
                name: "broken".to_owned(),
                argv: vec![],
            }],
        },
    )
    .expect_err("must fail");
    assert!(error.message().contains("empty argv"));
    let _ = root;
}

#[test]
fn cleanup_refuses_a_dirty_worktree_and_removes_a_clean_one() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = dir.path().join("ws").display().to_string();
    provision(&root, &workspace);
    let shell = Shell::inherited();

    std::fs::write(std::path::Path::new(&workspace).join("junk.txt"), "x\n").expect("dirty");
    let refused = handlers::cleanup(
        &shell,
        CleanupInput {
            repo_root: root.clone(),
            workspace_path: workspace.clone(),
        },
    )
    .expect("cleanup runs");
    assert!(!refused.removed);
    assert!(refused.detail.contains("uncommitted"));

    std::fs::remove_file(std::path::Path::new(&workspace).join("junk.txt")).expect("clean");
    let removed = handlers::cleanup(
        &shell,
        CleanupInput {
            repo_root: root,
            workspace_path: workspace.clone(),
        },
    )
    .expect("cleanup runs");
    assert!(removed.removed, "detail: {}", removed.detail);
    assert!(!std::path::Path::new(&workspace).exists());
}

#[test]
fn provision_reclaims_a_stale_clean_worktree_holding_the_brief_branch() {
    // A failed prior run abandons its worktree (cleanup never ran); a fresh
    // run of the same brief must reclaim the branch, not die at provision.
    let (dir, root) = repo_with_initial_commit();
    let stale = dir.path().join("stale-ws").display().to_string();
    provision(&root, &stale);

    let fresh = dir.path().join("fresh-ws").display().to_string();
    let info = provision(&root, &fresh);
    assert!(std::path::Path::new(&fresh).is_dir());
    assert!(
        !std::path::Path::new(&stale).exists(),
        "the stale clean holder is removed"
    );
    assert_eq!(info.branch, "dev/DB-T");
}

#[test]
fn provision_refuses_to_destroy_a_dirty_stale_holder() {
    let (dir, root) = repo_with_initial_commit();
    let stale = dir.path().join("stale-ws").display().to_string();
    provision(&root, &stale);
    std::fs::write(std::path::Path::new(&stale).join("wip.txt"), "precious\n").expect("dirty");

    let fresh = dir.path().join("fresh-ws").display().to_string();
    let error = handlers::provision(
        &Shell::inherited(),
        ProvisionInput {
            repo_root: root,
            base_branch: "main".to_owned(),
            branch: "dev/DB-T".to_owned(),
            workspace_path: fresh,
        },
    )
    .expect_err("must refuse");
    assert!(
        error.message().contains("UNCOMMITTED"),
        "{}",
        error.message()
    );
    assert!(
        std::path::Path::new(&stale).join("wip.txt").is_file(),
        "the dirty holder and its work survive"
    );
}
