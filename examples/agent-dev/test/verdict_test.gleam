//// Unit tests for the defensive review-verdict extraction.

import agent_dev/verdict
import agent_dev_io as io
import gleeunit/should

pub fn parses_a_bare_verdict_object_test() {
  verdict.parse("{\"pass\": true, \"blockers\": [], \"summary\": \"clean\"}")
  |> should.equal(
    Ok(io.ReviewVerdict(pass: True, blockers: [], summary: "clean")),
  )
}

pub fn parses_a_trailing_verdict_after_prose_test() {
  verdict.parse(
    "I reviewed the adapter thoroughly.\n\nVerdict follows.\n{\"pass\": false, \"blockers\": [\"no fixture test\"], \"summary\": \"changes required\"}",
  )
  |> should.equal(
    Ok(io.ReviewVerdict(
      pass: False,
      blockers: ["no fixture test"],
      summary: "changes required",
    )),
  )
}

pub fn parses_a_verdict_in_a_trailing_code_fence_test() {
  verdict.parse(
    "Done.\n```json\n{\"pass\": true, \"blockers\": [], \"summary\": \"ok\"}\n```",
  )
  |> should.equal(Ok(io.ReviewVerdict(pass: True, blockers: [], summary: "ok")))
}

pub fn parses_a_verdict_whose_strings_contain_braces_test() {
  // The inner `{` inside a blocker string must not derail the innermost-first
  // suffix scan: that suffix fails to parse and the scan walks out to the
  // object's real opening brace.
  verdict.parse(
    "{\"pass\": false, \"blockers\": [\"fix the {} placeholder\"], \"summary\": \"nearly\"}",
  )
  |> should.equal(
    Ok(io.ReviewVerdict(
      pass: False,
      blockers: ["fix the {} placeholder"],
      summary: "nearly",
    )),
  )
}

pub fn tolerates_trailing_whitespace_test() {
  verdict.parse(
    "{\"pass\": true, \"blockers\": [], \"summary\": \"ok\"}\n\n   ",
  )
  |> should.equal(Ok(io.ReviewVerdict(pass: True, blockers: [], summary: "ok")))
}

pub fn rejects_prose_after_the_object_test() {
  // The verdict must be TRAILING: text after the object is a parse failure
  // by design (the bounded re-ask handles it).
  verdict.parse(
    "{\"pass\": true, \"blockers\": [], \"summary\": \"ok\"}\nAnd one more thought...",
  )
  |> should.equal(Error(Nil))
}

pub fn rejects_text_with_no_json_test() {
  verdict.parse("Looks good to me! Ship it.")
  |> should.equal(Error(Nil))
}

pub fn rejects_an_object_of_the_wrong_shape_test() {
  verdict.parse("{\"ok\": true}")
  |> should.equal(Error(Nil))
}

pub fn rejects_empty_text_test() {
  verdict.parse("") |> should.equal(Error(Nil))
}
