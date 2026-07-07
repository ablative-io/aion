//// Unit tests for the wave-plan strata validation (`remediation/wave`):
//// serial-strata ordering is GIVEN by the signed plan, so the tests pin the
//// runnability rules — every id known, no brief twice, no brief dropped.
////
//// Also covers the Change-2 non-cascade reducer (`WaveProgress`): the SAME
//// pure `fold_stratum` transitions `remediation_wave.execute` drives stratum
//// by stratum, so a test here is a test of the production policy — no
//// engine or child mocking required.

import gleam/list
import gleam/option.{None, Some}
import gleeunit/should
import remediation/types.{
  Brief, TestManifest, WaveBrief, WaveBriefFailure, WaveBriefSkip,
}
import remediation/wave.{
  BriefCompleted, BriefRunFailed, DuplicateBrief, EmptyPlan, MissingBrief,
  UnknownBrief,
}

fn wave_brief(id: String, wave_number: Int) -> types.WaveBrief {
  WaveBrief(
    brief: Brief(
      id: id,
      finding_ids: ["YG-1"],
      root_cause: "rc",
      files_expected: [],
      boundaries: [],
      acceptance: ["a"],
      wave: wave_number,
      deep_cluster: False,
    ),
    entries: [],
  )
}

pub fn a_complete_plan_validates_test() {
  wave.validate(
    [wave_brief("B-1", 0), wave_brief("B-2", 0), wave_brief("B-3", 0)],
    [["B-1", "B-2"], ["B-3"]],
  )
  |> should.equal(Ok(Nil))
}

pub fn an_empty_plan_is_rejected_test() {
  wave.validate([], [])
  |> should.equal(Error(EmptyPlan))
}

pub fn an_unknown_brief_id_is_rejected_test() {
  wave.validate([wave_brief("B-1", 0)], [["B-1", "B-9"]])
  |> should.equal(Error(UnknownBrief("B-9")))
}

pub fn a_duplicated_brief_id_is_rejected_test() {
  // Duplicates are rejected whether within one stratum or across strata.
  wave.validate([wave_brief("B-1", 0), wave_brief("B-2", 0)], [
    ["B-1"],
    ["B-2", "B-1"],
  ])
  |> should.equal(Error(DuplicateBrief("B-1")))
}

pub fn a_brief_in_no_stratum_is_rejected_as_silently_dropped_test() {
  wave.validate([wave_brief("B-1", 0), wave_brief("B-2", 0)], [["B-1"]])
  |> should.equal(Error(MissingBrief("B-2")))
}

pub fn every_rejection_names_the_offending_brief_test() {
  wave.strata_error_message(UnknownBrief("B-9"))
  |> should.equal("stratum names unknown brief `B-9`")
  wave.strata_error_message(MissingBrief("B-2"))
  |> should.equal(
    "brief `B-2` appears in no stratum and would silently never run",
  )
}

pub fn wave_number_is_the_highest_brief_wave_test() {
  wave.wave_number([wave_brief("B-1", 0), wave_brief("B-2", 2)])
  |> should.equal(2)
  wave.wave_number([])
  |> should.equal(0)
}

// --- Change 2: wave non-cascade on child (brief) failure -------------------
//
// Real incident, 2026-07-07: a transient provider error failed a
// remediation_brief child workflow, and the parent remediation_wave cascaded
// to Failed, losing the wave's whole bookkeeping — results for briefs that
// had already succeeded were lost. These tests drive `wave.fold_stratum`
// exactly the way `remediation_wave.run_stratum` does: skip (via an empty
// outcome list) once `blocked_by` is `Some`, otherwise fold in the real
// per-brief outcomes — so this IS a test of the production policy.

fn stub_brief_result(id: String) -> types.BriefResult {
  types.BriefResult(
    brief_id: id,
    disposition: types.Accepted,
    fix_cycles: 1,
    first_pass_accepted: True,
    could_not_reproduce: [],
    test_edit_attempts: 0,
    verdict_mismatches: [],
    branch: "remediation/" <> id,
    manifest: TestManifest(brief_id: id, entries: []),
    fix_report: None,
    verdict: None,
    ledger: [],
    workspace_removed: True,
    summary: "",
  )
}

/// The exact decision `remediation_wave.run_stratum` makes for each stratum:
/// once the wave is blocked, later strata are folded in with NO outcomes (the
/// caller never even spawns them); otherwise the given outcomes are folded.
fn drive_wave(
  strata: List(#(List(String), List(#(String, wave.BriefRunOutcome)))),
) -> wave.WaveProgress {
  list.fold(strata, wave.empty_progress(), fn(progress, entry) {
    let #(stratum, outcomes) = entry
    case progress.blocked_by {
      Some(_) -> wave.fold_stratum(progress, stratum, [])
      None -> wave.fold_stratum(progress, stratum, outcomes)
    }
  })
}

pub fn a_failed_sibling_does_not_stop_the_rest_of_its_own_stratum_test() {
  // B-1 succeeds, B-2 fails — both dispatched CONCURRENTLY in the same
  // stratum, so B-2's failure must not erase B-1's already-landed result.
  let progress =
    wave.fold_stratum(wave.empty_progress(), ["B-1", "B-2"], [
      #("B-1", BriefCompleted(stub_brief_result("B-1"))),
      #("B-2", BriefRunFailed("transient provider error")),
    ])

  progress.succeeded |> should.equal([stub_brief_result("B-1")])
  progress.failed
  |> should.equal([
    WaveBriefFailure(brief_id: "B-2", reason: "transient provider error"),
  ])
  progress.blocked_by |> should.equal(Some(["B-2"]))
}

pub fn subsequent_strata_are_skipped_naming_the_blocking_brief_test() {
  let progress =
    drive_wave([
      #(["B-1", "B-2"], [
        #("B-1", BriefCompleted(stub_brief_result("B-1"))),
        #("B-2", BriefRunFailed("transient provider error")),
      ]),
      #(["B-3"], [#("B-3", BriefCompleted(stub_brief_result("B-3")))]),
      #(["B-4", "B-5"], [
        #("B-4", BriefCompleted(stub_brief_result("B-4"))),
        #("B-5", BriefCompleted(stub_brief_result("B-5"))),
      ]),
    ])

  // B-3, B-4, B-5's scripted outcomes are never consulted: once blocked, the
  // stratum is skipped outright (mirroring that the parent never even spawns
  // them), each naming B-2 as the blocking brief.
  progress.skipped
  |> should.equal([
    WaveBriefSkip(
      brief_id: "B-3",
      blocking_brief_ids: ["B-2"],
      reason: wave.skip_reason(["B-2"]),
    ),
    WaveBriefSkip(
      brief_id: "B-4",
      blocking_brief_ids: ["B-2"],
      reason: wave.skip_reason(["B-2"]),
    ),
    WaveBriefSkip(
      brief_id: "B-5",
      blocking_brief_ids: ["B-2"],
      reason: wave.skip_reason(["B-2"]),
    ),
  ])
}

pub fn the_wave_completes_with_the_full_per_brief_outcome_map_test() {
  // Every brief across all three strata is accounted for EXACTLY once,
  // across succeeded/failed/skipped — the wave never silently drops one, and
  // (by construction: `fold_stratum` has no error path) never fails the
  // whole wave over one child's failure.
  let progress =
    drive_wave([
      #(["B-1", "B-2"], [
        #("B-1", BriefCompleted(stub_brief_result("B-1"))),
        #("B-2", BriefRunFailed("transient provider error")),
      ]),
      #(["B-3"], [#("B-3", BriefCompleted(stub_brief_result("B-3")))]),
    ])

  progress.succeeded |> should.equal([stub_brief_result("B-1")])
  progress.failed
  |> should.equal([
    WaveBriefFailure(brief_id: "B-2", reason: "transient provider error"),
  ])
  progress.skipped
  |> should.equal([
    WaveBriefSkip(
      brief_id: "B-3",
      blocking_brief_ids: ["B-2"],
      reason: wave.skip_reason(["B-2"]),
    ),
  ])
}

pub fn every_stratum_succeeding_leaves_the_wave_unblocked_test() {
  let progress =
    drive_wave([
      #(["B-1"], [#("B-1", BriefCompleted(stub_brief_result("B-1")))]),
      #(["B-2"], [#("B-2", BriefCompleted(stub_brief_result("B-2")))]),
    ])

  progress.succeeded
  |> should.equal([stub_brief_result("B-1"), stub_brief_result("B-2")])
  progress.failed |> should.equal([])
  progress.skipped |> should.equal([])
  progress.blocked_by |> should.equal(None)
}

pub fn multiple_failures_in_one_stratum_name_every_blocking_brief_test() {
  let progress =
    drive_wave([
      #(["B-1", "B-2"], [
        #("B-1", BriefRunFailed("timeout")),
        #("B-2", BriefRunFailed("declined")),
      ]),
      #(["B-3"], [#("B-3", BriefCompleted(stub_brief_result("B-3")))]),
    ])

  progress.failed
  |> should.equal([
    WaveBriefFailure(brief_id: "B-1", reason: "timeout"),
    WaveBriefFailure(brief_id: "B-2", reason: "declined"),
  ])
  progress.skipped
  |> should.equal([
    WaveBriefSkip(
      brief_id: "B-3",
      blocking_brief_ids: ["B-1", "B-2"],
      reason: wave.skip_reason(["B-1", "B-2"]),
    ),
  ])
}
