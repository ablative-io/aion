//// The brief_dev child workflow: the v2 all-norn pipeline as a durable
//// workflow (ADR-008 — the inner child of the stacked-dev family).
////
//// Stage flow (BD-003):
////
//// 1. `scout` — a read-only norn round in its own session (`<branch>-scout`,
////    CN4) that orients the dev. Its report rides the result.
//// 2. `workflow.all([warm_build, dev])` — the build cache warms while the dev
////    agent works, via the existing tagged StartupTask/StartupResult
////    envelope. `dev` returns a dev report; a blocked requirement fails the
////    run typed.
//// 3. The bounded verify-fix loop: `scoped_checks` over the latest report's
////    changed files; on `CheckFail` a durable `round_backoff_ms` sleep and a
////    `dev_resume` carrying the projected diagnostics feedback (a FULL
////    replacement report), bounded by `verify_fix_cap`.
//// 4. `dev_review` — an adversarial norn round in its own session
////    (`<branch>-review`, NEVER the dev session, CN4): the reviewer sees the
////    dev's attestation AND the measured check result (P1, S10).
//// 5. Harden: any requirement left `drifted` fails the run typed; if review
////    fixes are non-empty, the scoped checks re-run and a regression fails
////    the run typed (C15).
////
//// Every cap and backoff is a required input (ADR-001/ADR-003, CN2); this
//// module bakes no defaults and dispatches only scout, warm_build, dev,
//// scoped_checks, dev_resume, and dev_review. It performs no filesystem
//// access and writes no brief file (CN1/P5 — enrichment is BD-004's).

import aion/codec
import aion/duration
import aion/query
import aion/workflow
import aion_stacked_dev_io as stage_io
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/list
import stacked_dev/activities
import stacked_dev/codecs_workflows
import stacked_dev/errors
import stacked_dev/prompts
import stacked_dev/types.{
  type BriefDevError, type BriefDevInput, type BriefDevResult, type BuildWarm,
  type CheckResult, type DriftedRequirement, BriefDevResult, BriefDevStageFailed,
  BriefDevStatus, CheckFail, CheckPass, CheckResult, DevBlocked, DevInput,
  Developed, DriftedRequirement, HardenRegressed, ResumeInput, ReviewDrifted,
  ReviewInput, ScopedInput, ScoutFailed, ScoutInput, VerifyFixExhausted, Warmed,
}

/// The child workflow type the parent passes to `workflow.spawn_and_wait`.
/// A deployed workflow type is its entry module name, so this is exactly
/// this module's name.
pub const workflow_type = "brief_dev"

/// Name of the live `{phase, round}` status query this workflow answers.
pub const status_query_name = "brief_dev_status"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  BriefDevInput,
  BriefDevResult,
  BriefDevError,
) {
  workflow.define(
    "brief-dev",
    codecs_workflows.brief_dev_input_codec(),
    codecs_workflows.brief_dev_result_codec(),
    codecs_workflows.brief_dev_error_codec(),
    execute,
  )
}

/// Engine entry point for one child execution.
///
/// The runtime delivers the start input as a raw JSON string. Success and
/// failure are both encoded back to JSON text here: the engine records these
/// exact payloads as the child terminal, and the awaiting parent decodes
/// them with the same codecs `stacked_dev/codecs_workflows` exports.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs_workflows.brief_dev_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) ->
              Ok(codecs_workflows.brief_dev_result_codec().encode(output))
            Error(brief_dev_error) ->
              Error(codecs_workflows.brief_dev_error_codec().encode(
                brief_dev_error,
              ))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            codecs_workflows.brief_dev_error_codec().encode(BriefDevStageFailed(
              stage: "decode_input",
              message: "failed to decode brief-dev input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        codecs_workflows.brief_dev_error_codec().encode(BriefDevStageFailed(
          stage: "decode_input",
          message: "brief-dev input payload was not a string",
        )),
      )
  }
}

/// Typed workflow body: scout, concurrent start-up, the bounded verify-fix
/// loop, adversarial review, and the harden re-verify.
pub fn execute(input: BriefDevInput) -> Result(BriefDevResult, BriefDevError) {
  use _ <- result_try(set_status("scouting", 0))
  use scout_report <- result_try(run_scout(input))
  use _ <- result_try(set_status("developing", 0))
  use #(build_warm, dev_report) <- result_try(run_startup(input, scout_report))
  use _ <- result_try(blocked_requirements(dev_report))
  verify_loop(input, scout_report, build_warm, dev_report, 1)
}

/// The read-only scout stage in its own session (`<branch>-scout`).
fn run_scout(
  input: BriefDevInput,
) -> Result(stage_io.ScoutReport, BriefDevError) {
  case
    workflow.run(
      activities.scout(ScoutInput(
        workspace: input.workspace,
        prompt: prompts.scout_prompt(input.document, input.context),
      )),
    )
  {
    Ok(scout_report) -> Ok(scout_report)
    Error(activity_error) ->
      Error(ScoutFailed(message: errors.activity_message(activity_error)))
  }
}

/// The concurrent warm-build + dev fan-out via `workflow.all`. Both share the
/// StartupTask/StartupResult envelope, so results come back in input order; a
/// shape violation is `BriefDevStageFailed(stage: "startup")` (C12).
fn run_startup(
  input: BriefDevInput,
  scout_report: stage_io.ScoutReport,
) -> Result(#(BuildWarm, stage_io.DevReport), BriefDevError) {
  case
    workflow.all([
      activities.warm_build(input.workspace),
      activities.dev(DevInput(
        workspace: input.workspace,
        prompt: prompts.dev_prompt(input.document, input.context, scout_report),
      )),
    ])
  {
    Ok([Warmed(build_warm: build_warm), Developed(dev_report: dev_report)]) ->
      Ok(#(build_warm, dev_report))
    Ok(_) ->
      Error(BriefDevStageFailed(
        stage: "startup",
        message: "startup fan-out settled with a result shape other than"
          <> " [warm_build, dev] — envelope contract violation",
      ))
    Error(activity_error) ->
      Error(BriefDevStageFailed(
        stage: "startup",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// Fail with `DevBlocked` listing the R# ids of every enrichment the dev
/// report marked `blocked` (C18); otherwise proceed.
fn blocked_requirements(
  dev_report: stage_io.DevReport,
) -> Result(Nil, BriefDevError) {
  let blocked =
    dev_report.enrichments
    |> list.filter(fn(entry) {
      entry.status == stage_io.DevReportEnrichmentsItemStatusBlocked
    })
    |> list.map(fn(entry) { entry.id })
  case blocked {
    [] -> Ok(Nil)
    ids -> Error(DevBlocked(requirement_ids: ids))
  }
}

/// One bounded verify-fix round: scoped checks over the latest report's
/// deduplicated changed files; on convergence proceed to review, on failure a
/// durable backoff, a session resume carrying the projected diagnostics
/// feedback, and recursion with the attempt counter.
fn verify_loop(
  input: BriefDevInput,
  scout_report: stage_io.ScoutReport,
  build_warm: BuildWarm,
  dev_report: stage_io.DevReport,
  round: Int,
) -> Result(BriefDevResult, BriefDevError) {
  use _ <- result_try(set_status("verifying", round))
  case run_scoped_checks(input, dev_report, round) {
    Ok(CheckResult(verdict: CheckPass, ..) as check) ->
      review_stage(input, scout_report, build_warm, dev_report, check, round)
    Ok(CheckResult(verdict: CheckFail(diagnostics: diagnostics), ..)) ->
      case round >= input.verify_fix_cap {
        True ->
          Error(VerifyFixExhausted(rounds: round, diagnostics: diagnostics))
        False -> fix_round(input, scout_report, build_warm, round, diagnostics)
      }
    Error(brief_dev_error) -> Error(brief_dev_error)
  }
}

/// One fix round: a durable backoff, then a `dev_resume` carrying the
/// projected feedback for a FULL replacement report, then recursion. The
/// previous report is intentionally discarded — the resume returns a complete
/// replacement (wholesale, never a partial merge).
fn fix_round(
  input: BriefDevInput,
  scout_report: stage_io.ScoutReport,
  build_warm: BuildWarm,
  round: Int,
  diagnostics: String,
) -> Result(BriefDevResult, BriefDevError) {
  use _ <- result_try(set_status("fixing", round))
  case workflow.sleep(duration.milliseconds(input.round_backoff_ms)) {
    Ok(Nil) ->
      case
        workflow.run(
          activities.dev_resume(ResumeInput(
            session_id: input.workspace.branch,
            feedback: prompts.resume_feedback(input.document, diagnostics),
          )),
        )
      {
        Ok(resumed) ->
          verify_loop(input, scout_report, build_warm, resumed, round + 1)
        Error(activity_error) ->
          Error(BriefDevStageFailed(
            stage: "dev_resume round " <> int.to_string(round),
            message: errors.activity_message(activity_error),
          ))
      }
    Error(engine_error) ->
      Error(BriefDevStageFailed(
        stage: "round_backoff round " <> int.to_string(round),
        message: errors.engine_message(engine_error),
      ))
  }
}

/// The adversarial review stage in its own session (`<branch>-review`): the
/// reviewer sees the attestation and the measured result (S10). Drift fails
/// the run BEFORE the harden re-verify; review fixes trigger the harden pass.
fn review_stage(
  input: BriefDevInput,
  scout_report: stage_io.ScoutReport,
  build_warm: BuildWarm,
  dev_report: stage_io.DevReport,
  check: CheckResult,
  round: Int,
) -> Result(BriefDevResult, BriefDevError) {
  use _ <- result_try(set_status("reviewing", round))
  case
    workflow.run(
      activities.dev_review(ReviewInput(
        workspace: input.workspace,
        prompt: prompts.review_prompt(
          input.document,
          input.context,
          scout_report,
          dev_report,
          check,
        ),
      )),
    )
  {
    Ok(review_report) ->
      harden_stage(
        input,
        scout_report,
        build_warm,
        dev_report,
        review_report,
        round,
      )
    Error(activity_error) ->
      Error(BriefDevStageFailed(
        stage: "dev_review",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// Drift check then harden re-verify. Any requirement left `drifted` fails
/// with `ReviewDrifted` (evaluated FIRST, so a drifted-and-fixed-elsewhere
/// report still fails). If any review fix is non-empty, the scoped checks
/// re-run with the same files; a regression fails with `HardenRegressed`.
fn harden_stage(
  input: BriefDevInput,
  scout_report: stage_io.ScoutReport,
  build_warm: BuildWarm,
  dev_report: stage_io.DevReport,
  review_report: stage_io.ReviewReport,
  round: Int,
) -> Result(BriefDevResult, BriefDevError) {
  case drifted_requirements(review_report) {
    [_, ..] as drifted -> Error(ReviewDrifted(drifted: drifted))
    [] ->
      case has_fixes(review_report) {
        False ->
          converge(scout_report, build_warm, dev_report, review_report, round)
        True -> {
          use _ <- result_try(set_status("hardening", round))
          case run_scoped_checks(input, dev_report, round) {
            Ok(CheckResult(verdict: CheckPass, ..)) ->
              converge(
                scout_report,
                build_warm,
                dev_report,
                review_report,
                round,
              )
            Ok(CheckResult(verdict: CheckFail(diagnostics: diagnostics), ..)) ->
              Error(HardenRegressed(diagnostics: diagnostics))
            Error(brief_dev_error) -> Error(brief_dev_error)
          }
        }
      }
  }
}

/// Register the converged phase and return the result.
fn converge(
  scout_report: stage_io.ScoutReport,
  build_warm: BuildWarm,
  dev_report: stage_io.DevReport,
  review_report: stage_io.ReviewReport,
  round: Int,
) -> Result(BriefDevResult, BriefDevError) {
  use _ <- result_try(set_status("converged", round))
  Ok(BriefDevResult(
    scout: scout_report,
    dev: dev_report,
    review: review_report,
    verify_rounds: round,
    build_warm: build_warm,
  ))
}

/// Run the scoped checks over the deduplicated changed-file paths of the
/// latest dev report. A stage failure is tagged with the round.
fn run_scoped_checks(
  input: BriefDevInput,
  dev_report: stage_io.DevReport,
  round: Int,
) -> Result(CheckResult, BriefDevError) {
  case
    workflow.run(
      activities.scoped_checks(ScopedInput(
        workspace: input.workspace,
        files_touched: changed_files(dev_report),
      )),
    )
  {
    Ok(check) -> Ok(check)
    Error(activity_error) ->
      Error(BriefDevStageFailed(
        stage: "scoped_checks round " <> int.to_string(round),
        message: errors.activity_message(activity_error),
      ))
  }
}

/// The deduplicated `files_changed` paths across every enrichment of a dev
/// report, in first-seen order.
fn changed_files(dev_report: stage_io.DevReport) -> List(String) {
  dev_report.enrichments
  |> list.flat_map(fn(entry) {
    list.map(entry.files_changed, fn(change) { change.path })
  })
  |> list.unique
}

/// Every requirement the review left `drifted`, with its issues (C16).
fn drifted_requirements(
  review_report: stage_io.ReviewReport,
) -> List(DriftedRequirement) {
  review_report.enrichments
  |> list.filter(fn(entry) {
    entry.alignment == stage_io.ReviewReportEnrichmentsItemAlignmentDrifted
  })
  |> list.map(fn(entry) {
    DriftedRequirement(id: entry.id, issues: entry.issues)
  })
}

/// Whether any review enrichment recorded a non-empty fixes list (C15).
fn has_fixes(review_report: stage_io.ReviewReport) -> Bool {
  list.any(review_report.enrichments, fn(entry) { entry.fixes != [] })
}

/// Re-register the status handler with the current phase and round, so
/// `brief_dev_status` queries answer live state at every yield point
/// (re-registration per stage, per docs/guides/workflows.md). Phases:
/// scouting, developing, verifying, fixing, reviewing, hardening, converged.
fn set_status(phase: String, round: Int) -> Result(Nil, BriefDevError) {
  let status = BriefDevStatus(phase: phase, round: round)
  case
    query.handler(
      status_query_name,
      codecs_workflows.brief_dev_status_codec(),
      fn() { status },
    )
  {
    Ok(Nil) -> Ok(Nil)
    Error(query_error) ->
      Error(BriefDevStageFailed(
        stage: "register_status",
        message: errors.query_message(query_error),
      ))
  }
}

fn result_try(
  result: Result(value, BriefDevError),
  next: fn(value) -> Result(output, BriefDevError),
) -> Result(output, BriefDevError) {
  case result {
    Ok(value) -> next(value)
    Error(brief_dev_error) -> Error(brief_dev_error)
  }
}
