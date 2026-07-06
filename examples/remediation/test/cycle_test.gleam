//// Unit tests for the per-brief fix-cycle cap accounting
//// (`remediation/cycle`), driven through the pure [`simulate`] control flow.
//// Each scenario scripts what gate 2 and the verifier would return and
//// asserts the terminal disposition and the developer-round count — the
//// exact accounting the child workflow carries.

import gleeunit/should
import remediation/cycle.{CycleSummary}
import remediation/types.{Accepted, CycleCapExhausted}

pub fn first_pass_acceptance_test() {
  // dev (1), gate2 pass, verdict accepts -> Accepted after exactly 1 cycle.
  cycle.simulate(3, [True], [True])
  |> should.equal(CycleSummary(Accepted, 1))
}

pub fn adverse_verdict_loops_back_then_accepts_test() {
  // dev (1), gate2 pass, verdict adverse; dev (2), gate2 pass, verdict
  // accepts -> Accepted after 2 cycles.
  cycle.simulate(3, [True, True], [False, True])
  |> should.equal(CycleSummary(Accepted, 2))
}

pub fn a_red_gate2_consumes_a_cycle_test() {
  // dev (1), gate2 RED (no verifier run); dev (2), gate2 pass, verdict
  // accepts -> Accepted after 2 cycles: the gate loop and the verdict loop
  // share ONE budget.
  cycle.simulate(3, [False, True], [True])
  |> should.equal(CycleSummary(Accepted, 2))
}

pub fn verdict_exhaustion_is_terminal_not_an_error_test() {
  // cap 3, every verdict adverse: exactly 3 developer rounds run, then the
  // machine stops with CycleCapExhausted — a disposition, never a silent
  // success and never a 4th round.
  cycle.simulate(3, [True, True, True], [False, False, False])
  |> should.equal(CycleSummary(CycleCapExhausted, 3))
}

pub fn gate_exhaustion_terminates_without_reaching_the_verifier_test() {
  // cap 2, gate2 red twice: dev (1), gate red; dev (2), gate red -> the cap
  // check refuses a 3rd round. The verifier never ran.
  cycle.simulate(2, [False, False], [True])
  |> should.equal(CycleSummary(CycleCapExhausted, 2))
}

pub fn mixed_gate_and_verdict_loopbacks_share_the_budget_test() {
  // cap 2: dev (1), gate red; dev (2), gate pass, verdict adverse -> the
  // budget is spent; exhausted at 2 rounds (the adverse verdict cannot buy a
  // 3rd).
  cycle.simulate(2, [False, True], [False])
  |> should.equal(CycleSummary(CycleCapExhausted, 2))
}

pub fn cap_one_gives_exactly_one_shot_test() {
  cycle.simulate(1, [True], [False])
  |> should.equal(CycleSummary(CycleCapExhausted, 1))
}

pub fn resolve_cap_accepts_a_sane_override_test() {
  cycle.resolve_cap(5, 3)
  |> should.equal(5)
}

pub fn resolve_cap_falls_back_on_a_nonsense_value_test() {
  // A cap below 1 would forbid the first developer round; it resolves to the
  // default rather than deadlocking.
  cycle.resolve_cap(0, 3)
  |> should.equal(3)
  cycle.resolve_cap(-2, 3)
  |> should.equal(3)
}
