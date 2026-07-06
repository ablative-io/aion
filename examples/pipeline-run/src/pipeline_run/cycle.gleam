//// The per-unit dev/review/gate cap accounting, as a PURE state machine.
////
//// The child `pipeline_unit` workflow drives this machine as a trampoline: it
//// asks [`plan`] for the next [`Instruction`], performs exactly that one effect
//// (a driven review, a driven dev resume, or a cargo gate), and folds the
//// outcome back with the matching `on_*` transition. Because every branch is a
//// pure function of the machine and a single pass/fail bit, the whole cap
//// logic is unit-tested without the engine, agents, or cargo (`test/cycle_test`
//// via [`simulate`]) — and the tested transitions ARE the production
//// transitions, so the model can never drift from the workflow.
////
//// The semantics mirror meridian_dev_pipeline: the dev<->review loop is bounded
//// by `dev_review_cap` (a CUMULATIVE ceiling across the whole cycle, including
//// the loops a gate failure re-enters); the gate loop by `gate_cap`. Exhausting
//// either cap is not an error — it is a terminal [`Disposition`] the unit still
//// returns (and the parent still lands what it can and notifies).

import pipeline_run/types.{
  type Disposition, GateCapExhausted, Passed, ReviewCapExhausted,
}

/// The cap-accounting state. `rounds` is the cumulative count of reviews run;
/// `gate_rounds` the count of gates run; `review_started` records whether the
/// single review session exists yet (so the next review is a resume). `phase`
/// is the position in the dev/review/gate flow.
pub type Machine {
  Machine(
    rounds: Int,
    gate_rounds: Int,
    review_started: Bool,
    dev_review_cap: Int,
    gate_cap: Int,
    phase: Phase,
  )
}

/// Where the cycle is. Not exposed to the workflow beyond [`plan`]/`on_*`.
pub type Phase {
  /// About to run a review (the cap is re-checked here before running).
  EnterReview
  /// The last review failed below cap: resume dev with the findings, then
  /// review again.
  AfterReviewFail
  /// About to run the cargo gate.
  EnterGate
  /// The last gate failed below cap: resume dev with the diagnostics, then
  /// re-enter the review loop on the fix.
  AfterGateFail
  /// Terminal: the cycle is done with this disposition.
  Stopped(Disposition)
}

/// The single effect the trampoline should perform next.
pub type Instruction {
  /// Run a review round — `resume` is true once the review session exists.
  Review(resume: Bool)
  /// Run the cargo gate.
  Gate
  /// Resume the dev session with the latest review findings (no branch).
  DevReview
  /// Resume the dev session with the latest gate diagnostics (no branch).
  DevGate
  /// Stop: the cycle reached this terminal disposition.
  Stop(Disposition)
}

/// The machine a unit enters after its first dev round (`dev_start`) has run:
/// nothing reviewed yet, at the head of the review loop.
pub fn initial(dev_review_cap: Int, gate_cap: Int) -> Machine {
  Machine(
    rounds: 0,
    gate_rounds: 0,
    review_started: False,
    dev_review_cap: dev_review_cap,
    gate_cap: gate_cap,
    phase: EnterReview,
  )
}

/// The next instruction, a pure function of the machine's phase and caps.
pub fn plan(machine: Machine) -> Instruction {
  case machine.phase {
    EnterReview ->
      // Cap checked BEFORE each review so the cumulative budget is never
      // overrun. Only reachable at the cap on a gate-driven re-entry — the
      // first entry is rounds = 0 with dev_review_cap >= 1.
      case machine.rounds >= machine.dev_review_cap {
        True -> Stop(ReviewCapExhausted)
        False -> Review(resume: machine.review_started)
      }
    AfterReviewFail -> DevReview
    EnterGate -> Gate
    AfterGateFail -> DevGate
    Stopped(disposition) -> Stop(disposition)
  }
}

/// Fold a completed review's verdict (`pass`) into the machine.
pub fn on_review(machine: Machine, pass: Bool) -> Machine {
  let rounds = machine.rounds + 1
  let phase = case pass {
    True -> EnterGate
    False ->
      case rounds >= machine.dev_review_cap {
        True -> Stopped(ReviewCapExhausted)
        False -> AfterReviewFail
      }
  }
  Machine(..machine, rounds: rounds, review_started: True, phase: phase)
}

/// Fold a completed gate's verdict (`pass`) into the machine.
pub fn on_gate(machine: Machine, pass: Bool) -> Machine {
  let gate_rounds = machine.gate_rounds + 1
  let phase = case pass {
    True -> Stopped(Passed)
    False ->
      case gate_rounds >= machine.gate_cap {
        True -> Stopped(GateCapExhausted)
        False -> AfterGateFail
      }
  }
  Machine(..machine, gate_rounds: gate_rounds, phase: phase)
}

/// Fold a completed dev-resume-with-review-findings back to the review loop.
pub fn on_dev_review(machine: Machine) -> Machine {
  Machine(..machine, phase: EnterReview)
}

/// Fold a completed dev-resume-with-gate-diagnostics back to the review loop
/// (the fix must survive review again before it is re-gated).
pub fn on_dev_gate(machine: Machine) -> Machine {
  Machine(..machine, phase: EnterReview)
}

/// The terminal disposition of a stopped machine, or `None` while it runs.
pub fn disposition(machine: Machine) -> Result(Disposition, Nil) {
  case machine.phase {
    Stopped(value) -> Ok(value)
    _ -> Error(Nil)
  }
}

// --- pure simulation (test driver AND executable spec) ---------------------

/// The terminal accounting of a simulated cycle: disposition and the two
/// round counts, exactly as the workflow would carry them.
pub type CycleSummary {
  CycleSummary(
    disposition: Disposition,
    dev_review_rounds: Int,
    gate_rounds: Int,
  )
}

/// Drive the machine to termination against SCRIPTED outcomes: `reviews` is the
/// pass/fail each review returns in order, `gates` the pass/fail each gate
/// returns in order. This is the exact control flow the child workflow runs,
/// with the effects replaced by reading the next scripted bit — so a test over
/// [`simulate`] is a test of the production accounting.
///
/// A script that runs out of outcomes before the machine stops is a mis-written
/// scenario; it terminates as if the missing outcome were a fail, keeping the
/// function total (tests supply enough outcomes; production never runs dry
/// because the caps bound the number of rounds).
pub fn simulate(
  dev_review_cap: Int,
  gate_cap: Int,
  reviews: List(Bool),
  gates: List(Bool),
) -> CycleSummary {
  drive(initial(dev_review_cap, gate_cap), reviews, gates)
}

fn drive(
  machine: Machine,
  reviews: List(Bool),
  gates: List(Bool),
) -> CycleSummary {
  case plan(machine) {
    Stop(disposition) ->
      CycleSummary(
        disposition: disposition,
        dev_review_rounds: machine.rounds,
        gate_rounds: machine.gate_rounds,
      )
    Review(_resume) ->
      case reviews {
        [pass, ..rest] -> drive(on_review(machine, pass), rest, gates)
        [] -> drive(on_review(machine, False), [], gates)
      }
    Gate ->
      case gates {
        [pass, ..rest] -> drive(on_gate(machine, pass), reviews, rest)
        [] -> drive(on_gate(machine, False), reviews, [])
      }
    DevReview -> drive(on_dev_review(machine), reviews, gates)
    DevGate -> drive(on_dev_gate(machine), reviews, gates)
  }
}
