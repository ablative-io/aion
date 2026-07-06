//// Unit tests for the wave-plan strata validation (`remediation/wave`):
//// serial-strata ordering is GIVEN by the signed plan, so the tests pin the
//// runnability rules — every id known, no brief twice, no brief dropped.

import gleeunit/should
import remediation/types.{Brief, WaveBrief}
import remediation/wave.{DuplicateBrief, EmptyPlan, MissingBrief, UnknownBrief}

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
