//// The per-brief fix-cycle cap accounting, as a PURE state machine (the
//// pipeline-run `cycle` discipline applied to the remediation loop).
////
//// The child `remediation_brief` workflow drives this machine as a
//// trampoline: it asks [`plan`] for the next [`Instruction`], performs
//// exactly that one effect (a driven developer round, the mechanical gate 2,
//// or a driven verification), and folds the outcome back with the matching
//// `on_*` transition. Every branch is a pure function of the machine and a
//// single bit, so the whole cap logic is unit-tested without the engine,
//// agents, or cargo (`test/cycle_test` via [`simulate`]) — and the tested
//// transitions ARE the production transitions.
////
//// Semantics (task contract): `max_fix_cycles` bounds the number of DEVELOPER
//// rounds — the initial fix plus every loop-back, whether the loop-back came
//// from a failing gate 2 or an adverse verdict (`partial` / `not_fixed` /
//// `regression_introduced`). Exhausting the cap is not an error: it is the
//// terminal [`CycleCapExhausted`] disposition the brief still returns and the
//// ledger still records — never a silent success.

import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import remediation/types.{type Disposition, Accepted, CycleCapExhausted}

/// The cap-accounting state. `fix_rounds` is the cumulative count of developer
/// rounds run; `cap` the budget; `phase` the position in the
/// developer -> gate2 -> verifier flow. `last_gate2_failure_signature` is the
/// most recent FAILING gate-2's signature (reset to `None` on a pass — a
/// passing gate is progress, so it never contributes to the identical-failure
/// streak); `early_abort_detail` is set the moment [`on_gate2`] short-circuits
/// the cap on two consecutive identical, no-progress failures (Change 1,
/// 2026-07-07 incident W0-B2: five consecutive gate runs failed with the
/// IDENTICAL single error — a clippy lint in a forbidden file — burning the
/// full 3-cycle budget for zero diff progress; this abort trims that to the
/// second occurrence).
pub type Machine {
  Machine(
    fix_rounds: Int,
    cap: Int,
    phase: Phase,
    last_gate2_failure_signature: Option(String),
    early_abort_detail: Option(String),
  )
}

/// Where the cycle is. Not exposed to the workflow beyond [`plan`]/`on_*`.
pub type Phase {
  /// About to run a developer round (the cap is re-checked here first).
  EnterDeveloper
  /// The developer round completed: run the mechanical gate.
  EnterGate2
  /// Gate 2 passed: run the adversarial verification.
  EnterVerifier
  /// Terminal: the cycle is done with this disposition.
  Stopped(Disposition)
}

/// The single effect the trampoline should perform next.
pub type Instruction {
  /// Run a developer round (initial, or a loop-back with the latest
  /// verdict/gate feedback in the input).
  Developer
  /// Run the mechanical gate 2.
  Gate2
  /// Run the adversarial verifier.
  Verifier
  /// Stop: the cycle reached this terminal disposition.
  Stop(Disposition)
}

/// The machine a brief enters after gate 1 passed: no developer round run yet.
pub fn initial(cap: Int) -> Machine {
  Machine(
    fix_rounds: 0,
    cap: cap,
    phase: EnterDeveloper,
    last_gate2_failure_signature: None,
    early_abort_detail: None,
  )
}

/// Resolve a cap: the caller's value if it is a sane ceiling (>= 1), otherwise
/// the supplied default. A cap below 1 would forbid the very first developer
/// round, which can never be the author's intent — an overridable default,
/// never a silent zero.
pub fn resolve_cap(provided: Int, default: Int) -> Int {
  case provided >= 1 {
    True -> provided
    False -> default
  }
}

/// The next instruction, a pure function of the machine's phase and cap.
pub fn plan(machine: Machine) -> Instruction {
  case machine.phase {
    EnterDeveloper ->
      // Cap checked BEFORE each developer round so the budget is never
      // overrun. Reachable at the cap only on a loop-back — the first entry is
      // fix_rounds = 0 with cap >= 1.
      case machine.fix_rounds >= machine.cap {
        True -> Stop(CycleCapExhausted)
        False -> Developer
      }
    EnterGate2 -> Gate2
    EnterVerifier -> Verifier
    Stopped(disposition) -> Stop(disposition)
  }
}

/// Fold a completed developer round into the machine.
pub fn on_developer(machine: Machine) -> Machine {
  Machine(..machine, fix_rounds: machine.fix_rounds + 1, phase: EnterGate2)
}

/// Fold a completed gate 2 into the machine. A failing gate loops back to the
/// developer (the cap is re-checked at [`plan`], so exhaustion via the gate
/// path terminates honestly without a verifier run) — UNLESS
/// `failure_signature` is IDENTICAL to the immediately preceding gate-2
/// failure's signature, in which case the loop is doomed to repeat with zero
/// progress and the machine stops early with [`CycleCapExhausted`], recording
/// why in `early_abort_detail` (Change 1: the identical-failure-no-progress
/// rule). A pass resets the streak — it is progress, even if a later verdict
/// loops back to the developer again.
pub fn on_gate2(
  machine: Machine,
  pass: Bool,
  failure_signature: String,
) -> Machine {
  case pass {
    True ->
      Machine(
        ..machine,
        phase: EnterVerifier,
        last_gate2_failure_signature: None,
      )
    False ->
      case machine.last_gate2_failure_signature {
        Some(previous) if previous == failure_signature ->
          Machine(
            ..machine,
            phase: Stopped(CycleCapExhausted),
            last_gate2_failure_signature: Some(failure_signature),
            early_abort_detail: Some(
              "identical_failure_no_progress after "
              <> int.to_string(machine.fix_rounds)
              <> " cycles",
            ),
          )
        _ ->
          Machine(
            ..machine,
            phase: EnterDeveloper,
            last_gate2_failure_signature: Some(failure_signature),
          )
      }
  }
}

/// The early-abort detail recorded when [`on_gate2`] short-circuited the cap
/// on an identical, no-progress failure — `None` on every other terminal
/// path, including an honest cap exhaustion where every failure differed.
pub fn early_abort_detail(machine: Machine) -> Option(String) {
  machine.early_abort_detail
}

/// Fold a completed verification into the machine. `accepted` is true only
/// when EVERY ruling in the verdict is `fixed`; anything else loops back to
/// the developer with the verdict attached (cap re-checked at [`plan`]).
pub fn on_verdict(machine: Machine, accepted: Bool) -> Machine {
  case accepted {
    True -> Machine(..machine, phase: Stopped(Accepted))
    False -> Machine(..machine, phase: EnterDeveloper)
  }
}

/// The terminal disposition of a stopped machine, or `Error(Nil)` while it
/// runs.
pub fn disposition(machine: Machine) -> Result(Disposition, Nil) {
  case machine.phase {
    Stopped(value) -> Ok(value)
    _ -> Error(Nil)
  }
}

// --- pure simulation (test driver AND executable spec) -------------------------

/// The terminal accounting of a simulated cycle: disposition and the developer
/// rounds consumed, exactly as the workflow would carry them.
pub type CycleSummary {
  CycleSummary(disposition: Disposition, fix_rounds: Int)
}

/// Drive the machine to termination against SCRIPTED outcomes: `gates` is the
/// pass/fail each gate 2 returns in order, `verdicts` the accepted/adverse
/// each verification returns in order. This is the exact control flow the
/// child workflow runs, with the effects replaced by reading the next
/// scripted bit — so a test over [`simulate`] is a test of the production
/// accounting.
///
/// Each failing gate is given its OWN unique index-derived signature, so this
/// convenience never trips the identical-failure early abort (Change 1) —
/// tests that need to exercise that rule use [`simulate_signed`] instead.
///
/// A script that runs out of outcomes before the machine stops terminates as
/// if the missing outcome were a fail, keeping the function total (tests
/// supply enough outcomes; production never runs dry because the cap bounds
/// the rounds).
pub fn simulate(
  cap: Int,
  gates: List(Bool),
  verdicts: List(Bool),
) -> CycleSummary {
  let signed =
    list.index_map(gates, fn(pass, index) {
      #(pass, "gate-" <> int.to_string(index))
    })
  simulate_signed(cap, signed, verdicts)
}

/// [`simulate`], but each gate-2 outcome also carries the failure signature it
/// would produce in production (ignored when the gate passes) — the driver
/// tests that pin the Change-1 identical-failure-no-progress abort use this
/// directly, exercising the EXACT [`on_gate2`] transition the child workflow
/// calls with `checks.gate2_failure_signature`'s output.
pub fn simulate_signed(
  cap: Int,
  gates: List(#(Bool, String)),
  verdicts: List(Bool),
) -> CycleSummary {
  let machine = simulate_signed_machine(cap, gates, verdicts)
  case disposition(machine) {
    Ok(value) ->
      CycleSummary(disposition: value, fix_rounds: machine.fix_rounds)
    // Unreachable: `drive_signed` only returns once `plan` has yielded `Stop`.
    Error(Nil) ->
      CycleSummary(
        disposition: CycleCapExhausted,
        fix_rounds: machine.fix_rounds,
      )
  }
}

/// [`simulate_signed`], returning the terminal MACHINE rather than its
/// summary — tests that also need [`early_abort_detail`] call this directly.
pub fn simulate_signed_machine(
  cap: Int,
  gates: List(#(Bool, String)),
  verdicts: List(Bool),
) -> Machine {
  drive_signed(initial(cap), gates, verdicts)
}

fn drive_signed(
  machine: Machine,
  gates: List(#(Bool, String)),
  verdicts: List(Bool),
) -> Machine {
  case plan(machine) {
    Stop(_) -> machine
    Developer -> drive_signed(on_developer(machine), gates, verdicts)
    Gate2 ->
      case gates {
        [#(pass, signature), ..rest] ->
          drive_signed(on_gate2(machine, pass, signature), rest, verdicts)
        [] ->
          drive_signed(
            on_gate2(machine, False, "exhausted-script"),
            [],
            verdicts,
          )
      }
    Verifier ->
      case verdicts {
        [accepted, ..rest] ->
          drive_signed(on_verdict(machine, accepted), gates, rest)
        [] -> drive_signed(on_verdict(machine, False), gates, [])
      }
  }
}
