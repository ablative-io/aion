//! Hermetic tests for the three shell activity bodies and the mechanical
//! commit path, against a REAL temporary git repository — the exact
//! production functions, no shims.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use dev_brief_worker::commit::{self, FixCommitOutcome};
use dev_brief_worker::handlers;
use dev_brief_worker::shell::Shell;
use dev_brief_worker::types::{
    CleanupInput, GateCommand, GateInput, ProvisionInput, ResetInput, VerifyInput,
};

/// A fresh git repo with one commit on `main`. Brief worktrees live under the
/// repo's `.yggdrasil-worktrees/dev-brief/` (git-ignored in production, and
/// the only tree the destructive-path guard permits reset/cleanup to touch).
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
    // The worktree root is git-ignored, exactly as in the real repo, so the
    // parent working tree never sees the nested per-run worktrees.
    std::fs::write(root.join(".gitignore"), ".yggdrasil-worktrees/\n").expect("seed gitignore");
    std::fs::write(root.join("lib.txt"), "original\n").expect("seed file");
    git(&["add", ".gitignore", "lib.txt"]);
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

/// A per-run worktree path under the repo's guarded dev-brief worktree root —
/// the only shape the destructive-path guard permits reset/cleanup to touch.
fn worktree(root: &str, name: &str) -> String {
    std::path::Path::new(root)
        .join(".yggdrasil-worktrees/dev-brief")
        .join(name)
        .display()
        .to_string()
}

/// A verify-log path under the repo's dev-brief logs directory (which the
/// handler creates).
fn log_file(root: &str, name: &str) -> String {
    std::path::Path::new(root)
        .join(".yggdrasil-worktrees/dev-brief/logs")
        .join(name)
        .display()
        .to_string()
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
    let workspace = worktree(&root, "ws");
    let info = provision(&root, &workspace);
    assert_eq!(info.branch, "dev/DB-T");
    assert!(std::path::Path::new(&workspace).join("lib.txt").is_file());
    assert_eq!(info.base_commit.len(), 40, "a full commit hash");
    // Idempotent: provisioning again over the same path succeeds cleanly.
    let again = provision(&root, &workspace);
    assert_eq!(again.base_commit, info.base_commit);
    let _ = dir;
}

#[test]
fn commit_dev_work_commits_tracked_edits_and_new_files_then_skips_when_clean() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
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
    let _ = dir;
}

#[test]
fn run_gates_records_green_and_red_commands_and_the_diff() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
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
    let _ = dir;
}

#[test]
fn run_gates_commits_normalization_a_mutating_command_leaves_behind() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
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
    let _ = dir;
}

#[test]
fn an_empty_gate_battery_is_a_recorded_vacuous_pass() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
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
    let _ = (dir, root);
}

#[test]
fn a_gate_with_an_empty_argv_is_a_loud_configuration_fault() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
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
    let _ = (dir, root);
}

#[test]
fn cleanup_refuses_a_dirty_worktree_and_removes_a_clean_one() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
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
    let _ = dir;
}

#[test]
fn provision_reclaims_a_stale_clean_worktree_holding_the_brief_branch() {
    // A failed prior run abandons its worktree (cleanup never ran); a fresh
    // run of the same brief must reclaim the branch, not die at provision.
    let (dir, root) = repo_with_initial_commit();
    let stale = worktree(&root, "stale-ws");
    provision(&root, &stale);

    let fresh = worktree(&root, "fresh-ws");
    let info = provision(&root, &fresh);
    assert!(std::path::Path::new(&fresh).is_dir());
    assert!(
        !std::path::Path::new(&stale).exists(),
        "the stale clean holder is removed"
    );
    assert_eq!(info.branch, "dev/DB-T");
    let _ = dir;
}

#[test]
fn provision_refuses_to_destroy_a_dirty_stale_holder() {
    let (dir, root) = repo_with_initial_commit();
    let stale = worktree(&root, "stale-ws");
    provision(&root, &stale);
    std::fs::write(std::path::Path::new(&stale).join("wip.txt"), "precious\n").expect("dirty");

    let fresh = worktree(&root, "fresh-ws");
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
    let _ = dir;
}

#[test]
fn run_gates_substitutes_the_base_commit_token_and_records_the_resolved_argv() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
    let info = provision(&root, &workspace);

    // A gate that pins to `{base_commit}`: after substitution the argv carries
    // the real SHA, and the recorded run shows exactly what executed.
    let outcome = handlers::run_gates(
        &Shell::inherited(),
        GateInput {
            workspace_path: workspace,
            base_commit: info.base_commit.clone(),
            gates: vec![GateCommand {
                name: "pin".to_owned(),
                argv: vec!["true".to_owned(), "{base_commit}".to_owned()],
            }],
        },
    )
    .expect("run_gates succeeds");
    assert!(outcome.pass);
    assert_eq!(
        outcome.runs[0].argv,
        vec!["true".to_owned(), info.base_commit.clone()],
        "the recorded argv must carry the substituted SHA, not the token"
    );
    assert!(
        !outcome.runs[0]
            .argv
            .iter()
            .any(|a| a.contains("{base_commit}")),
        "no argv element may still carry the literal token"
    );
    let _ = dir;
}

#[test]
fn reset_is_a_no_op_on_a_clean_worktree() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
    provision(&root, &workspace);

    let outcome = handlers::reset(
        &Shell::inherited(),
        ResetInput {
            repo_root: root.clone(),
            workspace_path: workspace,
        },
    )
    .expect("reset runs");
    assert!(outcome.was_clean, "a freshly provisioned worktree is clean");
    assert!(outcome.droppings.is_empty());
    let _ = dir;
}

#[test]
fn reset_records_and_removes_lens_droppings() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
    provision(&root, &workspace);

    // Simulate a misbehaving lens: an untracked file AND a tracked edit.
    let dropping = std::path::Path::new(&workspace).join("lens-scratch.txt");
    std::fs::write(&dropping, "a lens wrote this\n").expect("write dropping");
    std::fs::write(
        std::path::Path::new(&workspace).join("lib.txt"),
        "tampered\n",
    )
    .expect("edit");

    let outcome = handlers::reset(
        &Shell::inherited(),
        ResetInput {
            repo_root: root.clone(),
            workspace_path: workspace.clone(),
        },
    )
    .expect("reset runs");
    assert!(!outcome.was_clean, "droppings must be recorded, not silent");
    assert!(
        outcome
            .droppings
            .iter()
            .any(|line| line.contains("lens-scratch.txt")),
        "the untracked dropping must be recorded; droppings: {:?}",
        outcome.droppings
    );
    // git clean -fd removed the untracked dropping; git checkout -- . reverted
    // the tracked edit back to the committed content.
    assert!(!dropping.exists(), "the untracked dropping is removed");
    let lib =
        std::fs::read_to_string(std::path::Path::new(&workspace).join("lib.txt")).expect("read");
    assert_eq!(lib, "original\n", "the tracked edit is reverted to HEAD");
    let _ = dir;
}

#[test]
fn reset_refuses_a_target_that_is_the_repo_root() {
    // The handler applies the destructive-path guard: the repo itself must be
    // unreachable, even if a misconfigured input names it.
    let (dir, root) = repo_with_initial_commit();
    let error = handlers::reset(
        &Shell::inherited(),
        ResetInput {
            repo_root: root.clone(),
            workspace_path: root.clone(),
        },
    )
    .expect_err("the repo root must never be a reset target");
    assert!(
        error.message().contains("destructive git operation"),
        "{}",
        error.message()
    );
    let _ = dir;
}

#[test]
fn verify_gates_passes_on_a_clean_green_tree_and_writes_the_untruncated_log() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
    let info = provision(&root, &workspace);
    let log_path = log_file(&root, "wf-verify.log");

    let outcome = handlers::verify_gates(
        &Shell::inherited(),
        VerifyInput {
            workspace_path: workspace,
            base_commit: info.base_commit,
            gates: vec![GateCommand {
                name: "smoke".to_owned(),
                argv: vec!["sh".to_owned(), "-c".to_owned(), "echo verified".to_owned()],
            }],
            log_path: log_path.clone(),
        },
    )
    .expect("verify runs");
    assert!(
        outcome.pass,
        "a green gate on a clean tree passes: {}",
        outcome.detail
    );
    assert!(outcome.pre_clean && outcome.post_clean);
    let log = std::fs::read_to_string(&log_path).expect("the untruncated log is written");
    assert!(log.contains("pre_clean: true"), "log:\n{log}");
    assert!(
        log.contains("verified"),
        "the full gate output is logged; log:\n{log}"
    );
    let _ = dir;
}

#[test]
fn verify_gates_records_a_red_gate_without_failing_the_activity() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
    let info = provision(&root, &workspace);
    let log_path = log_file(&root, "red-verify.log");

    let outcome = handlers::verify_gates(
        &Shell::inherited(),
        VerifyInput {
            workspace_path: workspace,
            base_commit: info.base_commit,
            gates: vec![GateCommand {
                name: "red".to_owned(),
                argv: vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "echo boom; exit 1".to_owned(),
                ],
            }],
            log_path,
        },
    )
    .expect("a red verify gate is recorded data, never an activity error");
    assert!(!outcome.pass, "a red gate makes the verification fail");
    assert!(
        outcome.pre_clean && outcome.post_clean,
        "echo did not dirty the tree"
    );
    assert!(!outcome.runs[0].passed);
    assert!(outcome.runs[0].output_tail.contains("boom"));
    let _ = dir;
}

#[test]
fn verify_gates_flags_a_dirty_tree_as_not_pre_clean() {
    let (dir, root) = repo_with_initial_commit();
    let workspace = worktree(&root, "ws");
    let info = provision(&root, &workspace);
    // A dirty tree going into verify: the clean-tree assertion must record it.
    std::fs::write(
        std::path::Path::new(&workspace).join("uncommitted.txt"),
        "x\n",
    )
    .expect("dirty");
    let log_path = log_file(&root, "dirty-verify.log");

    let outcome = handlers::verify_gates(
        &Shell::inherited(),
        VerifyInput {
            workspace_path: workspace,
            base_commit: info.base_commit,
            gates: vec![GateCommand {
                name: "smoke".to_owned(),
                argv: vec!["true".to_owned()],
            }],
            log_path,
        },
    )
    .expect("verify runs");
    assert!(
        !outcome.pre_clean,
        "a dirty tree must flag pre_clean = false"
    );
    assert!(!outcome.pass, "verification never passes on a dirty tree");
    assert!(
        outcome.detail.contains("DIRTY"),
        "detail: {}",
        outcome.detail
    );
    let _ = dir;
}
