//// The brief_forge workflow: finding/task in → grounded, refuted,
//// dispatchable brief out (prospekt doctrine: workflows/brief-forge.md).
////
//// Topology:
////
//// 1. `scout` — grounding recon over the actual tree
////    (`schemas/scout-report.schema.json`).
//// 2. The capped design⇄refute loop: `design` drafts a brief
////    (`schemas/brief.schema.json`); `refute` attacks the ARTIFACT plus the
////    scout report — never the designer's reasoning
////    (`schemas/refutation.schema.json`).
//// 3. On `design_survives`: THE WORKFLOW stamps `refutation_survived` on
////    the brief (never the designer — any designer-set value is cleared on
////    receipt) and completes `Converged`.
//// 4. On cap exhaustion (`refute_cap`, a REQUIRED input — no baked
////    defaults): the run completes `Contested` carrying the last brief AND
////    the last refutation — the design space is contested and the operator
////    gets both sides; a finding, never an error crash.
////
//// `diagnose_only` rides the input into the design prompt and the output
//// verbatim: a diagnosis-complete brief is a landable terminus whose
//// dispatch is deliberately deferred.

import aion/codec
import aion/workflow
import dev_pipeline/activities
import dev_pipeline/codecs
import dev_pipeline/errors
import dev_pipeline/prompts
import dev_pipeline/types.{
  type Brief, type BriefForgeError, type BriefForgeInput, type BriefForgeResult,
  type Refutation, type ScoutReport, Brief, BriefForgeResult,
  BriefForgeStageFailed, Contested, Converged,
}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/int
import gleam/option.{None, Some}

/// The deployed workflow type: the entry module name.
pub const workflow_type = "brief_forge"

/// Typed definition binding the codecs to the execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  BriefForgeInput,
  BriefForgeResult,
  BriefForgeError,
) {
  workflow.define(
    "brief-forge",
    codecs.input_codec(),
    codecs.result_codec(),
    codecs.error_codec(),
    execute,
  )
}

/// Engine entry point for one execution. The runtime delivers the start
/// input as a raw JSON string; success and failure are both encoded back to
/// JSON text here — the engine records these exact payloads as the workflow
/// terminal.
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(output) -> Ok(codecs.result_codec().encode(output))
            Error(forge_error) ->
              Error(codecs.error_codec().encode(forge_error))
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(
            codecs.error_codec().encode(BriefForgeStageFailed(
              stage: "decode_input",
              message: "failed to decode brief-forge input: " <> reason,
            )),
          )
      }
    Error(_) ->
      Error(
        codecs.error_codec().encode(BriefForgeStageFailed(
          stage: "decode_input",
          message: "brief-forge input payload was not a string",
        )),
      )
  }
}

/// Typed workflow body: scout, then the capped design⇄refute loop.
pub fn execute(
  input: BriefForgeInput,
) -> Result(BriefForgeResult, BriefForgeError) {
  case run_scout(input) {
    Ok(scout_report) -> forge_loop(input, scout_report, None, 1)
    Error(forge_error) -> Error(forge_error)
  }
}

/// The grounding recon round. The activity input is the projected prompt
/// itself; the worker's driven-mode harness derives the norn session id
/// (`{workflow_id}-scout`) at spawn.
fn run_scout(input: BriefForgeInput) -> Result(ScoutReport, BriefForgeError) {
  case workflow.run(activities.scout(prompts.scout_prompt(input))) {
    Ok(scout_report) -> Ok(scout_report)
    Error(activity_error) ->
      Error(BriefForgeStageFailed(
        stage: "scout",
        message: errors.activity_message(activity_error),
      ))
  }
}

/// One design→refute round. `prior` carries the refutation the previous
/// round's design did not survive, as `(round, encoded_json)`.
fn forge_loop(
  input: BriefForgeInput,
  scout_report: ScoutReport,
  prior: option.Option(#(Int, String)),
  round: Int,
) -> Result(BriefForgeResult, BriefForgeError) {
  case run_design(input, scout_report, prior, round) {
    Ok(draft) ->
      case run_refute(input, scout_report, draft, round) {
        Ok(refutation) ->
          settle_round(input, scout_report, draft, refutation, round)
        Error(forge_error) -> Error(forge_error)
      }
    Error(forge_error) -> Error(forge_error)
  }
}

/// Judge one refutation: survived → the WORKFLOW stamps the brief and
/// completes `Converged`; not survived under the cap → the next round gets
/// the refutation as additional design input; cap exhausted → `Contested`
/// with both sides, surfaced, never crashed.
fn settle_round(
  input: BriefForgeInput,
  scout_report: ScoutReport,
  draft: Brief,
  refutation: Refutation,
  round: Int,
) -> Result(BriefForgeResult, BriefForgeError) {
  case refutation.design_survives {
    True ->
      Ok(BriefForgeResult(
        outcome: Converged,
        brief: stamp_refutation_survived(draft, input.task_ref, round),
        refutation: refutation,
        rounds: round,
        diagnose_only: input.diagnose_only,
      ))
    False ->
      case round >= input.refute_cap {
        True ->
          Ok(BriefForgeResult(
            outcome: Contested,
            brief: draft,
            refutation: refutation,
            rounds: round,
            diagnose_only: input.diagnose_only,
          ))
        False ->
          forge_loop(
            input,
            scout_report,
            Some(#(round, codecs.refutation_codec().encode(refutation))),
            round + 1,
          )
      }
  }
}

/// One design round. The harness session (`{workflow_id}-design`, resumed
/// via `--resume-if-exists`) keeps the designer's own context across loop
/// rounds. Any designer-set `refutation_survived` is cleared on receipt:
/// the stamp is the workflow's alone.
fn run_design(
  input: BriefForgeInput,
  scout_report: ScoutReport,
  prior: option.Option(#(Int, String)),
  round: Int,
) -> Result(Brief, BriefForgeError) {
  let scout_json = codecs.scout_report_codec().encode(scout_report)
  case
    workflow.run(
      activities.design(prompts.design_prompt(input, scout_json, prior)),
    )
  {
    Ok(draft) -> Ok(clear_stamp(draft))
    Error(activity_error) ->
      Error(BriefForgeStageFailed(
        stage: "design round " <> int.to_string(round),
        message: errors.activity_message(activity_error),
      ))
  }
}

/// One refute round: the refuter is handed the brief artifact and the scout
/// report only — never the designer's reasoning. Driven-mode deviation: the
/// harness session id is `{workflow_id}-refute`, so loop rounds within one
/// run resume ONE refuter session rather than each getting a fresh one (no
/// per-round spawn template exists yet).
fn run_refute(
  input: BriefForgeInput,
  scout_report: ScoutReport,
  draft: Brief,
  round: Int,
) -> Result(Refutation, BriefForgeError) {
  let brief_json = codecs.brief_codec().encode(draft)
  let scout_json = codecs.scout_report_codec().encode(scout_report)
  case
    workflow.run(
      activities.refute(prompts.refute_prompt(input, brief_json, scout_json)),
    )
  {
    Ok(refutation) -> Ok(refutation)
    Error(activity_error) ->
      Error(BriefForgeStageFailed(
        stage: "refute round " <> int.to_string(round),
        message: errors.activity_message(activity_error),
      ))
  }
}

/// Strip any designer-set `refutation_survived` — the field is the
/// workflow's to stamp (accept step, plain workflow code).
fn clear_stamp(draft: Brief) -> Brief {
  Brief(..draft, refutation_survived: None)
}

/// The accept step: stamp the reference to the refutation this design
/// survived. Plain workflow code — never the designer.
fn stamp_refutation_survived(
  draft: Brief,
  task_ref: String,
  round: Int,
) -> Brief {
  Brief(
    ..draft,
    refutation_survived: Some(task_ref <> "-refute-r" <> int.to_string(round)),
  )
}
