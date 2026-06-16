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
use stacked_dev_worker::handlers;
use stacked_dev_worker::shell::Shell;
use stacked_dev_worker::types::{
    Alignment, AssembleInput, Attestation, BriefDocument, BriefRequirement, CheckVerdict, Claim,
    DevInput, EnrichInput, Enrichment, ExecutionBlock, ExecutionStatus, ExecutionVerdict,
    GateBlock, GateInput, GateScope, GateVerdict, Isolation, LandInput, Placement, ProvisionInput,
    RequirementFiles, ResumeInput, ReviewInput, ReviewRequest, ScopedInput, ScoutInput,
    StartupResult, StartupTask, Workspace,
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
        branch: "stacked-dev-brief-7".to_owned(),
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

    let expected_path = format!("{repo_root}/.yggdrasil-worktrees/stacked-dev-brief-7");
    assert_eq!(provisioned.path, expected_path);
    assert_eq!(provisioned.branch, "stacked-dev-brief-7");
    assert!(
        Path::new(&expected_path).is_dir(),
        "provision must create the worktree directory"
    );
    let log = shims.log("yg");
    assert!(log.contains("branch add stacked-dev-brief-7 main"));
    assert!(log.contains(&format!(
        "branch provision stacked-dev-brief-7 --path {expected_path}"
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
        prompt: "Implement the brief...".to_owned(),
    }
}

/// A captured-real dev-report envelope body (the bare dev-report shape the
/// fake-CLI shims emit). R1 implemented, touching one file.
const DEV_REPORT_BODY: &str = r#"{"summary":"implemented the brief","commit_message":"feat: R1","enrichments":[{"id":"R1","status":"implemented","files_changed":[{"path":"crates/aion-core/src/lib.rs","change":"modified","note":"added"}],"how":"added it","deviation":"","checklist":[],"stories":[]}],"attestation":{"no_panics":true,"no_unsafe":true,"boundaries_respected":true,"tests_pass":true}}"#;

#[test]
fn dev_parses_the_canned_dev_report() -> TestResult {
    let shims = Shims::new()?;
    shims.write("norn", &format!("printf '%s' '{DEV_REPORT_BODY}'"))?;

    let result = handlers::startup_task(
        &shims.shell(),
        StartupTask::Dev {
            dev_input: dev_input(shims.root_string()),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match result {
        StartupResult::Developed { dev_report } => {
            assert_eq!(dev_report.summary, "implemented the brief");
            assert_eq!(dev_report.enrichments.len(), 1);
            assert_eq!(dev_report.enrichments[0].id, "R1");
            assert!(dev_report.attestation.tests_pass.0);
        }
        other @ StartupResult::Warmed { .. } => {
            return Err(format!("dev must answer Developed: {other:?}").into());
        }
    }
    let log = shims.log("norn");
    assert!(log.contains("--print --session-id stacked-dev-brief-7"));
    assert!(log.contains(&format!("--workspace-root {}", shims.root_string())));
    assert!(log.contains("--output-format json"));
    assert!(log.contains("Implement the brief..."), "prompt rides last");
    Ok(())
}

#[test]
fn dev_unwraps_real_norns_output_envelope_ignoring_telemetry_fields() -> TestResult {
    let shims = Shims::new()?;
    // A captured-real {"output": <dev report>} completion envelope (P4) with
    // norn's telemetry fields alongside, which the handler ignores.
    shims.write(
        "norn",
        &format!(
            r#"printf '%s' '{{"output":{DEV_REPORT_BODY},"usage":{{"input_tokens":1,"output_tokens":2}},"model":"m","session_id":"x","events":[{{"type":"UserMessage"}}]}}'"#
        ),
    )?;

    let result = handlers::startup_task(
        &shims.shell(),
        StartupTask::Dev {
            dev_input: dev_input(shims.root_string()),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    match result {
        StartupResult::Developed { dev_report } => {
            assert_eq!(dev_report.summary, "implemented the brief");
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
    // A full replacement dev report (BD-003); the resume returns the whole
    // report, never a partial merge.
    shims.write(
        "norn",
        r#"printf '%s' '{"summary":"applied feedback","commit_message":"fix: address diagnostics","enrichments":[{"id":"R1","status":"implemented","files_changed":[{"path":"a.rs","change":"modified","note":"fixed"}],"how":"applied","deviation":"","checklist":[],"stories":[]}],"attestation":{"no_panics":true,"no_unsafe":true,"boundaries_respected":true,"tests_pass":true}}'"#,
    )?;
    // `dev_resume` runs with cwd "." (the workspace root is not on
    // ResumeInput), so the shim must be reachable through the Shell's own
    // search path — which is exactly what Shell::with_path provides.
    let resumed = handlers::dev_resume(
        &shims.shell(),
        ResumeInput {
            session_id: "stacked-dev-brief-7".to_owned(),
            feedback: "error: unused variable count".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(resumed.summary, "applied feedback");
    assert_eq!(resumed.enrichments.len(), 1);
    let log = shims.log("norn");
    assert!(log.contains("--print --resume stacked-dev-brief-7"));
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

/// Collective shim: accepts any send command, exits zero with a JSON result.
const COLLECTIVE_SHIM: &str = r#"printf '%s' '{"results":[{"status":"ok"}]}'"#;

#[test]
fn request_review_sends_collective_dm_to_each_reviewer() -> TestResult {
    let shims = Shims::new()?;
    shims.write("collective", COLLECTIVE_SHIM)?;
    let workspace = workspace(shims.root_string());

    let acked = handlers::request_review(
        &shims.shell(),
        &ReviewRequest {
            workspace: workspace.clone(),
            brief_id: "brief-7".to_owned(),
            reviewers: vec!["sample-reviewer".to_owned()],
            dev_result: stacked_dev_worker::types::DevResult {
                session_id: "stacked-dev-brief-7".to_owned(),
                files_touched: Vec::new(),
                summary: "implemented the brief".to_owned(),
            },
            gate_result: stacked_dev_worker::types::GateResult {
                verdict: GateVerdict::Pass,
            },
            workflow_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(acked.request_id, workspace.branch);
    let log = shims.log("collective");
    assert!(log.contains("send"), "expected collective send, got: {log}");
    assert!(
        log.contains("sample-reviewer"),
        "expected reviewer name in args, got: {log}"
    );
    assert!(
        log.contains("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        "expected workflow_id in signal command, got: {log}"
    );
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
            dev_result: stacked_dev_worker::types::DevResult {
                session_id: "stacked-dev-brief-7".to_owned(),
                files_touched: Vec::new(),
                summary: "implemented the brief".to_owned(),
            },
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(landed.branch, "stacked-dev-brief-7");
    assert_eq!(landed.merged_into, "main");
    let git_log = shims.log("git");
    assert!(git_log.contains("add -A"));
    assert!(git_log.contains("commit -m stacked-dev-brief-7: implemented the brief"));
    let log = shims.log("yg");
    assert!(log.contains("branch merge stacked-dev-brief-7 --yes"));
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
            dev_result: stacked_dev_worker::types::DevResult {
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
            dev_result: stacked_dev_worker::types::DevResult {
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

// --- scout / dev_review (the new norn stages) -------------------------------

/// A captured-real scout-report completion envelope (P4): the bare report sits
/// under "output" alongside norn's telemetry, which the handler ignores.
const SCOUT_ENVELOPE: &str = r#"printf '%s' '{"output":{"summary":"scouted","enrichments":[{"id":"R1","files":["src/a.gleam"],"context":["match conventions"],"approach":"add it","notes":"watch the codec ordering"}],"verification":["gleam test"]},"usage":{"input_tokens":1,"output_tokens":2},"model":"m","session_id":"x"}'"#;

/// A captured-real review-report completion envelope (P4) carrying a
/// distinguishable note string.
const REVIEW_ENVELOPE: &str = r#"printf '%s' '{"output":{"summary":"the diff holds up","commit_message":"","enrichments":[{"id":"R1","alignment":"aligned","acceptance":[{"criterion":"it exists","met":true,"evidence":"src/a.gleam:1"}],"checklist":[],"stories":[],"issues":[],"fixes":[]}],"verification":[]},"usage":{"input_tokens":1,"output_tokens":2},"model":"m"}'"#;

#[test]
fn scout_runs_the_scout_session_and_parses_the_captured_envelope() -> TestResult {
    let shims = Shims::new()?;
    shims.write("norn", SCOUT_ENVELOPE)?;

    let report = handlers::scout(
        &shims.shell(),
        ScoutInput {
            workspace: workspace(shims.root_string()),
            prompt: "Scout this brief...".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    assert_eq!(report.summary, "scouted");
    assert_eq!(report.enrichments.len(), 1);
    assert_eq!(report.enrichments[0].notes, "watch the codec ordering");
    let log = shims.log("norn");
    assert!(
        log.contains("--session-id stacked-dev-brief-7-scout"),
        "the scout session id must end -scout: {log}"
    );
    assert!(log.contains("--output-format json"));
    Ok(())
}

#[test]
fn dev_review_runs_the_review_session_and_surfaces_the_note() -> TestResult {
    let shims = Shims::new()?;
    shims.write("norn", REVIEW_ENVELOPE)?;

    let report = handlers::dev_review(
        &shims.shell(),
        ReviewInput {
            workspace: workspace(shims.root_string()),
            prompt: "Review this brief...".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    // A distinguishable note string from the shim's review report arrives
    // verbatim, and the session id ends -review (never the dev session, CN4).
    assert_eq!(report.summary, "the diff holds up");
    assert_eq!(report.enrichments[0].alignment, Alignment::Aligned);
    let log = shims.log("norn");
    assert!(
        log.contains("--session-id stacked-dev-brief-7-review"),
        "the review session id must end -review: {log}"
    );
    assert!(
        !log.contains("--session-id stacked-dev-brief-7 "),
        "the review must never use the bare dev session id"
    );
    Ok(())
}

#[test]
fn stage_parser_accepts_bare_then_envelope_then_fails_on_a_third_shape() -> TestResult {
    // Bare report shape.
    let bare = Shims::new()?;
    bare.write(
        "norn",
        r#"printf '%s' '{"summary":"bare","enrichments":[],"verification":[]}'"#,
    )?;
    let report = handlers::scout(
        &bare.shell(),
        ScoutInput {
            workspace: workspace(bare.root_string()),
            prompt: "p".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    assert_eq!(report.summary, "bare");

    // Envelope shape.
    let enveloped = Shims::new()?;
    enveloped.write(
        "norn",
        r#"printf '%s' '{"output":{"summary":"enveloped","enrichments":[],"verification":[]}}'"#,
    )?;
    let report = handlers::scout(
        &enveloped.shell(),
        ScoutInput {
            workspace: workspace(enveloped.root_string()),
            prompt: "p".to_owned(),
        },
    )
    .map_err(|failure| failure.message().to_owned())?;
    assert_eq!(report.summary, "enveloped");

    // A third shape fails terminally naming both attempted shapes.
    let third = Shims::new()?;
    third.write("norn", r#"printf '%s' '{"wrapper":{"summary":"nope"}}'"#)?;
    let failure = handlers::scout(
        &third.shell(),
        ScoutInput {
            workspace: workspace(third.root_string()),
            prompt: "p".to_owned(),
        },
    )
    .err()
    .ok_or("a third shape must fail the activity")?;
    assert_terminal(&failure, "produced unparseable output");
    assert_terminal(&failure, "bare report shape");
    assert_terminal(&failure, "{\"output\": …}");
    Ok(())
}

// --- enrich_brief (file IO, no CLI shim) ------------------------------------

fn enrich_document() -> BriefDocument {
    BriefDocument {
        id: "BD-900".to_owned(),
        cluster: "brief-dev".to_owned(),
        title: "Enrichment test brief".to_owned(),
        depends_on: Vec::new(),
        blocked_by: Vec::new(),
        checklist: vec!["C19".to_owned()],
        stories: vec!["S12".to_owned()],
        design_anchor: vec!["ADR-007".to_owned()],
        purpose: "Exercise the append-only merge.".to_owned(),
        task: "Merge and inspect.".to_owned(),
        requirements: vec![BriefRequirement {
            id: "R1".to_owned(),
            title: "Requirement R1".to_owned(),
            spec: "Spec for R1.".to_owned(),
            acceptance: vec!["Acceptance for R1.".to_owned()],
            files: RequirementFiles {
                create: Vec::new(),
                modify: Vec::new(),
                delete: Vec::new(),
            },
            checklist: vec!["C19".to_owned()],
            stories: vec!["S12".to_owned()],
            scout: None,
            dev: None,
            review: None,
        }],
        boundaries: vec!["No authored field changes.".to_owned()],
        verification: vec!["cargo test".to_owned()],
        execution: None,
    }
}

fn execution_block() -> ExecutionBlock {
    ExecutionBlock {
        status: ExecutionStatus::Landed,
        workflow_id: "wf-1".to_owned(),
        branch: "stacked-dev-BD-900".to_owned(),
        session_id: "stacked-dev-BD-900".to_owned(),
        gate: GateBlock {
            fmt: true,
            clippy: true,
            tests: true,
            fix_rounds: 0,
        },
        attestation: Attestation {
            no_panics: Claim(true),
            no_unsafe: Claim(true),
            boundaries_respected: Claim(true),
            tests_pass: Claim(true),
        },
        review_verdict: ExecutionVerdict::Approved,
        landed_commit: String::new(),
        merged_into: "main".to_owned(),
        completed_at: "123".to_owned(),
    }
}

/// Seed the brief at its design-system path under `root` and return that path.
fn seed_brief(root: &Path, document: &BriefDocument) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let path = root
        .join("docs")
        .join("design")
        .join(&document.cluster)
        .join("briefs")
        .join(format!("{}.json", document.id));
    std::fs::create_dir_all(path.parent().ok_or("brief path has no parent")?)?;
    std::fs::write(&path, serde_json::to_string(document)?)?;
    Ok(path)
}

#[test]
fn enrich_brief_merges_and_writes_the_worktree_brief_in_place() -> TestResult {
    let shims = Shims::new()?;
    let document = enrich_document();
    let path = seed_brief(shims.root(), &document)?;

    let returned = handlers::enrich_brief(
        &shims.shell(),
        EnrichInput {
            workspace: Workspace {
                path: shims.root_string(),
                branch: "stacked-dev-BD-900".to_owned(),
                placement: Placement::Local,
                isolation: Isolation::Worktree,
            },
            document: document.clone(),
            enrichment: Enrichment::Execution {
                block: execution_block(),
            },
        },
    )
    .map_err(|failure| failure.message().to_owned())?;

    // The file decodes as the merged document afterwards, with the execution
    // block's gate and attestation as two distinct objects (P1).
    let raw = std::fs::read_to_string(&path)?;
    assert!(raw.contains("\"gate\""));
    assert!(raw.contains("\"attestation\""));
    let on_disk: BriefDocument = serde_json::from_str(&raw)?;
    assert_eq!(on_disk, returned);
    let execution = on_disk.execution.ok_or("execution must be present")?;
    assert_eq!(execution, execution_block());
    Ok(())
}

#[test]
fn enrich_brief_refuses_an_authored_divergence_and_leaves_the_file_unchanged() -> TestResult {
    let shims = Shims::new()?;
    let document = enrich_document();
    // The on-disk brief carries a different authored task field.
    let mut on_disk_doc = document.clone();
    on_disk_doc.task = "A different task.".to_owned();
    let path = seed_brief(shims.root(), &on_disk_doc)?;
    let before = std::fs::read(&path)?;

    let failure = handlers::enrich_brief(
        &shims.shell(),
        EnrichInput {
            workspace: Workspace {
                path: shims.root_string(),
                branch: "stacked-dev-BD-900".to_owned(),
                placement: Placement::Local,
                isolation: Isolation::Worktree,
            },
            document,
            enrichment: Enrichment::Execution {
                block: execution_block(),
            },
        },
    )
    .err()
    .ok_or("an authored divergence must fail the activity")?;

    assert_terminal(&failure, "task");
    assert_terminal(&failure, "refusing to write");
    // The file bytes are identical before and after the refused call.
    let after = std::fs::read(&path)?;
    assert_eq!(before, after, "the file must be byte-unchanged");
    Ok(())
}

#[test]
fn enrich_brief_with_an_absent_file_is_terminal() -> TestResult {
    let shims = Shims::new()?;
    // No brief seeded: the worktree path does not exist.
    let failure = handlers::enrich_brief(
        &shims.shell(),
        EnrichInput {
            workspace: Workspace {
                path: shims.root_string(),
                branch: "stacked-dev-BD-900".to_owned(),
                placement: Placement::Local,
                isolation: Isolation::Worktree,
            },
            document: enrich_document(),
            enrichment: Enrichment::Execution {
                block: execution_block(),
            },
        },
    )
    .err()
    .ok_or("a missing brief file must fail the activity")?;
    assert_terminal(&failure, "cannot read");
    Ok(())
}

// --- assemble_wave (ledger reads + resolution, no CLI shim) ------------------
//
// These tests write a complete fixture design_dir tree (the two ledgers plus a
// synthetic `demo` cluster's documents and briefs) and drive the handler
// directly. They cover the four refusal classes and the ordering/landed
// acceptance from BD-006 R4; the handler mirrors `assemble.run`
// decision-for-decision, so the same fixtures exercise the deployed path.

/// One test's fixture design directory.
struct Design {
    dir: tempfile::TempDir,
}

impl Design {
    fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            dir: tempfile::tempdir()?,
        })
    }

    fn path(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn write_json(&self, relative: &str, value: &serde_json::Value) -> TestResult {
        let path = self.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().ok_or("path has no parent")?)?;
        std::fs::write(&path, serde_json::to_string(value)?)?;
        Ok(())
    }

    /// Seed the two project ledgers and the `demo` cluster documents the
    /// resolver reads for every brief.
    fn seed_standard(&self) -> TestResult {
        self.write_json(
            "roadmap.json",
            &serde_json::json!({
                "items": [{
                    "id": "RM-1",
                    "links": { "cluster": "demo" },
                    "provenance": { "requested_by": "Tom", "quote": "do it" }
                }]
            }),
        )?;
        self.write_json("decisions.json", &serde_json::json!({ "decisions": [] }))?;
        self.write_json(
            "demo/checklist.json",
            &serde_json::json!({
                "cluster": "demo",
                "sections": [{ "name": "S", "items": [
                    { "id": "C1", "text": "ct", "done": true },
                    { "id": "C3", "text": "c3", "done": true }
                ] }]
            }),
        )?;
        self.write_json(
            "demo/stories.json",
            &serde_json::json!({
                "cluster": "demo",
                "personas": [{ "name": "P", "stories": [{ "id": "S1", "text": "st" }] }]
            }),
        )?;
        self.write_json(
            "demo/design.json",
            &serde_json::json!({
                "cluster": "demo",
                "intention": "i",
                "constraints": [{ "id": "CN1", "text": "nt" }],
                "structure": []
            }),
        )
    }

    fn write_brief(&self, id: &str, value: &serde_json::Value) -> TestResult {
        self.write_json(&format!("demo/briefs/{id}.json"), value)
    }
}

/// A schema-valid v2 brief document (P4): empty checklist/stories and a single
/// requirement unless overridden by `header_checklist`/`req_checklist`.
fn brief_value(
    id: &str,
    depends_on: &[&str],
    header_checklist: &[&str],
    req_checklist: &[&str],
    execution_landed: bool,
) -> serde_json::Value {
    let mut document = serde_json::json!({
        "id": id,
        "cluster": "demo",
        "title": "t",
        "depends_on": depends_on,
        "blocked_by": [],
        "checklist": header_checklist,
        "stories": [],
        "design_anchor": [],
        "purpose": "p",
        "task": "k",
        "requirements": [{
            "id": "R1",
            "title": "rt",
            "spec": "rs",
            "acceptance": ["a"],
            "files": { "create": [], "modify": [], "delete": [] },
            "checklist": req_checklist,
            "stories": []
        }],
        "boundaries": [],
        "verification": []
    });
    if execution_landed {
        document["execution"] = serde_json::json!({
            "status": "landed",
            "workflow_id": "wf",
            "branch": "b",
            "session_id": "s",
            "gate": { "fmt": true, "clippy": true, "tests": true, "fix_rounds": 0 },
            "attestation": {
                "no_panics": true, "no_unsafe": true,
                "boundaries_respected": true, "tests_pass": true
            },
            "review_verdict": "approved",
            "landed_commit": "",
            "merged_into": "main",
            "completed_at": "1"
        });
    }
    document
}

fn assemble(
    design: &Design,
    wave: &[&str],
) -> Result<stacked_dev_worker::types::AssembledWave, ActivityFailure> {
    handlers::assemble_wave(
        &Shell::inherited(),
        AssembleInput {
            design_dir: design.path(),
            wave: wave.iter().map(|id| (*id).to_owned()).collect(),
        },
    )
}

#[test]
fn assemble_orders_the_wave_by_depends_on() -> TestResult {
    let design = Design::new()?;
    design.seed_standard()?;
    design.write_brief("W-001", &brief_value("W-001", &[], &[], &[], false))?;
    design.write_brief("W-002", &brief_value("W-002", &["W-001"], &[], &[], false))?;

    // Wave given out of order; the dependency must come first.
    let wave = assemble(&design, &["W-002", "W-001"]).map_err(|f| f.message().to_owned())?;
    let ids: Vec<String> = wave
        .entries
        .iter()
        .map(|entry| entry.brief_document.id.clone())
        .collect();
    assert_eq!(ids, ["W-001", "W-002"]);
    // The resolved provenance carries the roadmap requester verbatim.
    assert_eq!(
        wave.entries[0].resolved_context.provenance.requested_by,
        "Tom"
    );
    assert_eq!(wave.entries[0].resolved_context.provenance.quote, "do it");
    Ok(())
}

#[test]
fn assemble_accepts_a_landed_out_of_wave_dependency() -> TestResult {
    let design = Design::new()?;
    design.seed_standard()?;
    // W-001 is on disk and landed (execution.status = landed) but not in the
    // wave; W-002 depends on it.
    design.write_brief("W-001", &brief_value("W-001", &[], &[], &[], true))?;
    design.write_brief("W-002", &brief_value("W-002", &["W-001"], &[], &[], false))?;

    let wave = assemble(&design, &["W-002"]).map_err(|f| f.message().to_owned())?;
    let ids: Vec<String> = wave
        .entries
        .iter()
        .map(|entry| entry.brief_document.id.clone())
        .collect();
    assert_eq!(ids, ["W-002"]);
    Ok(())
}

#[test]
fn assemble_refuses_an_unlanded_out_of_wave_dependency() -> TestResult {
    let design = Design::new()?;
    design.seed_standard()?;
    // W-001 is on disk WITHOUT an execution block; W-002 depends on it.
    design.write_brief("W-001", &brief_value("W-001", &[], &[], &[], false))?;
    design.write_brief("W-002", &brief_value("W-002", &["W-001"], &[], &[], false))?;

    let failure = assemble(&design, &["W-002"])
        .err()
        .ok_or("an unlanded dependency must refuse the wave")?;
    assert_terminal(&failure, "W-002");
    assert_terminal(&failure, "W-001");
    assert_terminal(&failure, "is not landed on disk");
    Ok(())
}

#[test]
fn assemble_refuses_a_coverage_violation() -> TestResult {
    let design = Design::new()?;
    design.seed_standard()?;
    // The header checklist lists C3, which no R# cites — a coverage break.
    design.write_brief("W-010", &brief_value("W-010", &[], &["C3"], &[], false))?;

    let failure = assemble(&design, &["W-010"])
        .err()
        .ok_or("a coverage violation must refuse the wave")?;
    assert_terminal(&failure, "W-010");
    assert_terminal(&failure, "C3");
    Ok(())
}

#[test]
fn assemble_refuses_a_dependency_cycle() -> TestResult {
    let design = Design::new()?;
    design.seed_standard()?;
    design.write_brief("W-020", &brief_value("W-020", &["W-021"], &[], &[], false))?;
    design.write_brief("W-021", &brief_value("W-021", &["W-020"], &[], &[], false))?;

    let failure = assemble(&design, &["W-020", "W-021"])
        .err()
        .ok_or("a cycle must refuse the wave")?;
    assert_terminal(&failure, "dependency cycle");
    assert_terminal(&failure, "W-020");
    assert_terminal(&failure, "W-021");
    Ok(())
}
