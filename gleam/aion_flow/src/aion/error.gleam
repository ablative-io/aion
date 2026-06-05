//// Typed error taxonomy for Aion workflow authors.
////
//// The `aion_flow` package models failure as data. Public fallible functions
//// return typed `Result` errors; library code must not panic, call `todo`, or
//// use assertions as control flow. Activity failures, decode failures, engine
//// lifecycle failures, non-determinism, and unknown live-interaction names are
//// all represented by the types in this module.

import aion/codec

/// An activity dispatch failure returned through `workflow.run`.
///
/// `Retryable` and `Terminal` are the Gleam mirror of aion-core's retryable /
/// terminal activity-error classification (aion-core C21). The engine retries
/// `Retryable` failures according to the activity's explicit `RetryPolicy` until
/// its attempts are exhausted, and fails immediately for `Terminal` failures.
///
/// Engine-originated failures that can occur while dispatching or decoding an
/// activity result are also represented here so `workflow.run(Activity(i, o))`
/// remains a single typed `Result(o, ActivityError)` boundary. Decode failures
/// preserve the typed codec error and are never swallowed or raised as panics.
///
/// `details` carries structured detail data encoded by the author-facing codec
/// layer. The SDK keeps it as an opaque string here so activity authors do not
/// depend on engine payload internals.
pub type ActivityError {
  Retryable(message: String, details: String)
  Terminal(message: String, details: String)
  ActivityDecodeFailed(codec.DecodeError)
  ActivityTimedOut(TimeoutError)
  ActivityCancelled(CancellationError)
  ActivityNonDeterministic(NonDeterminismError)
  ActivityEngineFailure(message: String)
}

/// Construct a retryable activity failure with no detail payload.
pub fn retryable(message: String) -> ActivityError {
  Retryable(message: message, details: "")
}

/// Construct a retryable activity failure with an encoded detail payload.
pub fn retryable_with_details(message: String, details: String) -> ActivityError {
  Retryable(message: message, details: details)
}

/// Construct a terminal activity failure with no detail payload.
pub fn terminal(message: String) -> ActivityError {
  Terminal(message: message, details: "")
}

/// Construct a terminal activity failure with an encoded detail payload.
pub fn terminal_with_details(message: String, details: String) -> ActivityError {
  Terminal(message: message, details: details)
}

/// A timeout reported by the engine lifecycle.
///
/// Later workflow primitives such as `with_timeout`, timers, and activity
/// timeout handling use this type when the engine decides that a deadline has
/// expired. This is separate from author retry/terminal decisions because
/// authors must not classify an engine timeout as an activity retry decision.
pub type TimeoutError {
  TimedOut(message: String)
}

/// A cancellation reported by the engine lifecycle.
///
/// Cancellation originates from the engine's workflow or command lifecycle and
/// remains separate from author-returned activity decisions.
pub type CancellationError {
  Cancelled(reason: String)
}

/// A deterministic replay violation reported by AD.
///
/// This corresponds to AD's `NonDeterminismError`: the workflow command stream
/// produced during replay did not match recorded history. Workflow code handles
/// it as an engine-originated failure, never as an author retry/terminal
/// decision.
pub type NonDeterminismError {
  NonDeterminismViolation(message: String)
}

/// A generic engine-originated primitive failure.
///
/// Deterministic primitives such as `workflow.now` and `workflow.random` are
/// bound to AD through the FFI layer. If the engine reports an unavailable
/// context or malformed deterministic value, the SDK returns this data rather
/// than panicking.
pub type EngineError {
  EngineFailure(message: String)
}

/// A boundary decode failure for workflow primitives.
///
/// The wrapped `codec.DecodeError` preserves the typed decode reason and path
/// produced by AF-002 codecs. Decode failures are returned as data and are never
/// swallowed or raised as panics.
pub type BoundaryDecodeError {
  DecodeFailed(codec.DecodeError)
}

/// A signal receive failure.
///
/// Future signal primitives use this type to distinguish malformed payloads,
/// unknown signal names, cancellation, and deterministic replay violations.
pub type ReceiveError {
  ReceiveDecodeFailed(codec.DecodeError)
  UnknownSignal(name: String)
  ReceiveCancelled(CancellationError)
  ReceiveNonDeterministic(NonDeterminismError)
}

/// A query failure.
///
/// Future query primitives use this type to distinguish malformed query
/// payloads, unknown query names, cancellation, and deterministic replay
/// violations. Queries do not record workflow events.
pub type QueryError {
  QueryDecodeFailed(codec.DecodeError)
  UnknownQuery(name: String)
  QueryCancelled(CancellationError)
  QueryNonDeterministic(NonDeterminismError)
}

/// A timeout wrapper for primitives that can either finish or expire.
pub type TimeoutResultError(error) {
  TimedOutError(TimeoutError)
  InnerError(error)
}
