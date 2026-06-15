//// Shared rendering assertions for the prompt projections (BD-002 R1):
//// the decision-line format, verbatim quote attribution, and the static
//// per-stage instruction constants.

import gleam/list
import gleam/string
import gleeunit/should
import stacked_dev/prompts
import stacked_dev/types.{ResolvedAdr}

/// ADR-003 with its ledger texts verbatim (docs/design/decisions.json).
fn adr_003() -> types.ResolvedAdr {
  ResolvedAdr(
    id: "ADR-003",
    title: "No default timeouts anywhere in the engine",
    decision: "The engine imposes no activity time bound of its own. "
      <> "Activity waits are unbounded, terminated only by completion, "
      <> "worker loss, server shutdown, or a workflow-level timeout the "
      <> "author explicitly chose. Rejected: a bigger default — agentic "
      <> "activities legitimately run for over an hour, and any number we "
      <> "picked would be ADR-001 violated.",
    quote: "we shouldn't have a default timeout, the agent steps can take "
      <> "well over an hour",
    decided_by: "Tom",
  )
}

pub fn decision_line_renders_id_title_and_decision_test() {
  let rendered = prompts.decision_context([adr_003()])
  let assert [first, ..] = string.split(rendered, "\n")
  string.starts_with(
    first,
    "ADR-003: No default timeouts anywhere in the engine — The engine "
      <> "imposes no activity time bound of its own.",
  )
  |> should.be_true
}

pub fn attribution_line_follows_the_decision_line_test() {
  let rendered = prompts.decision_context([adr_003()])
  let assert [_, attribution] = string.split(rendered, "\n")
  attribution
  |> should.equal(
    "Tom: \"we shouldn't have a default timeout, the agent steps can take "
    <> "well over an hour\"",
  )
}

pub fn empty_quote_renders_exactly_one_line_test() {
  let adr = ResolvedAdr(..adr_003(), quote: "")
  let rendered = prompts.decision_context([adr])
  let assert [_] = string.split(rendered, "\n")
  string.contains(rendered, "\"\"") |> should.be_false
}

pub fn adrs_render_one_entry_each_in_the_order_given_test() {
  let second = ResolvedAdr(..adr_003(), id: "ADR-009", quote: "")
  let rendered = prompts.decision_context([adr_003(), second])
  let assert Ok(#(before, after)) = string.split_once(rendered, "ADR-009:")
  string.contains(before, "ADR-003:") |> should.be_true
  string.contains(after, "ADR-003:") |> should.be_false
}

pub fn instruction_constants_are_bounded_and_non_empty_test() {
  [
    prompts.scout_instructions,
    prompts.dev_instructions,
    prompts.review_instructions,
  ]
  |> list.each(fn(instructions) {
    { instructions != "" } |> should.be_true
    { string.length(instructions) <= 900 } |> should.be_true
  })
}

pub fn scout_instructions_declare_the_stage_read_only_test() {
  string.contains(prompts.scout_instructions, "read-only") |> should.be_true
}

pub fn dev_instructions_defer_the_gate_to_the_workflow_test() {
  string.contains(prompts.dev_instructions, "Stay inside the boundaries")
  |> should.be_true
  string.contains(prompts.dev_instructions, "deviation") |> should.be_true
}

pub fn review_instructions_direct_verifying_the_actual_diff_test() {
  string.contains(
    prompts.review_instructions,
    "Verify the actual diff, never the dev report",
  )
  |> should.be_true
}
