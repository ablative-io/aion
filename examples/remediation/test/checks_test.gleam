//// Unit tests for the pure mechanical checks (`remediation/checks`): gate-1
//// coverage and routing, D4 could_not_reproduce surfacing, gate-2 fix-report
//// accounting (exactly-one rule), and gate-3 derive-and-check acceptance.

import gleam/list
import gleam/option.{None, Some}
import gleeunit/should
import remediation/checks
import remediation/types.{
  Accept, AcceptanceCheck, Completion, Correction, Deviation, FindingBounce,
  FindingFix, FindingRuling, FixReport, Fixed, Gate1Check, LedgerEntry,
  ManifestEntry, NotFixed, Partial, PartialAccept, RegressionIntroduced, Reject,
  TestManifest, Verdict,
}

fn entry(id: String, category: types.Category) -> types.LedgerEntry {
  LedgerEntry(
    id: id,
    title: "t",
    file: "f.rs",
    line: 1,
    category: category,
    severity: "high",
    detail: "d",
    failure_scenario: "fs",
    recommendation: "r",
  )
}

fn manifest_entry(
  id: String,
  tests: List(String),
  could_not_reproduce: Bool,
) -> types.ManifestEntry {
  ManifestEntry(
    finding_id: id,
    test_names: tests,
    test_file: case tests {
      [] -> ""
      _ -> "crates/yg/tests/" <> id <> ".rs"
    },
    expected_failure_signature: case tests {
      [] -> ""
      _ -> "signature for " <> id
    },
    fail_evidence: "",
    could_not_reproduce: could_not_reproduce,
    could_not_reproduce_reason: case could_not_reproduce {
      True -> Some("refactored away")
      False -> None
    },
    manual_acceptance: None,
  )
}

fn manual_entry(id: String, criterion: String) -> types.ManifestEntry {
  ManifestEntry(
    ..manifest_entry(id, [], False),
    manual_acceptance: Some(criterion),
  )
}

// --- gate 1: coverage --------------------------------------------------------

pub fn a_tested_correction_is_covered_test() {
  checks.uncovered_corrections(
    [entry("YG-1", Correction)],
    TestManifest(brief_id: "B", entries: [manifest_entry("YG-1", ["t1"], False)]),
  )
  |> should.equal([])
}

pub fn a_could_not_reproduce_correction_is_covered_test() {
  checks.uncovered_corrections(
    [entry("YG-1", Correction)],
    TestManifest(brief_id: "B", entries: [manifest_entry("YG-1", [], True)]),
  )
  |> should.equal([])
}

pub fn a_correction_missing_from_the_manifest_is_uncovered_test() {
  checks.uncovered_corrections(
    [entry("YG-1", Correction), entry("YG-2", Correction)],
    TestManifest(brief_id: "B", entries: [manifest_entry("YG-1", ["t1"], False)]),
  )
  |> should.equal(["YG-2"])
}

pub fn manual_acceptance_does_not_cover_a_correction_test() {
  // The contract reserves manual_acceptance for improvement/completion
  // findings; a correction with only a criterion is uncovered.
  checks.uncovered_corrections(
    [entry("YG-1", Correction)],
    TestManifest(brief_id: "B", entries: [manual_entry("YG-1", "criterion")]),
  )
  |> should.equal(["YG-1"])
}

pub fn non_corrections_do_not_require_tests_test() {
  checks.uncovered_corrections(
    [entry("YG-1", Completion), entry("YG-2", types.Improvement)],
    TestManifest(brief_id: "B", entries: [manual_entry("YG-1", "criterion")]),
  )
  |> should.equal([])
}

// --- gate 1: routing ----------------------------------------------------------

pub fn an_entry_with_no_evidence_channel_is_unroutable_test() {
  checks.unroutable_entries(
    TestManifest(brief_id: "B", entries: [
      manifest_entry("YG-1", ["t"], False),
      manifest_entry("YG-2", [], False),
      manual_entry("YG-3", "criterion"),
      manifest_entry("YG-4", [], True),
    ]),
  )
  |> should.equal(["YG-2"])
}

pub fn a_blank_manual_acceptance_is_no_evidence_channel_test() {
  checks.unroutable_entries(
    TestManifest(brief_id: "B", entries: [manual_entry("YG-1", "   ")]),
  )
  |> should.equal(["YG-1"])
}

pub fn a_runnable_entry_without_a_signature_is_flagged_test() {
  let unsigned =
    ManifestEntry(
      ..manifest_entry("YG-1", ["t1"], False),
      expected_failure_signature: "  ",
    )
  checks.missing_signatures(TestManifest(brief_id: "B", entries: [unsigned]))
  |> should.equal(["YG-1"])
  // Manual and could_not_reproduce entries legitimately carry no signature.
  checks.missing_signatures(
    TestManifest(brief_id: "B", entries: [
      manual_entry("YG-2", "criterion"),
      manifest_entry("YG-3", [], True),
    ]),
  )
  |> should.equal([])
}

pub fn runnable_checks_carry_tests_and_signatures_test() {
  checks.runnable_checks(
    TestManifest(brief_id: "B", entries: [
      manifest_entry("YG-1", ["t1", "t2"], False),
      manifest_entry("YG-2", ["ghost"], True),
      manual_entry("YG-3", "criterion"),
    ]),
  )
  |> should.equal([
    Gate1Check(
      finding_id: "YG-1",
      test_names: ["t1", "t2"],
      expected_failure_signature: "signature for YG-1",
    ),
  ])
}

pub fn acceptance_checks_route_manual_entries_test() {
  checks.acceptance_checks(
    TestManifest(brief_id: "B", entries: [
      manifest_entry("YG-1", ["t1"], False),
      manual_entry("YG-2", "error names the offending path"),
      manifest_entry("YG-3", [], True),
    ]),
  )
  |> should.equal([
    AcceptanceCheck(
      finding_id: "YG-2",
      criterion: "error names the offending path",
    ),
  ])
}

pub fn test_files_are_the_nonempty_paths_deduped_test() {
  let shared =
    ManifestEntry(
      ..manifest_entry("YG-2", ["t2"], False),
      test_file: "crates/yg/tests/YG-1.rs",
    )
  checks.test_files(
    TestManifest(brief_id: "B", entries: [
      manifest_entry("YG-1", ["t1"], False),
      shared,
      manifest_entry("YG-3", [], True),
    ]),
  )
  |> should.equal(["crates/yg/tests/YG-1.rs"])
}

pub fn could_not_reproduce_ids_are_surfaced_test() {
  checks.could_not_reproduce_ids(
    TestManifest(brief_id: "B", entries: [
      manifest_entry("YG-1", ["t1"], False),
      manifest_entry("YG-2", [], True),
      manifest_entry("YG-3", [], True),
    ]),
  )
  |> should.equal(["YG-2", "YG-3"])
}

// --- gate 2: exactly-one accounting ---------------------------------------------

fn report(addressed: List(String), bounced: List(String)) -> types.FixReport {
  FixReport(
    brief_id: "B",
    commits: ["c"],
    findings_addressed: list.map(addressed, fn(id) {
      FindingFix(finding_id: id, how: "how")
    }),
    findings_bounced: list.map(bounced, fn(id) {
      FindingBounce(finding_id: id, reason: "why")
    }),
    deviations: [Deviation(what: "w", why: "y", approved_by: "planner")],
    new_tests: [],
    class_instances_found: [],
  )
}

pub fn a_fully_accounted_report_is_clean_test() {
  checks.accounting_violations(
    ["YG-1", "YG-2", "YG-3"],
    report(["YG-1", "YG-3"], ["YG-2"]),
  )
  |> should.equal([])
}

pub fn a_finding_in_neither_list_is_a_violation_test() {
  checks.accounting_violations(["YG-1", "YG-2"], report(["YG-1"], []))
  |> should.equal(["YG-2: in neither findings_addressed nor findings_bounced"])
}

pub fn a_finding_in_both_lists_is_a_violation_test() {
  checks.accounting_violations(["YG-1"], report(["YG-1"], ["YG-1"]))
  |> should.equal([
    "YG-1: accounted more than once across findings_addressed/findings_bounced",
  ])
}

pub fn a_finding_duplicated_within_one_list_is_a_violation_test() {
  checks.accounting_violations(["YG-1"], report(["YG-1", "YG-1"], []))
  |> should.equal([
    "YG-1: accounted more than once across findings_addressed/findings_bounced",
  ])
}

// --- gate 3: derive-and-check ------------------------------------------------------

fn verdict_with(
  rulings: List(types.FindingRuling),
  overall: types.Overall,
  reject_reason: option.Option(String),
) -> types.Verdict {
  Verdict(
    brief_id: "B",
    per_finding: rulings,
    class_siblings_found: [],
    regression_risks: [],
    standards_violations: [],
    overall: overall,
    reject_reason: reject_reason,
  )
}

fn ruling(id: String, the_ruling: types.Ruling) -> types.FindingRuling {
  FindingRuling(finding_id: id, ruling: the_ruling, evidence: "e")
}

pub fn derive_overall_accepts_only_all_fixed_test() {
  verdict_with([ruling("YG-1", Fixed), ruling("YG-2", Fixed)], Accept, None)
  |> checks.derive_overall
  |> should.equal(Accept)
}

pub fn derive_overall_rejects_on_not_fixed_or_regression_test() {
  verdict_with([ruling("YG-1", Fixed), ruling("YG-2", NotFixed)], Reject, None)
  |> checks.derive_overall
  |> should.equal(Reject)

  verdict_with(
    [ruling("YG-1", Partial), ruling("YG-2", RegressionIntroduced)],
    Reject,
    None,
  )
  |> checks.derive_overall
  |> should.equal(Reject)
}

pub fn derive_overall_partial_accepts_on_partial_only_test() {
  verdict_with([ruling("YG-1", Fixed), ruling("YG-2", Partial)], Reject, None)
  |> checks.derive_overall
  |> should.equal(PartialAccept)
}

pub fn a_vacuous_verdict_derives_reject_test() {
  verdict_with([], Accept, None)
  |> checks.derive_overall
  |> should.equal(Reject)
}

pub fn a_consistent_verdict_has_no_issues_test() {
  checks.verdict_issues(verdict_with([ruling("YG-1", Fixed)], Accept, None))
  |> should.equal([])
  checks.verdict_issues(verdict_with(
    [ruling("YG-1", Partial)],
    PartialAccept,
    Some("YG-1 class sibling survives"),
  ))
  |> should.equal([])
}

pub fn an_asserted_overall_disagreeing_with_the_derivation_is_rejected_test() {
  // The verifier asserts accept over a partial ruling: the mismatch is a
  // recorded violation, never a silent acceptance of either value.
  checks.verdict_issues(verdict_with([ruling("YG-1", Partial)], Accept, None))
  |> should.equal([
    "verifier asserted overall=accept but the rulings derive partial_accept",
  ])
  checks.verdict_accepts(verdict_with([ruling("YG-1", Partial)], Accept, None))
  |> should.equal(False)

  // The inverse disagreement (asserting reject over all-fixed rulings) is
  // ALSO a violation — and it blocks acceptance even though the derivation
  // says accept.
  let asserted_reject =
    verdict_with([ruling("YG-1", Fixed)], Reject, Some("changed my mind"))
  checks.verdict_issues(asserted_reject)
  |> should.equal([
    "verifier asserted overall=reject but the rulings derive accept",
  ])
  checks.verdict_accepts(asserted_reject)
  |> should.equal(False)
}

pub fn a_non_accept_overall_requires_a_reject_reason_test() {
  checks.verdict_issues(verdict_with([ruling("YG-1", NotFixed)], Reject, None))
  |> should.equal(["overall=reject requires a non-empty reject_reason"])
  checks.verdict_issues(verdict_with(
    [ruling("YG-1", NotFixed)],
    Reject,
    Some("   "),
  ))
  |> should.equal(["overall=reject requires a non-empty reject_reason"])
}

pub fn verdict_accepts_flows_through_the_derived_overall_test() {
  // The ONE source of truth: acceptance = derived Accept + consistency.
  checks.verdict_accepts(verdict_with([ruling("YG-1", Fixed)], Accept, None))
  |> should.equal(True)
  checks.verdict_accepts(verdict_with(
    [ruling("YG-1", Partial)],
    PartialAccept,
    Some("sibling survives"),
  ))
  |> should.equal(False)
  checks.verdict_accepts(verdict_with([], Accept, None))
  |> should.equal(False)
}

pub fn adverse_rulings_render_id_and_ruling_test() {
  checks.adverse_rulings(verdict_with(
    [ruling("YG-1", Fixed), ruling("YG-2", Partial)],
    PartialAccept,
    Some("partial"),
  ))
  |> should.equal(["YG-2: partial"])
}

pub fn branch_safe_reduces_hostile_ids_test() {
  checks.branch_safe("B 1/weird~id")
  |> should.equal("B-1-weird-id")
}
