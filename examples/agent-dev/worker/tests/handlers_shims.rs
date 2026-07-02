//! Hermetic handler tests with fake-CLI shims, following the
//! stacked-dev-remote suite: each test builds its own directory of stub
//! scripts (`git`, `cargo`) that emit canned output and append their argv to
//! per-executable log files, then constructs a `Shell` whose search path is
//! that directory ALONE. The handlers stay honest — they really shell out —
//! and the shims intercept at the process boundary. A CLI the test did not
//! stub is genuinely absent, which proves a missing CLI is a loud terminal
//! failure, never a silent skip. The `Shell` carries the search path (the
//! global `PATH` is never mutated), so the tests are parallel-safe.

#![cfg(unix)]

use std::error::Error;
use std::path::{Path, PathBuf};

use agent_dev_worker::handlers;
use agent_dev_worker::shell::Shell;
use agent_dev_worker::types::{GateInput, LandInput, ProvisionInput, Workspace};
use aion_worker::{ActivityFailure, Classification};

type TestResult = Result<(), Box<dyn Error>>;

/// One test's shim directory. A `workspace-root` subdirectory serves as the
/// stable workspace root handed to `provision`.
struct Shims {
    dir: tempfile::TempDir,
}

impl Shims {
    fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            dir: tempfile::tempdir()?,
        })
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn root_string(&self) -> String {
        self.root().to_string_lossy().into_owned()
    }

    /// The stable workspace root threaded to `provision`, as the composition
    /// root would after `resolve_workspace_root()`.
    fn workspace_root(&self) -> PathBuf {
        self.root().join("workspace-root")
    }

    /// A `Shell` resolving executables against the shim directory ALONE.
    fn shell(&self) -> Shell {
        Shell::with_path(self.root())
    }

    /// Write one shim: a `/bin/sh` script that records its argv to
    /// `<root>/<name>.log` and then runs `body`.
    fn write(&self, name: &str, body: &str) -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let path = self.root().join(name);
        let script = format!(
            "#!/bin/sh\nPATH=/usr/bin:/bin\necho \"$@\" >> \"{}/{name}.log\"\n{body}\n",
            self.root_string()
        );
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    /// Read one shim's argv recording (empty when the shim never ran).
    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join(format!("{name}.log"))).unwrap_or_default()
    }
}

fn assert_terminal(failure: &ActivityFailure, expected_fragment: &str) {
    assert_eq!(
        failure.classification(),
        &Classification::Terminal,
        "failure must be terminal: {failure:?}"
    );
    assert!(
        failure.message().contains(expected_fragment),
        "message {:?} must contain {expected_fragment:?}",
        failure.message()
    );
}

/// The `git` shim shared across provision scenarios: a clone that creates the
/// target directory it is handed (arg 3: `clone <url> <dir>`), everything
/// else a recorded success.
const GIT_PROVISION_SHIM: &str = r#"case "$1" in
  clone) mkdir -p "$3"; exit 0 ;;
  *) exit 0 ;;
esac"#;

fn provision_input(repo_url: String, run_id: &str) -> ProvisionInput {
    ProvisionInput {
        repo_url,
        base_ref: "main".to_owned(),
        brief_id: "brief-7".to_owned(),
        run_id: run_id.to_owned(),
    }
}

// --- provision -----------------------------------------------------------------

#[test]
fn provision_clones_into_the_run_keyed_layout_and_creates_the_branch() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", GIT_PROVISION_SHIM)?;
    let root = shims.workspace_root();

    let workspace = handlers::provision(
        &shims.shell(),
        &root,
        provision_input("https://example.test/repo.git".to_owned(), "wf-123"),
    )
    .map_err(|failure| failure.message().to_owned())?;

    let expected_repo = root.join("wf-123").join("repo");
    assert_eq!(PathBuf::from(&workspace.path), expected_repo);
    assert_eq!(workspace.branch, "agent-dev-brief-7");
    assert!(
        expected_repo.is_dir(),
        "the clone target directory must exist"
    );
    let log = shims.log("git");
    assert!(
        log.contains(&format!(
            "clone https://example.test/repo.git {}",
            expected_repo.display()
        )),
        "git clone must target <root>/<run_id>/repo; got log: {log}"
    );
    assert!(
        log.contains("checkout -b agent-dev-brief-7 main"),
        "the branch must be created off base_ref; got log: {log}"
    );
    Ok(())
}

#[test]
fn provision_collision_renames_the_stale_attempt_aside() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", GIT_PROVISION_SHIM)?;
    let root = shims.workspace_root();

    // An earlier partial attempt of the SAME run id, with salvageable content.
    let stale = root.join("wf-123");
    std::fs::create_dir_all(&stale)?;
    std::fs::write(stale.join("partial-clone.txt"), "half a clone\n")?;

    let workspace = handlers::provision(
        &shims.shell(),
        &root,
        provision_input("https://example.test/repo.git".to_owned(), "wf-123"),
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(
        PathBuf::from(&workspace.path),
        root.join("wf-123").join("repo")
    );
    // The stale attempt survives, renamed aside — never deleted.
    let renamed: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("wf-123.superseded-"))
        })
        .collect();
    assert_eq!(
        renamed.len(),
        1,
        "exactly one renamed stale attempt; got {renamed:?}"
    );
    assert!(
        renamed[0].join("partial-clone.txt").exists(),
        "the stale attempt's contents must survive the rename intact"
    );
    Ok(())
}

#[test]
fn provision_rejects_a_run_id_that_escapes_the_root() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", GIT_PROVISION_SHIM)?;
    let root = shims.workspace_root();

    for bad_run_id in ["../escape", "a/b", "/abs", "..", ".", ""] {
        let failure = handlers::provision(
            &shims.shell(),
            &root,
            provision_input("https://example.test/repo.git".to_owned(), bad_run_id),
        )
        .err()
        .ok_or_else(|| format!("run_id {bad_run_id:?} must be refused"))?;
        assert_terminal(&failure, "not a single path component");
    }
    assert!(shims.log("git").is_empty(), "no git call may run");
    Ok(())
}

#[test]
fn provision_failing_clone_is_terminal_with_diagnostics() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", "echo 'fatal: repository not found' >&2\nexit 128")?;
    let root = shims.workspace_root();

    let failure = handlers::provision(
        &shims.shell(),
        &root,
        provision_input("https://example.test/missing.git".to_owned(), "wf-404"),
    )
    .err()
    .ok_or("a failing clone must fail the activity")?;
    assert_terminal(&failure, "git clone failed — exit status 128");
    assert_terminal(&failure, "repository not found");
    Ok(())
}

// --- gate ------------------------------------------------------------------------

#[test]
fn gate_pass_runs_clippy_then_tests() -> TestResult {
    let shims = Shims::new()?;
    shims.write("cargo", "exit 0")?;

    let result = handlers::gate(
        &shims.shell(),
        GateInput {
            path: shims.root_string(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert!(result.pass, "both commands exit zero: the gate passes");
    assert!(
        result.diagnostics.is_empty(),
        "a pass carries no diagnostics"
    );
    let log = shims.log("cargo");
    assert!(
        log.contains("clippy --workspace --all-targets -- -D warnings"),
        "clippy must run with the full strict argv; got log: {log}"
    );
    assert!(
        log.contains("test --workspace"),
        "the test suite must run after clippy; got log: {log}"
    );
    Ok(())
}

#[test]
fn gate_failure_is_recorded_data_carrying_the_diagnostics_tail() -> TestResult {
    let shims = Shims::new()?;
    // Clippy fails; the test suite would pass but must not even run.
    shims.write(
        "cargo",
        r#"if [ "$1" = clippy ]; then echo 'error: unused variable `count`'; exit 1; fi
exit 0"#,
    )?;

    let result = handlers::gate(
        &shims.shell(),
        GateInput {
            path: shims.root_string(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert!(!result.pass, "a failing command is a recorded fail verdict");
    assert!(
        result
            .diagnostics
            .contains("cargo clippy failed — exit status 1"),
        "diagnostics must name the failing command; got: {}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .contains("error: unused variable `count`"),
        "diagnostics must carry the command output; got: {}",
        result.diagnostics
    );
    assert!(
        !shims.log("cargo").contains("test --workspace"),
        "the test suite must not run after a failed clippy"
    );
    Ok(())
}

#[test]
fn gate_test_failure_after_clean_clippy_is_recorded_data() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "cargo",
        r#"if [ "$1" = test ]; then echo 'test result: FAILED. 1 failed'; exit 101; fi
exit 0"#,
    )?;

    let result = handlers::gate(
        &shims.shell(),
        GateInput {
            path: shims.root_string(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert!(!result.pass);
    assert!(
        result
            .diagnostics
            .contains("cargo test failed — exit status 101"),
        "diagnostics must name the failing command; got: {}",
        result.diagnostics
    );
    assert!(result.diagnostics.contains("test result: FAILED"));
    Ok(())
}

#[test]
fn gate_that_cannot_run_at_all_is_a_loud_terminal_failure() -> TestResult {
    // No cargo shim is written, and the shim directory is the entire search
    // path, so cargo is genuinely absent — a broken environment, not a
    // recorded gate verdict.
    let shims = Shims::new()?;

    let failure = handlers::gate(
        &shims.shell(),
        GateInput {
            path: shims.root_string(),
        },
    )
    .err()
    .ok_or("a missing CLI must fail the activity")?;
    assert_terminal(
        &failure,
        "cargo clippy: executable not found on PATH: cargo",
    );
    Ok(())
}

#[test]
fn gate_against_a_missing_workspace_is_the_explicit_diagnostic() -> TestResult {
    let shims = Shims::new()?;
    shims.write("cargo", "exit 0")?;

    let failure = handlers::gate(
        &shims.shell(),
        GateInput {
            path: format!("{}/never-provisioned/repo", shims.root_string()),
        },
    )
    .err()
    .ok_or("a missing workspace must fail the activity")?;
    assert_terminal(&failure, "workspace missing at");
    assert_terminal(&failure, "run cannot resume");
    assert!(shims.log("cargo").is_empty(), "no gate command may run");
    Ok(())
}

// --- land ------------------------------------------------------------------------

#[test]
fn land_commits_and_returns_the_commit_sha() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "git",
        r#"if [ "$1" = rev-parse ]; then printf '%s\n' 0123456789abcdef0123456789abcdef01234567; fi
exit 0"#,
    )?;

    let landed = handlers::land(
        &shims.shell(),
        LandInput {
            workspace: Workspace {
                path: shims.root_string(),
                branch: "agent-dev-brief-7".to_owned(),
            },
            brief_id: "brief-7".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(
        landed.commit_sha,
        "0123456789abcdef0123456789abcdef01234567"
    );
    let log = shims.log("git");
    assert!(log.contains("add -A"));
    assert!(
        log.contains("commit -m agent-dev: brief-7"),
        "the commit message must be `agent-dev: <brief_id>`; got log: {log}"
    );
    assert!(log.contains("rev-parse HEAD"));
    Ok(())
}

#[test]
fn land_with_nothing_to_commit_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "git",
        "if [ \"$1\" = commit ]; then echo 'nothing to commit, working tree clean'; exit 1; fi\nexit 0",
    )?;

    let failure = handlers::land(
        &shims.shell(),
        LandInput {
            workspace: Workspace {
                path: shims.root_string(),
                branch: "agent-dev-brief-7".to_owned(),
            },
            brief_id: "brief-7".to_owned(),
        },
    )
    .err()
    .ok_or("a no-op land must fail the activity")?;
    assert_terminal(&failure, "git commit failed — exit status 1");
    assert_terminal(&failure, "nothing to commit");
    assert!(
        !shims.log("git").contains("rev-parse"),
        "no sha may be read after a failed commit"
    );
    Ok(())
}

#[test]
fn land_with_an_empty_rev_parse_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", "exit 0")?;

    let failure = handlers::land(
        &shims.shell(),
        LandInput {
            workspace: Workspace {
                path: shims.root_string(),
                branch: "agent-dev-brief-7".to_owned(),
            },
            brief_id: "brief-7".to_owned(),
        },
    )
    .err()
    .ok_or("an empty rev-parse must fail the activity")?;
    assert_terminal(&failure, "printed no commit sha");
    Ok(())
}
