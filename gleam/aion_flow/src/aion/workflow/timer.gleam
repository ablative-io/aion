//// Durable workflow timers over canonical `Duration` values.

import aion/duration
import aion/error
import aion/internal/ffi
import aion/internal/pump
import gleam/int
import gleam/string

/// A reference to a named durable timer started by workflow code.
///
/// The reference carries the author-supplied named timer identifier returned by
/// the engine. Anonymous sleeps deliberately do not expose a `TimerRef` because
/// AT only permits named timers to be cancelled independently.
pub opaque type TimerRef {
  TimerRef(timer_id: String)
}

/// Return the engine timer identifier carried by a named timer reference.
pub fn timer_id(reference: TimerRef) -> String {
  reference.timer_id
}

/// Wait for an anonymous durable timer to fire.
///
/// The timer is durable: it survives engine restart and, during replay, returns
/// instantly once the matching timer-fired event is already present in history.
/// Anonymous sleeps are not separately cancellable; cancelling a sleep means
/// cancelling the workflow that is blocked on it (AT D3). Use `start_timer` when
/// workflow code needs a named timer that can be cancelled independently.
/// The await is a yield point: pending workflow queries are serviced by the
/// query pump while the timer is parked. `with_timeout` needs no pump of its
/// own — the awaits running inside its operation are the yield points.
pub fn sleep(duration: duration.Duration) -> Result(Nil, error.EngineError) {
  // The boundary is precomputed so the pump thunk's body is exactly one
  // shielded FFI call on captured values — the re-execution-safety contract
  // for suspending awaits (see `aion/internal/pump`): a wake re-executes the
  // call instruction that invoked the suspending NIF, so nothing in the
  // thunk may recompute state on re-entry.
  let boundary = duration_to_boundary(duration)
  case pump.run(fn() { pump.shield(ffi.sleep(boundary)) }) {
    Ok(_) -> Ok(Nil)
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

/// Start a named durable timer and return its cancellable reference.
///
/// The supplied `name` is the named timer identifier handed to AT. The SDK does
/// not invent a default duration or rewrite the identifier; engine-side timer ID
/// validation remains authoritative.
pub fn start_timer(
  name: String,
  duration: duration.Duration,
) -> Result(TimerRef, error.EngineError) {
  case ffi.start_timer(name, duration_to_boundary(duration)) {
    Ok(timer_id) -> Ok(TimerRef(timer_id: timer_id))
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

/// Cancel a named durable timer.
///
/// AT treats cancelling an already-fired or already-cancelled named timer as an
/// idempotent no-op, so a successful engine response is always returned as
/// `Ok(Nil)`. Anonymous sleeps cannot be cancelled through this function because
/// they never produce a `TimerRef`.
pub fn cancel_timer(reference: TimerRef) -> Result(Nil, error.EngineError) {
  case ffi.cancel_timer(reference.timer_id) {
    Ok(_) -> Ok(Nil)
    Error(raw_error) -> Error(error.EngineFailure(message: raw_error))
  }
}

/// Run an awaiting operation with a durable deadline.
///
/// The operation is a thunk so the engine/test FFI can establish the timeout
/// before the await begins. If the operation completes before the deadline its
/// `Ok` value is returned. If the operation returns its own typed error, that
/// error is wrapped in `InnerError`. If AT reports that the deadline expired
/// (the engine's `timeout:`-tagged result), the result is
/// `TimedOutError(TimedOut(...))`. Any other engine error is surfaced as
/// `TimeoutEngineFailure` — an infrastructure fault must never be mistaken
/// for a deadline expiry.
pub fn with_timeout(
  operation: fn() -> Result(value, inner_error),
  deadline: duration.Duration,
) -> Result(value, error.TimeoutResultError(inner_error)) {
  case ffi.with_timeout(duration_to_boundary(deadline), operation) {
    Ok(Ok(value)) -> Ok(value)
    Ok(Error(inner_error)) -> Error(error.InnerError(inner_error))
    Error(raw_error) ->
      case string.starts_with(raw_error, "timeout:") {
        True ->
          Error(
            error.TimedOutError(
              error.TimedOut(message: string.drop_start(raw_error, 8)),
            ),
          )
        False -> Error(error.TimeoutEngineFailure(message: raw_error))
      }
  }
}

fn duration_to_boundary(duration: duration.Duration) -> String {
  duration
  |> duration.to_milliseconds
  |> int.to_string
}
