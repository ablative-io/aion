//// Pure Gleam workflow test harness.
////
//// `aion/testing` is the recommended way to test workflow author code. It runs
//// under `gleam test` with no engine, beamr, store, external services, or Rust
//// NIFs. Test code initialises a process-scoped `TestEnv`; the test-only
//// Erlang module `test/aion_flow_ffi.erl` occupies the same production FFI
//// namespace so workflow code and `@external` declarations are byte-identical
//// in tests and production.

import aion/activity.{type Activity}
import aion/codec.{type Codec}
import aion/duration
import aion/error
import aion/internal/ffi
import aion/testing/clock
import aion/testing/mock
import aion/testing/replay

/// Process-scoped test environment handle.
///
/// The runtime state is held by the test FFI double under the current Erlang
/// process. The handle keeps test APIs explicit and prevents unrelated tests from
/// sharing state when gleeunit runs them concurrently in separate processes.
pub opaque type TestEnv {
  TestEnv(process_key: String)
}

/// Build a fresh `TestEnv` for the current test process.
///
/// The simulated clock, activity mock registry, child/query/signal fixtures, and
/// observation capture are reset for the current process only.
pub fn new() -> Result(TestEnv, error.EngineError) {
  case ffi.testing_reset() {
    Ok(process_key) -> Ok(TestEnv(process_key: process_key))
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

/// Return the process key assigned by the test FFI double.
pub fn process_key(env: TestEnv) -> String {
  env.process_key
}

/// Run a workflow thunk under a fresh process-scoped test environment.
pub fn run(workflow: fn(TestEnv) -> value) -> Result(value, error.EngineError) {
  case new() {
    Ok(env) -> Ok(workflow(env))
    Error(engine_error) -> Error(engine_error)
  }
}

/// Advance the simulated test clock by a canonical duration.
pub fn advance(
  env: TestEnv,
  by: duration.Duration,
) -> Result(TestEnv, error.EngineError) {
  clock.advance(env, by)
}

/// Return the current simulated clock value in milliseconds.
pub fn current_time_milliseconds(
  env: TestEnv,
) -> Result(Int, error.EngineError) {
  clock.current_time_milliseconds(env)
}

/// Register a typed activity mock for the current test process.
pub fn mock_activity(
  env: TestEnv,
  activity_value: Activity(input, output),
  handler: fn(input) -> Result(output, error.ActivityError),
) -> Result(TestEnv, error.EngineError) {
  mock.activity(env, activity_value, handler)
}

/// Register a typed child-workflow double for the current test process.
///
/// `workflow.spawn_and_wait` calls with the same child name run `handler`
/// in-process and record its typed result as the child terminal. Register the
/// child module's real `execute` function to exercise full parent-child
/// composition under `gleam test`.
pub fn mock_child(
  env: TestEnv,
  name: String,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
  handler: fn(input) -> Result(output, workflow_error),
) -> Result(TestEnv, error.EngineError) {
  mock.child(env, name, input_codec, output_codec, error_codec, handler)
}

/// Capture the current observation sequence emitted by the test FFI double.
pub fn observations(_env: TestEnv) -> Result(String, error.EngineError) {
  case ffi.testing_observations() {
    Ok(raw) -> Ok(raw)
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

/// Clear the observation sequence for the current process.
pub fn clear_observations(env: TestEnv) -> Result(TestEnv, error.EngineError) {
  case ffi.testing_clear_observations() {
    Ok(_) -> Ok(env)
    Error(raw_error) -> Error(error.EngineFailure(raw_error))
  }
}

/// Assert that a workflow emits the same observation sequence on a second run.
///
/// This mirrors AD's production non-determinism detection in a lightweight test
/// harness: if replay emits different observable commands, the helper returns a
/// clear `ReplayError` diagnostic instead of requiring a live engine.
pub fn assert_replay(
  env: TestEnv,
  workflow: fn() -> Result(value, workflow_error),
) -> Result(value, replay.ReplayError(workflow_error)) {
  replay.assert_replay(env, workflow)
}
