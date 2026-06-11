//// The workflow-side query pump loop around suspending awaits.
////
//// The engine answers workflow queries at yield points (AT-007 C20): when a
//// query is pending for the workflow, a suspending await returns the
//// sentinel `Error("aion_query:" <> json)` instead of resolving. `run`
//// recognises the sentinel, services the query through
//// `aion_flow_query_pump` (handler lookup, try/catch, reply), and re-enters
//// the same await, which re-resolves identically — pump iterations are
//// invisible to history and to replay. Every other result passes through
//// untouched.

import aion/internal/ffi

/// Run a suspending await thunk, servicing any pending queries the engine
/// surfaces as `aion_query:` sentinels before the await's own resolution.
///
/// The loop is tail-recursive: each serviced query is answered exactly once,
/// then the await is re-entered until it resolves with a non-sentinel
/// result. A query handler raise never crashes the workflow — the Erlang
/// pump converts it into a `reply_query_error` and the loop continues.
pub fn run(do: fn() -> Result(String, String)) -> Result(String, String) {
  case do() {
    Error("aion_query:" <> sentinel_payload) -> {
      ffi.service_query(sentinel_payload)
      run(do)
    }
    outcome -> outcome
  }
}
