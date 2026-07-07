//// Unit tests for the per-brief fix-cycle cap accounting
//// (`dev_brief/cycle`), driven through the pure [`simulate`] control flow.
//// Each scenario scripts what the gate battery and the review round would
//// return and asserts the terminal disposition and the developer-round
//// count — the exact accounting the workflow carries.

import dev_brief/cycle.{CycleSummary}
import dev_brief/types.{Accepted, CycleCapExhausted}
import gleeunit/should

pub fn first_pass_acceptance_test() {
  // dev (1), gate pass, review accepts -> Accepted after exactly 1 cycle.
  cycle.simulate(3, [True], [True])
  |> should.equal(CycleSummary(Accepted, 1))
}

pub fn adverse_review_loops_back_then_accepts_test() {
  // dev (1), gate pass, review adverse; dev (2), gate pass, review accepts
  // -> Accepted after 2 cycles.
  cycle.simulate(3, [True, True], [False, True])
  |> should.equal(CycleSummary(Accepted, 2))
}

pub fn a_red_gate_consumes_a_cycle_test() {
  // dev (1), gate RED (no review run); dev (2), gate pass, review accepts
  // -> Accepted after 2 cycles: the gate loop and the review loop share ONE
  // budget.
  cycle.simulate(3, [False, True], [True])
  |> should.equal(CycleSummary(Accepted, 2))
}

pub fn review_exhaustion_is_terminal_not_an_error_test() {
  // cap 3, every review adverse: exactly 3 developer rounds run, then the
  // machine stops with CycleCapExhausted — a disposition, never a silent
  // success and never a 4th round.
  cycle.simulate(3, [True, True, True], [False, False, False])
  |> should.equal(CycleSummary(CycleCapExhausted, 3))
}

pub fn gate_exhaustion_terminates_without_reaching_review_test() {
  // cap 2, every gate red: 2 developer rounds, no review ever runs, honest
  // exhaustion.
  cycle.simulate(2, [False, False], [])
  |> should.equal(CycleSummary(CycleCapExhausted, 2))
}

pub fn cap_one_is_a_single_round_test() {
  cycle.simulate(1, [True], [False])
  |> should.equal(CycleSummary(CycleCapExhausted, 1))
}

pub fn resolve_cap_keeps_a_sane_ceiling_test() {
  cycle.resolve_cap(5, 3)
  |> should.equal(5)
}

pub fn resolve_cap_replaces_a_forbidding_value_with_the_default_test() {
  cycle.resolve_cap(0, 3)
  |> should.equal(3)
  cycle.resolve_cap(-2, 3)
  |> should.equal(3)
}
