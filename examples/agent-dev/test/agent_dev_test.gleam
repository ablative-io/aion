//// Behavioural tests for the agent-dev pipeline.
////
//// Every test runs the REAL workflow body (`agent_dev.execute`) under the
//// `aion/testing` harness with local scenario handlers registered per
//// activity name — the same dispatch path, names, and codecs the deployed
//// workflow uses. Review handlers return TERMINAL TEXT (prose plus a
//// trailing JSON verdict, or deliberately unparseable garbage), so the
//// workflow's defensive verdict decode and its bounded re-ask are exercised
//// end to end, not unit-tested in isolation.

import agent_dev
import agent_dev_codecs as codecs
import agent_dev_io as io
import aion/error
import gleam/dynamic
import gleam/list
import gleam/string
import gleeunit
import gleeunit/should
import support/harness.{Handlers}

pub fn main() {
  gleeunit.main()
}

pub fn happy_path_passes_and_lands_test() {
  let handlers = harness.passing()
  harness.register(
    Handlers(..handlers, land: fn(land_input: io.LandInput) {
      let _ = harness.counter_next("happy-land")
      land_input.brief_id |> should.equal("CHIRON-RUFF-001")
      Ok(io.LandOutput(commit_sha: "cafe1234"))
    }),
  )

  let assert Ok(result) = agent_dev.execute(harness.base_input())

  result.disposition |> should.equal(io.Passed)
  result.dev_review_rounds |> should.equal(1)
  result.gate_rounds |> should.equal(1)
  result.last_review.pass |> should.be_true
  result.gate_detail.pass |> should.be_true
  result.branch |> should.equal("agent-dev/CHIRON-RUFF-001")
  result.workspace_path |> should.equal("/work/CHIRON-RUFF-001")
  // land ran exactly once (the probe is invocation two).
  harness.counter_next("happy-land") |> should.equal(2)
}

pub fn review_fail_feeds_back_then_passes_test() {
  // The reviewer blocks until its blocker text has made the round trip
  // through the dev session: the default dev handler echoes its prompt into
  // the report, and the lean feedback prompt carries the blockers, so the
  // SECOND review prompt contains the marker and passes.
  let marker = "handle empty ruff output"
  let handlers = harness.passing()
  harness.register(
    Handlers(..handlers, review: fn(prompt) {
      case string.contains(prompt, marker) {
        True -> Ok(harness.pass_verdict_text)
        False -> Ok(harness.fail_verdict_text(marker))
      }
    }),
  )

  let assert Ok(result) = agent_dev.execute(harness.base_input())

  result.disposition |> should.equal(io.Passed)
  result.dev_review_rounds |> should.equal(2)
  result.gate_rounds |> should.equal(1)
  result.last_review.pass |> should.be_true
}

pub fn gate_fail_drives_a_dev_round_then_passes_test() {
  // The gate fails once with diagnostics, the dev session gets a feedback
  // round, the bounded review loop re-passes, and the second gate passes.
  let handlers = harness.passing()
  harness.register(
    Handlers(..handlers, gate: fn(_workspace) {
      case harness.counter_next("gate-converges") {
        1 ->
          Ok(io.GateDetail(
            pass: False,
            diagnostics: "clippy: unused variable `events`",
          ))
        _ -> Ok(io.GateDetail(pass: True, diagnostics: ""))
      }
    }),
  )

  let assert Ok(result) = agent_dev.execute(harness.base_input())

  result.disposition |> should.equal(io.Passed)
  result.gate_rounds |> should.equal(2)
  // The initial review plus the re-review after the gate-fix dev round.
  result.dev_review_rounds |> should.equal(2)
  result.gate_detail.pass |> should.be_true
}

pub fn review_cap_exhaustion_is_a_disposition_and_skips_gate_and_land_test() {
  let handlers = harness.passing()
  harness.register(
    Handlers(
      ..handlers,
      review: fn(_prompt) {
        Ok(harness.fail_verdict_text("still missing the fixture test"))
      },
      gate: harness.must_not_run("gate"),
      land: harness.must_not_run("land"),
    ),
  )
  let input = io.Input(..harness.base_input(), dev_review_cap: 2)

  let assert Ok(result) = agent_dev.execute(input)

  result.disposition |> should.equal(io.ReviewCapExhausted)
  result.dev_review_rounds |> should.equal(2)
  result.gate_rounds |> should.equal(0)
  result.last_review.pass |> should.be_false
  // The gate never ran: honest not-run detail, never a fake pass.
  result.gate_detail.pass |> should.be_false
  result.gate_detail.diagnostics |> should.equal("")
  // The workspace still surfaces for inspection.
  result.branch |> should.equal("agent-dev/CHIRON-RUFF-001")
}

pub fn gate_cap_exhaustion_is_a_disposition_and_skips_land_test() {
  let handlers = harness.passing()
  harness.register(
    Handlers(
      ..handlers,
      gate: fn(_workspace) {
        Ok(io.GateDetail(
          pass: False,
          diagnostics: "clippy: needless_borrow in adapter.rs",
        ))
      },
      land: harness.must_not_run("land"),
    ),
  )
  let input = io.Input(..harness.base_input(), gate_cap: 2)

  let assert Ok(result) = agent_dev.execute(input)

  result.disposition |> should.equal(io.GateCapExhausted)
  result.gate_rounds |> should.equal(2)
  // The initial review plus the re-review after the first gate failure.
  result.dev_review_rounds |> should.equal(2)
  result.gate_detail.pass |> should.be_false
  result.gate_detail.diagnostics
  |> string.contains("needless_borrow")
  |> should.be_true
}

pub fn gate_fail_with_spent_review_budget_terminates_without_a_dev_round_test() {
  // The edge case: the dev<->review budget is spent on the FIRST review
  // (cap 1, review passes round one), then the gate fails with gate budget
  // remaining. The review budget is checked BEFORE the gate-feedback dev
  // round: the run terminates immediately as review_cap_exhausted with NO
  // further dev or review dispatch (a diagnostics round could never be
  // reviewed or gated), pairing the last REAL review (a pass) with the
  // failing gate detail honestly.
  let handlers = harness.passing()
  harness.register(
    Handlers(
      ..handlers,
      dev: fn(prompt) {
        let _ = harness.counter_next("re-entry-dev")
        Ok("DEV-REPORT\n" <> prompt)
      },
      review: fn(_prompt) {
        let _ = harness.counter_next("re-entry-review")
        Ok(harness.pass_verdict_text)
      },
      gate: fn(_workspace) {
        Ok(io.GateDetail(pass: False, diagnostics: "tests: 1 failed"))
      },
      land: harness.must_not_run("land"),
    ),
  )
  let input = io.Input(..harness.base_input(), dev_review_cap: 1, gate_cap: 5)

  let assert Ok(result) = agent_dev.execute(input)

  result.disposition |> should.equal(io.ReviewCapExhausted)
  result.dev_review_rounds |> should.equal(1)
  result.gate_rounds |> should.equal(1)
  // The honest pairing: the last real review passed; the failing gate
  // result (with its diagnostics) says why the run is exhausted anyway.
  result.last_review.pass |> should.be_true
  result.gate_detail.pass |> should.be_false
  result.gate_detail.diagnostics |> should.equal("tests: 1 failed")
  // Exactly ONE dev dispatch ran — the initial round; no diagnostics round
  // was dispatched after the gate failure (the probe is invocation two).
  harness.counter_next("re-entry-dev") |> should.equal(2)
  // Exactly one review dispatch ran (the probe is invocation two).
  harness.counter_next("re-entry-review") |> should.equal(2)
}

pub fn unparseable_verdict_recovers_on_the_bounded_reask_test() {
  // The reviewer's first reply carries no JSON; the workflow re-asks once
  // ("respond with only the JSON verdict") and the reply parses.
  let handlers = harness.passing()
  harness.register(
    Handlers(..handlers, review: fn(prompt) {
      let _ = harness.counter_next("reask-review")
      case string.contains(prompt, "only the JSON verdict") {
        True -> Ok(harness.pass_verdict_text)
        False -> Ok("Looks good to me! Ship it.")
      }
    }),
  )

  let assert Ok(result) = agent_dev.execute(harness.base_input())

  result.disposition |> should.equal(io.Passed)
  // The recovered verdict still counts as ONE review round.
  result.dev_review_rounds |> should.equal(1)
  // Two review dispatches ran: the round and its re-ask (probe is three).
  harness.counter_next("reask-review") |> should.equal(3)
}

pub fn unparseable_verdict_after_reask_counts_as_a_failed_round_test() {
  // Garbage on the round AND on the re-ask: the round counts as failed with
  // an honest verdict saying so — never an invented pass, never an error.
  let handlers = harness.passing()
  harness.register(
    Handlers(
      ..handlers,
      review: fn(_prompt) {
        let _ = harness.counter_next("garbage-review")
        Ok("no verdict here, only vibes")
      },
      gate: harness.must_not_run("gate"),
      land: harness.must_not_run("land"),
    ),
  )
  let input = io.Input(..harness.base_input(), dev_review_cap: 1)

  let assert Ok(result) = agent_dev.execute(input)

  result.disposition |> should.equal(io.ReviewCapExhausted)
  result.dev_review_rounds |> should.equal(1)
  result.last_review.pass |> should.be_false
  result.last_review.blockers
  |> list.any(string.contains(_, "parseable JSON verdict"))
  |> should.be_true
  // Exactly two review dispatches: the round and its ONE re-ask (probe is
  // three) — the re-ask is bounded, not a loop.
  harness.counter_next("garbage-review") |> should.equal(3)
}

pub fn zero_dev_review_cap_is_rejected_at_input_decode_test() {
  // A degenerate review budget never reaches the workflow body: the decoded-
  // input validation rejects it through the documented input_decode envelope,
  // naming the field. No activity runs (no handlers are registered here —
  // any dispatch would fail loudly).
  let raw =
    codecs.input_codec().encode(
      io.Input(..harness.base_input(), dev_review_cap: 0),
    )

  let assert Error(payload) = agent_dev.run(dynamic.string(raw))

  payload
  |> string.contains("\"aion_error\":\"input_decode\"")
  |> should.be_true
  payload
  |> string.contains("dev_review_cap must be >= 1, got 0")
  |> should.be_true
  payload |> string.contains("\"path\":[\"dev_review_cap\"]") |> should.be_true
}

pub fn negative_gate_cap_is_rejected_at_input_decode_test() {
  let raw =
    codecs.input_codec().encode(io.Input(..harness.base_input(), gate_cap: -3))

  let assert Error(payload) = agent_dev.run(dynamic.string(raw))

  payload
  |> string.contains("\"aion_error\":\"input_decode\"")
  |> should.be_true
  payload |> string.contains("gate_cap must be >= 1, got -3") |> should.be_true
  payload |> string.contains("\"path\":[\"gate_cap\"]") |> should.be_true
}

pub fn activity_failure_is_a_typed_stage_error_test() {
  // A stage that cannot execute at all (here: provisioning) is a typed
  // workflow error naming the stage — unlike a failed review or gate, which
  // are recorded data.
  let handlers = harness.passing()
  harness.register(
    Handlers(..handlers, provision: fn(_input) {
      Error(error.terminal("git clone failed: repository not found"))
    }),
  )

  let assert Error(io.AgentDevError(stage: stage, message: message)) =
    agent_dev.execute(harness.base_input())

  stage |> should.equal("provision")
  message |> string.contains("repository not found") |> should.be_true
}
