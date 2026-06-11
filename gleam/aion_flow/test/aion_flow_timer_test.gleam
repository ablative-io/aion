//// aion_flow timer primitive tests.

import aion/duration
import aion/error
import aion/workflow
import gleeunit
import gleeunit/should

pub fn main() {
  gleeunit.main()
}

pub fn workflow_sleep_waits_until_simulated_timer_fires_test() {
  workflow.sleep(duration.seconds(1))
  |> should.equal(Ok(Nil))
}

pub fn workflow_sleep_maps_engine_failure_to_typed_error_test() {
  workflow.sleep(duration.milliseconds(-1))
  |> should.equal(Error(error.EngineFailure("invalid duration")))
}

pub fn workflow_start_timer_returns_named_reference_test() {
  let timer = workflow.start_timer("approval-deadline", duration.minutes(30))

  case timer {
    Ok(reference) ->
      reference
      |> workflow.timer_id
      |> should.equal("approval-deadline")
    Error(_) -> should.fail()
  }
}

pub fn workflow_start_timer_maps_engine_failure_to_typed_error_test() {
  workflow.start_timer("error-empty", duration.seconds(1))
  |> should.equal(Error(error.EngineFailure("invalid timer")))
}

pub fn workflow_cancel_timer_before_fire_disarms_it_test() {
  case workflow.start_timer("cancel-before-fire", duration.seconds(5)) {
    Ok(reference) ->
      reference
      |> workflow.cancel_timer
      |> should.equal(Ok(Nil))
    Error(_) -> should.fail()
  }
}

pub fn workflow_cancel_timer_after_fire_is_no_op_test() {
  case workflow.start_timer("already-fired", duration.milliseconds(1)) {
    Ok(reference) -> {
      workflow.sleep(duration.milliseconds(1))
      |> should.equal(Ok(Nil))

      reference
      |> workflow.cancel_timer
      |> should.equal(Ok(Nil))
    }
    Error(_) -> should.fail()
  }
}

pub fn workflow_cancel_timer_maps_engine_failure_to_typed_error_test() {
  case workflow.start_timer("cancel-error", duration.seconds(1)) {
    Ok(reference) ->
      reference
      |> workflow.cancel_timer
      |> should.equal(Error(error.EngineFailure("timer cancellation failed")))
    Error(_) -> should.fail()
  }
}

pub fn workflow_with_timeout_returns_value_before_deadline_test() {
  workflow.with_timeout(fn() { Ok("signal-arrived") }, duration.seconds(2))
  |> should.equal(Ok("signal-arrived"))
}

pub fn workflow_with_timeout_wraps_inner_error_test() {
  workflow.with_timeout(
    fn() { Error("receive-cancelled") },
    duration.seconds(2),
  )
  |> should.equal(Error(error.InnerError("receive-cancelled")))
}

pub fn workflow_with_timeout_returns_typed_timeout_on_expiry_test() {
  workflow.with_timeout(fn() { Ok("late-signal") }, duration.milliseconds(0))
  |> should.equal(
    Error(error.TimedOutError(error.TimedOut("deadline expired"))),
  )
}

pub fn workflow_with_timeout_surfaces_engine_failure_as_engine_failure_test() {
  // An engine fault while arming/settling the scope must never be reported
  // as a deadline expiry: callers branch on TimedOutError to take the
  // "deadline passed" business path.
  workflow.with_timeout(fn() { Ok("never-runs") }, duration.milliseconds(-1))
  |> should.equal(
    Error(error.TimeoutEngineFailure("durability:scope state missing")),
  )
}
