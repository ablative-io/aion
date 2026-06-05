//// Typed activity values and policy configuration.

import aion/codec
import aion/duration
import aion/error

/// Backoff strategy carried with an explicit retry policy.
///
/// The SDK only stores this configuration for the engine to interpret during
/// dispatch. It does not apply retries or invent missing timing values.
pub type Backoff {
  /// Exponential backoff from `initial`, scaled by `multiplier`, capped at `max`.
  Exponential(
    initial: duration.Duration,
    multiplier: Float,
    max: duration.Duration,
  )

  /// Linear backoff from `initial`, adding `increment`, capped at `max`.
  Linear(
    initial: duration.Duration,
    increment: duration.Duration,
    max: duration.Duration,
  )

  /// Fixed backoff with the same `delay` between attempts.
  Fixed(delay: duration.Duration)
}

/// Explicit retry policy for an activity.
///
/// No default retry policy is baked into the SDK. An activity built with `new`
/// and no `retry` decorator carries no policy and runs exactly once when the
/// engine dispatches it.
pub type RetryPolicy {
  RetryPolicy(max_attempts: Int, backoff: Backoff)
}

/// A typed activity invocation value.
///
/// `i` is the statically-known input type and `o` is the statically-known output
/// type. The output codec is carried so later workflow dispatch can decode the
/// type-erased engine payload back to `o` without reflection.
pub opaque type Activity(i, o) {
  Activity(
    name: String,
    input: i,
    output_codec: codec.Codec(o),
    runner: fn(i) -> Result(o, error.ActivityError),
    retry_policy: Option(RetryPolicy),
    timeout: Option(duration.Duration),
    heartbeat: Option(duration.Duration),
  )
}

/// Build a typed activity value with no retry, timeout, or heartbeat config.
///
/// Absence of config is intentional data: there are no hidden defaults. In
/// particular, an activity with no `retry` decorator runs exactly once when it
/// is dispatched by the engine.
pub fn new(
  name: String,
  input: i,
  output_codec: codec.Codec(o),
  run: fn(i) -> Result(o, error.ActivityError),
) -> Activity(i, o) {
  Activity(
    name: name,
    input: input,
    output_codec: output_codec,
    runner: run,
    retry_policy: None,
    timeout: None,
    heartbeat: None,
  )
}

/// Attach an explicit retry policy to an activity.
///
/// Later calls replace earlier retry policy values; the SDK does not merge or
/// synthesize policy fields.
pub fn retry(activity: Activity(i, o), policy: RetryPolicy) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: Some(policy),
    timeout: activity.timeout,
    heartbeat: activity.heartbeat,
  )
}

/// Attach an explicit timeout duration to an activity.
pub fn timeout(
  activity: Activity(i, o),
  timeout_duration: duration.Duration,
) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: Some(timeout_duration),
    heartbeat: activity.heartbeat,
  )
}

/// Attach an explicit heartbeat interval to an activity.
pub fn heartbeat(
  activity: Activity(i, o),
  heartbeat_interval: duration.Duration,
) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: activity.timeout,
    heartbeat: Some(heartbeat_interval),
  )
}

/// Return the activity name used by the engine dispatch boundary.
pub fn name(activity: Activity(i, o)) -> String {
  activity.name
}

/// Return the typed input captured by the activity value.
pub fn input(activity: Activity(i, o)) -> i {
  activity.input
}

/// Return the typed output codec captured by the activity value.
pub fn output_codec(activity: Activity(i, o)) -> codec.Codec(o) {
  activity.output_codec
}

/// Return the typed runner captured by the activity value.
pub fn runner(
  activity: Activity(i, o),
) -> fn(i) -> Result(o, error.ActivityError) {
  activity.runner
}

/// Return the explicitly attached retry policy, if one exists.
pub fn retry_policy(activity: Activity(i, o)) -> Option(RetryPolicy) {
  activity.retry_policy
}

/// Return the explicitly attached timeout duration, if one exists.
pub fn timeout_duration(activity: Activity(i, o)) -> Option(duration.Duration) {
  activity.timeout
}

/// Return the explicitly attached heartbeat interval, if one exists.
pub fn heartbeat_interval(activity: Activity(i, o)) -> Option(duration.Duration) {
  activity.heartbeat
}
