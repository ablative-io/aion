# Changelog

## 0.5.0

### Added

- **`workflow.entrypoint(definition, raw_input)`** — the engine-facing run
  adapter assembled from a `WorkflowDefinition`'s codecs and typed entry
  function, so a workflow module's engine entry collapses to the one-line shim
  `pub fn run(raw_input: Dynamic) { workflow.entrypoint(definition(), raw_input) }`.
  Success encodes with the output codec and typed failure with the error codec
  (byte-identical to the hand-written adapter); an undecodable input yields the
  documented `{"aion_error":"input_decode","reason":...,"path":[...]}` JSON
  envelope as the failure payload.

## 0.3.0

### Fixed

- **Every suspending await crashed on wake against engines embedding beamr
  0.5.0** (`bad function term {ok, <<...>>}`): the 0.2.0 query-pump thunks
  tail-called the suspending engine NIFs (`sleep`, `receive_signal`,
  `await_activity_result`, `await_child`, `collect_all`, `collect_race`).
  beamr's message-wakeable suspension re-executes the BEAM call instruction
  that invoked the NIF on wake, and a tail call (`call_ext_last`) deallocates
  the caller's stack frame as part of that instruction — so the wake popped a
  second frame, desynced the return path, and the NIF's result (or the
  `aion_query:` sentinel, which is how a query delivered to a suspended
  workflow killed the run) was invoked as a function. Every pump thunk now
  pins its FFI call out of tail position via `aion/internal/pump.shield`: the
  suspending call sits in argument position of a cross-module call, which the
  Erlang compiler can neither tail-call nor inline, so it always compiles to
  a re-execution-safe `call_ext`. In addition, every pump thunk's arguments
  are precomputed outside the thunk (`timer.sleep`'s boundary,
  `signal.receive`'s name/config, `child.await`'s child id), so a thunk body
  is exactly one shielded FFI call on captured values and re-entry recomputes
  nothing. The call-shape contract is documented in `aion/internal/pump`.

## 0.2.0

- Two-phase suspend + query-pump generation: suspending awaits service
  pending workflow queries at yield points through the
  `aion_flow_query_pump` loop.

## 0.1.0

- Initial release.
