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
import {{name}}_gate
import gleam/int
import gleam/list
import gleam/string
import {{name}}_dev
import {{name}}/activities
import {{name}}/codecs_workflows
import {{name}}/types.{
  type Workspace, DevInput, DevResult, GateInput, GatePass, GateResult,
  LandInput, Local, ProvisionInput, ResumeInput, ReviewRequest, ScopedInput,
  Workspace, WorkspaceWide, Worktree,
}

@external(erlang, "{{name}}_test_ffi", "make_shim_root")
fn raw_make_shim_root() -> Result(String, String)

@external(erlang, "{{name}}_test_ffi", "write_executable")
fn raw_write_executable(
  path: String,
  contents: String,
) -> Result(String, String)

@external(erlang, "{{name}}_test_ffi", "put_env")
fn raw_put_env(name: String, value: String) -> Result(String, String)

@external(erlang, "{{name}}_test_ffi", "read_log")
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
/// (`<project>-<brief_id>`), so for `brief-7` it is exactly this.
pub const session_id = "{{name}}-brief-7"

/// The PR URL the meridian stack-submit shim reports.
pub const pr_url = "https://example.test/pr/41"

/// The merge commit the meridian stack-land shim reports.
pub const merge_commit = "deadbeefcafe"

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

/// Install the `meridian` shim: review acks, the stack submits then lands.
/// Provisioning and checks belong to `yg` now.
pub fn write_meridian(shims: Shims) -> Nil {
  write_shim(shims, "meridian", [
    "case \"$1\" in",
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

/// Install the `norn` shim. The dev invocation (`--print --session-id ...`)
/// reports one touched file; resume (`--print --resume ...`) keeps the session
/// and reports the feedback applied. The activity overrides the session id
/// with the one it set, so the value reported here only anchors the shape.
pub fn write_norn(shims: Shims) -> Nil {
  write_shim(shims, "norn", [
    "case \"$2\" in",
    "  --session-id)",
    "    printf '%s' '{\"session_id\":\""
      <> session_id
      <> "\",\"files_touched\":[\"crates/aion-core/src/lib.rs\"],\"summary\":\"implemented the brief\"}'",
    "    ;;",
    "  --resume)",
    "    printf '%s' '{\"session_id\":\""
      <> session_id
      <> "\",\"files_touched\":[\"crates/aion-core/src/lib.rs\",\"crates/aion-core/src/error.rs\"],\"summary\":\"applied feedback\"}'",
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
/// functions from `{{name}}/locals`, carried by each `activity.new`) as
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
      {{name}}_dev.workflow_type,
      codecs_workflows.dev_flow_input_codec(),
      codecs_workflows.dev_flow_result_codec(),
      codecs_workflows.dev_flow_error_codec(),
      {{name}}_dev.execute,
    )
  let assert Ok(_) =
    testing.mock_child(
      env,
      {{name}}_gate.workflow_type,
      codecs_workflows.gate_input_codec(),
      codecs_workflows.gate_result_codec(),
      codecs_workflows.gate_error_codec(),
      {{name}}_gate.execute,
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
    branch: "pipeline/sample",
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
