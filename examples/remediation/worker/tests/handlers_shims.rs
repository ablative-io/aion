#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hermetic shim tests for the five shell-activity handlers.
//!
//! Each test builds a directory of fake `git`/`cargo`/`python3` scripts,
//! points a [`Shell`] at exactly that directory (nothing else on `PATH`), and
//! drives the REAL handler bodies. The shims record their argv to a log file
//! and emit chosen exit statuses/output, so the tests exercise the exact
//! production shell-out — the same seam the pipeline-run worker's
//! `handlers_shims` uses — and assert both the handler's returned outcome AND
//! the commands it issued.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use remediation_worker::handlers;
use remediation_worker::shell::Shell;
use remediation_worker::types::{
    ArtifactKind, CleanupInput, Gate1Input, Gate2Input, LedgerUpdateInput, ProvisionInput,
};

/// A directory of fake CLIs plus the argv log they append to.
struct Shims {
    dir: tempfile::TempDir,
}

impl Shims {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn log_path(&self) -> PathBuf {
        self.dir.path().join("argv.log")
    }

    /// Install a fake executable `name` whose body is `script` (a `sh`
    /// program). The script runs with `$LOG` set to the argv log path.
    fn install(&self, name: &str, script: &str) {
        let path = self.dir.path().join(name);
        let log = self.log_path();
        let body = format!(
            "#!/bin/sh\nLOG=\"{}\"\necho \"{name} $*\" >> \"$LOG\"\n{script}\n",
            log.display()
        );
        fs::write(&path, body).expect("write shim");
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
    }

    fn shell(&self) -> Shell {
        Shell::with_path(self.dir.path())
    }

    fn log(&self) -> String {
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }
}

/// A real directory to use as a cwd (the handlers require an existing cwd).
fn workdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("workdir")
}

// --- provision_workspace -------------------------------------------------------

#[test]
fn provision_creates_the_worktree_and_reports_the_base_commit() {
    let shims = Shims::new();
    shims.install(
        "git",
        "case \"$*\" in \
           *rev-parse*) echo abc123def ;; \
           *) : ;; \
         esac\nexit 0",
    );
    let work = workdir();

    let info = handlers::provision(
        &shims.shell(),
        ProvisionInput {
            repo_root: work.path().display().to_string(),
            base_branch: "main".to_owned(),
            branch: "remediation/B-1".to_owned(),
            workspace_path: work.path().join("ws-b1").display().to_string(),
        },
    )
    .expect("provision ok");
    assert_eq!(info.branch, "remediation/B-1");
    assert_eq!(info.base_commit, "abc123def");

    let log = shims.log();
    assert!(log.contains("worktree add"), "log: {log}");
    assert!(log.contains("-B remediation/B-1"), "log: {log}");
    assert!(log.contains("main"), "log: {log}");
    assert!(log.contains("rev-parse HEAD"), "log: {log}");
}

#[test]
fn provision_is_terminal_when_git_cannot_run() {
    let shims = Shims::new();
    let work = workdir();
    let error = handlers::provision(
        &shims.shell(),
        ProvisionInput {
            repo_root: work.path().display().to_string(),
            base_branch: "main".to_owned(),
            branch: "b".to_owned(),
            workspace_path: work.path().join("ws").display().to_string(),
        },
    )
    .expect_err("must fail");
    assert!(error.to_string().contains("git"), "error: {error}");
}

// --- gate1 -----------------------------------------------------------------------

/// A git shim for gate1: clean status, a fixed HEAD, and an authored-file
/// diff of `diff_paths` (newline-separated printf body).
fn gate1_git_with_diff(shims: &Shims, diff_paths: &str) {
    shims.install(
        "git",
        &format!(
            "case \"$*\" in \
               *status*) : ;; \
               *rev-parse*) echo feedbeef ;; \
               *'diff --name-only'*) printf '{diff_paths}' ;; \
               *) : ;; \
             esac\nexit 0"
        ),
    );
}

fn gate1_git(shims: &Shims) {
    gate1_git_with_diff(shims, "crates/yg/tests/yg268_teardown.rs\\n");
}

/// One runnable check guarding YG-268, expecting the given signature.
fn check(signature: &str) -> remediation_worker::types::Gate1Check {
    remediation_worker::types::Gate1Check {
        finding_id: "YG-268".to_owned(),
        test_names: vec!["yg268_teardown".to_owned()],
        expected_failure_signature: signature.to_owned(),
    }
}

fn gate1_input(
    work: &std::path::Path,
    checks: Vec<remediation_worker::types::Gate1Check>,
) -> Gate1Input {
    Gate1Input {
        workspace_path: work.display().to_string(),
        base_commit: "abc123".to_owned(),
        checks,
        acceptance: vec![],
        test_files: vec!["crates/yg/tests/yg268_teardown.rs".to_owned()],
    }
}

#[test]
fn gate1_passes_when_every_authored_test_fails_with_its_signature() {
    let shims = Shims::new();
    gate1_git(&shims);
    // cargo test: the authored test runs and FAILS printing the signature.
    shims.install(
        "cargo",
        "echo 'running 1 test'\necho 'test yg268 ... FAILED: teardown deleted uncommitted work' 1>&2\nexit 101",
    );
    let work = workdir();

    let outcome = handlers::gate1(
        &shims.shell(),
        gate1_input(
            work.path(),
            vec![check("teardown deleted uncommitted work")],
        ),
    )
    .expect("gate1 ok");
    assert!(outcome.pass, "detail: {}", outcome.detail);
    assert_eq!(outcome.tests_commit, "feedbeef");
    assert_eq!(
        outcome.authored_test_paths,
        vec!["crates/yg/tests/yg268_teardown.rs".to_owned()]
    );
    assert!(outcome.scope_violations.is_empty());
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(outcome.results[0].finding_id, "YG-268");
    assert!(outcome.results[0].failed);
    assert!(outcome.results[0].signature_matched);
    assert!(outcome.results[0].evidence.contains("FAILED"));
    assert!(
        shims.log().contains("test --workspace yg268_teardown"),
        "log: {}",
        shims.log()
    );
}

#[test]
fn gate1_fails_when_a_test_fails_without_its_signature() {
    let shims = Shims::new();
    gate1_git(&shims);
    // The test FAILS, but for the wrong reason: the expected signature never
    // appears in the output (e.g. a compile error or an unrelated panic).
    shims.install(
        "cargo",
        "echo 'running 1 test'\necho 'error[E0432]: unresolved import' 1>&2\nexit 101",
    );
    let work = workdir();

    let outcome = handlers::gate1(
        &shims.shell(),
        gate1_input(
            work.path(),
            vec![check("teardown deleted uncommitted work")],
        ),
    )
    .expect("gate1 ok (a wrong-reason failure is a recorded gate failure)");
    assert!(!outcome.pass);
    assert!(outcome.results[0].failed);
    assert!(!outcome.results[0].signature_matched);
    assert!(
        outcome.detail.contains("wrong reason"),
        "detail: {}",
        outcome.detail
    );
}

#[test]
fn gate1_fails_when_the_authored_diff_touches_production_code() {
    let shims = Shims::new();
    // The diff includes a production path alongside the declared test file.
    gate1_git_with_diff(
        &shims,
        "crates/yg/tests/yg268_teardown.rs\\ncrates/yg/src/teardown.rs\\n",
    );
    shims.install("cargo", "echo 'running 1 test'\necho 'sig' 1>&2\nexit 101");
    let work = workdir();

    let outcome = handlers::gate1(&shims.shell(), gate1_input(work.path(), vec![check("sig")]))
        .expect("gate1 ok (a scope violation is a recorded gate failure)");
    assert!(!outcome.pass);
    assert_eq!(
        outcome.scope_violations,
        vec!["crates/yg/src/teardown.rs".to_owned()]
    );
    assert!(
        outcome.detail.contains("non-test paths"),
        "detail: {}",
        outcome.detail
    );
}

#[test]
fn gate1_allows_undeclared_paths_that_match_the_test_path_rule() {
    let shims = Shims::new();
    // A helper fixture the manifest did not declare, but living under tests/:
    // allowed by the shared test-path rule.
    gate1_git_with_diff(
        &shims,
        "crates/yg/tests/yg268_teardown.rs\\ncrates/yg/tests/support/fixtures.rs\\n",
    );
    shims.install("cargo", "echo 'running 1 test'\necho 'sig' 1>&2\nexit 101");
    let work = workdir();

    let outcome = handlers::gate1(&shims.shell(), gate1_input(work.path(), vec![check("sig")]))
        .expect("gate1 ok");
    assert!(outcome.pass, "detail: {}", outcome.detail);
    assert!(outcome.scope_violations.is_empty());
}

#[test]
fn gate1_fails_when_an_authored_test_passes_on_unfixed_code() {
    let shims = Shims::new();
    gate1_git(&shims);
    shims.install(
        "cargo",
        "echo 'running 1 test'\necho 'test result: ok'\nexit 0",
    );
    let work = workdir();

    let outcome = handlers::gate1(&shims.shell(), gate1_input(work.path(), vec![check("sig")]))
        .expect("gate1 ok (a passing test is a recorded gate failure, not an error)");
    assert!(!outcome.pass);
    assert!(
        outcome.detail.contains("did not fail"),
        "detail: {}",
        outcome.detail
    );
    assert!(!outcome.results[0].failed);
}

#[test]
fn gate1_flags_a_test_name_that_matched_nothing() {
    let shims = Shims::new();
    gate1_git(&shims);
    // Exit 0 with zero tests run anywhere: the name matched nothing.
    shims.install("cargo", "echo 'running 0 tests'\nexit 0");
    let work = workdir();

    let mut ghost = check("sig");
    ghost.test_names = vec!["ghost_test".to_owned()];
    let outcome =
        handlers::gate1(&shims.shell(), gate1_input(work.path(), vec![ghost])).expect("gate1 ok");
    assert!(!outcome.pass);
    assert!(
        outcome.detail.contains("no test matched the name"),
        "detail: {}",
        outcome.detail
    );
}

#[test]
fn gate1_fails_when_the_authored_tests_are_not_committed() {
    let shims = Shims::new();
    shims.install(
        "git",
        "case \"$*\" in \
           *status*) echo ' M tests/new_test.rs' ;; \
           *rev-parse*) echo feedbeef ;; \
           *) : ;; \
         esac\nexit 0",
    );
    shims.install("cargo", "exit 101");
    let work = workdir();

    let outcome = handlers::gate1(&shims.shell(), gate1_input(work.path(), vec![check("sig")]))
        .expect("gate1 ok");
    assert!(!outcome.pass);
    assert!(
        outcome.detail.contains("not committed"),
        "detail: {}",
        outcome.detail
    );
    // The tests were never run: an uncommitted authored set voids the gate.
    assert!(outcome.results.is_empty());
    assert!(
        !shims.log().contains("cargo test"),
        "tests must not run on a dirty tree"
    );
}

#[test]
fn gate1_echoes_manual_acceptance_entries_without_running_anything() {
    let shims = Shims::new();
    gate1_git_with_diff(&shims, "");
    shims.install("cargo", "exit 0");
    let work = workdir();

    let outcome = handlers::gate1(
        &shims.shell(),
        Gate1Input {
            workspace_path: work.path().display().to_string(),
            base_commit: "abc123".to_owned(),
            checks: vec![],
            acceptance: vec![remediation_worker::types::AcceptanceCheck {
                finding_id: "YG-401".to_owned(),
                criterion: "error type carries the offending path".to_owned(),
            }],
            test_files: vec![],
        },
    )
    .expect("gate1 ok");
    assert!(outcome.pass);
    assert_eq!(outcome.acceptance_checks.len(), 1);
    assert_eq!(outcome.acceptance_checks[0].finding_id, "YG-401");
    assert!(
        outcome.detail.contains("no authored tests to re-run"),
        "detail: {}",
        outcome.detail
    );
    assert!(
        !shims.log().contains("cargo"),
        "nothing runs for manual-acceptance entries: {}",
        shims.log()
    );
}

#[test]
fn is_test_path_matches_the_shared_rule() {
    for path in [
        "crates/yg/tests/yg268.rs",
        "crates/yg/test/helper.rs",
        "src/teardown_test.rs",
        "src/teardown_tests.rs",
        "tests/test_teardown.py",
    ] {
        assert!(handlers::is_test_path(path), "{path} should be a test path");
    }
    for path in [
        "crates/yg/src/teardown.rs",
        "src/testing.rs",
        "docs/tests.md.bak/../evil.rs",
        "src/contest.rs",
    ] {
        assert!(
            !handlers::is_test_path(path),
            "{path} should NOT be a test path"
        );
    }
}

// --- gate2 ------------------------------------------------------------------------

/// A git shim for gate2 whose `diff --name-only` output over the authored
/// paths is `tamper_lines`, and whose plain `diff` (verifier evidence) is a
/// fixed patch body.
fn gate2_git(shims: &Shims, tamper_lines: &str) {
    shims.install(
        "git",
        &format!(
            "case \"$*\" in \
               *'diff --name-only'*) printf '{tamper_lines}' ;; \
               *diff*) echo 'diff --git a/src/fix.rs b/src/fix.rs' ;; \
               *) : ;; \
             esac\nexit 0"
        ),
    );
}

#[test]
fn gate2_passes_when_tests_untouched_and_cargo_green() {
    let shims = Shims::new();
    gate2_git(&shims, "");
    shims.install("cargo", "exit 0");
    let work = workdir();

    let outcome = handlers::gate2(
        &shims.shell(),
        Gate2Input {
            workspace_path: work.path().display().to_string(),
            tests_commit: "feedbeef".to_owned(),
            authored_test_paths: vec!["tests/yg268.rs".to_owned()],
        },
    )
    .expect("gate2 ok");
    assert!(outcome.pass, "diagnostics: {}", outcome.diagnostics);
    assert!(outcome.checks.test_diff_clean);
    assert!(outcome.checks.clippy_pass);
    assert!(outcome.checks.suite_pass);
    assert!(outcome.diff.contains("diff --git"));

    let log = shims.log();
    assert!(
        log.contains("diff --name-only feedbeef -- tests/yg268.rs"),
        "log: {log}"
    );
    assert!(
        log.contains("clippy --workspace --all-targets"),
        "log: {log}"
    );
    assert!(log.contains("test --workspace"), "log: {log}");
    assert!(
        log.contains("fmt --all"),
        "autoformat runs in write mode: {log}"
    );
}

#[test]
fn gate2_records_an_authored_test_edit_as_a_tamper() {
    let shims = Shims::new();
    gate2_git(&shims, "tests/yg268.rs\\n");
    shims.install("cargo", "exit 0");
    let work = workdir();

    let outcome = handlers::gate2(
        &shims.shell(),
        Gate2Input {
            workspace_path: work.path().display().to_string(),
            tests_commit: "feedbeef".to_owned(),
            authored_test_paths: vec!["tests/yg268.rs".to_owned()],
        },
    )
    .expect("gate2 ok (a tamper is DATA, not an error)");
    assert!(!outcome.pass);
    assert!(!outcome.checks.test_diff_clean);
    assert!(
        outcome.diagnostics.contains("authored tests were modified"),
        "diagnostics: {}",
        outcome.diagnostics
    );
    // Cargo checks still ran: the loop-back carries the full picture.
    assert!(outcome.checks.clippy_pass);
    assert!(outcome.checks.suite_pass);
}

#[test]
fn gate2_records_a_red_clippy_with_its_output() {
    let shims = Shims::new();
    gate2_git(&shims, "");
    shims.install(
        "cargo",
        "case \"$1\" in \
           fmt) exit 0 ;; \
           clippy) echo 'error: clippy found a lint' 1>&2; exit 101 ;; \
           *) exit 0 ;; \
         esac",
    );
    let work = workdir();

    let outcome = handlers::gate2(
        &shims.shell(),
        Gate2Input {
            workspace_path: work.path().display().to_string(),
            tests_commit: "feedbeef".to_owned(),
            authored_test_paths: vec![],
        },
    )
    .expect("gate2 ok");
    assert!(!outcome.pass);
    assert!(
        outcome.checks.test_diff_clean,
        "empty authored set is trivially clean"
    );
    assert!(!outcome.checks.clippy_pass);
    assert!(
        outcome.diagnostics.contains("clippy found a lint"),
        "diagnostics: {}",
        outcome.diagnostics
    );
}

#[test]
fn gate2_records_a_red_suite_with_its_output() {
    let shims = Shims::new();
    gate2_git(&shims, "");
    shims.install(
        "cargo",
        "case \"$1\" in \
           test) echo 'test yg268 ... FAILED' 1>&2; exit 101 ;; \
           *) exit 0 ;; \
         esac",
    );
    let work = workdir();

    let outcome = handlers::gate2(
        &shims.shell(),
        Gate2Input {
            workspace_path: work.path().display().to_string(),
            tests_commit: "feedbeef".to_owned(),
            authored_test_paths: vec![],
        },
    )
    .expect("gate2 ok");
    assert!(!outcome.pass);
    assert!(!outcome.checks.suite_pass);
    assert!(
        outcome.diagnostics.contains("FAILED"),
        "diagnostics: {}",
        outcome.diagnostics
    );
}

// --- ledger_update -------------------------------------------------------------------

#[test]
fn ledger_update_invokes_the_applier_with_the_contracted_arguments() {
    let shims = Shims::new();
    shims.install("python3", "echo 'applied 2 transitions'\nexit 0");
    let work = workdir();

    let outcome = handlers::ledger_update(
        &shims.shell(),
        LedgerUpdateInput {
            repo_root: work.path().display().to_string(),
            ledger_path: "docs/reviews/audit.ledger.json".to_owned(),
            kind: ArtifactKind::TestManifest,
            artifact_json: "{\"brief_id\":\"B-1\",\"entries\":[]}".to_owned(),
        },
    )
    .expect("ledger_update ok");
    assert!(outcome.applied);
    assert!(
        outcome.detail.contains("applied"),
        "detail: {}",
        outcome.detail
    );

    let log = shims.log();
    assert!(
        log.contains("python3 scripts/remediation/apply_transitions.py"),
        "log: {log}"
    );
    assert!(
        log.contains("--ledger docs/reviews/audit.ledger.json"),
        "log: {log}"
    );
    assert!(log.contains("--kind test_manifest"), "log: {log}");
    assert!(log.contains("--artifact "), "log: {log}");
}

#[test]
fn ledger_update_reports_an_applier_refusal_honestly() {
    let shims = Shims::new();
    shims.install(
        "python3",
        "echo 'transition open->fixed_verified is not legal' 1>&2\nexit 3",
    );
    let work = workdir();

    let outcome = handlers::ledger_update(
        &shims.shell(),
        LedgerUpdateInput {
            repo_root: work.path().display().to_string(),
            ledger_path: "l.json".to_owned(),
            kind: ArtifactKind::Disposition,
            artifact_json: "{}".to_owned(),
        },
    )
    .expect("a refused transition is DATA, not an activity error");
    assert!(!outcome.applied);
    assert!(
        outcome.detail.contains("exited 3"),
        "detail: {}",
        outcome.detail
    );
    assert!(
        outcome.detail.contains("not legal"),
        "the applier's own words ride back: {}",
        outcome.detail
    );
}

#[test]
fn ledger_update_is_terminal_when_python_is_missing() {
    let shims = Shims::new();
    let work = workdir();
    let error = handlers::ledger_update(
        &shims.shell(),
        LedgerUpdateInput {
            repo_root: work.path().display().to_string(),
            ledger_path: "l.json".to_owned(),
            kind: ArtifactKind::Verdict,
            artifact_json: "{}".to_owned(),
        },
    )
    .expect_err("missing python3 is infrastructure");
    assert!(error.to_string().contains("python3"), "error: {error}");
}

// --- cleanup_workspace ------------------------------------------------------------------

#[test]
fn cleanup_removes_a_clean_worktree() {
    let shims = Shims::new();
    shims.install("git", "exit 0");
    let repo = workdir();
    let workspace = workdir();

    let outcome = handlers::cleanup(
        &shims.shell(),
        CleanupInput {
            repo_root: repo.path().display().to_string(),
            workspace_path: workspace.path().display().to_string(),
        },
    )
    .expect("cleanup ok");
    assert!(outcome.removed, "detail: {}", outcome.detail);
    assert!(
        shims.log().contains("worktree remove --force"),
        "log: {}",
        shims.log()
    );
}

#[test]
fn cleanup_refuses_to_remove_a_dirty_worktree() {
    let shims = Shims::new();
    shims.install(
        "git",
        "case \"$*\" in \
           *status*) echo ' M src/half_finished_fix.rs' ;; \
           *) : ;; \
         esac\nexit 0",
    );
    let repo = workdir();
    let workspace = workdir();

    let outcome = handlers::cleanup(
        &shims.shell(),
        CleanupInput {
            repo_root: repo.path().display().to_string(),
            workspace_path: workspace.path().display().to_string(),
        },
    )
    .expect("cleanup ok");
    assert!(!outcome.removed);
    assert!(
        outcome.detail.contains("uncommitted changes"),
        "detail: {}",
        outcome.detail
    );
    assert!(
        !shims.log().contains("worktree remove"),
        "a dirty worktree must never be removed: {}",
        shims.log()
    );
}

#[test]
fn cleanup_reports_an_absent_workspace_without_failing() {
    let shims = Shims::new();
    shims.install("git", "exit 0");
    let repo = workdir();

    let outcome = handlers::cleanup(
        &shims.shell(),
        CleanupInput {
            repo_root: repo.path().display().to_string(),
            workspace_path: repo.path().join("never-created").display().to_string(),
        },
    )
    .expect("cleanup ok");
    assert!(!outcome.removed);
    assert!(
        outcome.detail.contains("not present"),
        "detail: {}",
        outcome.detail
    );
}

// --- clip ------------------------------------------------------------------------------

#[test]
fn clip_marks_a_truncation_and_keeps_head_and_tail() {
    let long = format!("HEAD{}TAIL", "x".repeat(60_000));
    let clipped = handlers::clip(&long);
    assert!(clipped.len() < long.len());
    assert!(clipped.starts_with("HEAD"));
    assert!(clipped.ends_with("TAIL"));
    assert!(clipped.contains("truncated"), "the cut must be explicit");

    let short = "small output";
    assert_eq!(handlers::clip(short), short, "short output is untouched");
}

// --- isolation sanity --------------------------------------------------------------------

#[test]
fn shims_are_isolated_from_the_real_path() {
    let shims = Shims::new();
    let result = shims.shell().run("git", &["--version"], ".");
    assert!(
        result.is_err(),
        "an empty shim dir must not resolve the real git"
    );
}
