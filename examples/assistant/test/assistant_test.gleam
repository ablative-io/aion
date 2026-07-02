//// Behavioural tests for the assistant session.
////
//// Every test runs the REAL workflow body (`assistant.execute`) under the
//// `aion/testing` harness with local scenario handlers registered per
//// activity name — the same dispatch path, names, and codecs the deployed
//// workflow uses. Operator turns are pre-queued on the `assistant_continue`
//// signal with the SAME generated codec the console encodes with, so the
//// signal decode path is exercised end to end on real wire bytes.

import aion/error
import aion/workflow
import assistant
import assistant_io as io
import gleam/int
import gleam/option.{Some}
import gleam/string
import gleeunit
import gleeunit/should
import support/harness.{Handlers}

pub fn main() {
  gleeunit.main()
}

// --- the round loop ----------------------------------------------------------

pub fn one_round_then_operator_end_test() {
  register_counting_passing("one-round")
  harness.queue_end()

  let assert Ok(result) = assistant.execute(harness.base_input())

  result.disposition |> should.equal(io.OperatorEnded)
  result.rounds |> should.equal(1)
  // The test double's workflow id keys the workspace, mirroring the
  // production `<root>/<run_id>/repo` discipline.
  result.workspace_path |> should.equal("/work/test-workflow-id")
  // The echoed round-one prompt carries the objective and the contract.
  result.last_reply
  |> string.contains(harness.base_input().objective)
  |> should.be_true
  // Exactly one agent round dispatched (the probe is invocation two).
  harness.counter_next("one-round") |> should.equal(2)
}

pub fn continuation_message_reaches_the_session_verbatim_test() {
  harness.register(harness.passing())
  harness.queue_message("Now add a timer to that workflow")
  harness.queue_end()

  let assert Ok(result) = assistant.execute(harness.base_input())

  result.disposition |> should.equal(io.OperatorEnded)
  result.rounds |> should.equal(2)
  // The continuation prompt is the operator's message VERBATIM — no
  // framing: the pinned norn session already holds the contract.
  result.last_reply
  |> should.equal("REPLY\nNow add a timer to that workflow")
}

pub fn end_wins_over_a_message_in_the_same_payload_test() {
  register_counting_passing("end-wins")
  harness.queue_continuation(io.Continuation(
    message: Some("and one more thing"),
    end: Some(True),
  ))

  let assert Ok(result) = assistant.execute(harness.base_input())

  result.disposition |> should.equal(io.OperatorEnded)
  result.rounds |> should.equal(1)
  // The message riding an end payload is never dispatched.
  harness.counter_next("end-wins") |> should.equal(2)
}

pub fn blank_message_is_a_noop_nudge_test() {
  register_counting_passing("blank-nudge")
  harness.queue_message("   ")
  harness.queue_end()

  let assert Ok(result) = assistant.execute(harness.base_input())

  result.disposition |> should.equal(io.OperatorEnded)
  result.rounds |> should.equal(1)
  harness.counter_next("blank-nudge") |> should.equal(2)
}

pub fn undecodable_operator_payloads_never_kill_the_session_test() {
  register_counting_passing("bad-payload")
  // A wrong-typed field and outright non-JSON, then a clean end: both bad
  // payloads are consumed as no-op nudges.
  harness.queue_raw_continuation("{\"message\": 5}")
  harness.queue_raw_continuation("not json at all")
  harness.queue_end()

  let assert Ok(result) = assistant.execute(harness.base_input())

  result.disposition |> should.equal(io.OperatorEnded)
  result.rounds |> should.equal(1)
  harness.counter_next("bad-payload") |> should.equal(2)
}

pub fn round_cap_exhaustion_is_a_disposition_test() {
  register_counting_passing("cap")
  // More operator turns than the budget: the session must stop at
  // max_rounds with the honest disposition, leaving the surplus unconsumed.
  queue_turns(assistant.max_rounds + 5)

  let assert Ok(result) = assistant.execute(harness.base_input())

  result.disposition |> should.equal(io.RoundCapExhausted)
  result.rounds |> should.equal(assistant.max_rounds)
  harness.counter_next("cap") |> should.equal(assistant.max_rounds + 1)
}

// --- honest ends: stage errors -------------------------------------------------

pub fn provision_failure_is_a_typed_stage_error_test() {
  harness.register(
    Handlers(..harness.passing(), provision: fn(_input) {
      Error(error.terminal("git is not installed on the worker host"))
    }),
  )

  let assert Error(io.AssistantError(stage: stage, message: message)) =
    assistant.execute(harness.base_input())

  stage |> should.equal("provision")
  message |> string.contains("git is not installed") |> should.be_true
}

pub fn assistant_round_failure_is_a_typed_stage_error_test() {
  harness.register(
    Handlers(..harness.passing(), assistant: fn(_prompt) {
      Error(error.terminal("norn protocol mismatch"))
    }),
  )
  harness.queue_end()

  let assert Error(io.AssistantError(stage: stage, message: message)) =
    assistant.execute(harness.base_input())

  stage |> should.equal("assistant")
  message |> string.contains("norn protocol mismatch") |> should.be_true
}

pub fn broken_signal_channel_is_a_typed_stage_error_test() {
  // No operator turns queued at all: the test double reports the signal as
  // unknown, standing in for a broken receive channel. The workflow must
  // surface it as a typed await_operator error, never hang or invent an end.
  harness.register(harness.passing())

  let assert Error(io.AssistantError(stage: stage, message: _)) =
    assistant.execute(harness.base_input())

  stage |> should.equal("await_operator")
}

// --- the input boundary ---------------------------------------------------------

pub fn blank_objective_is_rejected_at_the_input_boundary_test() {
  let codec = workflow.input_codec(assistant.definition())
  let assert Error(decode_error) =
    codec.decode("{\"objective\":\"   \",\"repo_path\":\"/repos/aion\"}")
  decode_error.path |> should.equal(["objective"])
  decode_error.reason |> string.contains("blank") |> should.be_true
}

pub fn empty_repo_path_is_a_legal_scratch_session_test() {
  let codec = workflow.input_codec(assistant.definition())
  let assert Ok(input) =
    codec.decode("{\"objective\":\"explain signals\",\"repo_path\":\"\"}")
  input.objective |> should.equal("explain signals")
  input.repo_path |> should.equal("")
}

// --- helpers --------------------------------------------------------------------

/// Queue `count` distinct operator turns on the control signal.
fn queue_turns(count: Int) -> Nil {
  case count <= 0 {
    True -> Nil
    False -> {
      harness.queue_message("turn " <> int.to_string(count))
      queue_turns(count - 1)
    }
  }
}

/// Register the passing baseline with an assistant handler that counts its
/// dispatches under `key`.
fn register_counting_passing(key: String) -> Nil {
  let handlers = harness.passing()
  harness.register(
    Handlers(..handlers, assistant: fn(prompt) {
      let _ = harness.counter_next(key)
      Ok("REPLY\n" <> prompt)
    }),
  )
}
