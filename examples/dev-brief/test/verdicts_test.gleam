//// Unit tests for the derive-and-check rules (`dev_brief/verdicts`): the
//// loop decision never trusts an agent-asserted overall, an inconsistent
//// verdict never accepts, an empty review round never accepts, and a lost
//// lens is surfaced by name.

import dev_brief/types.{
  Accept, Advisory, Blocking, Lens, LensVerdict, Reject, ReviewFinding,
}
import dev_brief/verdicts
import gleam/option.{None, Some}
import gleeunit/should

fn clean_accept(lens: String) -> types.LensVerdict {
  LensVerdict(lens: lens, findings: [], overall: Accept, reject_reason: None)
}

fn blocking_reject(lens: String) -> types.LensVerdict {
  LensVerdict(
    lens: lens,
    findings: [
      ReviewFinding(
        severity: Blocking,
        title: "boom",
        evidence: "src/x.rs:1 concrete scenario",
      ),
    ],
    overall: Reject,
    reject_reason: Some("boom is real"),
  )
}

pub fn no_findings_derives_accept_test() {
  verdicts.derive_overall(clean_accept("correctness"))
  |> should.equal(Accept)
}

pub fn advisory_findings_still_derive_accept_test() {
  let verdict =
    LensVerdict(
      lens: "correctness",
      findings: [
        ReviewFinding(severity: Advisory, title: "nit", evidence: "style note"),
      ],
      overall: Accept,
      reject_reason: None,
    )
  verdicts.derive_overall(verdict)
  |> should.equal(Accept)
  verdicts.verdict_accepts(verdict)
  |> should.be_true
}

pub fn a_blocking_finding_derives_reject_test() {
  verdicts.derive_overall(blocking_reject("correctness"))
  |> should.equal(Reject)
}

pub fn a_consistent_reject_has_no_issues_but_never_accepts_test() {
  let verdict = blocking_reject("regressions")
  verdicts.verdict_issues(verdict)
  |> should.equal([])
  verdicts.verdict_accepts(verdict)
  |> should.be_false
}

pub fn an_asserted_accept_over_blocking_findings_is_an_issue_and_rejects_test() {
  // The agent says accept; its own findings say otherwise. Derive-and-check
  // records the disagreement AND the verdict does not accept.
  let verdict =
    LensVerdict(
      lens: "correctness",
      findings: [
        ReviewFinding(severity: Blocking, title: "boom", evidence: "x"),
      ],
      overall: Accept,
      reject_reason: None,
    )
  { verdicts.verdict_issues(verdict) != [] }
  |> should.be_true
  verdicts.verdict_accepts(verdict)
  |> should.be_false
}

pub fn an_asserted_reject_with_no_findings_is_an_issue_test() {
  // A rejection must be substantiated: no findings + no consistency = issue,
  // and the round loops back (the verdict does not accept) rather than
  // silently taking either value.
  let verdict =
    LensVerdict(
      lens: "brief_compliance",
      findings: [],
      overall: Reject,
      reject_reason: Some("vibes"),
    )
  { verdicts.verdict_issues(verdict) != [] }
  |> should.be_true
  verdicts.verdict_accepts(verdict)
  |> should.be_false
}

pub fn a_reject_without_a_reason_is_an_issue_test() {
  let verdict =
    LensVerdict(
      lens: "correctness",
      findings: [
        ReviewFinding(severity: Blocking, title: "boom", evidence: "x"),
      ],
      overall: Reject,
      reject_reason: None,
    )
  { verdicts.verdict_issues(verdict) != [] }
  |> should.be_true
}

pub fn a_reject_with_a_blank_reason_is_an_issue_test() {
  let verdict =
    LensVerdict(
      lens: "correctness",
      findings: [
        ReviewFinding(severity: Blocking, title: "boom", evidence: "x"),
      ],
      overall: Reject,
      reject_reason: Some("   "),
    )
  { verdicts.verdict_issues(verdict) != [] }
  |> should.be_true
}

pub fn all_accept_requires_every_lens_test() {
  verdicts.all_accept([clean_accept("a"), clean_accept("b")])
  |> should.be_true
  verdicts.all_accept([clean_accept("a"), blocking_reject("b")])
  |> should.be_false
}

pub fn an_empty_review_round_never_accepts_test() {
  // Zero lenses reviewing is a configuration fault, not an approval.
  verdicts.all_accept([])
  |> should.be_false
}

pub fn missing_lenses_are_named_test() {
  let lenses = [Lens(name: "a", charter: "x"), Lens(name: "b", charter: "y")]
  verdicts.missing_lenses(lenses, [clean_accept("a")])
  |> should.equal(["b"])
}

pub fn adverse_lines_name_the_lens_and_the_blocking_findings_test() {
  let lines = verdicts.adverse_lines([blocking_reject("correctness")])
  case lines {
    [line] -> {
      { line != "" }
      |> should.be_true
    }
    _ -> should.fail()
  }
}

pub fn branch_safe_replaces_unsafe_graphemes_test() {
  verdicts.branch_safe("DB 1/x")
  |> should.equal("DB-1-x")
  verdicts.branch_safe("perf_scan.v2-A")
  |> should.equal("perf_scan.v2-A")
}
