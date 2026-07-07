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
////
//// NON-CASCADE ON CHILD FAILURE (2026-07-07 incident): a child brief's own
//// failure is recorded data, never a parent error. Siblings already
//// dispatched in the same stratum still run to completion; every LATER
//// stratum is skipped (a later stratum may depend on the landed outcome of
//// the one that failed); the wave itself completes with the full per-brief
//// outcome map (`remediation/wave`'s `WaveProgress` reducer owns this
//// policy, pure and unit-tested). The parent only fails on a genuine
//// infrastructure fault of its own (bad strata, a failed child spawn).

import aion/child
import aion/codec
import aion/error
import aion/workflow
import gleam/dict.{type Dict}
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/list
import gleam/option
import gleam/string
import remediation/codecs
import remediation/report
import remediation/types.{
  type BriefResult, type RemediationError, type WaveBrief, type WaveInput,
  type WaveResult, BriefInput, ChildFailed, StrataInvalid, WaveResult,
}
import remediation/wave.{
  type BriefRunOutcome, type WaveProgress, BriefCompleted, BriefRunFailed,
}
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
///
/// Change 2 (2026-07-07 incident): a child brief's own failure is data, never
/// a parent error — `execute` only returns `Error` for a genuine
/// infrastructure fault of ITS OWN (bad strata, a failed child SPAWN). Every
/// stratum's per-brief outcomes fold into `remediation/wave`'s pure
/// `WaveProgress` reducer, which decides — once any brief has failed — that
/// every later stratum is skipped rather than run (serial-stratum execution:
/// a later stratum may depend on the landed outcome of the one that failed,
/// DESIGN.md Stage 0). The wave always completes with the full per-brief
/// outcome map: succeeded, failed-with-reason, and skipped-with-reason.
pub fn execute(input: WaveInput) -> Result(WaveResult, RemediationError) {
  use _ <- try(validate_strata(input))
  let brief_index = index_briefs(input.briefs)

  // Strata run SERIALLY: a later stratum's briefs may depend on the landed
  // outcome of an earlier one (wave ordering, DESIGN.md Stage 0). Briefs
  // WITHIN a stratum run in parallel as child workflows.
  use progress <- try(
    list.try_fold(input.strata, wave.empty_progress(), fn(progress, stratum) {
      run_stratum(input, stratum, brief_index, progress)
    }),
  )

  let wave_number = wave.wave_number(input.briefs)
  Ok(WaveResult(
    wave: wave_number,
    briefs: progress.succeeded,
    failed_briefs: progress.failed,
    skipped_briefs: progress.skipped,
    report: report.build(wave_number, progress.succeeded),
    summary: report.summary(
      wave_number,
      progress.succeeded,
      progress.failed,
      progress.skipped,
    ),
  ))
}

/// Run (or skip) one stratum and fold its outcome into `progress`. A stratum
/// the wave is already blocked on is not even spawned — `wave.fold_stratum`
/// records every one of its briefs skipped from `progress` alone. Otherwise
/// every brief in the stratum is spawned concurrently, awaited, and its
/// outcome (completed or GENERICALLY failed) folded in.
fn run_stratum(
  input: WaveInput,
  stratum: List(String),
  brief_index: Dict(String, WaveBrief),
  progress: WaveProgress,
) -> Result(WaveProgress, RemediationError) {
  case progress.blocked_by {
    option.Some(_) -> Ok(wave.fold_stratum(progress, stratum, []))
    option.None -> {
      use handles <- try(spawn_stratum(input, stratum, brief_index))
      let outcomes = await_all(handles)
      Ok(wave.fold_stratum(progress, stratum, outcomes))
    }
  }
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
/// concurrently; each handle is paired with its brief id so a later failed
/// await can be attributed to the right brief without needing a decoded
/// result. A SPAWN failure (the engine could not even start the child) is a
/// genuine infrastructure fault of the PARENT's own — unlike an awaited
/// child's own failure (Change 2), this still propagates.
fn spawn_stratum(
  input: WaveInput,
  stratum: List(String),
  brief_index: Dict(String, WaveBrief),
) -> Result(
  List(#(String, child.ChildHandle(BriefResult, RemediationError))),
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
          Ok([#(brief_id, handle), ..rest_handles])
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

/// Await every spawned handle in a stratum, converting a GENERIC child
/// failure into data (Change 2) rather than propagating it: every
/// `error.ChildError` variant funnels into the same `BriefRunFailed` outcome
/// — including `ChildErrorDecodeFailed`, which is not pattern-matched
/// specially, because the decode path is not reliable enough to gate
/// continuation on (2026-07-07 wire quirk). A completed child decodes
/// normally into `BriefCompleted`.
fn await_all(
  handles: List(#(String, child.ChildHandle(BriefResult, RemediationError))),
) -> List(#(String, BriefRunOutcome)) {
  list.map(handles, fn(pair) {
    let #(brief_id, handle) = pair
    case child.await(handle) {
      Ok(result) -> #(brief_id, BriefCompleted(result))
      Error(child_error) -> #(
        brief_id,
        BriefRunFailed(child_error_message(child_error)),
      )
    }
  })
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

/// Render any `error.ChildError` variant as a human-readable reason string —
/// the GENERIC path (Change 2): every variant is handled explicitly (matching
/// the precedent in `examples/batch-orchestrator`), but all four fold into
/// the same `BriefRunFailed` outcome at the call site, so no variant (in
/// particular `ChildErrorDecodeFailed`, which does not reliably fire) is
/// relied on to decide whether the wave keeps going.
fn child_error_message(
  child_error: error.ChildError(RemediationError),
) -> String {
  case child_error {
    error.ChildWorkflowFailed(remediation_error) ->
      "child brief failed: " <> string.inspect(remediation_error)
    error.ChildOutputDecodeFailed(_) ->
      "child brief result could not be decoded"
    error.ChildErrorDecodeFailed(_) -> "child brief error could not be decoded"
    error.ChildEngineFailure(message: message) ->
      "child brief engine failure: " <> message
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
