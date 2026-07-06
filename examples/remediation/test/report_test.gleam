//// Unit tests for the wave-report skeleton builder (`remediation/report`):
//// the computable metrics are computed, everything else stays None (an
//// honest null, never a fabricated zero).

import gleam/option.{None, Some}
import gleeunit/should
import remediation/report
import remediation/types.{
  Accepted, BriefResult, ClassSibling, CycleCapExhausted, Deviation, FindingFix,
  FindingRuling, FixReport, Fixed, ManifestEntry, TestManifest, Verdict,
}

fn result(
  id id: String,
  disposition disposition: types.Disposition,
  fix_cycles fix_cycles: Int,
  first_pass first_pass: Bool,
  manifest_entries manifest_entries: List(types.ManifestEntry),
  could_not_reproduce could_not_reproduce: List(String),
  fix_report fix_report: option.Option(types.FixReport),
  verdict verdict: option.Option(types.Verdict),
  test_edit_attempts test_edit_attempts: Int,
) -> types.BriefResult {
  BriefResult(
    brief_id: id,
    disposition: disposition,
    fix_cycles: fix_cycles,
    first_pass_accepted: first_pass,
    could_not_reproduce: could_not_reproduce,
    test_edit_attempts: test_edit_attempts,
    branch: "remediation/" <> id,
    manifest: TestManifest(brief_id: id, entries: manifest_entries),
    fix_report: fix_report,
    verdict: verdict,
    ledger: [],
    workspace_removed: True,
    summary: "",
  )
}

fn manifest_entry(
  id: String,
  could_not_reproduce: Bool,
) -> types.ManifestEntry {
  ManifestEntry(
    finding_id: id,
    test_names: case could_not_reproduce {
      True -> []
      False -> ["t"]
    },
    fail_evidence: "",
    could_not_reproduce: could_not_reproduce,
  )
}

fn sample_results() -> List(types.BriefResult) {
  [
    result(
      id: "B-1",
      disposition: Accepted,
      fix_cycles: 1,
      first_pass: True,
      manifest_entries: [
        manifest_entry("YG-1", False),
        manifest_entry("YG-2", True),
      ],
      could_not_reproduce: ["YG-2"],
      fix_report: Some(
        FixReport(
          brief_id: "B-1",
          commits: ["c1"],
          findings_addressed: [FindingFix(finding_id: "YG-1", how: "h")],
          deviations: [
            Deviation(what: "w", why: "y", approved_by: "planner"),
            Deviation(what: "w2", why: "y2", approved_by: "planner"),
          ],
          new_tests: [],
        ),
      ),
      verdict: Some(
        Verdict(
          brief_id: "B-1",
          per_finding: [
            FindingRuling(finding_id: "YG-1", ruling: Fixed, evidence: "e"),
          ],
          class_siblings_found: [
            ClassSibling(file: "f", line: 1, description: "d"),
            ClassSibling(file: "f", line: 2, description: "d"),
          ],
        ),
      ),
      test_edit_attempts: 0,
    ),
    result(
      id: "B-2",
      disposition: CycleCapExhausted,
      fix_cycles: 3,
      first_pass: False,
      manifest_entries: [
        manifest_entry("YG-3", False),
        manifest_entry("YG-4", False),
      ],
      could_not_reproduce: [],
      fix_report: None,
      verdict: None,
      test_edit_attempts: 1,
    ),
  ]
}

pub fn fix_cycles_per_brief_is_the_mean_test() {
  report.fix_cycles_per_brief(sample_results())
  |> should.equal(Some(2.0))
}

pub fn first_pass_acceptance_rate_counts_first_pass_accepts_test() {
  report.first_pass_acceptance_rate(sample_results())
  |> should.equal(Some(0.5))
}

pub fn could_not_reproduce_rate_is_over_all_manifest_entries_test() {
  // 1 unreproduced of 4 manifest entries.
  report.could_not_reproduce_rate(sample_results())
  |> should.equal(Some(0.25))
}

pub fn deviation_and_test_edit_counts_sum_across_briefs_test() {
  report.deviation_count(sample_results())
  |> should.equal(2)
  report.test_edit_attempts(sample_results())
  |> should.equal(1)
}

pub fn class_siblings_average_only_over_verified_briefs_test() {
  // Only B-1 produced a verdict: 2 siblings / 1 verified brief.
  report.class_siblings_per_brief(sample_results())
  |> should.equal(Some(2.0))
}

pub fn an_empty_wave_computes_no_rates_test() {
  report.fix_cycles_per_brief([]) |> should.equal(None)
  report.first_pass_acceptance_rate([]) |> should.equal(None)
  report.could_not_reproduce_rate([]) |> should.equal(None)
  report.class_siblings_per_brief([]) |> should.equal(None)
}

pub fn non_computable_metrics_stay_none_test() {
  let metrics = report.metrics(sample_results())
  metrics.test_authoring.valid_fail_first_rate |> should.equal(None)
  metrics.test_authoring.wrong_reason_fail_rate |> should.equal(None)
  metrics.verify.verdicts_overturned |> should.equal(None)
  metrics.re_audit.class_recurrence_rate |> should.equal(None)
  metrics.re_audit.new_finding_inflow |> should.equal(None)
  metrics.flow.lead_time_days |> should.equal(None)
  metrics.flow.terminal_state_ratio |> should.equal(None)
}

pub fn the_report_skeleton_leaves_ledger_fields_to_the_keeper_test() {
  let built = report.build(0, sample_results())
  built.new_entries |> should.equal([])
  built.deferred_queue |> should.equal([])
  built.refuted_queue |> should.equal([])
  built.wave |> should.equal(0)
}
