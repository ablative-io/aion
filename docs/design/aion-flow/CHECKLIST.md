# Aion-Flow — Checklist

## Package and Binding Layer

- [ ] **C1** — gleam/aion_flow is a valid Gleam package: gleam.toml declares the package name, version, and Hex metadata, with gleam_stdlib and gleam_json as the only runtime dependencies.
- [ ] **C2** — gleam build, gleam check, and gleam format --check all pass clean for the package with no engine present.
- [ ] **C3** — Every @external(erlang, ...) declaration lives in the single internal module aion/internal/ffi; no author-facing module declares an @external.
- [ ] **C4** — All ffi bindings use the Erlang module name "aion_flow_ffi" (the engine's NIF registry namespace registered via AE-004).
- [ ] **C5** — aion_flow.gleam is a top-level package-doc module re-exporting the curated public surface; it contains no @external and no business logic.

## Codec and Duration

- [ ] **C6** — Codec(a) is defined as a paired encoder fn(a) -> String and decoder fn(String) -> Result(a, DecodeError) over the ffi string form.
- [ ] **C7** — Default JSON codec helpers build a Codec(a) from a gleam_json encoder and a decoder; a decode failure yields a typed DecodeError, never a panic.
- [ ] **C8** — Duration is a typed quantity with constructors milliseconds, seconds, minutes, hours, and days, normalised to a canonical internal representation.
- [ ] **C9** — Every time-quantity argument (sleep, with_timeout, activity.timeout, start_timer, backoff config) takes a Duration, not a bare Int.

## Error Taxonomy

- [ ] **C10** — ActivityError has Retryable and Terminal constructors carrying a message and structured details, mapping aion-core's retryable/terminal classification.
- [ ] **C11** — Engine-originated failures (timeout, cancellation, non-determinism, decode) are distinct typed variants, separate from author-returned ActivityError.
- [ ] **C12** — No public function panics, calls todo, or uses assert as control flow; every fallible operation returns a typed Result.

## Activities

- [ ] **C13** — activity.new(name, input, run) builds an opaque Activity(i, o) carrying the name, typed input, output Codec, and runner fn(i) -> Result(o, ActivityError).
- [ ] **C14** — activity.retry, activity.timeout, and activity.heartbeat each return a new Activity(i, o), composing in a pipeline.
- [ ] **C15** — RetryPolicy carries max_attempts and a Backoff; no default RetryPolicy is baked in — an undecorated activity runs once.
- [ ] **C16** — Backoff is a sum type with Exponential, Linear, and Fixed variants carrying their typed parameters.

## Workflow Core and Determinism

- [ ] **C17** — workflow.run(Activity(i, o)) -> Result(o, ActivityError) is the single recorded activity dispatch, binding to the engine via ffi and decoding the typed result.
- [ ] **C18** — workflow.now() -> Timestamp binds to AD's determinism context (recorded event timestamp); the SDK exposes no wall-clock binding.
- [ ] **C19** — workflow.random() -> Float and workflow.random_int bind to AD's seeded RNG; the SDK exposes no entropy-source binding.
- [ ] **C20** — There is no generic side_effect(fn) escape hatch; the only side-effect dispatch is workflow.run (and all/race/map over activities).
- [ ] **C21** — workflow.define(name, input_codec, output_codec, error_codec, entry_fn) registers a typed entry contract consumable by the .aion manifest (AP).

## Timers

- [ ] **C22** — workflow.sleep(Duration) binds to an anonymous durable timer; it is not separately cancellable.
- [ ] **C23** — workflow.start_timer(name, Duration) returns a timer reference and workflow.cancel_timer(reference) cancels a named timer; cancelling an already-fired timer is a no-op.
- [ ] **C24** — workflow.with_timeout wraps an awaiting operation (e.g. a receive) with a Duration deadline, returning a typed TimedOut on expiry.

## Signals and Queries

- [ ] **C25** — signal.new(name, Codec(payload)) -> SignalRef(payload) carries the payload type.
- [ ] **C26** — workflow.receive(SignalRef(payload)) -> Result(payload, ReceiveError) decodes the recorded SignalReceived payload to the typed value.
- [ ] **C27** — A typed send helper (in-engine / Gleam-client side) sends a typed payload to a workflow by id and signal name.
- [ ] **C28** — query.handler(name, Codec(a), fn() -> a) registers a read-only handler whose return type a is fixed at registration; the reply is encoded by the SDK.
- [ ] **C29** — A query records no event (binds to AT's read-only query service); an unknown query name yields a typed QueryError.

## Children and Concurrency

- [ ] **C30** — workflow.spawn(name, workflow_fn, input, codecs) -> ChildHandle(o, e) and workflow.spawn_and_wait(...) -> Result(o, e) carry the child output and error types.
- [ ] **C31** — child.await(ChildHandle(o, e)) -> Result(o, e) collects the recorded child result.
- [ ] **C32** — workflow.all(List(Activity(i, o))) -> Result(List(o), ActivityError) returns results in input order and fails fast on any child failure.
- [ ] **C33** — workflow.race(List(Activity(i, o))) -> Result(o, ActivityError) returns the first result and cancels the rest.
- [ ] **C34** — workflow.map(List(a), fn(a) -> Activity(i, o)) -> Result(List(o), ActivityError) performs dynamic fan-out and collects like all.

## Type Safety

- [ ] **C35** — Activity, SignalRef, query handler, and ChildHandle types are static type parameters; representative invalid compositions (wrong activity input type, wrong signal payload type, wrong query return type) are documented as compile-fail and verified to be rejected by gleam build.
- [ ] **C36** — An end-to-end typed example workflow exercises run/now/sleep/receive/spawn/all and compiles, demonstrating the full surface composes with static types.

## Test Harness

- [ ] **C37** — A TestEnv runs entirely under gleam test with no beamr, engine, or store, binding the ffi surface to in-Gleam test implementations.
- [ ] **C38** — The simulated clock's advance(Duration) resolves pending sleeps and timers instantly, with no wall-clock wait.
- [ ] **C39** — mock(name, fn(input) -> Result(output, ActivityError)) intercepts workflow.run for that activity and returns the canned typed result without dispatching a NIF.
- [ ] **C40** — A replay assertion runs a workflow over a recorded observation sequence twice and asserts the two observation sequences are identical, catching accidental non-determinism.
