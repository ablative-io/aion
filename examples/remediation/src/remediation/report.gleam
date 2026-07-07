//// The wave-report skeleton builder: pure metric computation over the
//// collected [`BriefResult`]s, unit-tested (`test/report_test`).
////
//// The rule (task contract): fill exactly the metrics THIS run can compute
//// from its own artifacts — fix cycles, first-pass acceptance,
//// could-not-reproduce rate, deviation/test-edit counts, class siblings per
//// verified brief — and leave every ledger-derived or later-stage metric
//// `None` (JSON `null`) for the ledger-keeper. A null is honest; a fabricated
//// zero would read as a measured result.

import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import remediation/codecs
import remediation/types.{
  type BriefResult, type WaveBriefFailure, type WaveBriefSkip, type WaveMetrics,
  type WaveReport, Accepted, FixMetrics, TestAuthoringMetrics, VerifyMetrics,
  WaveReport,
}

/// Build the wave-report skeleton from the wave number and the collected
/// brief results. Ledger delta and queues stay empty (the applier owns
/// transitions; agents cannot defer/refute — DESIGN.md terminal-state rules).
pub fn build(wave: Int, results: List(BriefResult)) -> WaveReport {
  WaveReport(
    wave: wave,
    new_entries: [],
    metrics: metrics(results),
    deferred_queue: [],
    refuted_queue: [],
  )
}

/// The computable metrics block (everything else stays `None` via
/// [`codecs.empty_metrics`]).
pub fn metrics(results: List(BriefResult)) -> WaveMetrics {
  let base = codecs.empty_metrics()
  types.WaveMetrics(
    ..base,
    test_authoring: TestAuthoringMetrics(
      ..base.test_authoring,
      could_not_reproduce_rate: could_not_reproduce_rate(results),
    ),
    fix: FixMetrics(
      first_pass_acceptance_rate: first_pass_acceptance_rate(results),
      fix_cycles_per_brief: fix_cycles_per_brief(results),
      deviation_count: Some(deviation_count(results)),
      test_edit_attempts: Some(test_edit_attempts(results)),
    ),
    verify: VerifyMetrics(
      ..base.verify,
      class_siblings_per_brief: class_siblings_per_brief(results),
    ),
  )
}

/// could_not_reproduce entries / total manifest entries, across the wave.
/// `None` when no manifest entries exist (nothing to rate).
pub fn could_not_reproduce_rate(results: List(BriefResult)) -> Option(Float) {
  let total =
    list.fold(results, 0, fn(count, result) {
      count + list.length(result.manifest.entries)
    })
  let unreproduced =
    list.fold(results, 0, fn(count, result) {
      count + list.length(result.could_not_reproduce)
    })
  ratio(unreproduced, total)
}

/// Briefs accepted on their first developer round / total briefs. `None` for
/// an empty wave.
pub fn first_pass_acceptance_rate(results: List(BriefResult)) -> Option(Float) {
  let accepted_first =
    results
    |> list.filter(fn(result) { result.first_pass_accepted })
    |> list.length
  ratio(accepted_first, list.length(results))
}

/// Mean developer rounds per brief. `None` for an empty wave.
pub fn fix_cycles_per_brief(results: List(BriefResult)) -> Option(Float) {
  let total =
    list.fold(results, 0, fn(count, result) { count + result.fix_cycles })
  ratio(total, list.length(results))
}

/// Total declared deviations across the wave's fix reports.
pub fn deviation_count(results: List(BriefResult)) -> Int {
  list.fold(results, 0, fn(count, result) {
    case result.fix_report {
      Some(report) -> count + list.length(report.deviations)
      None -> count
    }
  })
}

/// Total authored-test-edit attempts caught at gate 2 across the wave (should
/// be zero — any occurrence is a prompt/guard failure, DESIGN.md metrics).
pub fn test_edit_attempts(results: List(BriefResult)) -> Int {
  list.fold(results, 0, fn(count, result) { count + result.test_edit_attempts })
}

/// Mean class siblings found per VERIFIED brief (briefs that produced a
/// verdict). `None` when no brief reached verification.
pub fn class_siblings_per_brief(results: List(BriefResult)) -> Option(Float) {
  let verified =
    list.filter_map(results, fn(result) {
      case result.verdict {
        Some(verdict) -> Ok(list.length(verdict.class_siblings_found))
        None -> Error(Nil)
      }
    })
  ratio(list.fold(verified, 0, int.add), list.length(verified))
}

/// A one-line human summary of the wave for the result payload. `failed` and
/// `skipped` are the Change-2 non-cascade outcomes (a child brief failure, or
/// a later-stratum skip caused by one) — surfaced here so a glance at the
/// summary never reads as "every brief ran" when some did not.
pub fn summary(
  wave: Int,
  results: List(BriefResult),
  failed: List(WaveBriefFailure),
  skipped: List(WaveBriefSkip),
) -> String {
  let accepted =
    results
    |> list.filter(fn(result) { result.disposition == Accepted })
    |> list.length
  let unreproduced =
    list.fold(results, 0, fn(count, result) {
      count + list.length(result.could_not_reproduce)
    })
  let total = list.length(results) + list.length(failed) + list.length(skipped)
  "Wave "
  <> int.to_string(wave)
  <> ": "
  <> int.to_string(accepted)
  <> "/"
  <> int.to_string(total)
  <> " briefs accepted; "
  <> int.to_string(unreproduced)
  <> " could_not_reproduce finding(s) for the operator; "
  <> int.to_string(list.length(failed))
  <> " failed; "
  <> int.to_string(list.length(skipped))
  <> " skipped."
}

fn ratio(numerator: Int, denominator: Int) -> Option(Float) {
  case denominator {
    0 -> None
    _ -> Some(int.to_float(numerator) /. int.to_float(denominator))
  }
}
