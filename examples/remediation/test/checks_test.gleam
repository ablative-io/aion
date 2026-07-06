//// Unit tests for the pure mechanical checks (`remediation/checks`): gate-1
//// coverage, D4 could_not_reproduce surfacing, gate-3 acceptance.

import gleeunit/should
import remediation/checks
import remediation/types.{
  Completion, Correction, FindingRuling, Fixed, LedgerEntry, ManifestEntry,
  NotFixed, RegressionIntroduced, TestManifest, Verdict,
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
    fail_evidence: "",
    could_not_reproduce: could_not_reproduce,
  )
}

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

pub fn a_correction_with_neither_tests_nor_flag_is_uncovered_test() {
  checks.uncovered_corrections(
    [entry("YG-1", Correction)],
    TestManifest(brief_id: "B", entries: [manifest_entry("YG-1", [], False)]),
  )
  |> should.equal(["YG-1"])
}

pub fn non_corrections_do_not_require_tests_test() {
  checks.uncovered_corrections(
    [entry("YG-1", Completion), entry("YG-2", types.Improvement)],
    TestManifest(brief_id: "B", entries: [manifest_entry("YG-1", [], False)]),
  )
  |> should.equal([])
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

pub fn runnable_tests_exclude_unreproduced_entries_test() {
  checks.runnable_tests(
    TestManifest(brief_id: "B", entries: [
      manifest_entry("YG-1", ["t1", "t2"], False),
      manifest_entry("YG-2", ["ghost"], True),
      manifest_entry("YG-3", ["t3"], False),
    ]),
  )
  |> should.equal(["t1", "t2", "t3"])
}

pub fn a_verdict_accepts_only_when_every_ruling_is_fixed_test() {
  checks.verdict_accepts(
    Verdict(
      brief_id: "B",
      per_finding: [
        FindingRuling(finding_id: "YG-1", ruling: Fixed, evidence: "e"),
        FindingRuling(finding_id: "YG-2", ruling: Fixed, evidence: "e"),
      ],
      class_siblings_found: [],
    ),
  )
  |> should.equal(True)

  checks.verdict_accepts(
    Verdict(
      brief_id: "B",
      per_finding: [
        FindingRuling(finding_id: "YG-1", ruling: Fixed, evidence: "e"),
        FindingRuling(finding_id: "YG-2", ruling: NotFixed, evidence: "e"),
      ],
      class_siblings_found: [],
    ),
  )
  |> should.equal(False)

  checks.verdict_accepts(
    Verdict(
      brief_id: "B",
      per_finding: [
        FindingRuling(
          finding_id: "YG-1",
          ruling: RegressionIntroduced,
          evidence: "e",
        ),
      ],
      class_siblings_found: [],
    ),
  )
  |> should.equal(False)
}

pub fn a_vacuous_verdict_never_accepts_test() {
  // Zero rulings proved nothing; acceptance requires positive evidence.
  checks.verdict_accepts(
    Verdict(brief_id: "B", per_finding: [], class_siblings_found: []),
  )
  |> should.equal(False)
}

pub fn adverse_rulings_render_id_and_ruling_test() {
  checks.adverse_rulings(
    Verdict(
      brief_id: "B",
      per_finding: [
        FindingRuling(finding_id: "YG-1", ruling: Fixed, evidence: "e"),
        FindingRuling(finding_id: "YG-2", ruling: types.Partial, evidence: "e"),
      ],
      class_siblings_found: [],
    ),
  )
  |> should.equal(["YG-2: partial"])
}

pub fn branch_safe_reduces_hostile_ids_test() {
  checks.branch_safe("B 1/weird~id")
  |> should.equal("B-1-weird-id")
}
