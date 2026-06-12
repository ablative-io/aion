//// Shared renderers from SDK error taxonomies to diagnostic strings.
////
//// The workflow modules keep their own typed error unions; these helpers
//// only turn SDK error values into the human-readable messages those typed
//// errors carry.

import aion/error

/// Render an activity failure as a single diagnostic line.
pub fn activity_message(activity_error: error.ActivityError) -> String {
  case activity_error {
    error.Retryable(message: message, details: _) -> message
    error.Terminal(message: message, details: _) -> message
    error.ActivityDecodeFailed(_) -> "activity result could not be decoded"
    error.ActivityTimedOut(error.TimedOut(message: message)) -> message
    error.ActivityCancelled(error.Cancelled(reason: reason)) -> reason
    error.ActivityNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ActivityEngineFailure(message: message) -> message
  }
}

/// Render a signal-receive failure as a single diagnostic line.
pub fn receive_message(receive_error: error.ReceiveError) -> String {
  case receive_error {
    error.ReceiveDecodeFailed(_) ->
      "review verdict payload could not be decoded"
    error.UnknownSignal(name: name) -> "unknown signal: " <> name
    error.ReceiveCancelled(error.Cancelled(reason: reason)) -> reason
    error.ReceiveNonDeterministic(error.NonDeterminismViolation(
      message: message,
    )) -> message
    error.ReceiveEngineFailure(message: message) -> message
  }
}

/// Render an engine failure as a single diagnostic line.
pub fn engine_message(engine_error: error.EngineError) -> String {
  case engine_error {
    error.EngineFailure(message: message) -> message
  }
}

/// Render a query-registration failure as a single diagnostic line.
pub fn query_message(query_error: error.QueryError) -> String {
  case query_error {
    error.UnknownQuery(name: name) -> "unknown query: " <> name
    error.QueryDecodeFailed(_) -> "query reply could not be decoded"
    error.QueryTimedOut(error.TimedOut(message: message)) -> message
    error.QueryNotRunning(workflow_id: workflow_id) ->
      "query target not running: " <> workflow_id
    error.QueryHandlerFailed(message: message) -> message
    error.QueryEngineFailure(message) -> message
  }
}
