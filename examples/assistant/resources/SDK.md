# SDK.md — aion_flow (Gleam) Quick Reference

You are authoring an aion workflow package in Gleam using the `aion_flow` SDK. This file is the ground-truth API surface, read from the real source at `gleam/aion_flow/src/aion/`. Signatures below are verbatim. Do not invent functions — if you need something not listed here, `grep` the real source before assuming it exists.

## Environment: check, never assume

Before writing anything, verify the toolchain in your shell:

```
gleam --version      # must exist; workflows compile with `gleam build`/`gleam test`
aion --help          # the packaging/ops binary; MAY be absent — check
git --version
```

- If `gleam` is missing you cannot compile or test — say so, do not fake success.
- If `aion` is missing you can still author + `gleam test`, but cannot `aion generate`/`aion package`/`aion deploy`. Report it.
- The SDK lives at `gleam/aion_flow/`, version `0.5.0`. Depend on it in `gleam.toml`:
  `aion_flow = ...` plus `gleam_stdlib` (`>= 0.34.0 and < 2.0.0`) and `gleam_json` (`>= 2.0.0 and < 4.0.0`). Copy the exact dep line from a sibling example's `gleam.toml` (e.g. `examples/hello-world/gleam.toml`) — do not guess the source (path vs hex).
- Study real packages before writing: `examples/hello-world` (minimal), `examples/agent-dev` + `examples/stacked-dev-remote` (agent pipelines, retry, signals, queries), `examples/assistant` (signals + status query), `examples/approval-gate` + `examples/subscription` (timeout-vs-signal races).

## Core model

- A workflow is deterministic Gleam. The ONLY recorded side-effect is `workflow.run(activity)` (and its `all`/`race`/`map` fan-out variants, timers, signal receives, child spawns). Everything else must be pure and reproducible on replay.
- Never call wall-clock, RNG, or IO directly in workflow code. Use `workflow.now()`, `workflow.random()`, `workflow.random_int()`, `workflow.id()` — these are recorded/deterministic.
- Failure is data. Public fallible functions return typed `Result`. Do not panic, `todo`, `assert`, or `let assert` as control flow.
- Codecs are generated, never hand-written for production (see Codecs). Hand-written codecs appear in examples only because they predate `aion generate`.

## `aion/workflow` — authoring surface

Definition + entrypoint:

```gleam
pub fn define(
  name: String,
  input_codec: Codec(input),
  output_codec: Codec(output),
  error_codec: Codec(workflow_error),
  entry_fn: fn(input) -> Result(output, workflow_error),
) -> WorkflowDefinition(input, output, workflow_error)

pub fn entrypoint(
  definition: WorkflowDefinition(input, output, workflow_error),
  raw_input: Dynamic,
) -> Result(String, String)
```

`entrypoint` makes the engine-facing `run` a one-line shim:

```gleam
pub fn run(raw_input: Dynamic) -> Result(String, String) {
  workflow.entrypoint(definition(), raw_input)
}
```

Success encodes with the output codec, typed failure with the error codec, an undecodable input yields the `{"aion_error":"input_decode",...}` envelope — byte-identical to a hand-written adapter. Prefer `entrypoint` over hand-decoding `Dynamic` (hello-world hand-decodes for teaching; do not copy that).

Dispatch:

```gleam
pub fn run(activity: Activity(input, output)) -> Result(output, error.ActivityError)
pub fn run_with_default(activity, workflow_default_task_queue: Option(String)) -> Result(output, error.ActivityError)
pub fn all(activities: List(Activity(input, output))) -> Result(List(output), error.ActivityError)   // ordered fan-out; any failure fails all
pub fn race(activities: List(Activity(input, output))) -> Result(output, error.ActivityError)         // first wins, losers cancelled
pub fn map(items: List(value), to_activity: fn(value) -> Activity(input, output)) -> Result(List(output), error.ActivityError)  // dynamic fan-out
// each has an `_with_default` variant taking Option(String) task queue
```

Note: `all`/`race`/`map` are ACTIVITY-only. `workflow.race` cannot race a signal against a timer — use `with_timeout` for that (see Signals).

Deterministic primitives:

```gleam
pub fn id() -> Result(String, error.EngineError)                    // workflow execution id, stable across replay
pub fn now() -> Result(Timestamp, error.EngineError)
pub fn random() -> Result(Float, error.EngineError)
pub fn random_int(min: Int, max: Int) -> Result(Int, error.EngineError)
pub fn timestamp_to_milliseconds(timestamp: Timestamp) -> Int
```

Timers:

```gleam
pub fn sleep(duration: duration.Duration) -> Result(Nil, error.EngineError)
pub fn start_timer(name: String, duration: duration.Duration) -> Result(TimerRef, error.EngineError)
pub fn cancel_timer(reference: TimerRef) -> Result(Nil, error.EngineError)
pub fn timer_id(reference: TimerRef) -> String
pub fn with_timeout(
  operation: fn() -> Result(value, inner_error),
  deadline: duration.Duration,
) -> Result(value, error.TimeoutResultError(inner_error))
```

Signals / children / continuation:

```gleam
pub fn receive(reference: SignalRef(payload)) -> Result(payload, error.ReceiveError)
pub fn spawn_and_wait(name, workflow_fn, input, input_codec, output_codec, error_codec) -> Result(output, error.ChildError(workflow_error))
pub fn spawn(...) -> Result(ChildHandle(output, workflow_error), error.EngineError)
pub fn continue_as_new(input: a, input_codec: Codec(a)) -> Result(Nil, error.WorkflowError)  // reset history for long-lived loops (see examples/subscription)
```

## `aion/activity` — activities + options

Build and decorate (all decorators are `Activity(i,o) -> Activity(i,o)`, later calls replace earlier, nothing merges):

```gleam
pub fn new(name: String, input: i, input_codec: Codec(i), output_codec: Codec(o),
           run: fn(i) -> Result(o, error.ActivityError)) -> Activity(i, o)
pub fn retry(activity, policy: RetryPolicy) -> Activity(i, o)
pub fn timeout(activity, timeout_duration: Duration) -> Activity(i, o)
pub fn heartbeat(activity, heartbeat_interval: Duration) -> Activity(i, o)
pub fn label(activity, key: String, value: String) -> Activity(i, o)     // display-only, never affects routing/replay
pub fn task_queue(activity, name: String) -> Activity(i, o)              // per-activity routing-pool override (highest precedence)
pub fn node(activity, name: String) -> Activity(i, o)                    // pin to one worker host; no default, None is final
pub fn execution_tier(activity, tier: Tier) -> Activity(i, o)
```

Retry + backoff (NO default policy — an undecorated activity runs exactly once):

```gleam
pub type RetryPolicy { RetryPolicy(max_attempts: Int, backoff: Backoff) }
pub type Backoff {
  Exponential(initial: Duration, multiplier: Float, max: Duration)
  Linear(initial: Duration, increment: Duration, max: Duration)
  Fixed(delay: Duration)
}
```

Real usage (from `examples/agent-dev/src/agent_dev/activities.gleam`):

```gleam
activity.retry(
  step,
  activity.RetryPolicy(max_attempts: 3, backoff: activity.Fixed(delay: duration.seconds(5))),
)
```

Only `Retryable` failures are retried; `Terminal` fails immediately.

Execution tier:

```gleam
pub type Tier { InVm  RemotePython  RemoteRust }
```

- `None` (no `execution_tier`) = remote worker wire (default).
- `InVm` runs the activity's `run` closure once, live, in a linked child process of the workflow — no remote worker, no task-queue routing — history/replay stay byte-identical. See `examples/invm-demo`.
- `RemotePython`/`RemoteRust` behave identically to `None` today (remote wire).

Agent activities: there is NO `activity.agent` primitive. An "agent" step is an ordinary REMOTE activity (usually `String -> String`, plain JSON string on the wire) dispatched to a norn/agent `task_queue`; a norn worker maps the activity name to a harness session. Pattern (agent-dev): `activity.new(role, prompt, text_codec(), text_codec(), unserved(role))` where `text_codec()` is `codec.json_codec(json.string, decode.string)` and the local runner is a terminal stub because the real body runs on the worker.

Declaration surface (for `aion generate` codegen): `activity.declare(name, tier, type_ref(...), type_ref(...))` produces a `Declaration`; `aion generate` derives the `activity.new` wrapper, codecs, worker stub, and `workflow.toml` entry from it. Read `docs/guides/codegen.md` before authoring declarations.

## `aion/signal` — signals

```gleam
pub fn new(name: String, payload_codec: Codec(payload)) -> SignalRef(payload)
pub fn receive(reference: SignalRef(payload)) -> Result(payload, error.ReceiveError)
pub fn send(workflow_id: String, reference: SignalRef(payload), payload: payload) -> Result(Nil, error.EngineError)
```

- `receive` SUSPENDS the workflow until a signal of that ONE name arrives, then decodes with the ref's codec. It is a yield point: pending queries are serviced before it resolves.
- ONE-NAME selective-receive constraint: each `receive` waits for exactly one signal name. You cannot select across multiple names in a single call. If a workflow must accept several kinds of control input, use ONE signal name with a discriminating payload and branch on it — e.g. assistant's `Continuation(message: Option(String), end: Option(Bool))`, subscription's `plan_change`, approval-gate's `Approval(decision: Approved|Rejected)`. Do NOT define parallel `receive`s on different names hoping to catch whichever arrives.
- Signal vs deadline: wrap the receive in `with_timeout` (approval-gate, subscription both do this):

```gleam
workflow.with_timeout(fn() { workflow.receive(approval_signal()) }, timeout)
// Ok(payload) -> signal won; Error(error.TimedOutError(_)) -> deadline; Error(error.InnerError(_)) -> receive failed
```

- Recover-and-keep-waiting on a bad payload: match `Error(error.ReceiveDecodeFailed(_))` and loop, rather than failing the workflow (assistant does this so an operator typo never kills a session).
- External senders use the CLI: `aion signal <workflow-id> <name> --payload '{...}'`.

## `aion/query` — live read-only queries

```gleam
pub fn handler(name: String, value_codec: Codec(value), reply: fn() -> value) -> Result(Nil, error.QueryError)
pub fn dispatch(name: String, value_codec: Codec(value)) -> Result(value, error.QueryError)
```

- Registration is an idempotent set-insert; re-register with a fresh closure at each state change. Standard pattern is a `set_status(phase, round)` helper called at every stage transition (assistant, agent-dev, batch-orchestrator).
- Register BEFORE the first yield point that should answer it (`sleep`, `receive`, `run`, `child.await`, `all`/`race`). Awaits reached earlier cannot service the query. Re-registration after replay recovery is automatic (workflow re-executes from top).
- Queries answer at yield points, append NO history, never block progress. Handlers MUST NOT mutate: by type they only return a value; by contract they must not call `workflow.run` or any recording primitive — the engine surfaces such calls as `query_failed`.
- `dispatch` is for in-engine callers and the pure test harness. Operators query via `aion query <workflow-id> <name>`.

## `aion/codec` — codecs (generated, do not hand-write for prod)

```gleam
pub type Codec(a) { Codec(encode: fn(a) -> String, decode: fn(String) -> Result(a, DecodeError)) }
pub type DecodeError { DecodeError(reason: String, path: List(String)) }
pub fn json_codec(encoder: fn(a) -> json.Json, decoder: decode.Decoder(a)) -> Codec(a)
```

- Production codecs are emitted by `aion generate` into `..._codecs.gleam` from your `..._io.gleam` types + a `schemas/` JSON descriptor. Do not hand-edit generated codecs; re-run `aion generate .` after every type change, and gate CI with `aion generate . --check`.
- `json_codec` is the primitive the generator uses; write one by hand only in throwaway experiments.

## `aion/duration`

Opaque `Duration`; constructors `milliseconds`, `seconds`, `minutes`, `hours`, `days` (each `Int -> Duration`); `to_milliseconds(Duration) -> Int`. There is no sub-millisecond or float duration.

## `aion/testing` — hermetic tests (`gleam test`, no engine)

```gleam
pub fn new() -> Result(TestEnv, error.EngineError)
pub fn run(workflow: fn(TestEnv) -> value) -> Result(value, error.EngineError)
pub fn mock_activity(env, activity_value: Activity(i, o), handler: fn(i) -> Result(o, error.ActivityError)) -> Result(TestEnv, error.EngineError)
pub fn mock_child(env, name, input_codec, output_codec, error_codec, handler) -> Result(TestEnv, error.EngineError)
pub fn advance(env, by: Duration) -> Result(TestEnv, error.EngineError)     // logical clock; sleeps/timers fire instantly
pub fn current_time_milliseconds(env) -> Result(Int, error.EngineError)
pub fn observations(env) -> Result(String, error.EngineError)
pub fn clear_observations(env) -> Result(TestEnv, error.EngineError)
pub fn assert_replay(env, workflow: fn() -> Result(value, workflow_error)) -> Result(value, replay.ReplayError(workflow_error))
```

- `mock_activity` is typed against the real `Activity(i,o)` value, so a wrong handler type fails at `gleam build`. Register mocks for every activity the code path dispatches, then run the entry.
- `assert_replay` runs the workflow twice and fails with `ObservationMismatch(recorded, replayed)` if observable commands differ — a hermetic stand-in for AD non-determinism detection. Add a replay assertion to every workflow test.
- `TestEnv` is process-scoped; state resets per test process, so gleeunit's concurrency is safe.
- Study a test harness (`examples/agent-dev/test/support/harness.gleam`, `examples/assistant/test/support/harness.gleam`) — they script signal queues and mock chains you should mirror.

## Build / package / run commands (verify `aion` exists first)

```
gleam build          # typecheck + compile the package
gleam test           # hermetic aion/testing suite
aion generate .      # regenerate codecs/stubs/workflow.toml from types  (aion generate . --check in CI)
aion package .       # produce the .aion package  (aion package . --build compiles+packages in one step)
aion deploy <pkg>.aion
aion start <workflow> --input '{...}'
aion signal <id> <name> --payload '{...}'
aion query <id> <name>
aion describe <id> --pretty
```

Reference `docs/GETTING-STARTED.md`, `docs/guides/workflows.md`, and `docs/guides/codegen.md` for the full lifecycle. `aion new <name>` scaffolds a complete package if you are starting fresh.

## Sharp edges — the 5 most common SDK mistakes

1. **Nondeterminism in workflow code.** Calling `erlang:system_time`, host RNG, env reads, or branching on unrecorded state breaks replay with a `NonDeterminismError`. FIX: only `workflow.now/random/random_int/id`; keep the entry pure; guard every workflow test with `testing.assert_replay`.
2. **Expecting a retry that was never configured.** An activity with no `retry` decorator runs exactly ONCE — absence is intentional, there is no hidden default. FIX: attach `activity.retry(RetryPolicy(max_attempts:, backoff:))`, and classify failures correctly — return `error.retryable(msg)` for transient, `error.terminal(msg)` for permanent; only `Retryable` is retried.
3. **Multiple signal names for one control channel.** `receive` waits on ONE name and you cannot select across names. FIX: use a single signal name with a discriminating payload (`Continuation`/`plan_change`/`Approval`) and branch on fields; wrap in `with_timeout` for a deadline instead of reaching for `workflow.race` (which is activity-only).
4. **Registering a query after (or on the wrong side of) the yield point, or mutating in a handler.** A query registered after the relevant await never answers; a handler that calls `workflow.run` is rejected as `query_failed`. FIX: register via a `set_status` helper at every stage transition, before the yield that should answer it; keep the handler a pure `fn() -> value`.
5. **Hand-editing generated codecs / skipping regeneration.** Generated `..._codecs.gleam` is overwritten and must match your types, or decode fails at the boundary. FIX: edit the `..._io.gleam` types, re-run `aion generate .`, never hand-patch generated files, and run `aion generate . --check` in CI to catch drift.
