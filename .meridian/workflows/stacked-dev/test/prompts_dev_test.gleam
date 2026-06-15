//// Dev projection content assertions and the character-budget pin
//// (BD-002 R3). The budget is the checklist's number: dev <= 9000.

import aion_stacked_dev_io as stage_io
import gleam/list
import gleam/string
import gleeunit/should
import stacked_dev/prompts
import support/prompt_fixture

pub fn dev_prompt_fits_the_9000_character_budget_test() {
  let prompt =
    prompts.dev_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
      prompt_fixture.fixture_scout(),
    )
  { string.length(prompt) <= 9000 } |> should.be_true
}

pub fn dev_prompt_inlines_each_scout_block_under_its_requirement_test() {
  let prompt =
    prompts.dev_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
      prompt_fixture.fixture_scout(),
    )
  let assert Ok(#(r1_section, r2_section)) =
    string.split_once(prompt, "R2 — Schema drift gate")
  string.contains(r1_section, "FIXTURE-SCOUT-APPROACH-R1") |> should.be_true
  string.contains(r1_section, "FIXTURE-SCOUT-APPROACH-R2") |> should.be_false
  string.contains(r2_section, "FIXTURE-SCOUT-APPROACH-R2") |> should.be_true
}

pub fn missing_scout_enrichment_renders_a_loud_marker_test() {
  let scout = prompt_fixture.fixture_scout()
  let partial =
    stage_io.ScoutReport(
      ..scout,
      enrichments: list.filter(scout.enrichments, fn(entry) { entry.id != "R2" }),
    )
  let prompt =
    prompts.dev_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
      partial,
    )
  string.contains(prompt, "Schema drift gate script wired into the test suite")
  |> should.be_true
  string.contains(prompt, "scout: none recorded") |> should.be_true
}

pub fn dev_instructions_demand_declared_deviations_test() {
  let prompt =
    prompts.dev_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
      prompt_fixture.fixture_scout(),
    )
  string.contains(prompt, "deviation") |> should.be_true
}

pub fn dev_prompt_carries_every_boundary_verbatim_test() {
  let document = prompt_fixture.fixture_document()
  let prompt =
    prompts.dev_prompt(
      document,
      prompt_fixture.fixture_context(),
      prompt_fixture.fixture_scout(),
    )
  list.each(document.boundaries, fn(boundary) {
    string.contains(prompt, boundary) |> should.be_true
  })
}
