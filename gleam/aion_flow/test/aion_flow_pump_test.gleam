//// Query pump loop tests against the in-process FFI double.
////
//// The scripted await (`test/aion_pump_script.erl`) plays the engine's role
//// at a yield point: enqueued `aion_query:` sentinels simulate pending
//// queries surfaced by a suspending await, and the final non-sentinel
//// result simulates the await's own resolution. The harness
//// (`test/aion_flow_ffi.erl`) records every `reply_query` /
//// `reply_query_error` attempt so tests can assert the reply channel was
//// used without touching recorded observations (queries never write
//// history).

import aion/child
import aion/codec
import aion/internal/ffi
import aion/internal/pump
import aion/query
import aion/testing
import gleam/dynamic/decode
import gleam/json
import gleam/string
import gleeunit
import gleeunit/should

pub fn main() {
  gleeunit.main()
}

@external(erlang, "aion_pump_script", "reset")
fn script_reset() -> Nil

@external(erlang, "aion_pump_script", "enqueue")
fn enqueue(result: Result(String, String)) -> Nil

@external(erlang, "aion_pump_script", "take")
fn scripted_await() -> Result(String, String)

@external(erlang, "aion_flow_ffi", "testing_query_replies")
fn testing_query_replies() -> Result(String, String)

fn fresh_env() -> Nil {
  let assert Ok(_) = testing.new()
  script_reset()
}

fn query_replies() -> String {
  let assert Ok(replies) = testing_query_replies()
  replies
}

fn reply_with(payload: String) -> fn(String) -> Result(String, String) {
  fn(query_id) { ffi.reply_query(query_id, payload) }
}

pub fn pump_passes_resolved_await_through_untouched_test() {
  fresh_env()
  enqueue(Ok("resolved-value"))

  pump.run(scripted_await)
  |> should.equal(Ok("resolved-value"))

  query_replies()
  |> should.equal("[]")
}

pub fn pump_passes_non_sentinel_error_through_untouched_test() {
  fresh_env()
  enqueue(Error("timeout:deadline expired"))

  pump.run(scripted_await)
  |> should.equal(Error("timeout:deadline expired"))

  query_replies()
  |> should.equal("[]")
}

pub fn pump_services_sentinel_then_reenters_await_test() {
  fresh_env()
  ffi.register_query_handler("pump-state", reply_with("{\"answer\":42}"))
  enqueue(Error("aion_query:{\"query_id\":\"q-1\",\"name\":\"pump-state\"}"))
  enqueue(Ok("await-resolved"))

  pump.run(scripted_await)
  |> should.equal(Ok("await-resolved"))

  query_replies()
  |> should.equal("[\"ok:q-1:{\\\"answer\\\":42}\"]")
}

pub fn pump_services_queued_sentinels_in_order_test() {
  fresh_env()
  ffi.register_query_handler("first", reply_with("{}"))
  ffi.register_query_handler("second", reply_with("{}"))
  enqueue(Error("aion_query:{\"query_id\":\"q-a\",\"name\":\"first\"}"))
  enqueue(Error("aion_query:{\"query_id\":\"q-b\",\"name\":\"second\"}"))
  enqueue(Ok("await-resolved"))

  pump.run(scripted_await)
  |> should.equal(Ok("await-resolved"))

  query_replies()
  |> should.equal("[\"ok:q-a:{}\",\"ok:q-b:{}\"]")
}

pub fn handler_raise_replies_error_and_workflow_survives_test() {
  fresh_env()
  ffi.register_query_handler("pump-boom", fn(_query_id) {
    panic as "handler exploded"
  })
  enqueue(Error("aion_query:{\"query_id\":\"q-raise\",\"name\":\"pump-boom\"}"))
  enqueue(Ok("survived"))

  pump.run(scripted_await)
  |> should.equal(Ok("survived"))

  let replies = query_replies()
  replies
  |> string.contains("error:q-raise:")
  |> should.be_true()
  replies
  |> string.contains("handler exploded")
  |> should.be_true()
}

pub fn missing_handler_replies_error_and_workflow_survives_test() {
  fresh_env()
  enqueue(Error("aion_query:{\"query_id\":\"q-ghost\",\"name\":\"ghost\"}"))
  enqueue(Ok("survived"))

  pump.run(scripted_await)
  |> should.equal(Ok("survived"))

  let replies = query_replies()
  replies
  |> string.contains("error:q-ghost:")
  |> should.be_true()
  replies
  |> string.contains("no handler registered")
  |> should.be_true()
}

pub fn failed_reply_after_caller_timeout_is_non_fatal_test() {
  fresh_env()
  ffi.register_query_handler("pump-late", reply_with("{}"))
  enqueue(Error(
    "aion_query:{\"query_id\":\"dropped-1\",\"name\":\"pump-late\"}",
  ))
  enqueue(Ok("survived"))

  pump.run(scripted_await)
  |> should.equal(Ok("survived"))

  query_replies()
  |> should.equal("[\"failed:dropped-1\"]")
}

pub fn failed_error_reply_after_caller_timeout_is_non_fatal_test() {
  fresh_env()
  // No handler for "ghost": the pump's reply_query_error attempt itself
  // fails (dropped caller) and must still not crash the workflow.
  enqueue(Error("aion_query:{\"query_id\":\"dropped-2\",\"name\":\"ghost\"}"))
  enqueue(Ok("survived"))

  pump.run(scripted_await)
  |> should.equal(Ok("survived"))

  query_replies()
  |> should.equal("[\"failed:dropped-2\"]")
}

pub fn sentinel_name_with_escaped_characters_resolves_handler_test() {
  fresh_env()
  ffi.register_query_handler("say \"hi\"\tnow", reply_with("{}"))
  enqueue(Error(
    "aion_query:{\"query_id\":\"q-esc\",\"name\":\"say \\\"hi\\\"\\u0009now\"}",
  ))
  enqueue(Ok("await-resolved"))

  pump.run(scripted_await)
  |> should.equal(Ok("await-resolved"))

  query_replies()
  |> should.equal("[\"ok:q-esc:{}\"]")
}

pub fn malformed_sentinel_without_query_id_is_skipped_test() {
  fresh_env()
  enqueue(Error("aion_query:not-json"))
  enqueue(Ok("survived"))

  pump.run(scripted_await)
  |> should.equal(Ok("survived"))

  // No reply channel is reachable without a query id; the engine-side
  // caller times out and the workflow keeps running.
  query_replies()
  |> should.equal("[]")
}

pub fn sentinel_missing_name_replies_malformed_error_test() {
  fresh_env()
  enqueue(Error("aion_query:{\"query_id\":\"q-noname\"}"))
  enqueue(Ok("survived"))

  pump.run(scripted_await)
  |> should.equal(Ok("survived"))

  let replies = query_replies()
  replies
  |> string.contains("error:q-noname:")
  |> should.be_true()
  replies
  |> string.contains("malformed query sentinel")
  |> should.be_true()
}

pub fn child_await_services_pending_query_before_resolving_test() {
  // `child.await` is a yield point like activity/signal/timer awaits: a
  // query sentinel surfaced while the parent is parked on a child terminal
  // must be serviced and the await re-entered, not surfaced as a bogus
  // child failure. The `queried-child` double yields one sentinel before
  // resolving.
  fresh_env()
  ffi.register_query_handler("child-state", reply_with("{\"pending\":1}"))

  let string_codec = codec.json_codec(json.string, decode.string)
  let assert Ok(handle) =
    child.spawn(
      "queried-child",
      fn(_input) { Ok("type-anchor-only") },
      "ignored-input",
      string_codec,
      string_codec,
      string_codec,
    )

  child.await(handle)
  |> should.equal(Ok("queried-child-receipt"))

  query_replies()
  |> should.equal("[\"ok:q-child:{\\\"pending\\\":1}\"]")
}

pub fn query_handler_registration_answers_through_pump_test() {
  fresh_env()
  let int_codec = codec.json_codec(json.int, decode.int)

  query.handler("typed-state", int_codec, fn() { 42 })
  |> should.equal(Ok(Nil))

  enqueue(Error(
    "aion_query:{\"query_id\":\"q-typed\",\"name\":\"typed-state\"}",
  ))
  enqueue(Ok("await-resolved"))

  pump.run(scripted_await)
  |> should.equal(Ok("await-resolved"))

  query_replies()
  |> should.equal("[\"ok:q-typed:42\"]")
}
