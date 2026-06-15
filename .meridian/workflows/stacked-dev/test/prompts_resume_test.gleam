//// Resume-feedback projection assertions (BD-002 R5): the fix-round
//// prompt carries the brief id, the wholesale-replacement instruction,
//// the diagnostics verbatim, and the boundaries — and does NOT re-render
//// the requirements (the resumed session already holds them).

import gleam/list
import gleam/string
import gleeunit/should
import stacked_dev/prompts
import support/prompt_fixture

pub fn resume_feedback_carries_the_diagnostics_verbatim_test() {
  let prompt =
    prompts.resume_feedback(
      prompt_fixture.fixture_document(),
      "error[E0308]: mismatched types",
    )
  string.contains(prompt, "error[E0308]: mismatched types") |> should.be_true
}

pub fn resume_feedback_carries_brief_id_and_every_boundary_test() {
  let document = prompt_fixture.fixture_document()
  let prompt = prompts.resume_feedback(document, "one failing check")
  string.contains(prompt, "BD-001") |> should.be_true
  { document.boundaries != [] } |> should.be_true
  list.each(document.boundaries, fn(boundary) {
    string.contains(prompt, boundary) |> should.be_true
  })
}

pub fn resume_feedback_demands_a_full_replacement_dev_report_test() {
  let prompt =
    prompts.resume_feedback(prompt_fixture.fixture_document(), "diag")
  string.contains(prompt, "full replacement dev report") |> should.be_true
  string.contains(prompt, "never a partial field merge") |> should.be_true
}

pub fn resume_feedback_does_not_re_render_the_requirements_test() {
  let document = prompt_fixture.fixture_document()
  let assert Ok(r1) = list.first(document.requirements)
  let prompt = prompts.resume_feedback(document, "diag")
  string.contains(prompt, r1.spec) |> should.be_false
}
