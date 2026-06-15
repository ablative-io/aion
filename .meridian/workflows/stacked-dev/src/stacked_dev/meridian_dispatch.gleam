//// Meridian activity dispatch layer for the stacked-dev workflow family.
////
//// Each function dispatches to a Meridian-registered activity worker via
//// `aion_dispatch`, maps between the workflow's domain types and Meridian's
//// wire types (NornInput/NornOutput, CheckInput/CheckOutput), and parses
//// the structured output back into the workflow's stage report types.
////
//// Activities that have no Meridian worker (provision, land, enrich,
//// assemble, warm_build) remain as `workflow.run(activities.xxx(...))` in
//// the calling workflow code — they still use the local implementations.

import aion/error
import aion_stacked_dev_io as stage_io
import gleam/dynamic/decode
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import meridian/agent
import meridian/check
import stacked_dev/aion_dispatch
import stacked_dev/codecs_brief
import stacked_dev/enrich
import stacked_dev/schemas
import stacked_dev/types.{
  type AssembleInput, type AssembledWave, type BriefDocument, type BuildWarm,
  type CheckResult, type EnrichInput, type LandInput, type ProvisionInput,
  type ReviewAck, type ReviewRequest, type Workspace, BuildWarm, CheckFail,
  CheckPass, CheckResult, Landed, ReviewAck, Workspace,
}

/// Run the read-only scout agent via Meridian's embedded Norn runtime.
pub fn run_scout(
  workspace: Workspace,
  workspace_id: String,
  prompt: String,
) -> Result(stage_io.ScoutReport, error.ActivityError) {
  let norn_input =
    build_norn_input(
      workspace,
      workspace_id,
      "norn-scout",
      prompt,
      None,
      Some(schemas.scout_output_schema),
    )
  use output <- result.try(dispatch_norn(norn_input, "scout"))
  parse_report(output.output, stage_io.scout_report_decoder(), "scout")
}

/// Run the dev agent via Meridian's embedded Norn runtime.
/// Returns the dev report and the session id for subsequent resume calls.
pub fn run_dev(
  workspace: Workspace,
  workspace_id: String,
  prompt: String,
) -> Result(#(stage_io.DevReport, String), error.ActivityError) {
  let norn_input =
    build_norn_input(
      workspace,
      workspace_id,
      "norn-developer",
      prompt,
      None,
      Some(schemas.dev_output_schema),
    )
  use output <- result.try(dispatch_norn(norn_input, "dev"))
  use report <- result.try(parse_report(
    output.output,
    stage_io.dev_report_decoder(),
    "dev",
  ))
  Ok(#(report, output.session_id))
}

/// Resume the dev agent session with feedback via Meridian's Norn runtime.
pub fn run_dev_resume(
  workspace: Workspace,
  workspace_id: String,
  session_id: String,
  feedback: String,
) -> Result(stage_io.DevReport, error.ActivityError) {
  let norn_input =
    build_norn_input(
      workspace,
      workspace_id,
      "developer",
      feedback,
      Some(session_id),
      Some(schemas.dev_output_schema),
    )
  use output <- result.try(dispatch_norn(norn_input, "dev_resume"))
  parse_report(output.output, stage_io.dev_report_decoder(), "dev_resume")
}

/// Run the adversarial reviewer via Meridian's embedded Norn runtime.
pub fn run_dev_review(
  workspace: Workspace,
  workspace_id: String,
  prompt: String,
) -> Result(stage_io.ReviewReport, error.ActivityError) {
  let norn_input =
    build_norn_input(
      workspace,
      workspace_id,
      "norn-reviewer",
      prompt,
      None,
      Some(schemas.review_output_schema),
    )
  use output <- result.try(dispatch_norn(norn_input, "dev_review"))
  parse_report(
    output.output,
    stage_io.review_report_decoder(),
    "dev_review",
  )
}

/// Run cargo fmt + clippy as scoped checks via Meridian's check workers.
pub fn run_scoped_checks(
  workspace: Workspace,
  packages: List(String),
) -> Result(CheckResult, error.ActivityError) {
  let package_args = build_package_args(packages)
  let check_input =
    check.CheckInput(worktree_path: workspace.path, extra_args: package_args)
  let input_json = check.encode_check_input(check_input)
  use fmt <- result.try(dispatch_check(
    check.cargo_fmt_activity_name,
    input_json,
    check.fmt_output_decoder(),
    "scoped_fmt",
  ))
  use clippy <- result.try(dispatch_check(
    check.cargo_clippy_activity_name,
    input_json,
    check.clippy_output_decoder(),
    "scoped_clippy",
  ))
  let scope = case packages {
    [] -> "workspace-wide fallback: no packages specified"
    _ -> "affected: " <> string.join(packages, ", ")
  }
  let verdict = case fmt.passed, clippy.passed {
    True, True -> CheckPass
    _, _ -> CheckFail(diagnostics: build_check_diagnostics(fmt, clippy))
  }
  Ok(CheckResult(
    verdict: verdict,
    affected_modules: packages,
    checked_scope: scope,
  ))
}

/// Run cargo fmt + clippy + test as the authoritative full gate via Meridian.
pub fn run_full_checks(
  workspace: Workspace,
) -> Result(types.GateResult, error.ActivityError) {
  let check_input =
    check.CheckInput(worktree_path: workspace.path, extra_args: [])
  let input_json = check.encode_check_input(check_input)
  use fmt <- result.try(dispatch_check(
    check.cargo_fmt_activity_name,
    input_json,
    check.fmt_output_decoder(),
    "gate_fmt",
  ))
  use clippy <- result.try(dispatch_check(
    check.cargo_clippy_activity_name,
    input_json,
    check.clippy_output_decoder(),
    "gate_clippy",
  ))
  use tests <- result.try(dispatch_check(
    check.cargo_test_activity_name,
    input_json,
    check.test_output_decoder(),
    "gate_test",
  ))
  case fmt.passed, clippy.passed, tests.failed == 0 {
    True, True, True -> Ok(types.GateResult(verdict: types.GatePass))
    _, _, _ ->
      Ok(types.GateResult(
        verdict: types.GateFail(report: build_gate_report(fmt, clippy, tests)),
      ))
  }
}

// --- internal helpers -------------------------------------------------------

fn build_norn_input(
  workspace: Workspace,
  workspace_id: String,
  profile: String,
  prompt: String,
  session_id: Option(String),
  output_schema: Option(String),
) -> agent.NornInput {
  agent.NornInput(
    profile: profile,
    profile_name: profile,
    prompt: prompt,
    tools: [],
    working_dir: workspace.path,
    workspace_id: workspace_id,
    session_id: session_id,
    workflow_id: None,
    activity_id: None,
    execution_id: None,
    step_number: None,
    visit: None,
    output_schema: output_schema,
  )
}

fn dispatch_norn(
  input: agent.NornInput,
  context: String,
) -> Result(agent.NornOutput, error.ActivityError) {
  let input_json = json.to_string(agent.encode_norn_input(input))
  use correlation_id <- result.try(
    aion_dispatch.dispatch_activity(
      agent.norn_activity_name,
      input_json,
      "{}",
    )
    |> result.map_error(fn(reason) {
      error.Terminal(context <> ": dispatch failed: " <> reason, "")
    }),
  )
  use output_json <- result.try(
    aion_dispatch.await_activity_result(correlation_id)
    |> result.map_error(fn(reason) {
      error.Terminal(context <> ": activity failed: " <> reason, "")
    }),
  )
  json.parse(output_json, agent.norn_output_decoder())
  |> result.map_error(fn(_) {
    error.Terminal(
      context <> ": failed to decode NornOutput from activity result",
      "",
    )
  })
}

fn parse_report(
  output: String,
  decoder: decode.Decoder(report),
  context: String,
) -> Result(report, error.ActivityError) {
  json.parse(output, decoder)
  |> result.map_error(fn(_) {
    error.Terminal(
      context
        <> ": agent output could not be parsed as the expected report shape",
      "",
    )
  })
}

fn dispatch_check(
  activity_name: String,
  input_json: json.Json,
  decoder: decode.Decoder(output),
  context: String,
) -> Result(output, error.ActivityError) {
  let serialized = json.to_string(input_json)
  use correlation_id <- result.try(
    aion_dispatch.dispatch_activity(activity_name, serialized, "{}")
    |> result.map_error(fn(reason) {
      error.Terminal(context <> ": dispatch failed: " <> reason, "")
    }),
  )
  use output_json <- result.try(
    aion_dispatch.await_activity_result(correlation_id)
    |> result.map_error(fn(reason) {
      error.Terminal(context <> ": activity failed: " <> reason, "")
    }),
  )
  json.parse(output_json, decoder)
  |> result.map_error(fn(_) {
    error.Terminal(
      context <> ": failed to decode check output from activity result",
      "",
    )
  })
}

fn build_package_args(packages: List(String)) -> List(String) {
  case packages {
    [] -> ["--workspace"]
    _ -> list.flat_map(packages, fn(name) { ["--package", name] })
  }
}

fn build_check_diagnostics(
  fmt: check.FmtOutput,
  clippy: check.ClippyOutput,
) -> String {
  let fmt_section = case fmt.passed {
    True -> ""
    False ->
      "cargo fmt: FAIL\n"
      <> fmt.stderr
      <> "\n"
      <> option.unwrap(fmt.diff, "")
  }
  let clippy_section = case clippy.passed {
    True -> ""
    False -> "cargo clippy: FAIL\n" <> clippy.stderr
  }
  string.join([fmt_section, clippy_section], "\n")
}

fn build_gate_report(
  fmt: check.FmtOutput,
  clippy: check.ClippyOutput,
  tests: check.TestOutput,
) -> String {
  string.join(
    [
      case fmt.passed {
        True -> "fmt: PASS"
        False ->
          "fmt: FAIL\n"
          <> fmt.stderr
          <> "\n"
          <> option.unwrap(fmt.diff, "")
      },
      case clippy.passed {
        True -> "clippy: PASS"
        False -> "clippy: FAIL\n" <> clippy.stderr
      },
      case tests.failed {
        0 -> "test: PASS (" <> int.to_string(tests.passed) <> " passed)"
        n ->
          "test: FAIL ("
          <> int.to_string(n)
          <> " failed)\n"
          <> tests.stderr
      },
    ],
    "\n\n",
  )
}

// --- shell-based activity dispatch -------------------------------------------

/// Run a shell command through Meridian's shell.command worker.
fn run_shell(
  command: String,
  args: List(String),
  cwd: String,
  context: String,
) -> Result(ShellResult, error.ActivityError) {
  let input_json =
    json.to_string(json.object([
      #("command", json.string(command)),
      #("args", json.array(args, json.string)),
      #("cwd", json.string(cwd)),
    ]))
  use correlation_id <- result.try(
    aion_dispatch.dispatch_activity("meridian.shell.command", input_json, "{}")
    |> result.map_error(fn(reason) {
      error.Terminal(context <> ": dispatch failed: " <> reason, "")
    }),
  )
  use output_json <- result.try(
    aion_dispatch.await_activity_result(correlation_id)
    |> result.map_error(fn(reason) {
      error.Terminal(context <> ": activity failed: " <> reason, "")
    }),
  )
  json.parse(output_json, shell_result_decoder())
  |> result.map_error(fn(_) {
    error.Terminal(context <> ": failed to decode shell result", "")
  })
}

type ShellResult {
  ShellResult(exit_code: Int, stdout: String, stderr: String, duration_ms: Int)
}

fn shell_result_decoder() -> decode.Decoder(ShellResult) {
  use exit_code <- decode.field("exit_code", decode.int)
  use stdout <- decode.field("stdout", decode.string)
  use stderr <- decode.field("stderr", decode.string)
  use duration_ms <- decode.field("duration_ms", decode.int)
  decode.success(ShellResult(
    exit_code: exit_code,
    stdout: stdout,
    stderr: stderr,
    duration_ms: duration_ms,
  ))
}

fn require_success(
  shell: ShellResult,
  context: String,
) -> Result(ShellResult, error.ActivityError) {
  case shell.exit_code {
    0 -> Ok(shell)
    code ->
      Error(error.Terminal(
        context
          <> " exit "
          <> int.to_string(code)
          <> ": "
          <> string.trim(shell.stderr <> "\n" <> shell.stdout),
        "",
      ))
  }
}

/// Provision a workspace via yg CLI through the shell worker.
pub fn run_provision(
  input: ProvisionInput,
) -> Result(Workspace, error.ActivityError) {
  let branch = "stacked-dev-" <> input.brief_id
  let worktree_path =
    input.repo_root <> "/.yggdrasil-worktrees/" <> branch
  use _ <- result.try(
    run_shell(
      "yg",
      ["branch", "add", branch, input.base_ref],
      input.repo_root,
      "yg branch add",
    )
    |> result.try(require_success(_, "yg branch add")),
  )
  use _ <- result.try(
    run_shell(
      "yg",
      ["branch", "provision", branch, "--path", worktree_path],
      input.repo_root,
      "yg branch provision",
    )
    |> result.try(require_success(_, "yg branch provision")),
  )
  Ok(Workspace(
    path: worktree_path,
    branch: branch,
    placement: input.placement,
    isolation: input.isolation,
  ))
}

/// Advisory warm build via shell worker. Failure forfeits the cache.
pub fn run_warm_build(
  workspace: Workspace,
) -> BuildWarm {
  case run_shell("cargo", ["build"], workspace.path, "cargo build") {
    Ok(shell) ->
      BuildWarm(ok: shell.exit_code == 0, duration_ms: shell.duration_ms)
    Error(_) -> BuildWarm(ok: False, duration_ms: 0)
  }
}

/// Request review via meridian CLI through the shell worker.
pub fn run_request_review(
  input: ReviewRequest,
) -> Result(ReviewAck, error.ActivityError) {
  let reviewer_args =
    list.flat_map(input.reviewers, fn(reviewer) { ["--reviewer", reviewer] })
  use shell <- result.try(
    run_shell(
      "meridian",
      list.flatten([
        ["review", "request", input.workspace.branch],
        reviewer_args,
        ["--as", "Meridian"],
      ]),
      input.workspace.path,
      "meridian review request",
    )
    |> result.try(require_success(_, "meridian review request")),
  )
  Ok(ReviewAck(request_id: input.workspace.branch))
}

/// Enrich a brief file via shell worker (read file, merge in Gleam is not
/// possible without file:read_file -- so we use the shell to cat the file,
/// do the merge in workflow code, then write it back).
pub fn run_enrich_brief(
  input: EnrichInput,
) -> Result(BriefDocument, error.ActivityError) {
  let brief_codec = codecs_brief.brief_document_codec()
  let brief_path =
    input.workspace.path
    <> "/docs/design/"
    <> input.document.cluster
    <> "/briefs/"
    <> input.document.id
    <> ".json"
  use read_shell <- result.try(
    run_shell("cat", [brief_path], input.workspace.path, "read brief")
    |> result.try(require_success(_, "read brief")),
  )
  use on_disk <- result.try(
    brief_codec.decode(string.trim(read_shell.stdout))
    |> result.map_error(fn(_) {
      error.Terminal("enrich_brief: brief file failed to decode", "")
    }),
  )
  case enrich.authored_divergence(on_disk, input.document) {
    Some(field) ->
      Error(error.Terminal(
        "enrich_brief: authored field "
        <> field
        <> " diverges from the handed document",
        "",
      ))
    None -> {
      use merged <- result.try(apply_enrichment(input))
      let content = brief_codec.encode(merged)
      use _ <- result.try(
        run_shell(
          "sh",
          ["-c", "cat > " <> brief_path],
          input.workspace.path,
          "write brief",
        )
        |> result.map_error(fn(reason) {
          error.Terminal(
            "enrich_brief: write failed: " <> string.inspect(reason),
            "",
          )
        }),
      )
      // The shell write via stdin won't work with our dispatch model.
      // Use tee instead to write content to file.
      use _ <- result.try(
        run_shell(
          "sh",
          ["-c", "printf '%s' '" <> escape_single_quotes(content) <> "' > " <> brief_path],
          input.workspace.path,
          "write brief",
        )
        |> result.try(require_success(_, "write brief")),
      )
      Ok(merged)
    }
  }
}

fn apply_enrichment(
  input: EnrichInput,
) -> Result(BriefDocument, error.ActivityError) {
  let result = case input.enrichment {
    types.ScoutEnrichment(report: report) ->
      enrich.merge_scout(input.document, report)
    types.DevEnrichment(report: report) ->
      enrich.merge_dev(input.document, report)
    types.ReviewEnrichment(report: report) ->
      enrich.merge_review(input.document, report)
    types.ExecutionEnrichment(block: block) ->
      enrich.merge_execution(input.document, block)
  }
  result
  |> result.map_error(fn(enrich_error) {
    error.Terminal("enrich_brief: " <> enrich.describe(enrich_error), "")
  })
}

fn escape_single_quotes(s: String) -> String {
  string.replace(s, "'", "'\\''")
}

/// Land the work: git add, git commit, yg branch merge.
pub fn run_land(input: LandInput) -> Result(types.Landed, error.ActivityError) {
  use _ <- result.try(
    run_shell("git", ["add", "-A"], input.workspace.path, "git add")
    |> result.try(require_success(_, "git add")),
  )
  let commit_msg = input.workspace.branch <> ": " <> input.dev_result.summary
  use _ <- result.try(
    run_shell(
      "git",
      ["commit", "-m", commit_msg],
      input.workspace.path,
      "git commit",
    )
    |> result.try(require_success(_, "git commit")),
  )
  use _ <- result.try(
    run_shell(
      "yg",
      ["branch", "merge", input.workspace.branch, "--yes"],
      input.repo_root,
      "yg branch merge",
    )
    |> result.try(require_success(_, "yg branch merge")),
  )
  Ok(Landed(branch: input.workspace.branch, merged_into: input.base_ref))
}

/// Assemble a dispatch wave: read design docs and resolve references.
pub fn run_assemble_wave(
  input: AssembleInput,
) -> Result(AssembledWave, error.ActivityError) {
  // The assemble logic reads files and resolves references. Since we can't
  // do file I/O in Beamr, we shell out to a Python script that does the
  // resolution and returns the assembled wave as JSON.
  // For now, fall back to reading each file via shell and doing resolution
  // in the workflow. This is complex -- for the first run, we'll read the
  // brief JSON via shell and build a minimal resolved context.
  Error(error.Terminal(
    "assemble_wave via shell dispatch is not yet implemented -- "
    <> "dispatch the stacked_dev workflow directly with a pre-assembled input",
    "",
  ))
}
