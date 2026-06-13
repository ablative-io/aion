//// Scout projection content assertions and the character-budget pin
//// (BD-002 R2). The budget is the checklist's number: scout <= 6000.

import gleam/list
import gleam/string
import gleeunit/should
import stacked_dev/prompts
import support/prompt_fixture

pub fn scout_prompt_fits_the_6000_character_budget_test() {
  let prompt =
    prompts.scout_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
    )
  { string.length(prompt) <= 6000 } |> should.be_true
}

pub fn scout_prompt_carries_the_brief_and_binding_decisions_test() {
  let prompt =
    prompts.scout_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
    )
  string.contains(prompt, "BD-001") |> should.be_true
  string.contains(
    prompt,
    "\nADR-002: No backwards compatibility during the build",
  )
  |> should.be_true
}

pub fn scout_prompt_carries_every_boundary_verbatim_test() {
  let document = prompt_fixture.fixture_document()
  let prompt = prompts.scout_prompt(document, prompt_fixture.fixture_context())
  { document.boundaries != [] } |> should.be_true
  list.each(document.boundaries, fn(boundary) {
    string.contains(prompt, boundary) |> should.be_true
  })
}

pub fn scout_prompt_inlines_the_resolved_c1_text_test() {
  let prompt =
    prompts.scout_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
    )
  string.contains(
    prompt,
    "byte-identical to their docs/design-system/schemas/ canon files",
  )
  |> should.be_true
}

pub fn scout_prompt_carries_the_provenance_quote_with_speaker_test() {
  let context = prompt_fixture.fixture_context()
  let prompt = prompts.scout_prompt(prompt_fixture.fixture_document(), context)
  string.contains(
    prompt,
    prompts.speaker_quote(
      context.provenance.requested_by,
      context.provenance.quote,
    ),
  )
  |> should.be_true
}

pub fn scout_prompt_renders_no_enrichment_blocks_test() {
  let prompt =
    prompts.scout_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
    )
  string.contains(prompt, "enrichments") |> should.be_false
  string.contains(prompt, "Scout findings:") |> should.be_false
  string.contains(prompt, "Dev record:") |> should.be_false
  string.contains(prompt, "Dev attestation") |> should.be_false
  string.contains(prompt, "Measured checks") |> should.be_false
}
