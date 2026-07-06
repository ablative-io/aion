//// Terminal ledger-pass contract tests for the child workflow.
////
//// THE LOAD-BEARING ONE: a gate-1-failed finish (no fix report, no verdict)
//// schedules exactly ONE ledger update — the test manifest — and NO finish
//// ever emits a `disposition` artifact. The applier's disposition kind is an
//// operator-signed ruling (refuted|deferred, DECISIONS.md D9); the live
//// drill proved the applier rejects a workflow-emitted one (missing
//// finding_ids/evidence/signed_by, brief_id not allowed), so the workflow
//// must never build one.

import gleam/list
import gleam/option.{None, Some}
import gleam/string
import gleeunit/should
import remediation/types.{
  ClassInstance, Deviation, FindingBounce, FindingFix, FindingRuling, FixReport,
  Fixed, ManifestEntry, TestManifest, Verdict,
}
import remediation_brief

fn manifest() -> types.TestManifest {
  TestManifest(brief_id: "B-1", entries: [
    ManifestEntry(
      finding_id: "YG-268",
      test_names: ["teardown::refuses_dirty_worktree"],
      test_file: "crates/yg-core/tests/yg268_teardown.rs",
      expected_failure_signature: "teardown deleted uncommitted work",
      fail_evidence: "assertion failed: teardown deleted uncommitted work",
      could_not_reproduce: False,
      could_not_reproduce_reason: None,
      manual_acceptance: None,
    ),
  ])
}

fn fix_report() -> types.FixReport {
  FixReport(
    brief_id: "B-1",
    commits: ["abc123"],
    findings_addressed: [
      FindingFix(finding_id: "YG-268", how: "added the dirty check"),
    ],
    findings_bounced: [
      FindingBounce(finding_id: "YG-367", reason: "unreachable since 9c2f11"),
    ],
    deviations: [
      Deviation(what: "touched cli", why: "shared helper", approved_by: "p"),
    ],
    new_tests: [],
    class_instances_found: [
      ClassInstance(file: "sync.rs", line: 189, fixed: True, note: "same"),
    ],
  )
}

fn verdict() -> types.Verdict {
  Verdict(
    brief_id: "B-1",
    per_finding: [
      FindingRuling(finding_id: "YG-268", ruling: Fixed, evidence: "read it"),
    ],
    class_siblings_found: [],
    regression_risks: [],
    standards_violations: [],
    overall: types.Accept,
    reject_reason: None,
  )
}

/// The gate-1-failed early exit (and any finish before the fix cycle ran):
/// no fix report, no verdict — exactly ONE ledger update is scheduled, the
/// test manifest. In particular: NO disposition artifact.
pub fn gate1_failed_finish_schedules_only_the_test_manifest_update_test() {
  let artifacts =
    remediation_brief.terminal_artifacts(
      manifest(),
      fix_report: None,
      verdict: None,
    )
  artifacts
  |> list.map(fn(artifact) { types.artifact_kind_to_string(artifact.0) })
  |> should.equal(["test_manifest"])
}

/// The full accepted path applies the three agent-stage artifacts in stage
/// order — and still no disposition.
pub fn full_finish_schedules_the_three_stage_artifacts_in_order_test() {
  let artifacts =
    remediation_brief.terminal_artifacts(
      manifest(),
      fix_report: Some(fix_report()),
      verdict: Some(verdict()),
    )
  artifacts
  |> list.map(fn(artifact) { types.artifact_kind_to_string(artifact.0) })
  |> should.equal(["test_manifest", "fix_report", "verdict"])
}

/// No finish, on any path, ever builds a disposition artifact: the kind is
/// operator-signed at the applier and the workflow cannot speak for the
/// operator.
pub fn no_terminal_artifact_is_ever_a_disposition_test() {
  [
    remediation_brief.terminal_artifacts(
      manifest(),
      fix_report: None,
      verdict: None,
    ),
    remediation_brief.terminal_artifacts(
      manifest(),
      fix_report: Some(fix_report()),
      verdict: None,
    ),
    remediation_brief.terminal_artifacts(
      manifest(),
      fix_report: Some(fix_report()),
      verdict: Some(verdict()),
    ),
  ]
  |> list.flatten
  |> list.filter(fn(artifact) {
    artifact.0 == types.DispositionArtifact
    || string.contains(artifact.1, "\"disposition\"")
  })
  |> should.equal([])
}
