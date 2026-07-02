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

- **In-VM execution tier** (CUT 3): `activity.execution_tier(a, InVm)` routes
  a dispatch through the engine's new arity-4 `dispatch_activity_in_vm` wire,
  which spawns the SDK-composed runner thunk (input capture + runner + output
  codec) as a linked child process of the workflow process — no remote
  worker, no task-queue subscription. Recorded-result semantics are identical
  to remote activities: the runner executes once, the result is recorded in
  history, and replay returns the recording without re-execution; a runner
  crash surfaces as a proper terminal `ActivityFailed` (the workflow process
  survives). The `Activity` value gains an optional `tier` selection
  (`selected_tier`; absence = the remote wire, today's exact behavior), the
  dispatch config JSON gains a `"tier"` field (canonical `tier_to_string`
  values, `null` when unselected), and runner errors cross the child boundary
  on the existing prefixed reason vocabulary (`retryable:`/`terminal:`/...),
  so `ActivityError` kind fidelity is preserved with zero new conventions.
  In-VM activities cannot join `collect_*` fan-outs (engine-refused at decode
  time); dispatch them individually via `workflow.run`.

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
