---
type: design
cluster: aion-flow
title: Aion Flow — The Gleam Workflow Authoring SDK
---

# Aion Flow — The Gleam Workflow Authoring SDK

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map. This cluster is
> the **`aion_flow` Gleam package** (published to Hex), living at
> `gleam/aion_flow`. It is pure Gleam: its primitives are `@external`
> (Erlang target) bindings that resolve at runtime to the engine-side
> mechanics in clusters **AE** (lifecycle, activity dispatch), **AD**
> (determinism, replay), and **AT** (timers, signals, queries, children,
> concurrency). It implements none of them.

## Intention

This is the surface a workflow author actually touches. Everything else in
Aion — the event store, the replay engine, the supervision tree, the timer
wheel — exists so that this package can present one thing: a plain,
statically-typed Gleam function that survives crashes, sleeps for months,
fans work out across the BEAM, and never charges the card twice.

An author imports `aion/workflow` and `aion/activity`, writes a function,
and reaches the outside world only through this SDK's activity, signal,
query, timer, and child-workflow primitives. The deterministic parts — the
decisions, the loops, the data transformation — are ordinary Gleam. The
side effects are activities. The SDK makes that boundary the obvious shape
of the code, not a rule the author has to remember.

It must feel inevitable and it must be **type-safe to the hilt**. An
activity that takes an `Order` and returns a `Receipt` is an
`Activity(Order, Receipt)`; `workflow.run` on it returns
`Result(Receipt, ActivityError)` with `Receipt` known at compile time.
Awaiting a signal of the wrong type, returning the wrong type from a query
handler, passing a `String` where an activity wants an `Order` — none of
these compile. This is the concrete advantage over Temporal's SDKs, where
type safety varies by language and most checking happens at runtime, three
days into a workflow that has already done irreversible work. In Gleam, the
mistake is a red squiggle at `gleam build`.

When this cluster is done, an author can write the dev-workflow from
DESIGN-OVERVIEW's Meridian example — typed, concurrent, durable,
cancellable — as a normal Gleam function, and can test it (a month-long
sleep, a signal on day seven, an LLM activity) entirely in `gleam test`
with no engine, no beamr, and no store, using the `aion/testing` harness
that mirrors what Temporal's test framework provides.

## Problem

A workflow author needs a single, ergonomic, fully type-safe Gleam surface
for the engine's primitives. Without it, the author would hand-write raw
`@external` bindings to engine NIFs — losing types, losing the
activity/signal/query/timer/child abstractions, and re-deriving the
serialisation discipline at every call. The SDK has to provide the
abstractions, carry the types, and own the binding in one auditable place.

Several hazards run underneath, none optional:

**Determinism is invisible but load-bearing.** `workflow.now` and
`workflow.random` must not read the wall clock or an entropy source — they
must bind to AD's determinism machinery (the recorded event timestamp; an
RNG seeded from `WorkflowId` + `RunId`). If an author can reach a real
clock from workflow code, replay desynchronises and the failure is silent.
The SDK must make the deterministic path the *only* path the author can
reach, and offer no wall-clock binding at all.

**The recorded/side-effectful boundary must live in the type surface.**
DESIGN-OVERVIEW's core concept is that side effects are activities
(recorded, returned from history on replay) and deterministic computation
is plain code (re-run on replay). The SDK must make `activity.new` the
obvious and only home for a side effect, and `workflow.run` the single
recorded call — with no generic `side_effect(fn)` escape hatch that would
let an author smuggle a non-deterministic read past the contract.

**Typed Gleam values must cross a type-erased boundary and come back
typed.** The engine NIFs speak `aion-core`'s `Payload` (opaque bytes plus a
content-type tag). A Gleam `Order` has to be encoded on the way out and an
`Receipt` decoded on the way back, with a typed decode-failure path — and
none of that encoding can leak into the author's business logic.

**Activity errors must carry the retryable/terminal classification the
engine acts on.** `aion-core`'s `ActivityError` splits retryable from
terminal; the engine consults it to apply the retry policy. The SDK must
express that split in Gleam's type system — not as a bool or a string —
so an author classifies a failure once and the engine does the right thing.

**Authors must test workflows without a running engine.** A month-long
sleep, a signal that arrives on day seven, an activity that calls an LLM —
none can be exercised in a normal unit test. The SDK must ship a harness
that simulates time, mocks activities, and asserts replay determinism. This
is precisely the gap Temporal fills with its test framework, and the Gleam
SDK needs the equivalent.

## Solution

A pure Gleam package, `aion_flow`, organised into author-facing modules
over a single internal binding layer.

- **`aion/internal/ffi`** — the one place that declares every
  `@external(erlang, "aion_flow_ffi", ...)` binding and holds the `Payload`
  codec glue. No other module declares an `@external`.
- **`aion/codec`** — `Codec(a)` (encode/decode over the ffi string form),
  default JSON codec helpers, and a typed `DecodeError`.
- **`aion/duration`** — the `Duration` type and its named constructors.
- **`aion/error`** — `ActivityError` (`Retryable`/`Terminal`) and the
  engine-originated failure types.
- **`aion/activity`** — `Activity(i, o)`, `activity.new`, the
  `retry`/`timeout`/`heartbeat` decorators, `RetryPolicy`, `Backoff`.
- **`aion/workflow`** (+ submodules) — `run`, `now`, `random`, `sleep`,
  the timers, `with_timeout`, `spawn`/`spawn_and_wait`, `all`/`race`/`map`,
  and `define`.
- **`aion/signal`**, **`aion/query`**, **`aion/child`** — the live
  interaction surfaces.
- **`aion/testing`** (+ submodules) — the test harness.

### The `@external` binding mechanism

This is the established pattern (see
`.meridian/workflows/onatopp-dev-gleam/src/meridian_ffi.gleam`). Each ffi
binding is:

```gleam
@external(erlang, "aion_flow_ffi", "run_activity")
fn run_activity(name: String, input: String, config: String) -> Result(String, String)
```

The Erlang module name `"aion_flow_ffi"` is the **NIF registry namespace
the engine registers** through `EngineBuilder::register_nifs` (cluster AE,
brief AE-004). At compile time, `gleam build` type-checks against the Gleam
signature with no engine present — `aion_flow` has zero Rust dependency and
publishes to Hex standalone. At runtime, when beamr loads the compiled
workflow and the engine has registered the NIFs, beamr resolves the call to
the native function. The binding is satisfied only inside a running engine;
the package itself is pure Gleam. (**D1, D2, CO1, CO2.**)

### Type safety is the through-line

Every primitive is parameterised by the types it moves, so an invalid
composition fails `gleam build` (**CO5**):

- `Activity(i, o)` carries both its input and output types;
  `workflow.run(Activity(i, o)) -> Result(o, ActivityError)`.
- `SignalRef(payload)` makes `workflow.receive(SignalRef(payload)) ->
  Result(payload, ReceiveError)`.
- A `query.handler` registered for type `a` must return `a`.
- `workflow.spawn` over `fn(i) -> Result(o, e)` yields `ChildHandle(o, e)`.

Passing a `String` to an `Activity(Order, _)`, awaiting the wrong signal
type, or returning the wrong type from a handler are all type errors. A
type-safety brief asserts representative invalid compositions are rejected
by an actual `gleam check` over committed negative fixtures (a `check.sh`
driver runs each in an ephemeral project and asserts the type-mismatch
failure), not merely documented in prose.

### Activities are typed values

`activity.new(name, input, run)` builds an opaque `Activity(i, o)` carrying
the activity name, the typed input, an output `Codec`, the runner
(`fn(i) -> Result(o, ActivityError)` for in-VM activities), and — once
decorated — retry/timeout/heartbeat config (**D3**). The decorators
(`activity.retry`, `activity.timeout`, `activity.heartbeat`) each return a
new `Activity(i, o)`, so they compose in a Gleam pipeline:

```gleam
activity.new("charge-payment", validated, charge_payment)
|> activity.retry(RetryPolicy(max_attempts: 3, backoff: Exponential(...)))
|> activity.timeout(duration.seconds(30))
```

`workflow.run` is the **single recorded dispatch** (**D4, CO7**). It binds
to the engine's activity dispatch (AE-008) and recording (AD), so its
result is recorded on first execution and returned from history on replay.
Plain Gleam in the workflow body is never recorded and is re-executed on
replay (safe because deterministic). There is no other side-effect path:
no `workflow.side_effect(fn)`, because that invites smuggling a
non-deterministic read past the activity contract.

### Determinism binds directly

`workflow.now() -> Timestamp` and `workflow.random() -> Float` (and
`random_int`) are `@external` bindings to AD's determinism context — the
recorded event timestamp, and a seeded RNG keyed on `WorkflowId` + `RunId`
(AD CO8/CO9). The SDK exposes **no** binding to a wall clock or entropy
source, and documents loudly that gleam stdlib clocks and `erlang:now` /
`rand` must never be called from workflow code (**D5, CO6**). The
deterministic primitives are always reachable, so the author never needs
the forbidden ones.

### Codecs carry types across the type-erased boundary

Every primitive that crosses the `@external` boundary with user data takes a
`Codec(a) = #(encode: fn(a) -> String, decode: fn(String) -> Result(a, DecodeError))`
(**D6, CO9**). The author supplies the codec once at the type boundary —
typically a JSON encode/decode pair, with default helpers in `aion/codec` —
and never touches encoding in business logic. The SDK encodes on the way
out to the ffi string form (which maps to `aion-core`'s `Payload` with a
JSON content-type on the engine side) and decodes on the way back, surfacing
a typed `DecodeError` on failure — never a panic. Gleam has no runtime
reflection, so a global serialiser is impossible and undesirable: the
explicit codec is what preserves the static guarantee end to end.

### Errors model the retryable/terminal split

`aion/error` defines `ActivityError` with two constructors —
`Retryable(message, details)` and `Terminal(message, details)` — the Gleam
expression of `aion-core`'s classification (aion-core C21). An activity
runner returns `Result(o, ActivityError)`; the engine retries on
`Retryable` until the `RetryPolicy`'s attempts are exhausted and fails
immediately on `Terminal` (**D7**). Engine-originated failures (timeout,
cancellation, non-determinism, decode) are surfaced as **distinct** typed
variants, so an author never confuses "my activity decided to fail" with
"the engine timed it out".

### Retry policy and durations are typed data

`RetryPolicy(max_attempts, backoff, ...)` and `Backoff` —
`Exponential(initial, multiplier, max)`, `Linear(initial, increment, max)`,
`Fixed(delay)` — are typed config attached to an `Activity` by the
decorators (**D8**). The engine reads them at dispatch; the SDK only
carries them. **No default policy is baked in** (**CO8**, CLAUDE.md
no-assumed-defaults): an activity with no `retry` decorator runs once, and
the author opts into retries explicitly — silent retries of a
non-idempotent activity are a footgun, and the right numbers are a
deployment-target decision.

`Duration` (**D9**) is the single time-quantity type, with constructors
`seconds`/`minutes`/`hours`/`days`/`milliseconds` normalised to a canonical
internal millisecond form. It is used by `sleep`, `with_timeout`,
`activity.timeout`, `start_timer`, and the backoff config, so there is one
unit-safe representation everywhere the engine receives a duration.

### Signals, queries, children, concurrency

- **Signals** (**D10**): `signal.new(name, Codec(payload)) ->
  SignalRef(payload)`; `workflow.receive(signal_ref) ->
  Result(payload, ReceiveError)` decodes the recorded `SignalReceived`
  payload to the typed value. A typed `send` helper covers the
  in-engine / Gleam-client side (not an HTTP client — that is AL).
- **Queries** (**D10**): `query.handler(name, Codec(a), fn() -> a)`
  registers a read-only handler whose return type `a` is fixed at
  registration; AT's query service calls it and the SDK encodes the reply.
  No event is recorded for a query (AT CO7). By type and by documented
  convention a handler returns a value and does not run an activity.
- **Children** (**D11**): `workflow.spawn(name, workflow_fn, input, codecs)
  -> ChildHandle(o, e)`, `workflow.spawn_and_wait(...) -> Result(o, e)`,
  `child.await(handle) -> Result(o, e)`. The handle carries the child's
  output and error types.
- **Concurrency** (**D11**): typed combinators over homogeneous lists —
  `workflow.all(List(Activity(i, o))) -> Result(List(o), ActivityError)`
  (ordered), `workflow.race(...) -> Result(o, ActivityError)` (first to
  settle — first to finish wins, success or failure, matching AT-011),
  `workflow.map(List(a), fn(a) -> Activity(i, o)) ->
  Result(List(o), ActivityError)` (dynamic fan-out). They bind to AT's
  selective-receive collectors; the SDK carries the types and per-element
  codecs. Typed-tuple variants (`all2`/`all3`) are a later addition;
  homogeneous lists cover the design's stated fan-out use.

### The workflow entry contract

A workflow is a plain Gleam function `fn(i) -> Result(o, e)`. It is made
engine-runnable by `workflow.define(name, input_codec, output_codec,
error_codec, entry_fn)` (**D13**), which returns an opaque
`WorkflowDefinition(i, o, e)` carrying the name, the three codecs, and the
entry function. The consumer is the **engine** (AE), not the Rust
`aion-package` crate: at spawn AE calls the package's entry function (named
in the manifest), receives the `WorkflowDefinition`, and uses the codecs to
decode the input `Payload` and encode the result/error at its type-erased
boundary. The `.aion` manifest records only the entry module and function
*names* — it never introspects this Gleam value. The SDK does not invoke the
function — AE spawns the process and calls the entry; the SDK provides the
typed wrapper and the codec declaration. The bare
`pub fn run(String) -> String` bootstrap convention from
`onatopp-dev-gleam` is a degenerate case the typed `define` supersedes,
because that convention loses exactly the static input/output types this
cluster exists to provide.

### The test harness — pure Gleam

`aion/testing` (**D12, CO11**) runs entirely in `gleam test` with no beamr,
engine, or store. A `TestEnv` holds:

- a **simulated clock** — `advance(duration)` resolves pending
  sleeps/timers and returns control without any wall-clock wait, so a
  `sleep(days: 30)` is exercised instantly;
- an **activity mock registry** — `mock(name, fn(input) -> Result(output,
  ActivityError))` intercepts `workflow.run` for that activity and returns
  the canned typed result instead of dispatching a NIF;
- **replay assertions** — run a workflow under the env, capture the
  observation sequence, replay it, and assert the two sequences are
  identical, catching accidental non-determinism in author code.

The harness binds the same ffi surface to in-Gleam test implementations via
a concrete test double — a hand-written `test/aion_flow_ffi.erl` that
implements the production `aion_flow_ffi` NIF namespace in-process against a
process-scoped `TestEnv` (process dictionary / ETS keyed by pid). `gleam
test` loads it so the identical `@external` declarations resolve to it; the
package ships no `aion_flow_ffi` in `src/` (production resolves those names
to engine-registered NIFs). The author's workflow code is byte-for-byte
identical between test and production — only which module backs the name at
load time differs.

### What this cluster does not own (the seams)

`aion_flow` declares the bindings and the types; it implements none of the
mechanics (**CO10**). Timer firing, signal routing, query dispatch, child
spawning, replay, and the determinism context are AE/AD/AT, reached via
`@external`. Authoring the NIFs that `aion_flow_ffi` names in Rust is
`aion-nif` (AN) and the engine (AE-004). Compiling the Gleam to `.beam` and
bundling a `.aion` archive is `aion-package` (AP) — `workflow.define`
provides the typed entry contract the engine invokes at spawn (the manifest
records only its module/function names), not the archive. Starting /
signalling / querying a workflow from across a network is the client SDKs
(AL) over `aion-server` (AW).

## Structure

```
gleam/aion_flow/
├── gleam.toml                              package manifest (Hex metadata, deps)
├── src/
│   ├── aion_flow.gleam                     top-level doc + curated re-exports
│   └── aion/
│       ├── internal/ffi.gleam              the single @external binding layer
│       ├── codec.gleam                     Codec(a), default JSON codecs, DecodeError
│       ├── duration.gleam                  Duration + seconds/minutes/hours/days/ms
│       ├── error.gleam                     ActivityError (Retryable/Terminal) + engine failures
│       ├── activity.gleam                  Activity(i,o), new, retry/timeout/heartbeat, RetryPolicy, Backoff
│       ├── workflow.gleam                  workflow re-exports (run/now/random/sleep/timers/spawn/all/race/map/define)
│       ├── workflow/
│       │   ├── run.gleam                   run (recorded dispatch) + now + random
│       │   ├── timer.gleam                 sleep / start_timer / cancel_timer / with_timeout
│       │   ├── concurrency.gleam           all / race / map
│       │   └── define.gleam                workflow.define typed entry contract
│       ├── signal.gleam                    signal.new, SignalRef, typed receive, typed send
│       ├── query.gleam                     typed query handler registration + reply
│       ├── child.gleam                     ChildHandle(o,e), spawn, await
│       └── testing.gleam                   testing re-exports (TestEnv, mock, advance, replay)
│           ├── testing/clock.gleam         simulated clock: advance resolves sleeps/timers
│           ├── testing/mock.gleam          activity mock registry
│           └── testing/replay.gleam        run-twice-over-history replay assertions
└── test/
    └── aion_flow_test.gleam                type-safety + harness + end-to-end example
```

## Constraints

- **CO1** — Pure Gleam package. No Rust, no engine code, no beamr dependency
  inside `aion_flow`. Publishes to Hex standalone (`gleam_stdlib` plus
  `gleam_json` for default JSON codecs).
- **CO2** — Every `@external(erlang, ...)` declaration lives in the single
  internal module `aion/internal/ffi`. No author-facing module declares an
  `@external` (per D1).
- **CO3** — Module files hold logic; the package root and re-export
  aggregators stay declarations/re-exports only. No god files — split before
  a module exceeds ~500 lines of code (excluding comments/whitespace).
- **CO4** — No partial implementations. Every public function is complete and
  total: no panic, no `todo`, no assert-as-control-flow; failure paths return
  typed `Result` errors.
- **CO5** — Type safety is enforced, not advisory: activity input/output,
  signal payload, query return, and child input/output are static type
  parameters; an invalid composition fails `gleam build`. A type-safety brief
  asserts representative invalid compositions are rejected.
- **CO6** — Determinism: the SDK exposes `workflow.now`/`workflow.random`
  bound to AD's determinism context and exposes NO binding to a wall clock or
  entropy source. The prohibition is documented in workflow-code docs.
- **CO7** — The recorded/side-effectful boundary is structural: the only
  side-effect dispatch is `workflow.run(activity)` (and `all`/`race`/`map`
  over activities). No generic `side_effect(fn)` escape hatch (per D4).
- **CO8** — No hardcoded defaults: no default `RetryPolicy`, no default
  timeout, no default backoff numbers in the SDK. An activity without a retry
  decorator runs once; the author supplies all policy values.
- **CO9** — User data crosses the boundary only through a `Codec(a)`; a decode
  failure is a typed `DecodeError`, never a panic. No primitive silently
  swallows a decode error.
- **CO10** — `aion_flow` does not implement engine mechanics (AE/AD/AT),
  does not author NIFs in Rust (AN), and is not packaged into `.aion` (AP).
  It owns only the Gleam API surface and its binding declarations.
- **CO11** — All acceptance uses Gleam tooling: `gleam build`, `gleam check`,
  `gleam format --check`, and `gleam test` must pass clean. The
  `aion/testing` harness runs under `gleam test` with no external services.

## Non-Goals

- **No engine mechanics** — timer firing, signal routing, query dispatch,
  child spawning, replay, and the determinism context are AE/AD/AT. This
  cluster declares the `@external` bindings and the Gleam types only.
- **No Rust NIF authoring** — writing/registering the NIFs that
  `aion_flow_ffi` names is `aion-nif` (AN) and the engine (AE-004).
- **No `.aion` packaging** — compiling Gleam to `.beam` and bundling the
  archive is `aion-package` (AP). `workflow.define` provides the typed entry
  contract the engine invokes at spawn (the manifest records only its
  module/function names), not the archive.
- **No network transport, client, or server** — starting/signalling/querying
  from outside is the client SDKs (AL) over `aion-server` (AW). `aion/signal`'s
  `send` is the in-engine/Gleam-client typed binding, not an HTTP client.
- **No Elixir SDK** — `aion_flow_ex` is a later phase with its own package.
- **No event store / persistence** — the SDK never touches a store.
- **No remote (Tier-3) worker SDK** — out-of-process activities in other
  languages are AR.
```
