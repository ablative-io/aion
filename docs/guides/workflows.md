# Workflow authoring guide

How to write Aion workflows in Gleam with the
[`aion_flow`](https://hex.pm/packages/aion_flow) SDK. This guide covers the
entry-point contract, the determinism rules, and every durable primitive:
activities, timers, signals, timeout races, queries, child workflows, and
continue-as-new.

A workflow project is an ordinary Gleam project (`gleam new my_flow`,
`gleam add aion_flow gleam_json`) plus a `workflow.toml` packaging
descriptor — see [`docs/packaging.md`](../packaging.md). The
[getting-started guide](../GETTING-STARTED.md) builds one end to end;
[`examples/order-fulfillment/`](../../examples/order-fulfillment/) is the
flagship reference.

## The entry-point contract

Every `[[workflow]]` entry in `workflow.toml` names an `entry_module` and an
`entry_function`. The engine calls that function with **one argument**, and
the contract is precise:

- **Input arrives as a raw JSON string inside a `Dynamic`.** Not a decoded
  JSON document — a `Dynamic` wrapping the JSON text itself. Decode the
  string first, then parse it with your input codec.
- **The success value must be a JSON string.** Encode your typed result back
  to JSON text; that string becomes the recorded result payload.
- **An `Error` return fails the run.** The run records `WorkflowFailed` and
  projects status `Failed`.

The canonical shape (this exact pattern ships in
[`examples/hello-world/src/hello_world.gleam`](../../examples/hello-world/src/hello_world.gleam)
and
[`examples/order-fulfillment/src/order_fulfillment.gleam`](../../examples/order-fulfillment/src/order_fulfillment.gleam)):

```gleam
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode

pub fn run(raw_input: Dynamic) -> Result(String, MyError) {
  case decode.run(raw_input, decode.string) {
    Ok(raw_json) ->
      case input_codec().decode(raw_json) {
        Ok(input) ->
          case execute(input) {
            // Re-encode the typed success value to its JSON string.
            Ok(output) -> Ok(output_codec().encode(output))
            Error(workflow_error) -> Error(workflow_error)
          }
        Error(codec.DecodeError(reason: reason, path: _)) ->
          Error(MyError("failed to decode workflow input: " <> reason))
      }
    Error(_) -> Error(MyError("workflow input payload was not a string"))
  }
}
```

Keep `execute(input) -> Result(MyOutput, MyError)` as the typed body; `run`
is a thin codec shell around it. Codecs are built with
`codec.json_codec(to_json, decoder)` from a `gleam/json` encoder and a
`gleam/dynamic/decode` decoder.

## Determinism: the one rule that matters

Workflow code is **re-executed from the top** whenever the engine replays it
— on recovery after a crash, and at suspension/resume boundaries. Replay
works because every side-effecting call returns its **recorded** result from
history instead of acting again. That only holds if your code is
deterministic:

- **No wall clock.** Use `workflow.now()` — it returns the recorded event
  timestamp, identical on every replay.
- **No ambient randomness.** Use `workflow.random()` / `workflow.random_int(min, max)`
  — seeded from the workflow and run identifiers, identical on every replay.
- **No direct I/O from workflow code.** No HTTP calls, file reads, database
  queries, or environment reads. Every side effect is an **activity**
  dispatched through `workflow.run(...)` — the single recorded side-effect
  boundary. There is deliberately no generic `side_effect(fn)` escape hatch.
- **No branching on anything non-recorded.** Map iteration order, external
  state, process dictionaries — if it can differ between executions, it
  cannot influence workflow control flow.

Plain pure computation (parsing, arithmetic, building data structures) is
fine and free — it just re-runs identically.

## Activities

An activity is a typed value describing one remote side effect:

```gleam
import aion/activity

fn charge_activity(input: ChargeInput) -> activity.Activity(ChargeInput, Receipt) {
  activity.new(
    "charge_payment",      // the name a worker registers
    input,
    charge_input_codec(),
    receipt_codec(),
    local_charge,          // test-harness implementation; never runs deployed
  )
}
```

`workflow.run(charge_activity(input))` encodes the input, records the
dispatch, and suspends until a worker reports a result; the output codec
decodes the recorded payload. On replay, the recorded result is returned
without re-dispatching.

Two things trip people up:

- The final argument to `activity.new` is a **local implementation used only
  by the `aion/testing` harness**. A deployed workflow always dispatches to
  a remote worker; if no worker serves the activity type, the run waits.
- Every activity name must be declared in `workflow.toml`'s `activities`
  list — packaging fails otherwise.

Failures arrive as typed `error.ActivityError` values: `Retryable` and
`Terminal` (the classification the worker chose), plus
`ActivityDecodeFailed`, `ActivityTimedOut`, `ActivityCancelled`,
`ActivityNonDeterministic`, and `ActivityEngineFailure`.

### Retries are workflow-driven today

`activity.new` attaches no retry policy and the activity runs exactly once
per dispatch. You can attach an explicit `RetryPolicy` (with exponential,
linear, or fixed backoff), and the engine records it — but **engine-side
automatic re-dispatch from that policy is not wired up yet**; dispatch
always stamps attempt 1. Today the honest, replay-deterministic pattern is a
workflow-driven retry loop: match `Error(error.Retryable(..))`, sleep a
durable backoff (`workflow.sleep`), and dispatch a fresh attempt. Each
attempt is its own recorded dispatch, so retry counts replay exactly.
[`examples/order-fulfillment/`](../../examples/order-fulfillment/) implements
this with a bounded attempt budget. See the
[activities and workers guide](activities-and-workers.md) for the
worker-side view.

## Durable timers

```gleam
import aion/duration
import aion/workflow

workflow.sleep(duration.minutes(30))
```

`sleep` records a durable timer and suspends. The timer survives restarts:
a workflow sleeping for three months can be killed and recovered any number
of times and still wakes on schedule. `workflow.start_timer` /
`workflow.cancel_timer` give you a cancellable `TimerRef` for manual races.

## Signals

Signals are typed, durable messages sent into a running workflow:

```gleam
import aion/signal

fn approval_signal() -> signal.SignalRef(Approval) {
  signal.new("approval_decision", approval_codec())
}

// In the workflow body — suspends until the signal arrives:
workflow.receive(approval_signal())
```

Senders use `aion signal <workflow-id> approval_decision --payload '{...}'`,
the HTTP/gRPC APIs, or a client SDK. Signals are recorded events: one
delivered before a crash is still there after recovery. Payloads that fail
the codec return `ReceiveDecodeFailed` as typed data.

## Timeout races

Race any awaitable operation against a durable deadline:

```gleam
case workflow.with_timeout(
  fn() { workflow.receive(approval_signal()) },
  duration.milliseconds(input.approval_timeout_ms),
) {
  Ok(approval) -> ...                                  // signal won
  Error(error.TimedOutError(_)) -> ...                 // deadline won
  Error(error.InnerError(receive_error)) -> ...        // the operation failed
  Error(error.TimeoutEngineFailure(message: m)) -> ... // engine fault
}
```

The race outcome is recorded, so replay resolves the same winner.
`workflow.all` and `workflow.race` combine multiple activities.

## Queries

Queries are **live, read-only** windows into a running workflow:

```gleam
import aion/query

query.handler("order_status", status_codec(), fn() { current_status })
```

The lifecycle is worth understanding precisely:

- **Registration is an idempotent set-insert, re-registered with a fresh
  closure at each state change.** Calling `query.handler` again with the
  same name replaces the reply closure — the standard pattern is a
  `set_status` helper called at every stage transition, so each reply
  reflects live state. Because workflow code re-executes on replay,
  re-registration after recovery is automatic.
- **Queries are answered at yield points.** Every suspending await
  (`workflow.sleep`, `workflow.receive`, `workflow.run`, `child.await`,
  `workflow.all`/`workflow.race`) services pending queries before it
  resolves. A workflow busy between yield points cannot answer until it
  reaches one — callers see `query_timeout` if it takes longer than the
  server's configured `runtime.query_timeout_ms`.
- **Queries never append history.** Handler replies ride a side channel; a
  query changes nothing about the run and does not perturb replay.
- **Handlers must not mutate.** By type a handler only returns a value; by
  contract it must not call `workflow.run` or any recording primitive — the
  engine refuses recording calls made while a query is serviced and surfaces
  them as a handler failure (`query_failed`).

Caller-side error semantics:

| Caller sees | Why |
|---|---|
| `unknown_query` | No handler registered under that name — wrong name, or the workflow has not reached its registration code yet. |
| `query_timeout` | No reply within `runtime.query_timeout_ms` — the workflow is busy between yield points. |
| `not_running` | The run is terminal (completed/failed/cancelled/timed out) or otherwise unable to answer live. **Querying a completed run is an error by design** — inspect terminal runs with `aion describe`, which reads history. |
| `query_failed` | The handler ran and reported an application-level failure. |

See the [errors reference](../errors.md) for the full taxonomy.

## Child workflows

A child workflow is a separate, durable run started and awaited by a parent:

```gleam
// Fire and await separately:
let handle = workflow.spawn("order_shipping", shipping_fn, input,
  input_codec(), output_codec(), error_codec())
child.await(handle)

// Or in one step:
workflow.spawn_and_wait("order_shipping", shipping_fn, input,
  input_codec(), output_codec(), error_codec())
```

The child's terminal result (success, failure, or its own error type) is
recorded in the parent's history. Two packaging rules:

- The child's entry module needs its **own `[[workflow]]` entry** in
  `workflow.toml`, producing its own `.aion` archive (all archives from one
  project share one content hash).
- **Deploy both archives.** The engine resolves a spawned child's workflow
  type by entry module name against loaded packages; loading only the parent
  leaves every spawn failing with an unknown child workflow type.

## Continue-as-new

Long-lived loops (subscriptions, polling cycles) should not grow history
forever. `workflow.continue_as_new(next_input, input_codec)` ends the
current run with a `ContinuedAsNew` terminal and starts a fresh run of the
same type with the supplied input and an empty history.
[`examples/subscription/`](../../examples/subscription/) shows the pattern.

## Workflow timeout

`timeout_seconds` in `workflow.toml` bounds the whole run; expiry records a
`WorkflowTimedOut` terminal (status `TimedOut`). Size it for the longest
legitimate run, including human-scale signal waits.

## Testing

`aion/testing` runs workflows in-process without a server: the harness
executes the local implementation function carried by each
`activity.new(...)`, so the typed body is testable with plain `gleam test`.
The module documentation on
[HexDocs](https://hexdocs.pm/aion_flow/) covers the harness surface.
