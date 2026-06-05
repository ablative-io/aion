//// Simulated clock helpers for `aion/testing`.
////
//// Advancing the clock updates process-scoped harness state only. It never uses
//// wall-clock timers, so sleeps and durable timers exercised by workflow tests
//// complete instantly under `gleam test`.

import aion/duration
import aion/error
import aion/internal/ffi
import gleam/int

/// Advance the current process's logical clock by `by`.
///
/// The test FFI double marks any recorded sleeps or timers whose deadline is now
/// reached as fired and returns immediately without wall-clock waiting.
pub fn advance(
  env: env,
  by: duration.Duration,
) -> Result(env, error.EngineError) {
  let raw_duration = by |> duration.to_milliseconds |> int.to_string
  case ffi.testing_advance(raw_duration) {
    Ok(_) -> Ok(env)
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

/// Return the current logical clock value for the current process.
pub fn current_time_milliseconds(_env: env) -> Result(Int, error.EngineError) {
  case ffi.now() {
    Ok(raw_timestamp) -> parse_int(raw_timestamp)
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

fn parse_int(raw: String) -> Result(Int, error.EngineError) {
  case int.parse(raw) {
    Ok(value) -> Ok(value)
    Error(_) ->
      Error(error.EngineFailure("Invalid test clock timestamp: " <> raw))
  }
}
