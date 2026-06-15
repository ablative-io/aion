//// Review projection content assertions and the character-budget pin
//// (BD-002 R4). The budget is the checklist's number: review <= 12000.
//// The attestation and the measured checks render as two distinct
//// labelled sections so divergence between them is visible signal (P1).

import gleam/list
import gleam/string
import gleeunit/should
import stacked_dev/prompts
import stacked_dev/types.{CheckFail, CheckResult}
import support/prompt_fixture

fn fixture_prompt() -> String {
  prompts.review_prompt(
    prompt_fixture.fixture_document(),
    prompt_fixture.fixture_context(),
    prompt_fixture.fixture_dev(),
    prompt_fixture.fixture_check(),
  )
}

pub fn review_prompt_fits_the_12000_character_budget_test() {
  { string.length(fixture_prompt()) <= 12_000 } |> should.be_true
}

pub fn review_prompt_carries_every_r1_acceptance_criterion_verbatim_test() {
  let document = prompt_fixture.fixture_document()
  let assert Ok(r1) = list.first(document.requirements)
  let prompt = fixture_prompt()
  { r1.acceptance != [] } |> should.be_true
  list.each(r1.acceptance, fn(criterion) {
    string.contains(prompt, criterion) |> should.be_true
  })
}

pub fn attestation_and_measured_results_are_distinct_sections_test() {
  let prompt = fixture_prompt()
  string.contains(
    prompt,
    "Dev attestation (the dev's claim, not a gate outcome):",
  )
  |> should.be_true
  string.contains(prompt, "Measured checks (measured by the workflow):")
  |> should.be_true
}

pub fn attestation_and_failing_checks_both_render_as_divergence_test() {
  let failing =
    CheckResult(
      verdict: CheckFail(
        "FIXTURE-MEASURED-DIAGNOSTICS: 1 test failed in stacked_dev",
      ),
      affected_modules: ["stacked_dev"],
      checked_scope: "workspace-wide",
    )
  let prompt =
    prompts.review_prompt(
      prompt_fixture.fixture_document(),
      prompt_fixture.fixture_context(),
      prompt_fixture.fixture_dev(),
      failing,
    )
  string.contains(prompt, "tests_pass: true") |> should.be_true
  string.contains(prompt, "verdict: fail") |> should.be_true
  string.contains(prompt, "FIXTURE-MEASURED-DIAGNOSTICS") |> should.be_true
}

pub fn review_prompt_inlines_dev_blocks_per_requirement_test() {
  let prompt = fixture_prompt()
  let assert Ok(#(r1_section, r2_section)) =
    string.split_once(prompt, "R2 — Schema drift gate")
  string.contains(r1_section, "FIXTURE-DEV-DEVIATION-R1") |> should.be_true
  string.contains(r2_section, "FIXTURE-DEV-DEVIATION-R2") |> should.be_true
}

pub fn review_prompt_excludes_the_scout_test() {
  // The reviewer is never handed the scout's orientation (ADR-010); only the
  // dev record, attestation, and measured checks reach it.
  string.contains(fixture_prompt(), "FIXTURE-SCOUT-APPROACH-R1")
  |> should.be_false
}

pub fn review_prompt_carries_every_verification_step_test() {
  let document = prompt_fixture.fixture_document()
  let prompt = fixture_prompt()
  { document.verification != [] } |> should.be_true
  list.each(document.verification, fn(step) {
    string.contains(prompt, step) |> should.be_true
  })
}
