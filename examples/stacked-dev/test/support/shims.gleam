//// Fake-CLI shim harness for the hermetic test suite.
////
//// Each test builds its own shim directory of stub scripts (`meridian`,
//// `norn`, `cargo`) that emit canned JSON and append their argv to
//// per-executable log files, then points `PATH` at that directory ALONE.
//// The activity local implementations stay honest — they really shell out —
//// and the shims are the test double at the process boundary: the most
//// realistic seam. Because `PATH` contains only the shim directory, a CLI
//// the test did not stub is genuinely absent, which the suite uses to prove
//// that a missing CLI is a loud activity failure.

import aion/activity
import aion/testing
import gate
import gleam/int
import gleam/list
import gleam/string
import onatopp_dev
import stacked_dev/activities
import stacked_dev/codecs_flow
import stacked_dev/codecs_workflows
import stacked_dev/types.{
  type Workspace, DevInput, DevResult, GateInput, GatePass, GateResult,
  LandInput, Local, ProvisionInput, ResumeInput, ReviewRequest, ScopedInput,
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

/// One test's shim directory plus the workspace directory its provision
/// shim hands back.
pub type Shims {
  Shims(root: String, workspace: String)
}

/// The canned diagnostics line the failing-clippy cargo shim emits; tests
/// assert it travels intact from the check failure into `dev_resume`'s argv
/// and into typed exhaustion errors.
pub const clippy_diagnostics = "error: unused variable count in crates/aion-core/src/lib.rs:42"

/// The session id the norn shim reports for run and resume.
pub const session_id = "sess-1"

/// The PR URL the meridian stack-submit shim reports.
pub const pr_url = "https://example.test/pr/41"

/// The merge commit the meridian stack-land shim reports.
pub const merge_commit = "deadbeefcafe"

/// Create a fresh shim directory and point `PATH` at it exclusively.
pub fn install() -> Shims {
  let assert Ok(root) = raw_make_shim_root()
  let assert Ok(_) = raw_put_env("PATH", root)
  Shims(root: root, workspace: root <> "/workspace")
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

/// Install the standard `meridian` shim: provision answers with this shim
/// set's workspace, scoping answers `aion-core`, review acks, and the stack
/// submits then lands.
pub fn write_meridian(shims: Shims) -> Nil {
  write_shim(shims, "meridian", [
    "case \"$1\" in",
    "  workspace)",
    "    printf '%s' '{\"path\":\""
      <> shims.workspace
      <> "\",\"branch\":\"stacked/brief-7\"}'",
    "    ;;",
    "  affected-modules)",
    "    printf '%s' '{\"affected_modules\":[\"aion-core\"]}'",
    "    ;;",
    "  review)",
    "    printf '%s' '{\"request_id\":\"rev-1\"}'",
    "    ;;",
    "  stack)",
    "    case \"$2\" in",
    "      submit) printf '%s' '{\"pr_url\":\"" <> pr_url <> "\"}' ;;",
    "      land) printf '%s' '{\"merge_commit\":\"" <> merge_commit <> "\"}' ;;",
    "      *) echo \"unknown stack subcommand: $2\" >&2; exit 64 ;;",
    "    esac",
    "    ;;",
    "  *)",
    "    echo \"unknown meridian subcommand: $1\" >&2",
    "    exit 64",
    "    ;;",
    "esac",
  ])
}

/// Install the standard `norn` shim: `run` opens session `sess-1` touching
/// one file; `resume` keeps the session and reports the feedback applied.
pub fn write_norn(shims: Shims) -> Nil {
  write_shim(shims, "norn", [
    "case \"$1\" in",
    "  run)",
    "    printf '%s' '{\"session_id\":\""
      <> session_id
      <> "\",\"files_touched\":[\"crates/aion-core/src/lib.rs\"],\"summary\":\"implemented the brief\"}'",
    "    ;;",
    "  resume)",
    "    printf '%s' '{\"session_id\":\""
      <> session_id
      <> "\",\"files_touched\":[\"crates/aion-core/src/lib.rs\",\"crates/aion-core/src/error.rs\"],\"summary\":\"applied feedback\"}'",
    "    ;;",
    "  *)",
    "    echo \"unknown norn subcommand: $1\" >&2",
    "    exit 64",
    "    ;;",
    "esac",
  ])
}

/// Install a `cargo` shim where every command succeeds.
pub fn write_cargo_passing(shims: Shims) -> Nil {
  write_shim(shims, "cargo", ["exit 0"])
}

/// Install a `cargo` shim whose SCOPED clippy (`clippy -p ...`) fails for
/// the first `failures` invocations with the canned diagnostics, then
/// passes. Workspace-wide clippy (the gate) and everything else always
/// pass, so verify-fix convergence is observable in isolation.
pub fn write_cargo_failing_scoped_clippy(shims: Shims, failures: Int) -> Nil {
  write_shim(shims, "cargo", [
    "if [ \"$1\" = \"clippy\" ] && [ \"$2\" = \"-p\" ]; then",
    "  RUNS=$(grep -c '^clippy -p' \"" <> shims.root <> "/cargo.log\")",
    "  if [ \"$RUNS\" -le " <> int_literal(failures) <> " ]; then",
    "    echo \"" <> clippy_diagnostics <> "\"",
    "    exit 1",
    "  fi",
    "fi",
    "exit 0",
  ])
}

/// Install a `cargo` shim where only `cargo build` (the warm build) fails.
pub fn write_cargo_failing_build(shims: Shims) -> Nil {
  write_shim(shims, "cargo", [
    "if [ \"$1\" = \"build\" ]; then",
    "  echo \"error: warm build exploded\"",
    "  exit 1",
    "fi",
    "exit 0",
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
      brief_id: "sample",
      base_ref: "main",
      placement: Local,
      isolation: Worktree,
    )),
  )
  register_activity(env, activities.warm_build(workspace))
  register_activity(
    env,
    activities.dev(
      DevInput(
        workspace: workspace,
        brief: "",
        design: "",
        checklist: "",
        stories: [],
      ),
    ),
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
      dev_result: dev_result,
      gate_result: GateResult(verdict: GatePass),
    )),
  )
  register_activity(
    env,
    activities.land(LandInput(workspace: workspace, dev_result: dev_result)),
  )

  let assert Ok(_) =
    testing.mock_child(
      env,
      onatopp_dev.workflow_type,
      codecs_workflows.onatopp_input_codec(),
      codecs_workflows.onatopp_result_codec(),
      codecs_workflows.onatopp_error_codec(),
      onatopp_dev.execute,
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
