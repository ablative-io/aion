//// Workflow authoring surface.
////
//// This module is an aggregator only: it forwards declarations from the
//// workflow submodules and contains no workflow business logic. `run` is the
//// only recorded activity dispatch surface in this brief; deterministic time
//// and random values come from AD through `aion/internal/ffi`.

import aion/activity.{type Activity}
import aion/child
import aion/codec.{type Codec}
import aion/duration
import aion/error
import aion/signal
import aion/workflow/concurrency
import aion/workflow/continuation
import aion/workflow/define as definition
import aion/workflow/run as dispatch
import aion/workflow/timer

pub type Timestamp =
  dispatch.Timestamp

pub type WorkflowDefinition(input, output, workflow_error) =
  definition.WorkflowDefinition(input, output, workflow_error)

pub type TimerRef =
  timer.TimerRef

pub type SignalRef(payload) =
  signal.SignalRef(payload)

pub type ChildHandle(output, workflow_error) =
  child.ChildHandle(output, workflow_error)

pub fn run(
  activity: Activity(input, output),
) -> Result(output, error.ActivityError) {
  dispatch.run(activity)
}

pub fn all(
  activities: List(Activity(input, output)),
) -> Result(List(output), error.ActivityError) {
  concurrency.all(activities)
}

pub fn race(
  activities: List(Activity(input, output)),
) -> Result(output, error.ActivityError) {
  concurrency.race(activities)
}

pub fn map(
  items: List(value),
  to_activity: fn(value) -> Activity(input, output),
) -> Result(List(output), error.ActivityError) {
  concurrency.map(items, to_activity)
}

pub fn now() -> Result(Timestamp, error.EngineError) {
  dispatch.now()
}

pub fn random() -> Result(Float, error.EngineError) {
  dispatch.random()
}

pub fn random_int(min: Int, max: Int) -> Result(Int, error.EngineError) {
  dispatch.random_int(min, max)
}

pub fn sleep(duration: duration.Duration) -> Result(Nil, error.EngineError) {
  timer.sleep(duration)
}

pub fn start_timer(
  name: String,
  duration: duration.Duration,
) -> Result(TimerRef, error.EngineError) {
  timer.start_timer(name, duration)
}

pub fn cancel_timer(reference: TimerRef) -> Result(Nil, error.EngineError) {
  timer.cancel_timer(reference)
}

pub fn with_timeout(
  operation: fn() -> Result(value, inner_error),
  deadline: duration.Duration,
) -> Result(value, error.TimeoutResultError(inner_error)) {
  timer.with_timeout(operation, deadline)
}

pub fn continue_as_new(
  input: a,
  input_codec: Codec(a),
) -> Result(Nil, error.WorkflowError) {
  continuation.continue_as_new(input, input_codec)
}

pub fn receive(
  reference: SignalRef(payload),
) -> Result(payload, error.ReceiveError) {
  signal.receive(reference)
}

pub fn timer_id(reference: TimerRef) -> String {
  timer.timer_id(reference)
}

pub fn spawn(
  name: String,
  workflow_fn: fn(input) -> Result(output, workflow_error),
  input: input,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
) -> Result(ChildHandle(output, workflow_error), error.EngineError) {
  child.spawn(name, workflow_fn, input, input_codec, output_codec, error_codec)
}

pub fn spawn_and_wait(
  name: String,
  workflow_fn: fn(input) -> Result(output, workflow_error),
  input: input,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
) -> Result(output, error.ChildError(workflow_error)) {
  child.spawn_and_wait(
    name,
    workflow_fn,
    input,
    input_codec,
    output_codec,
    error_codec,
  )
}

pub fn timestamp_to_milliseconds(timestamp: Timestamp) -> Int {
  dispatch.timestamp_to_milliseconds(timestamp)
}

pub fn define(
  name: String,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
  entry_fn: fn(input) -> Result(output, workflow_error),
) -> WorkflowDefinition(input, output, workflow_error) {
  definition.define(name, input_codec, output_codec, error_codec, entry_fn)
}

pub fn name(
  definition: WorkflowDefinition(input, output, workflow_error),
) -> String {
  definition.name(definition)
}

pub fn input_codec(
  definition: WorkflowDefinition(input, output, workflow_error),
) -> Codec(input) {
  definition.input_codec(definition)
}

pub fn output_codec(
  definition: WorkflowDefinition(input, output, workflow_error),
) -> Codec(output) {
  definition.output_codec(definition)
}

pub fn error_codec(
  definition: WorkflowDefinition(input, output, workflow_error),
) -> Codec(workflow_error) {
  definition.error_codec(definition)
}

pub fn entry_fn(
  definition: WorkflowDefinition(input, output, workflow_error),
) -> fn(input) -> Result(output, workflow_error) {
  definition.entry_fn(definition)
}
