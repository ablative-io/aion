//// Hermetic-test seam for the assistant session.
////
//// Each scenario registers one typed handler per activity through
//// `aion/testing.mock_activity` — the same names, codecs, and dispatch path
//// the deployed workflow uses, with the test's handlers standing where the
//// agent-dev worker stands in production. `passing/0` is the baseline;
//// scenarios override individual handlers with a record update.
////
//// Operator turns are pre-queued with `signal.send` BEFORE the workflow
//// body runs: the test FFI double queues per-name signal payloads FIFO and
//// each `signal.receive` consumes the next one, so a scenario scripts the
//// whole operator side up front (queue AFTER `register`, which resets the
//// process-scoped environment). `queue_raw_continuation` enqueues an
//// arbitrary raw payload on the control signal, the seam for exercising the
//// workflow's tolerance of undecodable operator payloads.
////
//// The default assistant handler ECHOES its prompt into the reply, so a
//// scenario can observe (statelessly) exactly which prompt each round
//// carried. `counter_next` (test FFI, process-dictionary scoped) is the
//// seam for counting dispatches.

import aion/codec
import aion/error
import aion/signal
import aion/testing
import assistant
import assistant/activities
import assistant_codecs as codecs
import assistant_io as io
import gleam/option.{None, Some}

/// One handler per activity the session dispatches.
pub type Handlers {
  Handlers(
    provision: fn(io.ProvisionInput) ->
      Result(io.Workspace, error.ActivityError),
    assistant: fn(String) -> Result(String, error.ActivityError),
  )
}

/// Per-process invocation counter (1 on the first call for a key). The
/// harness runs handlers in the test's own process, so this is test-scoped.
@external(erlang, "assistant_test_ffi", "counter_next")
pub fn counter_next(key: String) -> Int

/// The workflow input every scenario starts from.
pub fn base_input() -> io.Input {
  io.Input(
    objective: "How do I add a signal to my order workflow?",
    repo_path: "/repos/aion",
  )
}

/// The baseline: provisioning succeeds deterministically (the test double's
/// workflow id keys the directory, mirroring production) and the assistant
/// echoes its prompt into the reply.
pub fn passing() -> Handlers {
  Handlers(
    provision: fn(input) { Ok(io.Workspace(path: "/work/" <> input.run_id)) },
    assistant: fn(prompt) { Ok("REPLY\n" <> prompt) },
  )
}

/// Fresh harness env with both activities' scenario handlers registered.
pub fn register(handlers: Handlers) -> Nil {
  let assert Ok(env) = testing.new()
  let assert Ok(_) =
    testing.mock_activity(
      env,
      activities.provision(io.ProvisionInput(repo_path: "", run_id: "")),
      handlers.provision,
    )
  let assert Ok(_) =
    testing.mock_activity(env, activities.assistant(""), handlers.assistant)
  Nil
}

/// Queue an operator continuation turn on the control signal.
pub fn queue_message(message: String) -> Nil {
  queue_continuation(io.Continuation(message: Some(message), end: None))
}

/// Queue the operator's clean end on the control signal.
pub fn queue_end() -> Nil {
  queue_continuation(io.Continuation(message: None, end: Some(True)))
}

/// Queue a typed continuation payload on the control signal, encoded with
/// the same generated codec the deployed workflow decodes with — the queued
/// wire bytes are exactly what the console sends.
pub fn queue_continuation(continuation: io.Continuation) -> Nil {
  let assert Ok(_) =
    signal.send(
      "assistant-under-test",
      signal.new(assistant.continue_signal_name, codecs.continuation_codec()),
      continuation,
    )
  Nil
}

/// Queue an arbitrary RAW payload on the control signal — the seam for
/// exercising the undecodable-payload tolerance.
pub fn queue_raw_continuation(raw_payload: String) -> Nil {
  let assert Ok(_) =
    signal.send(
      "assistant-under-test",
      signal.new(assistant.continue_signal_name, raw_codec()),
      raw_payload,
    )
  Nil
}

/// A pass-through codec: encode emits the given string verbatim as the wire
/// payload, decode returns it untouched.
fn raw_codec() -> codec.Codec(String) {
  codec.Codec(encode: fn(raw) { raw }, decode: fn(raw) { Ok(raw) })
}
