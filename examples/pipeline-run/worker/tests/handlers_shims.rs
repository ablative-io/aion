#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hermetic shim tests for the four shell-activity handlers.
//!
//! Each test builds a directory of fake `git`/`cargo`/`collective` scripts,
//! points a [`Shell`] at exactly that directory (nothing else on `PATH`), and
//! drives the REAL handler bodies. The shims record their argv to a log file and
//! emit chosen exit statuses, so the tests exercise the exact production
//! shell-out — the same seam `meridian_dev_pipeline`'s `handlers_shims` uses — and
//! assert both the handler's returned outcome AND the commands it issued.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use pipeline_run_worker::handlers;
use pipeline_run_worker::shell::Shell;
use pipeline_run_worker::types::{GateInput, LandInput, LandUnit, NotifyInput, ProvisionInput};

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

    fn bin_dir(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn log_path(&self) -> PathBuf {
        self.dir.path().join("argv.log")
    }

    /// Install a fake executable `name` whose body is `script` (a `sh` program).
    /// The script runs with `$LOG` set to the argv log path.
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
        Shell::with_path(self.bin_dir())
    }

    fn log(&self) -> String {
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }
}

/// A real directory to use as a cwd (the handlers require an existing cwd).
fn workdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("workdir")
}

#[test]
fn provision_issues_a_worktree_add_and_returns_the_branch() {
    let shims = Shims::new();
    // git succeeds for every subcommand.
    shims.install("git", "exit 0");
    let work = workdir();

    let input = ProvisionInput {
        repo_root: work.path().display().to_string(),
        base_branch: "main".to_owned(),
        unit_branch: "pipeline/DC-1/u1".to_owned(),
        workspace_path: work.path().join("ws-u1").display().to_string(),
    };
    let info = handlers::provision(&shims.shell(), input).expect("provision ok");
    assert_eq!(info.branch, "pipeline/DC-1/u1");
    assert!(info.workspace_path.ends_with("ws-u1"));

    let log = shims.log();
    assert!(log.contains("worktree add"), "log was: {log}");
    assert!(log.contains("-B pipeline/DC-1/u1"), "log was: {log}");
    assert!(log.contains("main"), "log was: {log}");
}

#[test]
fn provision_is_terminal_when_git_cannot_run() {
    // Empty PATH dir: no git shim installed at all.
    let shims = Shims::new();
    let work = workdir();
    let input = ProvisionInput {
        repo_root: work.path().display().to_string(),
        base_branch: "main".to_owned(),
        unit_branch: "b".to_owned(),
        workspace_path: work.path().join("ws").display().to_string(),
    };
    let error = handlers::provision(&shims.shell(), input).expect_err("must fail");
    assert!(error.to_string().contains("git"), "error: {error}");
}

#[test]
fn gate_passes_when_clippy_and_test_both_exit_zero() {
    let shims = Shims::new();
    shims.install("cargo", "exit 0");
    let work = workdir();
    let outcome = handlers::gate(
        &shims.shell(),
        GateInput {
            workspace_path: work.path().display().to_string(),
        },
    )
    .expect("gate ok");
    assert!(outcome.pass, "should pass");
    assert_eq!(outcome.diagnostics, "");
    let log = shims.log();
    assert!(
        log.contains("clippy --workspace --all-targets"),
        "log: {log}"
    );
    assert!(log.contains("test --workspace"), "log: {log}");
}

#[test]
fn gate_fails_and_captures_diagnostics_when_clippy_is_red() {
    let shims = Shims::new();
    // clippy (the first `cargo` invocation after fmt) prints an error and fails;
    // fmt is the very first cargo call and must succeed, so key on the args.
    shims.install(
        "cargo",
        "case \"$1\" in \
           fmt) exit 0 ;; \
           clippy) echo 'error: clippy found a lint' 1>&2; exit 101 ;; \
           *) exit 0 ;; \
         esac",
    );
    let work = workdir();
    let outcome = handlers::gate(
        &shims.shell(),
        GateInput {
            workspace_path: work.path().display().to_string(),
        },
    )
    .expect("gate ok (a red gate is DATA, not an error)");
    assert!(!outcome.pass, "should fail");
    assert!(
        outcome.diagnostics.contains("clippy found a lint"),
        "diag: {}",
        outcome.diagnostics
    );
    // test must NOT have run after a red clippy.
    assert!(
        !shims.log().contains("test --workspace"),
        "test ran after red clippy"
    );
}

#[test]
fn land_merges_each_branch_and_records_them_in_order() {
    let shims = Shims::new();
    // git: show-ref (branch missing -> non-zero), everything else succeeds.
    shims.install(
        "git",
        "case \"$*\" in \
           *show-ref*) exit 1 ;; \
           *) exit 0 ;; \
         esac",
    );
    let work = workdir();
    let outcome = handlers::land(
        &shims.shell(),
        LandInput {
            repo_root: work.path().display().to_string(),
            base_branch: "main".to_owned(),
            integration_branch: "pipeline/DC-1/integration".to_owned(),
            units: vec![
                LandUnit {
                    unit_id: "u1".to_owned(),
                    branch: "pipeline/DC-1/u1".to_owned(),
                },
                LandUnit {
                    unit_id: "u2".to_owned(),
                    branch: "pipeline/DC-1/u2".to_owned(),
                },
            ],
        },
    )
    .expect("land ok");
    assert_eq!(outcome.landed, vec!["u1".to_owned(), "u2".to_owned()]);
    let log = shims.log();
    assert!(
        log.contains("merge --no-ff --no-edit pipeline/DC-1/u1"),
        "log: {log}"
    );
    assert!(
        log.contains("merge --no-ff --no-edit pipeline/DC-1/u2"),
        "log: {log}"
    );
    // First land seeds the integration branch from base with -B.
    assert!(log.contains("-B pipeline/DC-1/integration"), "log: {log}");
}

#[test]
fn land_stops_and_aborts_on_a_conflicting_merge() {
    let shims = Shims::new();
    // The merge of u2 conflicts (non-zero); u1 merges fine. show-ref missing.
    shims.install(
        "git",
        "case \"$*\" in \
           *show-ref*) exit 1 ;; \
           *merge\\ --no-ff*u2*) echo 'CONFLICT' 1>&2; exit 1 ;; \
           *) exit 0 ;; \
         esac",
    );
    let work = workdir();
    let outcome = handlers::land(
        &shims.shell(),
        LandInput {
            repo_root: work.path().display().to_string(),
            base_branch: "main".to_owned(),
            integration_branch: "pipeline/DC-1/integration".to_owned(),
            units: vec![
                LandUnit {
                    unit_id: "u1".to_owned(),
                    branch: "pipeline/DC-1/u1".to_owned(),
                },
                LandUnit {
                    unit_id: "u2".to_owned(),
                    branch: "pipeline/DC-1/u2".to_owned(),
                },
            ],
        },
    )
    .expect("land ok (a conflict is DATA, not an error)");
    assert_eq!(outcome.landed, vec!["u1".to_owned()], "only u1 landed");
    assert!(
        outcome.detail.contains("u2"),
        "detail should name the stopped unit: {}",
        outcome.detail
    );
    assert!(
        shims.log().contains("merge --abort"),
        "a conflict must abort"
    );
}

#[test]
fn notify_reports_sent_when_collective_succeeds() {
    let shims = Shims::new();
    shims.install("collective", "exit 0");
    let outcome = handlers::notify(
        &shims.shell(),
        NotifyInput {
            brief_id: "DC-1".to_owned(),
            summary: "all units landed".to_owned(),
        },
    )
    .expect("notify ok");
    assert!(outcome.sent, "should report sent");
    let log = shims.log();
    assert!(log.contains("send --as Meridian"), "log: {log}");
    assert!(
        log.contains("DC-1 pipeline complete"),
        "subject in log: {log}"
    );
}

#[test]
fn notify_degrades_to_log_only_when_collective_is_absent() {
    // No collective shim on PATH: notify must NOT fail, just report log-only.
    let shims = Shims::new();
    let outcome = handlers::notify(
        &shims.shell(),
        NotifyInput {
            brief_id: "DC-1".to_owned(),
            summary: "done".to_owned(),
        },
    )
    .expect("notify never fails for a missing notifier");
    assert!(!outcome.sent);
    assert!(
        outcome.detail.contains("not available"),
        "detail: {}",
        outcome.detail
    );
}

/// Sanity: the shim dir is the ONLY thing on the effective PATH (a real `git`
/// on the machine must not leak into these tests).
#[test]
fn shims_are_isolated_from_the_real_path() {
    let shims = Shims::new();
    // Nothing installed: even `git --version` must be unresolved.
    let result = shims
        .shell()
        .run("git", &["--version"], Path::new(".").to_str().unwrap());
    assert!(
        result.is_err(),
        "an empty shim dir must not resolve the real git"
    );
}
