//// Fake-CLI shim harness for the hermetic test suite.
////
//// Each test builds its own shim directory of stub scripts (`yg`, `norn`,
//// `cargo`, `meridian`) that emit canned output and append their argv to
//// per-executable log files, then points `PATH` at that directory ALONE.
//// The activity local implementations stay honest — they really shell out —
//// and the shims are the test double at the process boundary: the most
//// realistic seam. Because `PATH` contains only the shim directory, a CLI
//// the test did not stub is genuinely absent, which the suite uses to prove
//// that a missing CLI is a loud activity failure.

import aion/activity
import aion/testing
import brief_dev
import gate
import gleam/int
import gleam/list
import gleam/option
import gleam/string
import stacked_dev/activities
import stacked_dev/codecs_flow
import stacked_dev/codecs_workflows
import stacked_dev/types.{
  type BriefDocument, type EnrichInput, type ExecutionBlock, type Workspace,
  AttestationBlock, BriefDocument, DevInput, DevResult, EnrichInput,
  ExecutionBlock, ExecutionEnrichment, ExecutionLanded, GateBlock, GateInput,
  GatePass, GateResult, LandInput, Local, ProvisionInput, ResumeInput,
  ReviewInput, ReviewRequest, ScopedInput, ScoutInput, VerdictApproved,
  Workspace, WorkspaceWide, Worktree,
}

@external(erlang, "stacked_dev_test_ffi", "make_shim_root")
fn raw_make_shim_root() -> Result(String, String)

@external(erlang, "stacked_dev_test_ffi", "write_executable")
fn raw_write_executable(
  path: String,
  contents: String,
) -> Result(String, String)

@external(erlang, "stacked_dev_test_ffi", "put_env")
fn raw_put_env(name: String, value: String) -> Result(String, String)

@external(erlang, "stacked_dev_test_ffi", "read_log")
fn raw_read_log(path: String) -> Result(String, String)

/// One test's shim directory. `root` doubles as the repo root the provision
/// activity provisions worktrees under.
pub type Shims {
  Shims(root: String)
}

/// The canned diagnostics line the failing-scoped diagnostics shim emits;
/// tests assert it travels intact from the check failure into `dev_resume`'s
/// argv and into typed exhaustion errors.
pub const scoped_diagnostics = "error: unused variable count in crates/aion-core/src/lib.rs:42"

/// The canned report line the failing-workspace diagnostics shim emits.
pub const workspace_report = "error: cross-crate lint failure only the full workspace sweep catches"

/// The affected package the `yg graph affected` shim reports for any change.
pub const affected_package = "aion-core"

/// The deterministic session id: the dev activity derives it from the branch
/// (`stacked-dev-<brief_id>`), so for `brief-7` it is exactly this.
pub const session_id = "stacked-dev-brief-7"

/// The branch the land step merges (deterministic: stacked-dev-<brief_id>).
pub const landed_branch = "stacked-dev-brief-7"

/// The tree parent the land step merges into (the base ref).
pub const merged_into = "main"

/// Create a fresh shim directory and point `PATH` at it exclusively.
///
/// `PATH` is VM-global (unlike the harness's process-scoped fixtures), so
/// this suite relies on gleeunit's default sequential runner: every test
/// repoints `PATH` at its own shim directory before running the pipeline.
/// Do not move these tests to a parallel runner.
pub fn install() -> Shims {
  let assert Ok(root) = raw_make_shim_root()
  let assert Ok(_) = raw_put_env("PATH", root)
  Shims(root: root)
}

/// Read one shim's argv recording (empty when the shim never ran).
pub fn log(shims: Shims, executable: String) -> String {
  let assert Ok(contents) =
    raw_read_log(shims.root <> "/" <> executable <> ".log")
  contents
}

/// Count the recorded invocations whose argv starts with `prefix`.
pub fn invocations(shims: Shims, executable: String, prefix: String) -> Int {
  log(shims, executable)
  |> string.split("\n")
  |> list.filter(fn(line) { string.starts_with(line, prefix) })
  |> list.length
}

/// Install the `meridian` shim: review request acks only — provisioning and
/// checks belong to `yg`, and landing is `yg branch merge` now. The canned
/// response is the REAL `review request` envelope (confirmed live): branch,
/// per-reviewer notification outcomes, no request id.
pub fn write_meridian(shims: Shims) -> Nil {
  write_shim(shims, "meridian", [
    "case \"$1\" in",
    "  review)",
    "    printf '%s' '{\"branch\":\""
      <> landed_branch
      <> "\",\"reviewers\":[{\"name\":\"sample-reviewer\",\"dm_status\":\"sent\"}],\"pending_reviewers_persisted\":true}'",
    "    ;;",
    "  *)",
    "    echo \"unknown meridian subcommand: $1\" >&2",
    "    exit 64",
    "    ;;",
    "esac",
  ])
}

/// The scout-report envelope the scout norn invocation (`<branch>-scout`)
/// emits: one entry for R1 against the scout-report schema.
pub const scout_report_json = "{\"summary\":\"scouted the brief\",\"enrichments\":[{\"id\":\"R1\",\"files\":[\"crates/aion-core/src/lib.rs\"],\"context\":[\"match the existing taxonomy\"],\"approach\":\"add the variant\",\"notes\":\"\"}],\"verification\":[\"gleam test\"]}"

/// The dev-report envelope the dev norn invocation (the bare `<branch>`
/// session) emits: R1 implemented, touching one file.
pub const dev_report_json = "{\"summary\":\"implemented the brief\",\"commit_message\":\"feat: implement R1\",\"enrichments\":[{\"id\":\"R1\",\"status\":\"implemented\",\"files_changed\":[{\"path\":\"crates/aion-core/src/lib.rs\",\"change\":\"modified\",\"note\":\"added the variant\"}],\"how\":\"added the variant\",\"deviation\":\"\",\"checklist\":[],\"stories\":[]}],\"attestation\":{\"no_panics\":true,\"no_unsafe\":true,\"boundaries_respected\":true,\"tests_pass\":true}}"

/// The dev-report envelope a resume round emits: a FULL replacement report
/// touching a second file, proving the fix round produced a complete report.
pub const dev_resume_report_json = "{\"summary\":\"applied feedback\",\"commit_message\":\"fix: address diagnostics\",\"enrichments\":[{\"id\":\"R1\",\"status\":\"implemented\",\"files_changed\":[{\"path\":\"crates/aion-core/src/lib.rs\",\"change\":\"modified\",\"note\":\"fixed\"},{\"path\":\"crates/aion-core/src/error.rs\",\"change\":\"modified\",\"note\":\"fixed\"}],\"how\":\"applied the diagnostics\",\"deviation\":\"\",\"checklist\":[],\"stories\":[]}],\"attestation\":{\"no_panics\":true,\"no_unsafe\":true,\"boundaries_respected\":true,\"tests_pass\":true}}"

/// The review-report envelope the reviewer norn invocation (`<branch>-review`)
/// emits: R1 aligned with no fixes, so no drift and no harden re-verify.
pub const review_report_json = "{\"summary\":\"verified the diff\",\"commit_message\":\"\",\"enrichments\":[{\"id\":\"R1\",\"alignment\":\"aligned\",\"acceptance\":[{\"criterion\":\"the variant exists\",\"met\":true,\"evidence\":\"crates/aion-core/src/lib.rs:42\"}],\"checklist\":[],\"stories\":[],\"issues\":[],\"fixes\":[]}],\"verification\":[{\"criterion\":\"gleam test\",\"passed\":true,\"note\":\"\"}]}"

/// Install the `norn` shim. Each stage is distinguished by its session-id
/// suffix: `<branch>-scout` emits a scout report, `<branch>-review` emits a
/// review report, the bare `<branch>` session emits a dev report, and resume
/// (`--print --resume ...`) emits a full replacement dev report. Every
/// envelope is a bare stage report against its schema.
pub fn write_norn(shims: Shims) -> Nil {
  write_shim(shims, "norn", [
    "case \"$2\" in",
    "  --session-id)",
    "    case \"$3\" in",
    "      *-scout)",
    "        printf '%s' '" <> scout_report_json <> "'",
    "        ;;",
    "      *-review)",
    "        printf '%s' '" <> review_report_json <> "'",
    "        ;;",
    "      *)",
    "        printf '%s' '" <> dev_report_json <> "'",
    "        ;;",
    "    esac",
    "    ;;",
    "  --resume)",
    "    printf '%s' '" <> dev_resume_report_json <> "'",
    "    ;;",
    "  *)",
    "    echo \"unexpected norn invocation: $*\" >&2",
    "    exit 64",
    "    ;;",
    "esac",
  ])
}

/// Install a `cargo` shim where the warm build succeeds.
pub fn write_cargo(shims: Shims) -> Nil {
  write_shim(shims, "cargo", ["exit 0"])
}

/// Install a `git` shim where staging and committing succeed (the land
/// activity commits the dev rounds' files before merging).
pub fn write_git(shims: Shims) -> Nil {
  write_shim(shims, "git", ["exit 0"])
}

/// Install a `cargo` shim where `cargo build` (the warm build) fails.
pub fn write_cargo_failing_build(shims: Shims) -> Nil {
  write_shim(shims, "cargo", [
    "if [ \"$1\" = \"build\" ]; then",
    "  echo \"error: warm build exploded\"",
    "  exit 1",
    "fi",
    "exit 0",
  ])
}

/// Install a `yg` shim where branch/provision/graph work and every diagnostics
/// check passes.
pub fn write_yg_passing(shims: Shims) -> Nil {
  write_shim(shims, "yg", yg_script(["    exit 0"]))
}

/// Install a `yg` shim whose SCOPED diagnostics check (`--package ...`) fails
/// for the first `failures` invocations with the canned diagnostics, then
/// passes. The workspace gate and everything else always pass, so verify-fix
/// convergence is observable in isolation.
pub fn write_yg_failing_scoped(shims: Shims, failures: Int) -> Nil {
  write_shim(
    shims,
    "yg",
    yg_script([
      "    if echo \"$*\" | grep -q -- '--package'; then",
      "      RUNS=$(grep -c 'diagnostics check --format json --package' \""
        <> shims.root
        <> "/yg.log\")",
      "      if [ \"$RUNS\" -le " <> int_literal(failures) <> " ]; then",
      "        echo \"" <> scoped_diagnostics <> "\"",
      "        exit 1",
      "      fi",
      "    fi",
      "    exit 0",
    ]),
  )
}

/// Install a `yg` shim where only the WORKSPACE diagnostics gate fails: the
/// fast scoped loop converges, but the authoritative gate catches what scoping
/// missed.
pub fn write_yg_failing_workspace(shims: Shims) -> Nil {
  write_shim(
    shims,
    "yg",
    yg_script([
      "    if echo \"$*\" | grep -q -- '--workspace'; then",
      "      echo \"" <> workspace_report <> "\"",
      "      exit 1",
      "    fi",
      "    exit 0",
    ]),
  )
}

/// The shared `yg` script body: real branch add, a provision that creates the
/// worktree directory at the `--path` it is handed (so downstream activities
/// hold a real cwd), and an affected-modules query that reports one package.
/// `diagnostics_body` is the per-scenario `diagnostics check` behaviour.
fn yg_script(diagnostics_body: List(String)) -> List(String) {
  list.flatten([
    [
      "case \"$1\" in",
      "  branch)",
      "    case \"$2\" in",
      "      add) exit 0 ;;",
      "      provision) mkdir -p \"$5\"; exit 0 ;;",
      "      merge) exit 0 ;;",
      "      *) echo \"unknown yg branch: $2\" >&2; exit 64 ;;",
      "    esac",
      "    ;;",
      "  graph)",
      "    printf '%s\\n' '" <> affected_package <> "'",
      "    exit 0",
      "    ;;",
      "  diagnostics)",
    ],
    diagnostics_body,
    [
      "    ;;",
      "  *)",
      "    echo \"unknown yg subcommand: $1\" >&2; exit 64",
      "    ;;",
      "esac",
    ],
  ])
}

/// Register every activity's REAL local implementation (the CLI-shelling
/// functions from `stacked_dev/locals`, carried by each `activity.new`) as
/// its harness handler, and both child workflows' real `execute` functions
/// as typed child doubles — so the full pipeline executes genuine code with
/// the shims intercepting at the process boundary.
pub fn register_pipeline(env: testing.TestEnv) -> Nil {
  let workspace = sample_workspace()
  let dev_result =
    DevResult(session_id: "sample", files_touched: [], summary: "")

  register_activity(
    env,
    activities.provision_workspace(ProvisionInput(
      repo_root: "/sample/repo",
      brief_id: "sample",
      base_ref: "main",
      placement: Local,
      isolation: Worktree,
    )),
  )
  register_activity(
    env,
    activities.scout(ScoutInput(workspace: workspace, prompt: "")),
  )
  register_activity(env, activities.warm_build(workspace))
  register_activity(
    env,
    activities.dev(DevInput(workspace: workspace, prompt: "")),
  )
  register_activity(
    env,
    activities.scoped_checks(
      ScopedInput(workspace: workspace, files_touched: []),
    ),
  )
  register_activity(
    env,
    activities.dev_resume(ResumeInput(session_id: "sample", feedback: "")),
  )
  register_activity(
    env,
    activities.dev_review(ReviewInput(workspace: workspace, prompt: "")),
  )
  register_activity(
    env,
    activities.full_checks(GateInput(
      workspace: workspace,
      files_touched: [],
      scope: WorkspaceWide,
    )),
  )
  register_activity(
    env,
    activities.request_review(ReviewRequest(
      workspace: workspace,
      brief_id: "sample",
      reviewers: ["sample-reviewer"],
      dev_result: dev_result,
      gate_result: GateResult(verdict: GatePass),
    )),
  )
  register_activity(
    env,
    activities.land(LandInput(
      workspace: workspace,
      repo_root: "/sample/repo",
      base_ref: "main",
      dev_result: dev_result,
    )),
  )
  // The enrich_brief activity writes the worktree brief from a real path; the
  // pipeline scenarios provision their worktree fresh and do not seed a brief
  // file, so the outer-arc execution-block write is doubled with a pure
  // handler that returns the handed document (its real filesystem behaviour
  // is covered by the BD-004 enrich_brief activity tests below). The outer arc
  // ignores the returned document, so the identity stub suffices.
  let enrich =
    activities.enrich_brief(EnrichInput(
      workspace: workspace,
      document: sample_brief_document(),
      enrichment: ExecutionEnrichment(block: sample_execution_block()),
    ))
  let assert Ok(_) =
    testing.mock_activity(env, enrich, fn(input: EnrichInput) {
      Ok(input.document)
    })

  let assert Ok(_) =
    testing.mock_child(
      env,
      brief_dev.workflow_type,
      codecs_workflows.brief_dev_input_codec(),
      codecs_workflows.brief_dev_result_codec(),
      codecs_workflows.brief_dev_error_codec(),
      brief_dev.execute,
    )
  let assert Ok(_) =
    testing.mock_child(
      env,
      gate.workflow_type,
      codecs_flow.gate_input_codec(),
      codecs_flow.gate_result_codec(),
      codecs_flow.gate_error_codec(),
      gate.execute,
    )
  Nil
}

fn register_activity(
  env: testing.TestEnv,
  activity_value: activity.Activity(input, output),
) -> Nil {
  // The registered handler IS the activity's own local implementation; the
  // sample input carried by `activity_value` only anchors the name/codecs.
  let assert Ok(_) =
    testing.mock_activity(env, activity_value, activity.runner(activity_value))
  Nil
}

fn sample_workspace() -> Workspace {
  Workspace(
    path: "/sample/workspace",
    branch: "stacked/sample",
    placement: Local,
    isolation: Worktree,
  )
}

/// A minimal brief document anchoring the enrich_brief activity's name/codecs
/// (the sample input never runs; the registered handler is the stub above).
fn sample_brief_document() -> BriefDocument {
  BriefDocument(
    id: "BD-000",
    cluster: "brief-dev",
    title: "sample",
    depends_on: [],
    blocked_by: [],
    checklist: [],
    stories: [],
    design_anchor: [],
    purpose: "",
    task: "",
    requirements: [],
    boundaries: [],
    verification: [],
    execution: option.None,
  )
}

/// A minimal execution block anchoring the enrich_brief activity's name/codecs.
fn sample_execution_block() -> ExecutionBlock {
  ExecutionBlock(
    status: ExecutionLanded,
    workflow_id: "wf",
    branch: "stacked/sample",
    session_id: "stacked/sample",
    gate: GateBlock(fmt: True, clippy: True, tests: True, fix_rounds: 0),
    attestation: AttestationBlock(
      no_panics: True,
      no_unsafe: True,
      boundaries_respected: True,
      tests_pass: True,
    ),
    review_verdict: VerdictApproved,
    landed_commit: "",
    merged_into: "main",
    completed_at: "",
  )
}

fn write_shim(shims: Shims, executable: String, body: List(String)) -> Nil {
  let script =
    string.join(
      [
        "#!/bin/sh",
        // The suite leaves only the shim directory on PATH; the scripts
        // themselves still need the standard tools.
        "PATH=/usr/bin:/bin",
        "echo \"$@\" >> \"" <> shims.root <> "/" <> executable <> ".log\"",
        ..body
      ],
      "\n",
    )
    <> "\n"
  let assert Ok(_) =
    raw_write_executable(shims.root <> "/" <> executable, script)
  Nil
}

fn int_literal(value: Int) -> String {
  int.to_string(value)
}
