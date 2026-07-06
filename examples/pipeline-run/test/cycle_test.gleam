//// Unit tests for the per-unit cap accounting (`pipeline_run/cycle`), driven
//// through the pure [`simulate`] control flow. Each scenario scripts what the
//// reviews and gates would return and asserts the terminal disposition and the
//// cumulative round counts — the exact accounting the child workflow carries.

import gleeunit/should
import pipeline_run/cycle.{CycleSummary}
import pipeline_run/types.{GateCapExhausted, Passed, ReviewCapExhausted}

pub fn first_review_passes_then_gate_passes_test() {
  // 1 review (pass), 1 gate (pass) -> Passed with 1/1.
  cycle.simulate(4, 2, [True], [True])
  |> should.equal(CycleSummary(Passed, 1, 1))
}

pub fn review_iterates_then_passes_then_gate_passes_test() {
  // review fails twice, passes on the third; gate passes first try.
  // rounds = 3 (three reviews run), gate_rounds = 1.
  cycle.simulate(4, 2, [False, False, True], [True])
  |> should.equal(CycleSummary(Passed, 3, 1))
}

pub fn review_never_converges_and_exhausts_its_cap_test() {
  // dev_review_cap = 3, every review fails: exactly 3 reviews run, then
  // ReviewCapExhausted. The gate is never reached.
  cycle.simulate(3, 2, [False, False, False, False], [True])
  |> should.equal(CycleSummary(ReviewCapExhausted, 3, 0))
}

pub fn a_failing_gate_re_enters_review_then_passes_test() {
  // review passes (1), gate fails (1), dev fixes, review passes again (2),
  // gate passes (2) -> Passed with rounds 2, gate_rounds 2.
  cycle.simulate(4, 3, [True, True], [False, True])
  |> should.equal(CycleSummary(Passed, 2, 2))
}

pub fn the_gate_cap_bounds_the_gate_loop_test() {
  // gate_cap = 2: review passes, gate fails, dev fixes, review passes, gate
  // fails again -> GateCapExhausted at 2 gate rounds; 2 review rounds ran.
  cycle.simulate(4, 2, [True, True], [False, False])
  |> should.equal(CycleSummary(GateCapExhausted, 2, 2))
}

pub fn the_review_cap_is_cumulative_across_a_gate_re_entry_test() {
  // dev_review_cap = 2. review passes (round 1), gate fails, dev fixes,
  // re-enter review loop: rounds is already 1; that review fails (round 2)
  // which hits the cap -> ReviewCapExhausted, NOT another review. This is the
  // cumulative-ceiling property: the gate re-entry does not get a fresh budget.
  cycle.simulate(2, 3, [True, False], [False])
  |> should.equal(CycleSummary(ReviewCapExhausted, 2, 1))
}

pub fn a_gate_re_entry_already_at_the_cap_runs_no_further_review_test() {
  // dev_review_cap = 1. First review passes (round 1 = cap). Gate fails; dev
  // fixes; re-enter review loop with rounds already at the cap -> the loop
  // runs NO further review and terminates ReviewCapExhausted with 1 review
  // round and 1 gate round. (Guards the "budget already spent on entry" edge.)
  cycle.simulate(1, 3, [True], [False])
  |> should.equal(CycleSummary(ReviewCapExhausted, 1, 1))
}

pub fn cap_one_that_passes_first_try_reaches_the_gate_test() {
  // dev_review_cap = 1, gate_cap = 1: one review (pass), one gate (pass).
  cycle.simulate(1, 1, [True], [True])
  |> should.equal(CycleSummary(Passed, 1, 1))
}
