//// Raw bindings to the engine-provided `aion_flow_ffi` runtime module.
////
//// This module is the only place in `aion_flow` that declares
//// `@external(erlang, "aion_flow_ffi", ...)` functions. The Erlang module
//// name is the engine's NIF registry namespace, registered by
//// `EngineBuilder::register_nifs` (AE-004) and resolved by beamr when a
//// compiled workflow is loaded inside an Aion engine runtime.
////
//// `gleam build` type-checks these signatures with no engine present. The
//// author-facing modules wrap this type-erased string boundary with typed
//// codecs and domain values; this module intentionally exposes only raw names,
//// encoded payload/config strings, handles, and `Result(String, String)` error
//// seams.

@external(erlang, "aion_flow_ffi", "run_activity")
pub fn run_activity(
  name: String,
  input: String,
  config: String,
) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "now")
pub fn now() -> Result(String, String)

@external(erlang, "aion_flow_ffi", "random")
pub fn random() -> Result(String, String)

@external(erlang, "aion_flow_ffi", "random_int")
pub fn random_int(min: String, max: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "sleep")
pub fn sleep(duration: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "start_timer")
pub fn start_timer(timer_id: String, duration: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "cancel_timer")
pub fn cancel_timer(timer_id: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "receive_signal")
pub fn receive_signal(name: String, config: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "send_signal")
pub fn send_signal(
  workflow_id: String,
  name: String,
  payload: String,
) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "register_query")
pub fn register_query(name: String, config: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "reply_query")
pub fn reply_query(query_id: String, payload: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "spawn_child")
pub fn spawn_child(
  workflow_name: String,
  input: String,
  config: String,
) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "await_child")
pub fn await_child(child_id: String) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "collect_all")
pub fn collect_all(collection_id: String, items: List(String)) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "collect_race")
pub fn collect_race(collection_id: String, items: List(String)) -> Result(String, String)

@external(erlang, "aion_flow_ffi", "collect_map")
pub fn collect_map(collection_id: String, items: List(String)) -> Result(String, String)
