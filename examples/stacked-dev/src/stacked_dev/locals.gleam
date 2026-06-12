//// Activity local implementations — the test seam (brief section 4).
////
//// Under the `aion/testing` harness each activity executes one of these
//// functions in-process; each shells to the real CLI named in the brief
//// (`norn` for dev work, `cargo` for checks and the warm build, `meridian`
//// for provisioning, scoping, review, and landing) through
//// `stacked_dev/cli`. The hermetic test suite intercepts at the process
//// boundary with fake-CLI shims placed first on `PATH` — the most realistic
//// seam — while these implementations stay honest: they really shell out,
//// and a missing CLI with no shim is a loud `Terminal` activity failure,
//// never a silent skip.
////
//// Deployed, a Meridian worker serves the same activity names and these
//// functions never run.

import aion/error
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/string
import stacked_dev/cli
import stacked_dev/codecs_core
import stacked_dev/types.{
  type CheckResult, type DevInput, type DevResult, type GateInput,
  type GateResult, type LandInput, type Landed, type ProvisionInput,
  type ResumeInput, type ReviewAck, type ReviewRequest, type ScopedInput,
  type StartupResult, type StartupTask, type Workspace, AffectedClosure,
  BuildWarm, CheckFail, CheckPass, CheckResult, Copy, DevTask, Developed,
  GateFail, GatePass, GateResult, Landed, Overlay, ReviewAck, Vm, WarmTask,
  Warmed, Workspace, WorkspaceWide, Worktree,
}

/// Provision an isolated workspace via the `meridian` CLI.
///
/// Only the worktree isolation mode has a local implementation today; the
/// other typed variants are explicit seams that fail loudly until Meridian's
/// dispatch exists.
pub fn provision_workspace(
  input: ProvisionInput,
) -> Result(Workspace, error.ActivityError) {
  case input.isolation {
    Worktree -> provision_worktree(input)
    Copy | Overlay | Vm ->
      // TODO(meridian): exchange-VM dispatch — Copy/Overlay/Vm isolation has
      // no local implementation yet; the typed variants exist so the rest of
      // the workflow never cares which isolation produced the Workspace.
      Error(error.terminal(
        "isolation mode "
        <> codecs_core.isolation_to_string(input.isolation)
        <> " is a typed seam with no local implementation"
        <> " (TODO(meridian): exchange-VM dispatch)",
      ))
  }
}

fn provision_worktree(
  input: ProvisionInput,
) -> Result(Workspace, error.ActivityError) {
  // TODO(meridian): confirm the provision subcommand and flag names; the
  // placement/isolation axis itself is real.
  use command_run <- require_run(
    cli.run(
      "meridian",
      [
        "workspace",
        "provision",
        "--brief-id",
        input.brief_id,
        "--base-ref",
        input.base_ref,
        "--isolation",
        codecs_core.isolation_to_string(input.isolation),
        "--placement",
        codecs_core.placement_to_string(input.placement),
      ],
      ".",
    ),
    "meridian workspace provision",
  )
  use provisioned <- require_json(command_run, "meridian workspace provision", {
    use path <- decode.field("path", decode.string)
    use branch <- decode.field("branch", decode.string)
    decode.success(#(path, branch))
  })
  let #(path, branch) = provisioned
  Ok(Workspace(
    path: path,
    branch: branch,
    placement: input.placement,
    isolation: input.isolation,
  ))
}

/// Run one startup fan-out task: the advisory warm build or the dev round.
pub fn startup_task(
  task: StartupTask,
) -> Result(StartupResult, error.ActivityError) {
  case task {
    WarmTask(workspace: workspace) -> warm_build(workspace)
    DevTask(dev_input: dev_input) -> dev(dev_input)
  }
}

/// Warm the build cache with `cargo build` in the workspace.
///
/// Advisory by contract (open question Q4): a failed build forfeits the warm
/// cache and is recorded as `ok: False` — it must never fail the run. A
/// missing `cargo` executable is still a loud `Terminal` failure: that is a
/// broken environment, not a forfeited cache.
fn warm_build(
  workspace: Workspace,
) -> Result(StartupResult, error.ActivityError) {
  case cli.run("cargo", ["build"], workspace.path) {
    Ok(command_run) ->
      Ok(
        Warmed(build_warm: BuildWarm(
          ok: cli.succeeded(command_run),
          duration_ms: command_run.duration_ms,
        )),
      )
    Error(failure) ->
      Error(error.terminal("cargo build: " <> cli.failure_message(failure)))
  }
}

/// Run the dev agent against the brief via the `norn` CLI.
fn dev(input: DevInput) -> Result(StartupResult, error.ActivityError) {
  // TODO(meridian): confirm the norn run flag names (the dev step itself is
  // the one the brief is surest about).
  use command_run <- require_run(
    cli.run(
      "norn",
      [
        "run",
        "--workspace",
        input.workspace.path,
        "--brief",
        input.brief,
        "--design",
        input.design,
        "--checklist",
        input.checklist,
        "--stories",
        string.join(input.stories, ","),
      ],
      input.workspace.path,
    ),
    "norn run",
  )
  use dev_result <- require_dev_result(command_run, "norn run")
  Ok(Developed(dev_result: dev_result))
}

/// Resume the same dev agent session with feedback (scoped-check diagnostics
/// or encoded review notes).
pub fn dev_resume(
  input: ResumeInput,
) -> Result(DevResult, error.ActivityError) {
  // TODO(meridian): confirm the norn resume flag names.
  use command_run <- require_run(
    cli.run(
      "norn",
      ["resume", "--session", input.session_id, "--feedback", input.feedback],
      ".",
    ),
    "norn resume",
  )
  use dev_result <- require_dev_result(command_run, "norn resume")
  Ok(dev_result)
}

/// Scoped verification: compute the affected module set, then run
/// clippy/test/fmt limited to it.
///
/// Resolves open question Q1 (scoping seam): the affected set comes from a
/// CLI call — the Gleam side stays pure and the workflow consumes
/// `affected_modules` from the activity result. An empty affected set falls
/// back LOUDLY to a named workspace-wide scope; zero checks are never run
/// silently.
pub fn scoped_checks(
  input: ScopedInput,
) -> Result(CheckResult, error.ActivityError) {
  // TODO(meridian): affected-modules subcommand — the libyggd dependency
  // graph query lives behind the meridian CLI for now.
  use command_run <- require_run(
    cli.run(
      "meridian",
      [
        "affected-modules",
        "--workspace-root",
        input.workspace.path,
        "--files",
        string.join(input.files_touched, ","),
      ],
      input.workspace.path,
    ),
    "meridian affected-modules",
  )
  use affected_modules <- require_json(
    command_run,
    "meridian affected-modules",
    {
      use affected <- decode.field(
        "affected_modules",
        decode.list(decode.string),
      )
      decode.success(affected)
    },
  )
  case affected_modules {
    [] -> {
      // Loud fallback to a named wider scope — never silently run nothing.
      let scope =
        "workspace-wide fallback: affected-module scoping returned an empty set"
      use verdict <- try_check_commands(
        workspace_check_commands(),
        input.workspace,
      )
      Ok(CheckResult(
        verdict: verdict,
        affected_modules: [],
        checked_scope: scope,
      ))
    }
    modules -> {
      let scope = "affected: " <> string.join(modules, ", ")
      use verdict <- try_check_commands(
        scoped_check_commands(modules),
        input.workspace,
      )
      Ok(CheckResult(
        verdict: verdict,
        affected_modules: modules,
        checked_scope: scope,
      ))
    }
  }
}

/// The authoritative gate: full fmt + clippy + test, stricter than the fast
/// scoped inner loop.
pub fn full_checks(
  input: GateInput,
) -> Result(GateResult, error.ActivityError) {
  case input.scope {
    WorkspaceWide -> {
      use verdict <- try_check_commands(
        workspace_check_commands(),
        input.workspace,
      )
      case verdict {
        CheckPass -> Ok(GateResult(verdict: GatePass))
        CheckFail(diagnostics: diagnostics) ->
          Ok(GateResult(verdict: GateFail(report: diagnostics)))
      }
    }
    AffectedClosure(modules: _) ->
      // Open question Q2: the affected-closure gate scope is a typed seam
      // only — nothing guessed until the graph-derived closure is trusted.
      Error(error.terminal(
        "affected-closure gate scope has no local implementation"
        <> " (TODO(meridian): complete affected closure from the workspace graph)",
      ))
  }
}

/// Emit a review request. It only requests — the verdict arrives later on
/// the `review_verdict` signal.
pub fn request_review(
  input: ReviewRequest,
) -> Result(ReviewAck, error.ActivityError) {
  // TODO(meridian): confirm the review request command and its output
  // schema (a meridian/collective message or an artifact write).
  use command_run <- require_run(
    cli.run(
      "meridian",
      [
        "review",
        "request",
        "--workspace",
        input.workspace.path,
        "--brief-id",
        input.brief_id,
        "--summary",
        input.dev_result.summary,
      ],
      input.workspace.path,
    ),
    "meridian review request",
  )
  use request_id <- require_json(command_run, "meridian review request", {
    use request_id <- decode.field("request_id", decode.string)
    decode.success(request_id)
  })
  Ok(ReviewAck(request_id: request_id))
}

/// Land the approved work: stack submit, then stack land. Never a manual
/// cherry-pick or merge.
pub fn land(input: LandInput) -> Result(Landed, error.ActivityError) {
  // TODO(meridian): confirm the stack submit/land output schemas.
  use submit_run <- require_run(
    cli.run("meridian", ["stack", "submit"], input.workspace.path),
    "meridian stack submit",
  )
  use pr_url <- require_json(submit_run, "meridian stack submit", {
    use pr_url <- decode.field("pr_url", decode.string)
    decode.success(pr_url)
  })
  use land_run <- require_run(
    cli.run("meridian", ["stack", "land"], input.workspace.path),
    "meridian stack land",
  )
  use merge_commit <- require_json(land_run, "meridian stack land", {
    use merge_commit <- decode.field("merge_commit", decode.string)
    decode.success(merge_commit)
  })
  Ok(Landed(pr_url: pr_url, merge_commit: merge_commit))
}

// --- helpers ---------------------------------------------------------------

/// Cargo commands for the workspace-wide scope (the gate, and the loud
/// scoped fallback).
fn workspace_check_commands() -> List(List(String)) {
  [
    ["fmt", "--check"],
    ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    ["test"],
  ]
}

/// Cargo commands limited to the affected modules: fmt once, then clippy and
/// test per module.
fn scoped_check_commands(modules: List(String)) -> List(List(String)) {
  [
    [["fmt", "--check"]],
    list.map(modules, fn(module) {
      ["clippy", "-p", module, "--all-targets", "--", "-D", "warnings"]
    }),
    list.map(modules, fn(module) { ["test", "-p", module] }),
  ]
  |> list.flatten
}

/// Run a list of cargo commands in the workspace, aggregating every failed
/// command's diagnostics. Infrastructure failures (missing executable,
/// spawn errors) are loud `Terminal` activity errors.
fn try_check_commands(
  commands: List(List(String)),
  workspace: Workspace,
  next: fn(types.CheckVerdict) -> Result(value, error.ActivityError),
) -> Result(value, error.ActivityError) {
  let outcomes =
    list.try_map(commands, fn(args) {
      case cli.run("cargo", args, workspace.path) {
        Ok(command_run) ->
          case cli.succeeded(command_run) {
            True -> Ok(CheckPass)
            False ->
              Ok(CheckFail(
                diagnostics: "cargo "
                <> string.join(args, " ")
                <> " failed — "
                <> cli.run_diagnostics(command_run),
              ))
          }
        Error(failure) ->
          Error(error.terminal(
            "cargo "
            <> string.join(args, " ")
            <> ": "
            <> cli.failure_message(failure),
          ))
      }
    })
  case outcomes {
    Ok(verdicts) -> {
      let failures =
        list.filter_map(verdicts, fn(verdict) {
          case verdict {
            CheckPass -> Error(Nil)
            CheckFail(diagnostics: diagnostics) -> Ok(diagnostics)
          }
        })
      case failures {
        [] -> next(CheckPass)
        _ -> next(CheckFail(diagnostics: string.join(failures, "\n")))
      }
    }
    Error(activity_error) -> Error(activity_error)
  }
}

/// Require a command to run AND exit zero; anything else is a `Terminal`
/// activity failure carrying the command's diagnostics.
fn require_run(
  outcome: Result(cli.CliRun, cli.CliFailure),
  context: String,
  next: fn(cli.CliRun) -> Result(value, error.ActivityError),
) -> Result(value, error.ActivityError) {
  case outcome {
    Ok(command_run) ->
      case cli.succeeded(command_run) {
        True -> next(command_run)
        False ->
          Error(error.terminal(
            context <> " failed — " <> cli.run_diagnostics(command_run),
          ))
      }
    Error(failure) ->
      Error(error.terminal(context <> ": " <> cli.failure_message(failure)))
  }
}

/// Decode a command's stdout as JSON with the supplied decoder; malformed
/// output is a `Terminal` activity failure carrying the raw text.
fn require_json(
  command_run: cli.CliRun,
  context: String,
  decoder: decode.Decoder(value),
  next: fn(value) -> Result(output, error.ActivityError),
) -> Result(output, error.ActivityError) {
  case json.parse(string.trim(command_run.output), decoder) {
    Ok(value) -> next(value)
    Error(_) ->
      Error(error.terminal(
        context
        <> " produced unparseable output: "
        <> string.trim(command_run.output),
      ))
  }
}

/// Decode a norn command's stdout as a `DevResult`.
fn require_dev_result(
  command_run: cli.CliRun,
  context: String,
  next: fn(DevResult) -> Result(value, error.ActivityError),
) -> Result(value, error.ActivityError) {
  let trimmed = string.trim(command_run.output)
  case codecs_core.dev_result_codec().decode(trimmed) {
    Ok(dev_result) -> next(dev_result)
    Error(_) ->
      Error(error.terminal(
        context <> " produced unparseable output: " <> trimmed,
      ))
  }
}
