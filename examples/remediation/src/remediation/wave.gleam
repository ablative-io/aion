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
import gleam/set.{type Set}
import remediation/types.{type WaveBrief}

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
