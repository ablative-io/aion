//! Hermetic handler tests with fake-CLI shims, mirroring the Gleam suite's
//! approach (`../../test/support/shims.gleam`): each test builds its own
//! directory of stub scripts (`yg`, `norn`, `cargo`, `meridian`) that emit
//! canned output and append their argv to per-executable log files, then
//! constructs a `Shell` whose search path is that directory ALONE. The
//! handlers stay honest — they really shell out — and the shims intercept at
//! the process boundary. A CLI the test did not stub is genuinely absent,
//! which proves a missing CLI is a loud terminal failure, never a silent
//! skip. Unlike the Gleam suite this never mutates the global `PATH` (the
//! `Shell` carries the search path), so the tests are parallel-safe.

#![cfg(unix)]

use std::error::Error;
use std::path::Path;

use aion_worker::{ActivityFailure, Classification};

// The scaffolded crate's own library, in its own import group so the
// statement order holds for any project name.
use {{name}}_worker::handlers;
use {{name}}_worker::shell::Shell;
use {{name}}_worker::types::{
    CheckVerdict, DevInput, GateInput, GateScope, GateVerdict, Isolation, LandInput, Placement,
    ProvisionInput, ResumeInput, ReviewRequest, ScopedInput, StartupResult, StartupTask, Workspace,
};

type TestResult = Result<(), Box<dyn Error>>;

/// One test's shim directory. `root` doubles as the repo root / workspace
/// path, exactly like the Gleam suite.
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

    /// A `Shell` resolving executables against the shim directory ALONE.
    fn shell(&self) -> Shell {
        Shell::with_path(self.root())
    }

    /// Write one shim: a `/bin/sh` script that records its argv to
    /// `<root>/<name>.log` and then runs `body`. Same skeleton as the Gleam
    /// suite's `write_shim`.
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

fn workspace(path: String) -> Workspace {
    Workspace {
        path,
        branch: "{{name}}-brief-7".to_owned(),
        placement: Placement::Local,
        isolation: Isolation::Worktree,
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

/// The `yg` shim shared across scenarios: real branch add, a provision that
/// creates the worktree directory at the `--path` it is handed, an
/// affected-modules query printing `affected`, and a per-scenario
/// `diagnostics check` body. Mirrors the Gleam suite's `yg_script`.
fn yg_script(affected: &str, diagnostics_body: &str) -> String {
    format!(
        r#"case "$1" in
  branch)
    case "$2" in
      add) exit 0 ;;
      provision) mkdir -p "$5"; exit 0 ;;
      *) echo "unknown yg branch: $2" >&2; exit 64 ;;
    esac
    ;;
  graph)
    printf '%s' '{affected}'
    exit 0
    ;;
  diagnostics)
{diagnostics_body}
    ;;
  *)
    echo "unknown yg subcommand: $1" >&2; exit 64
    ;;
esac"#
    )
}

// --- provision_workspace -----------------------------------------------------

#[test]
fn provision_creates_the_worktree_directory() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", &yg_script("", "    exit 0"))?;
    let repo_root = shims.root_string();

    let provisioned = handlers::provision_workspace(
        &shims.shell(),
        ProvisionInput {
            repo_root: repo_root.clone(),
            brief_id: "brief-7".to_owned(),
            base_ref: "main".to_owned(),
            placement: Placement::Local,
            isolation: Isolation::Worktree,
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    let expected_path = format!("{repo_root}/.yggdrasil-worktrees/{{name}}-brief-7");
    assert_eq!(provisioned.path, expected_path);
    assert_eq!(provisioned.branch, "{{name}}-brief-7");
    assert!(
        Path::new(&expected_path).is_dir(),
        "provision must create the worktree directory"
    );
    let log = shims.log("yg");
    assert!(log.contains("branch add {{name}}-brief-7 main"));
    assert!(log.contains(&format!(
        "branch provision {{name}}-brief-7 --path {expected_path}"
    )));
    Ok(())
}

#[test]
fn provision_rejects_unimplemented_isolation_modes() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", &yg_script("", "    exit 0"))?;

    let failure = handlers::provision_workspace(
        &shims.shell(),
        ProvisionInput {
            repo_root: shims.root_string(),
            brief_id: "brief-7".to_owned(),
            base_ref: "main".to_owned(),
            placement: Placement::Remote,
            isolation: Isolation::Vm,
        },
    )
    .err()
    .ok_or("vm isolation must fail")?;
    assert_terminal(&failure, "isolation mode vm is a typed seam");
    assert!(shims.log("yg").is_empty(), "no yg call may run");
    Ok(())
}

#[test]
fn provision_failing_yg_is_terminal_with_diagnostics() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", "echo 'branch already exists' >&2\nexit 3")?;

    let failure = handlers::provision_workspace(
        &shims.shell(),
        ProvisionInput {
            repo_root: shims.root_string(),
            brief_id: "brief-7".to_owned(),
            base_ref: "main".to_owned(),
            placement: Placement::Local,
            isolation: Isolation::Worktree,
        },
    )
    .err()
    .ok_or("failing yg must fail the activity")?;
    assert_terminal(&failure, "yg branch add failed — exit status 3");
    assert_terminal(&failure, "branch already exists");
    Ok(())
}

// --- warm_build / dev (the StartupTask envelope) ------------------------------

#[test]
fn warm_build_success_reports_ok_true() -> TestResult {
    let shims = Shims::new()?;
    shims.write("cargo", "exit 0")?;

    let result = handlers::startup_task(
        &shims.shell(),
        StartupTask::WarmBuild {
            workspace: workspace(shims.root_string()),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match result {
        StartupResult::Warmed { build_warm } => assert!(build_warm.ok),
        other @ StartupResult::Developed { .. } => {
            return Err(format!("warm_build must answer Warmed: {other:?}").into());
        }
    }
    assert!(shims.log("cargo").contains("build"));
    Ok(())
}

#[test]
fn warm_build_failure_is_recorded_as_ok_false_never_an_error() -> TestResult {
    let shims = Shims::new()?;
    shims.write("cargo", "echo 'error: warm build exploded'\nexit 1")?;

    let result = handlers::startup_task(
        &shims.shell(),
        StartupTask::WarmBuild {
            workspace: workspace(shims.root_string()),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match result {
        StartupResult::Warmed { build_warm } => {
            assert!(!build_warm.ok, "a failed build forfeits the cache");
        }
        other @ StartupResult::Developed { .. } => {
            return Err(format!("warm_build must answer Warmed: {other:?}").into());
        }
    }
    Ok(())
}

#[test]
fn missing_cli_is_a_loud_terminal_failure() -> TestResult {
    // No cargo shim is written, and the shim directory is the entire search
    // path, so cargo is genuinely absent.
    let shims = Shims::new()?;

    let failure = handlers::startup_task(
        &shims.shell(),
        StartupTask::WarmBuild {
            workspace: workspace(shims.root_string()),
        },
    )
    .err()
    .ok_or("a missing CLI must fail the activity")?;
    assert_terminal(&failure, "cargo build: executable not found on PATH: cargo");
    Ok(())
}

fn dev_input(workspace_path: String) -> DevInput {
    DevInput {
        workspace: workspace(workspace_path),
        brief: "Implement the widget".to_owned(),
        design: "docs/design.md".to_owned(),
        checklist: "docs/checklist.md".to_owned(),
        stories: vec!["story-1".to_owned()],
    }
}

#[test]
fn dev_parses_the_canned_dev_result_and_overrides_the_session_id() -> TestResult {
    let shims = Shims::new()?;
    // The shim reports a DIFFERENT session id; the handler must override it
    // with the deterministic branch-derived id it passed via --session-id.
    shims.write(
        "norn",
        r#"printf '%s' '{"session_id":"whatever-norn-minted","files_touched":["crates/aion-core/src/lib.rs"],"summary":"implemented the brief"}'"#,
    )?;

    let result = handlers::startup_task(
        &shims.shell(),
        StartupTask::Dev {
            dev_input: dev_input(shims.root_string()),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match result {
        StartupResult::Developed { dev_result } => {
            assert_eq!(dev_result.session_id, "{{name}}-brief-7");
            assert_eq!(dev_result.files_touched, ["crates/aion-core/src/lib.rs"]);
            assert_eq!(dev_result.summary, "implemented the brief");
        }
        other @ StartupResult::Warmed { .. } => {
            return Err(format!("dev must answer Developed: {other:?}").into());
        }
    }
    let log = shims.log("norn");
    assert!(log.contains("--print --session-id {{name}}-brief-7"));
    assert!(log.contains(&format!("--workspace-root {}", shims.root_string())));
    assert!(log.contains("--output-format json"));
    assert!(log.contains("Implement the widget"), "prompt rides last");
    Ok(())
}

#[test]
fn dev_unwraps_real_norns_output_envelope_ignoring_telemetry_fields() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "norn",
        r#"printf '%s' '{"output":{"session_id":"x","files_touched":["a.rs"],"summary":"enveloped"},"usage":{"input_tokens":1,"output_tokens":2},"model":"m","session_id":"x","events":[{"type":"UserMessage"}]}'"#,
    )?;

    let result = handlers::startup_task(
        &shims.shell(),
        StartupTask::Dev {
            dev_input: dev_input(shims.root_string()),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match result {
        StartupResult::Developed { dev_result } => {
            assert_eq!(dev_result.session_id, "{{name}}-brief-7");
            assert_eq!(dev_result.summary, "enveloped");
        }
        other @ StartupResult::Warmed { .. } => {
            return Err(format!("dev must answer Developed: {other:?}").into());
        }
    }
    Ok(())
}

#[test]
fn dev_output_matching_neither_shape_is_terminal_with_the_output_head() -> TestResult {
    let shims = Shims::new()?;
    shims.write("norn", "printf '%s' 'norn exploded mid-flight'")?;

    let failure = handlers::startup_task(
        &shims.shell(),
        StartupTask::Dev {
            dev_input: dev_input(shims.root_string()),
        },
    )
    .err()
    .ok_or("unparseable norn output must fail the activity")?;
    assert_terminal(&failure, "norn dev produced unparseable output");
    assert_terminal(&failure, "norn exploded mid-flight");
    Ok(())
}

// --- dev_resume ---------------------------------------------------------------

#[test]
fn dev_resume_resumes_the_session_and_carries_the_feedback() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "norn",
        r#"printf '%s' '{"session_id":"x","files_touched":["a.rs","b.rs"],"summary":"applied feedback"}'"#,
    )?;
    // `dev_resume` runs with cwd "." (the workspace root is not on
    // ResumeInput), so the shim must be reachable through the Shell's own
    // search path — which is exactly what Shell::with_path provides.
    let resumed = handlers::dev_resume(
        &shims.shell(),
        ResumeInput {
            session_id: "{{name}}-brief-7".to_owned(),
            feedback: "error: unused variable count".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(resumed.session_id, "{{name}}-brief-7");
    assert_eq!(resumed.summary, "applied feedback");
    let log = shims.log("norn");
    assert!(log.contains("--print --resume {{name}}-brief-7"));
    assert!(
        log.contains("error: unused variable count"),
        "the diagnostics must reach norn's argv"
    );
    Ok(())
}

// --- scoped_checks ------------------------------------------------------------

#[test]
fn scoped_checks_run_per_affected_package() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", &yg_script("aion-core\n", "    exit 0"))?;

    let checked = handlers::scoped_checks(
        &shims.shell(),
        ScopedInput {
            workspace: workspace(shims.root_string()),
            files_touched: vec!["crates/aion-core/src/lib.rs".to_owned()],
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(checked.verdict, CheckVerdict::Pass);
    assert_eq!(checked.affected_modules, ["aion-core"]);
    assert_eq!(checked.checked_scope, "affected: aion-core");
    let log = shims.log("yg");
    assert!(log.contains("graph affected --plain --direct-only crates/aion-core/src/lib.rs"));
    assert!(log.contains("diagnostics check --format json --package aion-core"));
    Ok(())
}

#[test]
fn scoped_empty_affected_set_falls_back_loudly_to_workspace_wide() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", &yg_script("", "    exit 0"))?;

    let checked = handlers::scoped_checks(
        &shims.shell(),
        ScopedInput {
            workspace: workspace(shims.root_string()),
            files_touched: vec!["README.md".to_owned()],
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(checked.verdict, CheckVerdict::Pass);
    assert!(checked.affected_modules.is_empty());
    assert_eq!(
        checked.checked_scope,
        "workspace-wide fallback: affected scoping returned an empty set"
    );
    assert!(
        shims
            .log("yg")
            .contains("diagnostics check --workspace --format json"),
        "the fallback must really run the workspace sweep"
    );
    Ok(())
}

#[test]
fn scoped_check_failure_is_recorded_diagnostics_not_an_error() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "yg",
        &yg_script(
            "aion-core\n",
            "    echo 'error: unused variable count'\n    exit 1",
        ),
    )?;

    let checked = handlers::scoped_checks(
        &shims.shell(),
        ScopedInput {
            workspace: workspace(shims.root_string()),
            files_touched: vec!["crates/aion-core/src/lib.rs".to_owned()],
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match checked.verdict {
        CheckVerdict::Fail { diagnostics } => {
            assert!(diagnostics.contains("error: unused variable count"));
        }
        CheckVerdict::Pass => return Err("a failing check must carry diagnostics".into()),
    }
    Ok(())
}

// --- full_checks ----------------------------------------------------------------

#[test]
fn full_checks_pass_and_fail_are_recorded_verdicts() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", &yg_script("", "    exit 0"))?;
    let passed = handlers::full_checks(
        &shims.shell(),
        GateInput {
            workspace: workspace(shims.root_string()),
            files_touched: Vec::new(),
            scope: GateScope::WorkspaceWide,
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    assert_eq!(passed.verdict, GateVerdict::Pass);
    assert!(
        shims
            .log("yg")
            .contains("diagnostics check --workspace --format json")
    );

    let failing = Shims::new()?;
    failing.write(
        "yg",
        &yg_script("", "    echo 'error: cross-crate failure'\n    exit 1"),
    )?;
    let failed = handlers::full_checks(
        &failing.shell(),
        GateInput {
            workspace: workspace(failing.root_string()),
            files_touched: Vec::new(),
            scope: GateScope::WorkspaceWide,
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match failed.verdict {
        GateVerdict::Fail { report } => assert!(report.contains("error: cross-crate failure")),
        GateVerdict::Pass => return Err("the failing sweep must carry its report".into()),
    }
    Ok(())
}

#[test]
fn full_checks_affected_closure_scope_is_a_terminal_seam() -> TestResult {
    let shims = Shims::new()?;
    shims.write("yg", &yg_script("", "    exit 0"))?;

    let failure = handlers::full_checks(
        &shims.shell(),
        GateInput {
            workspace: workspace(shims.root_string()),
            files_touched: Vec::new(),
            scope: GateScope::AffectedClosure {
                modules: vec!["aion-core".to_owned()],
            },
        },
    )
    .err()
    .ok_or("the affected-closure seam must fail loudly")?;
    assert_terminal(
        &failure,
        "affected-closure gate scope has no local implementation",
    );
    assert!(shims.log("yg").is_empty(), "no check may run");
    Ok(())
}

// --- request_review / land -------------------------------------------------------

/// The meridian shim from the Gleam suite: review request acks only —
/// landing is `yg branch merge` now.
const MERIDIAN_SHIM: &str = r#"case "$1" in
  review)
    printf '%s' '{"branch":"{{name}}-brief-7","reviewers":[{"name":"sample-reviewer","dm_status":"sent"}],"pending_reviewers_persisted":true}'
    ;;
  *)
    echo "unknown meridian subcommand: $1" >&2
    exit 64
    ;;
esac"#;

#[test]
fn request_review_parses_the_request_id() -> TestResult {
    let shims = Shims::new()?;
    shims.write("meridian", MERIDIAN_SHIM)?;
    let workspace = workspace(shims.root_string());

    let acked = handlers::request_review(
        &shims.shell(),
        ReviewRequest {
            workspace: workspace.clone(),
            brief_id: "brief-7".to_owned(),
            reviewers: vec!["sample-reviewer".to_owned()],
            dev_result: {{name}}_worker::types::DevResult {
                session_id: "{{name}}-brief-7".to_owned(),
                files_touched: Vec::new(),
                summary: "implemented the brief".to_owned(),
            },
            gate_result: {{name}}_worker::types::GateResult {
                verdict: GateVerdict::Pass,
            },
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(acked.request_id, "{{name}}-brief-7");
    let log = shims.log("meridian");
    assert!(log.contains(&format!(
        "review request {} --reviewer sample-reviewer --as Meridian",
        workspace.branch
    )));
    Ok(())
}

#[test]
fn land_commits_then_merges_the_branch_into_its_parent_via_yg() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", "exit 0")?;
    shims.write("yg", "exit 0")?;

    let landed = handlers::land(
        &shims.shell(),
        LandInput {
            workspace: workspace(shims.root_string()),
            repo_root: shims.root_string(),
            base_ref: "main".to_owned(),
            dev_result: {{name}}_worker::types::DevResult {
                session_id: "{{name}}-brief-7".to_owned(),
                files_touched: Vec::new(),
                summary: "implemented the brief".to_owned(),
            },
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(landed.branch, "{{name}}-brief-7");
    assert_eq!(landed.merged_into, "main");
    let git_log = shims.log("git");
    assert!(git_log.contains("add -A"));
    assert!(git_log.contains("commit -m {{name}}-brief-7: implemented the brief"));
    let log = shims.log("yg");
    assert!(log.contains("branch merge {{name}}-brief-7 --yes"));
    Ok(())
}

#[test]
fn land_with_nothing_to_commit_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    shims.write(
        "git",
        "if [ \"$1\" = commit ]; then echo 'nothing to commit, working tree clean'; exit 1; fi\nexit 0",
    )?;
    shims.write("yg", "exit 0")?;

    let failure = handlers::land(
        &shims.shell(),
        LandInput {
            workspace: workspace(shims.root_string()),
            repo_root: shims.root_string(),
            base_ref: "main".to_owned(),
            dev_result: {{name}}_worker::types::DevResult {
                session_id: "s".to_owned(),
                files_touched: Vec::new(),
                summary: String::new(),
            },
        },
    )
    .err()
    .ok_or("a no-op land must fail the activity")?;
    assert_terminal(&failure, "git commit failed — exit status 1");
    assert_terminal(&failure, "nothing to commit");
    assert!(
        !shims.log("yg").contains("branch merge"),
        "the merge must not run after a failed commit"
    );
    Ok(())
}

#[test]
fn land_with_failing_merge_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    shims.write("git", "exit 0")?;
    shims.write("yg", "echo 'merge conflict in crates/x' >&2; exit 1")?;

    let failure = handlers::land(
        &shims.shell(),
        LandInput {
            workspace: workspace(shims.root_string()),
            repo_root: shims.root_string(),
            base_ref: "main".to_owned(),
            dev_result: {{name}}_worker::types::DevResult {
                session_id: "s".to_owned(),
                files_touched: Vec::new(),
                summary: String::new(),
            },
        },
    )
    .err()
    .ok_or("a failing merge must fail the activity")?;
    assert_terminal(
        &failure,
        "yg branch merge failed — exit status 1: merge conflict in crates/x",
    );
    Ok(())
}
