//// Activity local implementations — the test seam (brief section 4).
////
//// Under the `aion/testing` harness each activity executes one of these
//// functions in-process; each shells to the real CLI that owns the step
//// (`norn` for the dev agent, `yg` for worktree provisioning, affected-module
//// scoping, diagnostics checks, and landing, `cargo` for the advisory warm
//// build, `meridian` for review requests) through `stacked_dev/cli`.
//// The hermetic test suite intercepts at the process boundary with fake-CLI
//// shims placed first on `PATH` — the most realistic seam — while these
//// implementations stay honest: they really shell out, and a missing CLI with
//// no shim is a loud `Terminal` activity failure, never a silent skip.
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
  BuildWarm, CheckFail, CheckPass, CheckResult, Copy, DevResult, DevTask,
  Developed, GateFail, GatePass, GateResult, Landed, Overlay, ReviewAck, Vm,
  WarmTask, Warmed, Workspace, WorkspaceWide, Worktree,
}

/// Provision an isolated workspace via the `yg` CLI.
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
  // Worktree provisioning is two real yg verbs: add the branch as a child of
  // the base ref in the tree, then provision its worktree at a known path.
  // The worktree path is absolute (built from the repo root), so every
  // downstream activity holds a real directory and never a cwd-relative guess.
  let branch = "stacked-dev-" <> input.brief_id
  let worktree_path = input.repo_root <> "/.yggdrasil-worktrees/" <> branch

  use _added <- require_run(
    cli.run("yg", ["branch", "add", branch, input.base_ref], input.repo_root),
    "yg branch add",
  )
  // We pass an explicit --path so the worktree location is known a priori and
  // never parsed out of human output.
  use _provisioned <- require_run(
    cli.run(
      "yg",
      ["branch", "provision", branch, "--path", worktree_path],
      input.repo_root,
    ),
    "yg branch provision",
  )
  Ok(Workspace(
    path: worktree_path,
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
  // The session id is deterministic (the branch name), so resume rounds target
  // the same session without ever capturing a generated id. norn validates the
  // charset; "stacked-dev-<brief>" is legal.
  let session_id = input.workspace.branch
  let prompt = dev_prompt(input)

  // norn takes the prompt positionally; --print is headless, --session-id mints
  // exactly this id, --output-schema constrains the structured result, and
  // --output-format json emits the final envelope we decode.
  // TODO(meridian): add --profile <dev profile> and port the richer prompt
  // assembly (design-context extraction, per-R# rendering) from
  // .meridian/workflows/onatopp-dev-norn/workflow.rhai.
  use command_run <- require_run(
    cli.run(
      "norn",
      [
        "--print",
        "--session-id",
        session_id,
        "--workspace-root",
        input.workspace.path,
        "--output-schema",
        dev_output_schema(),
        "--output-format",
        "json",
        prompt,
      ],
      input.workspace.path,
    ),
    "norn dev",
  )
  use dev_result <- require_dev_result(command_run, "norn dev")
  Ok(Developed(dev_result: DevResult(..dev_result, session_id: session_id)))
}

/// The JSON Schema norn structures the dev/resume result against — the
/// `DevResult` shape (`session_id`, `files_touched`, `summary`). Passed inline
/// to `--output-schema` so there is no schema file to resolve in the workspace.
fn dev_output_schema() -> String {
  "{\"type\":\"object\","
  <> "\"required\":[\"session_id\",\"files_touched\",\"summary\"],"
  <> "\"additionalProperties\":false,"
  <> "\"properties\":{"
  <> "\"session_id\":{\"type\":\"string\"},"
  <> "\"files_touched\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}},"
  <> "\"summary\":{\"type\":\"string\"}}}"
}

/// Assemble the dev prompt from the brief and its design context.
fn dev_prompt(input: DevInput) -> String {
  string.join(
    [
      "Implement the following brief in this workspace.",
      "## Brief\n" <> input.brief,
      "## Design\n" <> input.design,
      "## Checklist\n" <> input.checklist,
      "## Stories\n" <> string.join(input.stories, "\n"),
      "Return your structured result matching the output schema.",
    ],
    "\n\n",
  )
}

/// Resume the same dev agent session with feedback (scoped-check diagnostics
/// or encoded review notes).
pub fn dev_resume(
  input: ResumeInput,
) -> Result(DevResult, error.ActivityError) {
  // Resume by the deterministic session id; the feedback is the prompt.
  // TODO(meridian): carry the workspace root on ResumeInput so resume can also
  // confine file tools with --workspace-root like the dev step does.
  use command_run <- require_run(
    cli.run(
      "norn",
      [
        "--print",
        "--resume",
        input.session_id,
        "--output-schema",
        dev_output_schema(),
        "--output-format",
        "json",
        input.feedback,
      ],
      ".",
    ),
    "norn resume",
  )
  use dev_result <- require_dev_result(command_run, "norn resume")
  Ok(DevResult(..dev_result, session_id: input.session_id))
}

/// Scoped verification: compute the affected package set from the dependency
/// graph, then run diagnostics limited to it.
///
/// Resolves open question Q1 (scoping seam): the affected set comes from a
/// CLI call — the Gleam side stays pure and the workflow consumes
/// `affected_modules` from the activity result. An empty affected set falls
/// back LOUDLY to a named workspace-wide scope; zero checks are never run
/// silently.
pub fn scoped_checks(
  input: ScopedInput,
) -> Result(CheckResult, error.ActivityError) {
  // Affected packages come from the dependency graph: `yg graph affected
  // --plain --direct-only` prints one bare crate name per line (direct-only =
  // the crates that actually contain the changed files; the gate runs broad).
  use affected_run <- require_run(
    cli.run(
      "yg",
      list.flatten([
        ["graph", "affected", "--plain", "--direct-only"],
        input.files_touched,
      ]),
      input.workspace.path,
    ),
    "yg graph affected",
  )
  let packages =
    affected_run.output
    |> string.split("\n")
    |> list.map(string.trim)
    |> list.filter(fn(name) { name != "" })

  case packages {
    [] -> {
      // No affected packages — fall back LOUDLY to a named workspace-wide
      // scope; zero checks are never run silently.
      let scope =
        "workspace-wide fallback: affected scoping returned an empty set"
      check_with(
        ["diagnostics", "check", "--workspace", "--format", "json"],
        input.workspace,
        [],
        scope,
      )
    }
    modules -> {
      // One scoped diagnostics run over exactly the affected packages.
      let package_args =
        list.flat_map(modules, fn(name) { ["--package", name] })
      let args =
        list.flatten([
          ["diagnostics", "check", "--format", "json"],
          package_args,
        ])
      let scope = "affected: " <> string.join(modules, ", ")
      check_with(args, input.workspace, modules, scope)
    }
  }
}

/// Run one `yg diagnostics check` invocation and shape the verdict. Exit zero
/// is a pass; a non-zero exit carries the diagnostics output. A command that
/// cannot run at all is a loud `Terminal` activity failure.
fn check_with(
  args: List(String),
  workspace: Workspace,
  affected_modules: List(String),
  scope: String,
) -> Result(CheckResult, error.ActivityError) {
  case cli.run("yg", args, workspace.path) {
    Ok(command_run) -> {
      let verdict = case cli.succeeded(command_run) {
        True -> CheckPass
        False -> CheckFail(diagnostics: command_run.output)
      }
      Ok(CheckResult(
        verdict: verdict,
        affected_modules: affected_modules,
        checked_scope: scope,
      ))
    }
    Error(failure) ->
      Error(error.terminal(
        "yg diagnostics check: " <> cli.failure_message(failure),
      ))
  }
}

/// The authoritative gate: the full workspace diagnostics run, stricter than
/// the fast scoped inner loop.
pub fn full_checks(
  input: GateInput,
) -> Result(GateResult, error.ActivityError) {
  case input.scope {
    WorkspaceWide ->
      case
        cli.run(
          "yg",
          ["diagnostics", "check", "--workspace", "--format", "json"],
          input.workspace.path,
        )
      {
        Ok(command_run) ->
          case cli.succeeded(command_run) {
            True -> Ok(GateResult(verdict: GatePass))
            False ->
              Ok(GateResult(verdict: GateFail(report: command_run.output)))
          }
        Error(failure) ->
          Error(error.terminal(
            "yg diagnostics check --workspace: " <> cli.failure_message(failure),
          ))
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
  // CONFIRMED against the real CLI (live runs, 2026-06-13):
  // `meridian review request <BRANCH> --reviewer <NAME>... --as Meridian`.
  // The branch positional must come FIRST: `--reviewer` is greedy
  // multi-value and swallows a trailing positional as another reviewer.
  // `--as` names the requesting identity — always the Meridian system
  // member (the CLI refuses to guess when the workspace has several
  // members). The meridian workspace resolves from the CLI's own global
  // config, never from workflow inputs.
  let reviewer_args =
    list.flat_map(input.reviewers, fn(reviewer) { ["--reviewer", reviewer] })
  use command_run <- require_run(
    cli.run(
      "meridian",
      list.flatten([
        ["review", "request", input.workspace.branch],
        reviewer_args,
        ["--as", "Meridian"],
      ]),
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

/// Land the approved work: `yg branch merge` into the tree parent. Never a
/// manual cherry-pick or merge.
pub fn land(input: LandInput) -> Result(Landed, error.ActivityError) {
  // Landing is a yg-level stack operation (confirmed direction, 2026-06-13):
  // `yg branch merge <branch>` merges the branch into its tree parent — the
  // base ref it was provisioned from. Local, no PR machinery; exit zero is
  // the whole contract.
  use _merged <- require_run(
    cli.run(
      "yg",
      ["branch", "merge", input.workspace.branch, "--yes"],
      input.workspace.path,
    ),
    "yg branch merge",
  )
  Ok(Landed(branch: input.workspace.branch, merged_into: input.base_ref))
}

// --- helpers ---------------------------------------------------------------

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
///
/// CONFIRMED against real norn (live run, 2026-06-13): `--output-format json`
/// emits a completion envelope with the schema-constrained result under
/// `"output"` (alongside `usage`/`model`/`events`, ignored here). The bare
/// shape is tried first because the fake-CLI shims emit the `DevResult` raw.
/// The session id is set by the caller (`--session-id`), so it is overwritten
/// after decode regardless.
fn require_dev_result(
  command_run: cli.CliRun,
  context: String,
  next: fn(DevResult) -> Result(value, error.ActivityError),
) -> Result(value, error.ActivityError) {
  let trimmed = string.trim(command_run.output)
  case codecs_core.dev_result_codec().decode(trimmed) {
    Ok(dev_result) -> next(dev_result)
    Error(_) ->
      case codecs_core.norn_envelope_codec().decode(trimmed) {
        Ok(dev_result) -> next(dev_result)
        Error(_) ->
          Error(error.terminal(
            context
            <> " produced unparseable output (tried the bare DevResult shape"
            <> " and norn's {\"output\": ...} envelope): "
            <> trimmed,
          ))
      }
  }
}
