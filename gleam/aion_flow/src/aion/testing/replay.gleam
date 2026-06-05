//// Replay assertions for `aion/testing`.
////
//// The helper runs a workflow twice in the same process-scoped harness, compares
//// the captured observation sequences, and reports mismatches as typed data. This
//// is a test-time simulation of AD's non-determinism detection: production replay
//// remains the responsibility of AD, but workflow authors can catch accidental
//// unrecorded branching in ordinary `gleam test` suites.

import aion/error
import aion/internal/ffi

/// A replay assertion failure.
pub type ReplayError(workflow_error) {
  /// The workflow returned an error during either the first run or replay.
  WorkflowFailed(workflow_error)

  /// The two runs completed but emitted different observable command sequences.
  ObservationMismatch(recorded: String, replayed: String)

  /// The test harness itself failed while capturing or resetting observations.
  ReplayHarnessFailure(error.EngineError)
}

/// Run a workflow twice and require identical observations.
pub fn assert_replay(
  _env: env,
  workflow: fn() -> Result(value, workflow_error),
) -> Result(value, ReplayError(workflow_error)) {
  case clear() {
    Error(harness_error) -> Error(harness_error)
    Ok(_) ->
      case workflow() {
        Error(workflow_error) -> Error(WorkflowFailed(workflow_error))
        Ok(value) ->
          case capture() {
            Error(harness_error) -> Error(harness_error)
            Ok(recorded) -> compare_replay(value, recorded, workflow)
          }
      }
  }
}

fn compare_replay(
  value: value,
  recorded: String,
  workflow: fn() -> Result(value, workflow_error),
) -> Result(value, ReplayError(workflow_error)) {
  case clear() {
    Error(harness_error) -> Error(harness_error)
    Ok(_) ->
      case workflow() {
        Error(workflow_error) -> Error(WorkflowFailed(workflow_error))
        Ok(_) ->
          case capture() {
            Error(harness_error) -> Error(harness_error)
            Ok(replayed) ->
              case recorded == replayed {
                True -> Ok(value)
                False ->
                  Error(ObservationMismatch(
                    recorded: recorded,
                    replayed: replayed,
                  ))
              }
          }
      }
  }
}

fn clear() -> Result(Nil, ReplayError(workflow_error)) {
  case ffi.testing_clear_observations() {
    Ok(_) -> Ok(Nil)
    Error(raw_error) ->
      Error(ReplayHarnessFailure(error.EngineFailure(raw_error)))
  }
}

fn capture() -> Result(String, ReplayError(workflow_error)) {
  case ffi.testing_observations() {
    Ok(observations) -> Ok(observations)
    Error(raw_error) ->
      Error(ReplayHarnessFailure(error.EngineFailure(raw_error)))
  }
}
