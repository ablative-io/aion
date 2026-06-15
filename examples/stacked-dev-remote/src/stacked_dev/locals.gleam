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

import aion/codec
import aion/error
import aion_stacked_dev_io as stage_io
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option
import gleam/string
import stacked_dev/assemble
import stacked_dev/cli
import stacked_dev/codecs_brief
import stacked_dev/codecs_core
import stacked_dev/enrich
import stacked_dev/types.{
  type AssembleInput, type AssembledWave, type BriefDocument, type CheckResult,
  type DevInput, type EnrichInput, type GateInput, type GateResult,
  type LandInput, type Landed, type ProvisionInput, type ResumeInput,
  type ReviewAck, type ReviewInput, type ReviewRequest, type ScopedInput,
  type ScoutInput, type StartupResult, type StartupTask, type TeardownInput,
  type TornDown, type Workspace, AffectedClosure, BuildWarm, CheckFail,
  CheckPass, CheckResult, Copy, DevEnrichment, DevTask, Developed,
  ExecutionEnrichment, GateFail, GatePass, GateResult, Landed, Overlay,
  ReviewAck, ReviewEnrichment, ScoutEnrichment, TornDown, Vm, WarmTask, Warmed,
  Workspace, WorkspaceWide, Worktree,
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

/// Run the read-only scout agent in its own deterministic session
/// (`<branch>-scout`, CN4) via the `norn` CLI. The projected scout prompt
/// rides positionally; the output validates against the scout-report schema.
pub fn scout(
  input: ScoutInput,
) -> Result(stage_io.ScoutReport, error.ActivityError) {
  let session_id = input.workspace.branch <> "-scout"
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
        scout_output_schema,
        "--output-format",
        "json",
        input.prompt,
      ],
      input.workspace.path,
    ),
    "norn scout",
  )
  require_report(
    command_run,
    "norn scout",
    stage_io.scout_report_decoder(),
    codecs_core.report_envelope_decoder(stage_io.scout_report_decoder()),
  )
}

/// Run the dev agent against the projected dev prompt via the `norn` CLI.
fn dev(input: DevInput) -> Result(StartupResult, error.ActivityError) {
  // The session id is deterministic (the branch name), so resume rounds target
  // the same session without ever capturing a generated id. norn validates the
  // charset; "stacked-dev-<brief>" is legal.
  let session_id = input.workspace.branch

  // norn takes the projected prompt positionally; --print is headless,
  // --session-id mints exactly this id, --output-schema constrains the
  // structured result to the dev-report shape, and --output-format json emits
  // the final envelope we decode.
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
        dev_output_schema,
        "--output-format",
        "json",
        input.prompt,
      ],
      input.workspace.path,
    ),
    "norn dev",
  )
  case
    require_report(
      command_run,
      "norn dev",
      stage_io.dev_report_decoder(),
      codecs_core.report_envelope_decoder(stage_io.dev_report_decoder()),
    )
  {
    Ok(dev_report) -> Ok(Developed(dev_report: dev_report))
    Error(activity_error) -> Error(activity_error)
  }
}

/// Run the adversarial reviewer agent in its own deterministic session
/// (`<branch>-review` — NEVER the dev session, CN4) via the `norn` CLI. The
/// projected review prompt rides positionally; the output validates against
/// the review-report schema.
pub fn dev_review(
  input: ReviewInput,
) -> Result(stage_io.ReviewReport, error.ActivityError) {
  let session_id = input.workspace.branch <> "-review"
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
        review_output_schema,
        "--output-format",
        "json",
        input.prompt,
      ],
      input.workspace.path,
    ),
    "norn review",
  )
  require_report(
    command_run,
    "norn review",
    stage_io.review_report_decoder(),
    codecs_core.report_envelope_decoder(stage_io.review_report_decoder()),
  )
}

/// Resume the same dev agent session with feedback (scoped-check diagnostics).
/// Returns a FULL replacement dev report against the dev-report schema.
pub fn dev_resume(
  input: ResumeInput,
) -> Result(stage_io.DevReport, error.ActivityError) {
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
        dev_output_schema,
        "--output-format",
        "json",
        input.feedback,
      ],
      ".",
    ),
    "norn resume",
  )
  require_report(
    command_run,
    "norn resume",
    stage_io.dev_report_decoder(),
    codecs_core.report_envelope_decoder(stage_io.dev_report_decoder()),
  )
}

/// The scout-report stage-contract schema, inline for `--output-schema`. A
/// Gleam string constant pinned structurally to `schemas/scout_report.json`
/// (which the drift gate pins byte-for-byte to the design-system canon, CN7)
/// by `test/stage_schema_test.gleam`.
pub const scout_output_schema = "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"title\":\"Scout Report\",\"description\":\"Structured output contract for the scout stage: read-only codebase exploration that gathers implementation context per requirement. The enrichments entries are appended in place to the brief's requirements as their scout blocks.\",\"type\":\"object\",\"required\":[\"summary\",\"enrichments\",\"verification\"],\"additionalProperties\":false,\"properties\":{\"summary\":{\"type\":\"string\",\"description\":\"2-3 sentences orienting the implementer\"},\"enrichments\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"id\",\"files\",\"context\",\"approach\",\"notes\"],\"additionalProperties\":false,\"properties\":{\"id\":{\"type\":\"string\",\"pattern\":\"^R\\\\d+$\",\"description\":\"R# id from the brief — one entry per requirement, no omissions\"},\"files\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Key files relevant to this R# (path:line-range — brief note). 2-5 per R#, chosen to save the implementer time, not to catalogue.\"},\"context\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Key findings: conventions to match, type signatures, gotchas. 2-4 per R#.\"},\"approach\":{\"type\":\"string\",\"description\":\"How to implement this R# — one paragraph, concrete\"},\"notes\":{\"type\":\"string\",\"description\":\"Anything non-obvious the brief might not have considered: edge cases, integration gotchas. Empty string if none.\"}}}},\"verification\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Concrete checks to run after implementation, discovered during exploration\"}}}"

/// The dev-report stage-contract schema, inline for `--output-schema`. A Gleam
/// string constant pinned structurally to `schemas/dev_report.json` (which the
/// drift gate pins byte-for-byte to the design-system canon, CN7) by
/// `test/stage_schema_test.gleam`.
pub const dev_output_schema = "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"title\":\"Dev Report\",\"description\":\"Structured output contract for the dev stage: implementation of every requirement in the brief. The enrichments entries are appended in place to the brief's requirements as their dev blocks. The attestation records what the agent BELIEVES — the workflow runs the real gate afterwards and stores both; the attestation is never trusted as the gate.\",\"type\":\"object\",\"required\":[\"summary\",\"commit_message\",\"enrichments\",\"attestation\"],\"additionalProperties\":false,\"properties\":{\"summary\":{\"type\":\"string\",\"description\":\"1-2 sentences on what was done\"},\"commit_message\":{\"type\":\"string\",\"description\":\"Conventional-commits style message for this round's commit\"},\"enrichments\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"id\",\"status\",\"files_changed\",\"how\",\"deviation\",\"checklist\",\"stories\"],\"additionalProperties\":false,\"properties\":{\"id\":{\"type\":\"string\",\"pattern\":\"^R\\\\d+$\",\"description\":\"R# id — one entry per requirement, blocked ones included with status blocked\"},\"status\":{\"type\":\"string\",\"enum\":[\"implemented\",\"blocked\"]},\"files_changed\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"path\",\"change\",\"note\"],\"additionalProperties\":false,\"properties\":{\"path\":{\"type\":\"string\"},\"change\":{\"type\":\"string\",\"enum\":[\"created\",\"modified\",\"deleted\"]},\"note\":{\"type\":\"string\"}}}},\"how\":{\"type\":\"string\",\"description\":\"How this requirement was met — the rationale and shape of the change, not a diff narration\"},\"deviation\":{\"type\":\"string\",\"description\":\"Empty if the scouted plan was followed; otherwise what changed and why — silent deviation is a review finding\"},\"checklist\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"id\",\"done\",\"note\"],\"additionalProperties\":false,\"properties\":{\"id\":{\"type\":\"string\",\"pattern\":\"^C\\\\d+$\"},\"done\":{\"type\":\"boolean\"},\"note\":{\"type\":\"string\"}}},\"description\":\"Delivery claim per C# assigned to this R#\"},\"stories\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"id\",\"satisfied\",\"note\"],\"additionalProperties\":false,\"properties\":{\"id\":{\"type\":\"string\",\"pattern\":\"^S\\\\d+$\"},\"satisfied\":{\"type\":\"boolean\"},\"note\":{\"type\":\"string\"}}},\"description\":\"Satisfaction claim per S# assigned to this R#\"}}}},\"attestation\":{\"type\":\"object\",\"required\":[\"no_panics\",\"no_unsafe\",\"boundaries_respected\",\"tests_pass\"],\"additionalProperties\":false,\"properties\":{\"no_panics\":{\"type\":\"boolean\",\"description\":\"No unwrap/expect/panic/todo in library code\"},\"no_unsafe\":{\"type\":\"boolean\",\"description\":\"No unsafe blocks added\"},\"boundaries_respected\":{\"type\":\"boolean\",\"description\":\"All SHALL NOT boundaries observed\"},\"tests_pass\":{\"type\":\"boolean\",\"description\":\"The agent's belief — the workflow measures the truth at the gate\"}}}}}"

/// The review-report stage-contract schema, inline for `--output-schema`. A
/// Gleam string constant pinned structurally to `schemas/review_report.json`
/// (which the drift gate pins byte-for-byte to the design-system canon, CN7)
/// by `test/stage_schema_test.gleam`.
pub const review_output_schema = "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"title\":\"Review Report\",\"description\":\"Structured output contract for the adversarial review stage. The reviewer verifies the ACTUAL DIFF against each acceptance criterion — never the dev's claims — then hardens. Verdict and fixes are both recorded; there is no severity taxonomy because there are no minor issues: everything found is fixed or named as an issue. The enrichments entries are appended in place to the brief's requirements as their review blocks.\",\"type\":\"object\",\"required\":[\"summary\",\"commit_message\",\"enrichments\",\"verification\"],\"additionalProperties\":false,\"properties\":{\"summary\":{\"type\":\"string\",\"description\":\"The honest overall read — what is solid, what was found, what was fixed\"},\"commit_message\":{\"type\":\"string\",\"description\":\"Conventional-commits style message for the harden commit; empty string when the harden pass changed nothing\"},\"enrichments\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"id\",\"alignment\",\"acceptance\",\"checklist\",\"stories\",\"issues\",\"fixes\"],\"additionalProperties\":false,\"properties\":{\"id\":{\"type\":\"string\",\"pattern\":\"^R\\\\d+$\",\"description\":\"R# id — one entry per requirement\"},\"alignment\":{\"type\":\"string\",\"enum\":[\"aligned\",\"drifted\",\"fixed\"],\"description\":\"aligned = implementation matches spec; drifted = it does not and remains so (a failing state); fixed = it drifted and the harden pass corrected it\"},\"acceptance\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"criterion\",\"met\",\"evidence\"],\"additionalProperties\":false,\"properties\":{\"criterion\":{\"type\":\"string\",\"description\":\"The acceptance criterion verbatim from the brief\"},\"met\":{\"type\":\"boolean\"},\"evidence\":{\"type\":\"string\",\"description\":\"What in the diff proves it: file:line, test name, command output — not the dev report\"}}},\"description\":\"One verdict per acceptance criterion. A single boolean for the whole requirement is not a review.\"},\"checklist\":{\"type\":\"array\",\"items\":{\"type\":\"string\",\"pattern\":\"^C\\\\d+$\"},\"description\":\"C-numbers VERIFIED delivered (verification flips done in the cluster checklist, not the dev's claim)\"},\"stories\":{\"type\":\"array\",\"items\":{\"type\":\"string\",\"pattern\":\"^S\\\\d+$\"},\"description\":\"S-numbers verified satisfied\"},\"issues\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"Everything found, fixed or not — an issue that was fixed still gets recorded here with its fix below\"},\"fixes\":{\"type\":\"array\",\"items\":{\"type\":\"string\"},\"description\":\"What the harden pass changed\"}}}},\"verification\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"criterion\",\"passed\",\"note\"],\"additionalProperties\":false,\"properties\":{\"criterion\":{\"type\":\"string\",\"description\":\"Brief-level verification step, verbatim\"},\"passed\":{\"type\":\"boolean\"},\"note\":{\"type\":\"string\"}}},\"description\":\"The brief's cross-cutting verification steps, each actually executed\"}}}"

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
  // CONFIRMED against the real CLI (live run, 2026-06-13): the response
  // envelope is `{"branch": .., "reviewers": [{"name", "dm_status", ..}]}`
  // — there is no request id. The branch IS the request's identity
  // (meridian persists `pending_reviewers` against the branch lifecycle),
  // so the recorded ack carries it. Every requested reviewer must have
  // been notified (`dm_status: "sent"`); anything else fails loudly.
  use response <- require_json(command_run, "meridian review request", {
    use branch <- decode.field("branch", decode.string)
    use reviewers <- decode.field(
      "reviewers",
      decode.list({
        use name <- decode.field("name", decode.string)
        use dm_status <- decode.field("dm_status", decode.string)
        decode.success(#(name, dm_status))
      }),
    )
    decode.success(#(branch, reviewers))
  })
  let #(branch, reviewers) = response
  case list.find(reviewers, fn(reviewer) { reviewer.1 != "sent" }) {
    Ok(#(name, dm_status)) ->
      Error(error.terminal(
        "meridian review request did not notify reviewer "
        <> name
        <> ": dm_status was "
        <> dm_status,
      ))
    Error(Nil) -> Ok(ReviewAck(request_id: branch))
  }
}

/// Land the approved work: commit the dev rounds' files on the branch, then
/// `yg branch merge` into the tree parent. Never a manual cherry-pick or
/// merge.
pub fn land(input: LandInput) -> Result(Landed, error.ActivityError) {
  // Confirmed live (2026-06-13): the dev rounds leave norn's work
  // UNCOMMITTED in the worktree, and `yg branch merge` merges committed
  // work only — so landing commits first. A dev round that changed nothing
  // fails loudly here ("nothing to commit"): landing a no-op is an error,
  // never a silent empty merge.
  use _staged <- require_run(
    cli.run("git", ["add", "-A"], input.workspace.path),
    "git add",
  )
  use _committed <- require_run(
    cli.run(
      "git",
      [
        "commit",
        "-m",
        input.workspace.branch <> ": " <> input.dev_result.summary,
      ],
      input.workspace.path,
    ),
    "git commit",
  )
  // Also confirmed live: the merge removes the branch's worktree as part of
  // landing, so it MUST run from the main repository — run from inside the
  // worktree it deletes its own git context mid-merge and dies.
  use _merged <- require_run(
    cli.run(
      "yg",
      ["branch", "merge", input.workspace.branch, "--yes"],
      input.repo_root,
    ),
    "yg branch merge",
  )
  Ok(Landed(branch: input.workspace.branch, merged_into: input.base_ref))
}

pub fn teardown_workspace(
  input: TeardownInput,
) -> Result(TornDown, error.ActivityError) {
  let branch = input.workspace.branch
  let _ =
    cli.run("yg", ["branch", "teardown", branch], input.repo_root)
  let _ =
    cli.run("yg", ["branch", "remove", "--yes", branch], input.repo_root)
  Ok(TornDown(branch: branch, cleaned: True))
}

/// Append one stage report or the execution block into the brief file inside
/// the run's worktree (ADR-007: one living document; ADR-009: enrichment
/// rides the branch and lands with the merge).
///
/// The write is guarded by CN3: the on-disk brief's authored subset must
/// equal the handed document's before anything is written — divergence is a
/// Terminal failure naming the brief path and the first divergent field,
/// never a silent overwrite. A missing, unreadable, or undecodable brief
/// file is a broken worktree: a can't-execute condition that fails terminally
/// (CN5), never a retry or a skip.
pub fn enrich_brief(
  input: EnrichInput,
) -> Result(BriefDocument, error.ActivityError) {
  let brief_codec = codecs_brief.brief_document_codec()
  // The design-system layout is a format constraint (briefs/ is what
  // validate.py keys its brief-schema detection on), so the path derives
  // from the handed document — never from a workflow-supplied guess.
  let brief_path =
    input.workspace.path
    <> "/docs/design/"
    <> input.document.cluster
    <> "/briefs/"
    <> input.document.id
    <> ".json"
  use raw <- require_brief_read(brief_path)
  use on_disk <- require_brief_decode(brief_codec, raw, brief_path)
  case enrich.authored_divergence(on_disk, input.document) {
    option.Some(field) ->
      Error(error.terminal(
        "enrich_brief: authored field "
        <> field
        <> " in "
        <> brief_path
        <> " diverges from the handed document; refusing to write (CN3)",
      ))
    option.None ->
      case apply_enrichment(input) {
        Error(enrich_error) ->
          Error(error.terminal(
            "enrich_brief: " <> enrich.describe(enrich_error),
          ))
        Ok(merged) ->
          case write_text_file(brief_path, brief_codec.encode(merged)) {
            Ok(Nil) -> Ok(merged)
            Error(reason) ->
              Error(error.terminal(
                "enrich_brief: cannot write " <> brief_path <> ": " <> reason,
              ))
          }
      }
  }
}

/// Apply the merge selected by the `Enrichment` variant to the handed
/// document. The execution block is written exactly as given — gate results
/// and attestation stay separate fields (P1).
fn apply_enrichment(
  input: EnrichInput,
) -> Result(BriefDocument, enrich.EnrichError) {
  case input.enrichment {
    ScoutEnrichment(report: report) ->
      enrich.merge_scout(input.document, report)
    DevEnrichment(report: report) -> enrich.merge_dev(input.document, report)
    ReviewEnrichment(report: report) ->
      enrich.merge_review(input.document, report)
    ExecutionEnrichment(block: block) ->
      enrich.merge_execution(input.document, block)
  }
}

fn require_brief_read(
  brief_path: String,
  next: fn(String) -> Result(value, error.ActivityError),
) -> Result(value, error.ActivityError) {
  case read_text_file(brief_path) {
    Ok(raw) -> next(raw)
    Error(reason) ->
      Error(error.terminal(
        "enrich_brief: cannot read " <> brief_path <> ": " <> reason,
      ))
  }
}

fn require_brief_decode(
  brief_codec: codec.Codec(BriefDocument),
  raw: String,
  brief_path: String,
  next: fn(BriefDocument) -> Result(value, error.ActivityError),
) -> Result(value, error.ActivityError) {
  case brief_codec.decode(raw) {
    Ok(document) -> next(document)
    Error(codec.DecodeError(reason: reason, path: field_path)) ->
      Error(error.terminal(
        "enrich_brief: brief file "
        <> brief_path
        <> " failed to decode at "
        <> string.join(field_path, "/")
        <> ": "
        <> reason,
      ))
  }
}

/// `assemble_wave`: resolve, order, and refuse a dispatch wave. The heavy
/// resolution lives in `stacked_dev/assemble` (the only ledger-reading,
/// reference-resolving code in the family, CN1); this wrapper lifts a refusal
/// or can't-execute diagnostic into a terminal activity failure (CN5).
pub fn assemble_wave(
  input: AssembleInput,
) -> Result(AssembledWave, error.ActivityError) {
  case assemble.run(input) {
    Ok(wave) -> Ok(wave)
    Error(message) -> Error(error.terminal(message))
  }
}

@external(erlang, "stacked_dev_file_ffi", "read_file")
fn read_text_file(path: String) -> Result(String, String)

@external(erlang, "stacked_dev_file_ffi", "write_file")
fn write_text_file(path: String, contents: String) -> Result(Nil, String)

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

/// Decode a norn command's stdout as a stage report, generic over the
/// report's inner decoder.
///
/// CONFIRMED against real norn (live run, 2026-06-13): `--output-format json`
/// emits a completion envelope with the schema-constrained result under
/// `"output"` (alongside `usage`/`model`/`events`, ignored here). Exactly two
/// shapes are accepted — the bare report (the fake-CLI shims emit it raw) and
/// the real `{"output": <report>}` envelope — and nothing else (C31). A third
/// shape fails terminally naming both accepted shapes.
fn require_report(
  command_run: cli.CliRun,
  context: String,
  bare: decode.Decoder(report),
  envelope: decode.Decoder(report),
) -> Result(report, error.ActivityError) {
  let trimmed = string.trim(command_run.output)
  case json.parse(trimmed, bare) {
    Ok(report) -> Ok(report)
    Error(_) ->
      case json.parse(trimmed, envelope) {
        Ok(report) -> Ok(report)
        Error(_) ->
          Error(error.terminal(
            context
            <> " produced unparseable output (tried the bare report shape"
            <> " and norn's {\"output\": ...} envelope): "
            <> trimmed,
          ))
      }
  }
}
