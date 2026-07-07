//// The wave-plan strata validation: pure, no effects, exhaustively
//// unit-tested (`test/wave_test`), because a wrong plan silently drops
//// findings — the one thing the remediation flow exists to prevent.
////
//// Strata are GIVEN by the signed wave plan (triage/Gate 0 happen outside the
//// workflow, DECISIONS.md D6), so unlike pipeline-run's `stack` this module
//// does not compute an ordering — it verifies the given one is runnable:
//// every stratum id names a known brief, no brief runs twice, and no brief is
//// left out of the strata (an omitted brief would silently never run).

import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/set.{type Set}
import gleam/string
import remediation/types.{
  type BriefResult, type WaveBrief, type WaveBriefFailure, type WaveBriefSkip,
  WaveBriefFailure, WaveBriefSkip,
}

/// Why a wave plan's strata are not runnable. Every variant names the
/// offending id — an actionable error, never a bare "invalid".
pub type StrataError {
  /// A stratum names a brief id absent from the plan's briefs.
  UnknownBrief(brief_id: String)
  /// A brief id appears more than once across the strata.
  DuplicateBrief(brief_id: String)
  /// A brief in the plan appears in no stratum — it would silently never run.
  MissingBrief(brief_id: String)
  /// The plan proposes briefs but no strata (or vice versa is caught by the
  /// checks above); an entirely empty plan is also rejected here.
  EmptyPlan
}

/// Validate the given strata against the plan's briefs. `Ok(Nil)` guarantees:
/// every stratum id resolves to exactly one brief, and every brief appears in
/// exactly one stratum.
pub fn validate(
  briefs: List(WaveBrief),
  strata: List(List(String)),
) -> Result(Nil, StrataError) {
  case briefs {
    [] -> Error(EmptyPlan)
    _ -> {
      let known =
        briefs
        |> list.map(fn(wave_brief) { wave_brief.brief.id })
        |> set.from_list
      let flat = list.flatten(strata)
      case first_error(flat, known, set.new()) {
        Ok(placed) ->
          case
            briefs
            |> list.filter(fn(wave_brief) {
              !set.contains(placed, wave_brief.brief.id)
            })
          {
            [] -> Ok(Nil)
            [dropped, ..] -> Error(MissingBrief(dropped.brief.id))
          }
        Error(strata_error) -> Error(strata_error)
      }
    }
  }
}

fn first_error(
  ids: List(String),
  known: Set(String),
  placed: Set(String),
) -> Result(Set(String), StrataError) {
  case ids {
    [] -> Ok(placed)
    [id, ..rest] ->
      case set.contains(known, id), set.contains(placed, id) {
        False, _ -> Error(UnknownBrief(id))
        _, True -> Error(DuplicateBrief(id))
        True, False -> first_error(rest, known, set.insert(placed, id))
      }
  }
}

/// Render a [`StrataError`] as a single actionable line naming the offending
/// brief.
pub fn strata_error_message(strata_error: StrataError) -> String {
  case strata_error {
    UnknownBrief(brief_id) -> "stratum names unknown brief `" <> brief_id <> "`"
    DuplicateBrief(brief_id) ->
      "brief `" <> brief_id <> "` appears in more than one stratum position"
    MissingBrief(brief_id) ->
      "brief `"
      <> brief_id
      <> "` appears in no stratum and would silently never run"
    EmptyPlan -> "the wave plan proposes no briefs"
  }
}

/// The wave number of a plan: every brief carries its wave (they should
/// agree); the maximum is taken so a mixed plan is at least reported against
/// its latest wave rather than an arbitrary first element. Zero for an empty
/// plan (which [`validate`] rejects before this matters).
pub fn wave_number(briefs: List(WaveBrief)) -> Int {
  list.fold(briefs, 0, fn(highest, wave_brief) {
    case wave_brief.brief.wave > highest {
      True -> wave_brief.brief.wave
      False -> highest
    }
  })
}

// --- Change 2: non-cascade on child (brief) failure ---------------------------
//
// Real incident, 2026-07-07: a transient provider error failed a
// remediation_brief child workflow, and the parent remediation_wave cascaded
// to Failed, losing the wave's whole bookkeeping — results for briefs that
// had already succeeded were lost. The rule from here down: a child brief's
// failure is recorded, never propagated; siblings ALREADY DISPATCHED in the
// same stratum still run to completion; every LATER stratum is skipped
// (serial-stratum execution means a later stratum may depend on the landed
// outcome of the one that failed, DESIGN.md Stage 0); the wave itself
// completes with the full per-brief outcome map. `remediation_wave.gleam`
// drives this pure reducer stratum by stratum — the effects (spawn/await) are
// the only part that module owns; the policy lives here, unit-tested without
// the engine.

/// One child brief's OWN run outcome, as the parent observes it after
/// awaiting: a normal terminal result, or a GENERIC child failure. `reason`
/// is built from every `aion/error.ChildError` variant uniformly (decode
/// failures included) — the decode path is not reliable enough to gate
/// continuation on a specific variant (2026-07-07 wire quirk).
pub type BriefRunOutcome {
  BriefCompleted(BriefResult)
  BriefRunFailed(reason: String)
}

/// The reducer's running state across strata. `blocked_by` is `None` while
/// every stratum run so far succeeded outright, and becomes `Some(ids)` the
/// moment any stratum produces a failure — every stratum folded in afterward
/// is recorded skipped, citing those same `ids`, instead of being run at all.
pub type WaveProgress {
  WaveProgress(
    succeeded: List(BriefResult),
    failed: List(WaveBriefFailure),
    skipped: List(WaveBriefSkip),
    blocked_by: Option(List(String)),
  )
}

/// The reducer's identity element: nothing has run, nothing is blocked.
pub fn empty_progress() -> WaveProgress {
  WaveProgress(succeeded: [], failed: [], skipped: [], blocked_by: None)
}

/// Fold one stratum into the running progress.
///
/// When the wave is ALREADY blocked (an earlier stratum had a failure), every
/// brief named in `stratum` is recorded skipped — `outcomes` is ignored, and
/// the caller should not have spawned or awaited anything for a blocked
/// stratum in the first place (a later stratum may depend on the landed
/// outcome of the one that failed).
///
/// Otherwise every named brief actually ran: `outcomes` pairs each brief id
/// with what its child returned, folded into `succeeded`/`failed`. If ANY of
/// them failed, the wave becomes blocked-by their ids for every stratum
/// folded in after this one.
pub fn fold_stratum(
  progress: WaveProgress,
  stratum: List(String),
  outcomes: List(#(String, BriefRunOutcome)),
) -> WaveProgress {
  case progress.blocked_by {
    Some(blocking) ->
      WaveProgress(
        ..progress,
        skipped: list.append(progress.skipped, skips_for(stratum, blocking)),
      )
    None -> {
      let #(succeeded, failed) = partition_outcomes(outcomes)
      let blocked_by = case failed {
        [] -> None
        _ -> Some(list.map(failed, fn(failure) { failure.brief_id }))
      }
      WaveProgress(
        succeeded: list.append(progress.succeeded, succeeded),
        failed: list.append(progress.failed, failed),
        skipped: progress.skipped,
        blocked_by: blocked_by,
      )
    }
  }
}

fn partition_outcomes(
  outcomes: List(#(String, BriefRunOutcome)),
) -> #(List(BriefResult), List(WaveBriefFailure)) {
  list.fold(outcomes, #([], []), fn(acc, pair) {
    let #(succeeded, failed) = acc
    case pair {
      #(_, BriefCompleted(result)) -> #(
        list.append(succeeded, [result]),
        failed,
      )
      #(brief_id, BriefRunFailed(reason)) -> #(
        succeeded,
        list.append(failed, [
          WaveBriefFailure(brief_id: brief_id, reason: reason),
        ]),
      )
    }
  })
}

fn skips_for(
  stratum: List(String),
  blocking: List(String),
) -> List(WaveBriefSkip) {
  let reason = skip_reason(blocking)
  list.map(stratum, fn(brief_id) {
    WaveBriefSkip(
      brief_id: brief_id,
      blocking_brief_ids: blocking,
      reason: reason,
    )
  })
}

/// The human-readable rendering of a skip, naming every blocking brief.
pub fn skip_reason(blocking: List(String)) -> String {
  "skipped: blocked by failed brief(s) " <> string.join(blocking, ", ")
}
