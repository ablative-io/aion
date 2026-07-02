//// Unit tests for prompt composition: round-one prompts carry the full
//// contract; resume prompts are lean and feedback-only (the worker pins one
//// norn session per role per run, and the session already holds the
//// contract from its first prompt).

import agent_dev/prompts
import agent_dev_io as io
import gleam/string
import gleeunit/should
import support/harness

fn contains(haystack: String, needle: String) {
  haystack |> string.contains(needle) |> should.be_true
}

fn lacks(haystack: String, needle: String) {
  haystack |> string.contains(needle) |> should.be_false
}

pub fn scout_prompt_carries_the_full_contract_test() {
  let input = harness.base_input()
  let prompt = prompts.scout(input)
  contains(prompt, input.brief_id)
  contains(prompt, input.brief)
  contains(prompt, input.design_notes)
  contains(prompt, "- ruff JSON output parses into DiagnosticEvents")
  contains(prompt, "- the adapter is registered for *.py patterns")
}

pub fn dev_start_prompt_carries_contract_and_scout_plan_test() {
  let input = harness.base_input()
  let prompt = prompts.dev_start(input, "PLAN: mirror the biome adapter")
  contains(prompt, input.brief)
  contains(prompt, input.design_notes)
  contains(prompt, "- ruff JSON output parses into DiagnosticEvents")
  contains(prompt, "PLAN: mirror the biome adapter")
}

pub fn dev_review_feedback_prompt_is_lean_and_carries_blockers_test() {
  let input = harness.base_input()
  let verdict =
    io.ReviewVerdict(
      pass: False,
      blockers: ["no fixture test", "unregistered pattern"],
      summary: "changes required",
    )
  let prompt = prompts.dev_review_feedback(verdict)
  contains(prompt, "- no fixture test")
  contains(prompt, "- unregistered pattern")
  contains(prompt, "changes required")
  // Lean: the pinned session already holds the contract.
  lacks(prompt, input.brief)
  lacks(prompt, input.design_notes)
}

pub fn dev_gate_feedback_prompt_is_lean_and_carries_diagnostics_test() {
  let input = harness.base_input()
  let prompt = prompts.dev_gate_feedback("clippy: unused variable `events`")
  contains(prompt, "clippy: unused variable `events`")
  lacks(prompt, input.brief)
}

pub fn review_start_prompt_carries_contract_report_and_verdict_contract_test() {
  let input = harness.base_input()
  let prompt = prompts.review_start(input, "DEV-REPORT: added the adapter")
  contains(prompt, input.brief)
  contains(prompt, input.design_notes)
  contains(prompt, "DEV-REPORT: added the adapter")
  contains(prompt, prompts.verdict_instruction)
  contains(prompt, "{\"pass\": <true|false>")
}

pub fn review_resume_prompt_is_lean_but_keeps_the_verdict_contract_test() {
  let input = harness.base_input()
  let prompt = prompts.review_resume("DEV-REPORT: applied the feedback")
  contains(prompt, "DEV-REPORT: applied the feedback")
  contains(prompt, prompts.verdict_instruction)
  // Lean: the pinned session already holds the contract.
  lacks(prompt, input.brief)
  lacks(prompt, input.design_notes)
}

pub fn verdict_reask_demands_json_only_test() {
  contains(prompts.verdict_reask, "only the JSON verdict")
  contains(prompts.verdict_reask, "{\"pass\": <true|false>")
}

pub fn empty_acceptance_lists_render_honestly_test() {
  let input = io.Input(..harness.base_input(), acceptance: [])
  contains(prompts.scout(input), "(none listed)")
}
