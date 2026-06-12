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
////
//// ## The suspension call-shape contract
////
//// Every suspending engine NIF (`sleep`, `receive_signal`,
//// `await_activity_result`, `await_child`, `collect_*`) parks the workflow
//// process with beamr's message-wakeable suspension: a wake RE-EXECUTES the
//// BEAM call instruction that invoked the NIF, and the NIF re-resolves from
//// recorded history (the engine's two-phase suspend). Re-execution is only
//// sound when that call instruction is idempotent. A tail call
//// (`call_ext_last`) is NOT idempotent — it deallocates the caller's stack
//// frame before the NIF runs, so re-executing it on wake pops a second
//// frame, desyncing the return path (observed as the NIF's result value
//// being *called as a function*: `bad function term {ok, <<"fired">>}`).
////
//// Therefore every suspending FFI call MUST sit in non-tail position. The
//// thunks passed to `run` enforce this with `shield`: the FFI call is the
//// *argument* of a cross-module call, and the Erlang compiler can neither
//// tail-call nor inline a remote call, so the suspending call always
//// compiles to a plain `call_ext` whose re-execution is safe.
////
//// In addition, every thunk's body must be exactly one shielded FFI call on
//// *captured values*: arguments are precomputed outside the thunk, never
//// derived inside it. Nothing in the thunk may recompute state on re-entry —
//// the same contract the engine documents for hand-rolled await funs in
//// `crates/aion/tests/fixtures/aion_fixture_query.erl` — and the pump itself
//// relies on it when it re-enters the same await after servicing a query.

import aion/internal/ffi

/// Run a suspending await thunk, servicing any pending queries the engine
/// surfaces as `aion_query:` sentinels before the await's own resolution.
///
/// The loop is tail-recursive: each serviced query is answered exactly once,
/// then the await is re-entered until it resolves with a non-sentinel
/// result. A query handler raise never crashes the workflow — the Erlang
/// pump converts it into a `reply_query_error` and the loop continues.
///
/// Thunk authors: the suspending FFI call inside the thunk MUST be wrapped
/// in [`shield`] (see the module docs for the call-shape contract).
pub fn run(do: fn() -> Result(String, String)) -> Result(String, String) {
  case do() {
    Error("aion_query:" <> sentinel_payload) -> {
      ffi.service_query(sentinel_payload)
      run(do)
    }
    outcome -> outcome
  }
}

/// Pin a suspending FFI call out of tail position.
///
/// Called as `pump.shield(ffi.sleep(...))` from another module, the FFI
/// call is evaluated as the argument of a remote call: argument position is
/// never tail position, and the Erlang compiler never inlines remote calls
/// (hot-code-loading semantics), so the suspending NIF is always invoked by
/// a re-execution-safe `call_ext`. The body re-matches the result so the
/// function cannot collapse to an identity even under whole-program
/// optimisation. See the module docs for why a `call_ext_last` to a
/// suspending NIF corrupts the stack on wake.
pub fn shield(outcome: Result(String, String)) -> Result(String, String) {
  case outcome {
    Ok(value) -> Ok(value)
    Error(reason) -> Error(reason)
  }
}
