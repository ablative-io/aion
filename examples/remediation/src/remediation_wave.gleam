//// The PARENT workflow: a signed wave plan -> a wave of remediated briefs.
////
////   validate the strata (pure; every brief in exactly one stratum — an
////   omitted brief would silently never run, the flow's cardinal sin)
////   -> for each stratum IN ORDER (serially, via `list.try_fold`):
////        spawn one `remediation_brief` CHILD per brief CONCURRENTLY (briefs
////        in a stratum are independent), await them all
////   -> collect every brief's terminal result into the wave-report skeleton
////      (`remediation/report`): metrics filled where this run can compute
////      them, everything ledger-derived left null for the ledger-keeper.
////
//// This module is the determinism boundary: it issues only recorded child
//// spawns and branches on their recorded outputs. The strata validation and
//// the metric arithmetic are pure (`remediation/wave`, `remediation/report`)
//// and unit-tested. Triage and Gate 0 happen OUTSIDE the workflow
//// (DECISIONS.md D6): this consumes the operator-approved plan.

import aion/child
import aion/codec
import aion/error
import aion/workflow
import gleam/dict.{type Dict}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/list
import gleam/string
import remediation/codecs
import remediation/report
import remediation/types.{
  type BriefResult, type RemediationError, type WaveBrief, type WaveInput,
  type WaveResult, BriefInput, ChildFailed, StrataInvalid, WaveResult,
}
import remediation/wave
import remediation_brief

/// Typed definition binding the codecs to the parent execute function.
pub fn definition() -> workflow.WorkflowDefinition(
  WaveInput,
  WaveResult,
  RemediationError,
) {
  workflow.define(
    "remediation_wave",
    codecs.wave_input_codec(),
    codecs.wave_result_codec(),
    codecs.remediation_error_codec(),
    execute,
  )
}

/// Engine entry point.
pub fn run(raw_input: Dynamic) -> Result(String, RemediationError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case codecs.wave_input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            Ok(result) -> Ok(codecs.wave_result_codec().encode(result))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(types.DecodeInputFailed(
            "failed to decode wave input: " <> reason,
          ))
      }
    Error(_) ->
      Error(types.DecodeInputFailed("wave input payload was not a string"))
  }
}

/// The parent body.
pub fn execute(input: WaveInput) -> Result(WaveResult, RemediationError) {
  use _ <- try(validate_strata(input))
  let brief_index = index_briefs(input.briefs)

  // Strata run SERIALLY: a later stratum's briefs may depend on the landed
  // outcome of an earlier one (wave ordering, DESIGN.md Stage 0). Briefs
  // WITHIN a stratum run in parallel as child workflows.
  use collected <- try(
    list.try_fold(input.strata, [], fn(acc, stratum) {
      use handles <- try(spawn_stratum(input, stratum, brief_index))
      use results <- try(await_all(handles, []))
      Ok(list.append(acc, results))
    }),
  )

  let wave_number = wave.wave_number(input.briefs)
  Ok(WaveResult(
    wave: wave_number,
    briefs: collected,
    report: report.build(wave_number, collected),
    summary: report.summary(wave_number, collected),
  ))
}

// --- strata processing -----------------------------------------------------------

fn validate_strata(input: WaveInput) -> Result(Nil, RemediationError) {
  case wave.validate(input.briefs, input.strata) {
    Ok(Nil) -> Ok(Nil)
    Error(strata_error) ->
      Error(StrataInvalid(reason: wave.strata_error_message(strata_error)))
  }
}

fn index_briefs(briefs: List(WaveBrief)) -> Dict(String, WaveBrief) {
  list.fold(briefs, dict.new(), fn(acc, wave_brief) {
    dict.insert(acc, wave_brief.brief.id, wave_brief)
  })
}

/// Spawn every brief in a stratum as a CHILD `remediation_brief`,
/// concurrently; the awaits then collect their terminal results in order.
fn spawn_stratum(
  input: WaveInput,
  stratum: List(String),
  brief_index: Dict(String, WaveBrief),
) -> Result(
  List(child.ChildHandle(BriefResult, RemediationError)),
  RemediationError,
) {
  case stratum {
    [] -> Ok([])
    [brief_id, ..rest] -> {
      use wave_brief <- try(lookup_brief(brief_index, brief_id))
      case
        child.spawn(
          "remediation_brief",
          remediation_brief.execute,
          BriefInput(
            brief: wave_brief.brief,
            entries: wave_brief.entries,
            config: input.config,
          ),
          codecs.brief_input_codec(),
          codecs.brief_result_codec(),
          codecs.remediation_error_codec(),
        )
      {
        Ok(handle) -> {
          use rest_handles <- try(spawn_stratum(input, rest, brief_index))
          Ok([handle, ..rest_handles])
        }
        Error(engine_error) ->
          Error(ChildFailed(
            reason: "could not spawn brief "
            <> brief_id
            <> ": "
            <> string.inspect(engine_error),
          ))
      }
    }
  }
}

fn await_all(
  handles: List(child.ChildHandle(BriefResult, RemediationError)),
  acc: List(BriefResult),
) -> Result(List(BriefResult), RemediationError) {
  case handles {
    [] -> Ok(list.reverse(acc))
    [handle, ..rest] ->
      case child.await(handle) {
        Ok(result) -> await_all(rest, [result, ..acc])
        Error(child_error) -> Error(child_error_to_remediation(child_error))
      }
  }
}

// --- helpers ---------------------------------------------------------------------

fn lookup_brief(
  brief_index: Dict(String, WaveBrief),
  brief_id: String,
) -> Result(WaveBrief, RemediationError) {
  case dict.get(brief_index, brief_id) {
    Ok(wave_brief) -> Ok(wave_brief)
    // Unreachable after validate_strata, but never silently defaulted.
    Error(_) ->
      Error(StrataInvalid(reason: "stratum names unknown brief " <> brief_id))
  }
}

fn child_error_to_remediation(
  child_error: error.ChildError(RemediationError),
) -> RemediationError {
  case child_error {
    // A child that failed with a typed remediation error propagates it —
    // the parent's history carries the child's own taxonomy.
    error.ChildWorkflowFailed(remediation_error) -> remediation_error
    error.ChildOutputDecodeFailed(_) ->
      ChildFailed(reason: "child brief result could not be decoded")
    error.ChildErrorDecodeFailed(_) ->
      ChildFailed(reason: "child brief error could not be decoded")
    error.ChildEngineFailure(message: message) ->
      ChildFailed(reason: "child brief engine failure: " <> message)
  }
}

fn try(
  result: Result(a, RemediationError),
  next: fn(a) -> Result(b, RemediationError),
) -> Result(b, RemediationError) {
  case result {
    Ok(value) -> next(value)
    Error(remediation_error) -> Error(remediation_error)
  }
}
