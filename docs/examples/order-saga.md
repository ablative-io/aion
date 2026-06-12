# The order-fulfillment saga, end to end

This walkthrough follows Aion's flagship example — a realistic
order-processing saga — through every major engine capability in one
coherent business flow: a retried payment charge, a human approval signal
raced against a durable deadline, a shipping child workflow, a live status
query, refund compensation, a mid-flight engine kill and recovery, and a
versioned redeploy with run pinning.

- Workflow source: [`examples/order-fulfillment/`](../../examples/order-fulfillment/)
- Runnable proof: [`crates/aion/tests/order_saga_e2e.rs`](../../crates/aion/tests/order_saga_e2e.rs)

## The business flow

An order arrives with an id, an item, an amount in cents, and an approval
deadline. The saga:

```text
received
  -> charging            charge_payment (attempt 1 fails transiently,
                          durable backoff sleep, attempt 2 succeeds)
  -> awaiting_approval   wait for the approval_decision signal,
                          raced against a durable deadline
       approve ----------> shipping     order_shipping child workflow
                             -> completed   (status "completed")
       reject / timeout -> compensating refund_payment
       shipping failure -/   -> cancelled   (status "cancelled")
```

Every arrow is a durably recorded step. Kill the engine anywhere and replay
re-executes the workflow code from the top, resolving each recorded step
from history instead of re-running it.

A compensated order is a *successful* workflow run: the saga returns
`OrderResult{status: "cancelled", refund_id: Some(..)}` rather than failing
the workflow. Workflow failure is reserved for faults the saga cannot
absorb.

## How each capability is authored

The parent workflow is `examples/order-fulfillment/src/order_fulfillment.gleam`;
shared types and codecs live in `order_types.gleam`; the child in
`order_shipping.gleam`.

### Activity with retry

The engine records each dispatch (`ActivityScheduled`), its delivery
(`ActivityStarted`), and its outcome (`ActivityCompleted` /
`ActivityFailed`). `charge_payment` attaches an explicit `RetryPolicy` and
the workflow drives a bounded retry loop over a durable `workflow.sleep`
backoff:

```gleam
fn charge_with_retries(input, attempt) {
  use _ <- result_try(set_status("charging", input.order_id, attempt, None))
  case workflow.run(charge_payment_activity(charge)) {
    Ok(receipt) -> Ok(#(receipt, attempt))
    Error(error.Retryable(..)) if attempt < max_payment_attempts ->
      case workflow.sleep(duration.milliseconds(payment_backoff_ms)) {
        Ok(Nil) -> charge_with_retries(input, attempt + 1)
        ...
      }
    Error(other) -> Error(OrderFailed(stage: "charge_payment", ...))
  }
}
```

Each retry is a *fresh recorded dispatch* whose input carries the attempt
number, so the attempt count is replay-deterministic. (Engine-side automatic
retry from the declared policy is not wired yet — see
[Findings](#findings-from-building-this-showcase).)

### Signal raced against a durable timeout

```gleam
case workflow.with_timeout(
  fn() { workflow.receive(approval_signal()) },
  duration.milliseconds(input.approval_timeout_ms),
) {
  Ok(Approval(decision: Approve, approver:)) -> ship(...)
  Ok(Approval(decision: Reject, approver:)) -> compensate(...)
  Error(error.TimedOutError(_)) -> compensate(...)
  ...
}
```

If the signal wins, history records `SignalReceived` plus `TimerCancelled`
for the deadline; if the deadline wins, `TimerFired` and the compensation
path. Both outcomes are durable facts replay resolves identically.

### Child workflow

`order_shipping` is its own `[[workflow]]` entry in `workflow.toml`, so it
ships as its own archive. The engine resolves a spawned child's workflow
type by entry module name against loaded packages — both archives must be
loaded:

```gleam
workflow.spawn_and_wait(
  order_shipping.workflow_type,  // "order_shipping"
  order_shipping.execute,
  ShippingInput(...),
  order_types.shipping_input_codec(),
  order_types.shipment_codec(),
  order_types.shipping_error_codec(),
)
```

The parent's history records `ChildWorkflowStarted` /
`ChildWorkflowCompleted`; the child has its own history with its own
`ship_order` activity events.

### Query at every stage

The `order_status` handler is re-registered with the current state before
each stage transition, so a reply always reflects live state:

```gleam
query.handler(status_query_name, order_types.order_status_codec(), fn() {
  status
})
```

Queries are answered at engine yield points (activity awaits, sleeps, signal
receives, child awaits), append nothing to history, and re-register
automatically on replay — a recovered workflow answers queries with no extra
author code. A terminal workflow answers with the typed `not_running` error.

### Compensation

Rejection, timeout, and shipping failure all converge on one compensation
routine: run `refund_payment`, then complete the order as `cancelled` with
the refund id and a human-readable reason in the result payload.

## The end-to-end proof

`crates/aion/tests/order_saga_e2e.rs` drives the packaged archives through
the real engine (in-process harness: real BEAM VM, real signal router, real
query service, real package loading — the activity worker is a deterministic
in-process `ActivityDispatcher` stand-in). The suite builds both archives
from the committed Gleam source on every run (`gleam build` + packaging; a
missing Gleam toolchain fails the gate loudly rather than skipping):

```sh
cargo test -p aion-rs --test order_saga_e2e
```

The four tests:

- **`order_completes_after_payment_retry_engine_restart_and_approval`** —
  the full happy path plus the durability proof. The dispatcher fails charge
  attempt 1 and gates attempt 2 open so the test can observe
  `charging`/attempt 2 through the query; after payment the engine is shut
  down mid-flight and a second engine is built over the same store. The test
  asserts post-recovery history is byte-identical to the pre-kill history,
  that the recovered run answers the same query, and that the post-restart
  approval signal drives the shipping child to completion — with the
  second engine's dispatcher seeing only `ship_order` (recorded activities
  are never re-executed).
- **`order_cancels_and_refunds_when_rejected`** — a rejection signal
  refunds and completes the order as business-`cancelled`; the deadline
  records `TimerCancelled`; no child workflow ever starts.
- **`order_cancels_and_refunds_when_approval_times_out`** — no decision
  arrives; the durable deadline fires (`TimerFired`), the saga refunds and
  cancels.
- **`v2_deploy_mid_flight_pins_v1_and_routes_new_orders_to_v2`** — with a
  v1 run parked at `awaiting_approval`, a v2 archive (same entry, new
  content hash) is deployed into the running engine via
  `Engine::load_package`. Unloading v1 is refused with the typed
  `VersionPinned` error while the run is live; a new order starts on v2 and
  completes the whole saga there; the pinned run completes on v1 (each
  run's `WorkflowStarted` records its package version); after the v1 run
  ends, v1 unloads cleanly. The HTTP/CLI deploy surface over this same
  engine API is covered separately by
  `crates/aion-server/tests/deploy_api_e2e.rs`.

## Findings from building this showcase

Building and running this scenario for real surfaced the following (all
verified empirically; see the test sources for the guards that encode them):

1. **beamr VM: >64-byte engine-to-Gleam await payloads killed the workflow
   (fixed in beamr 0.6.0 / aion 0.4.0).** On beamr 0.4.6–0.5.0, any activity
   result or failure payload over 64 bytes delivered to a *Gleam-compiled*
   workflow at an await died with `VM execution error: bad argument`. beamr
   0.6.0 fixed the refc-binary BIFs; the pin tests that tripped on this
   defect were flipped back to full end-to-end completion — live queries
   during child awaits with realistic payloads now complete.
2. **beamr VM: wake re-entry breaks when a suspending closure re-executes a
   cross-module call.** A pump-wrapped await whose closure body computes an
   argument via a cross-module call (e.g. the SDK's old
   `fn() { ffi.sleep(duration_to_boundary(duration)) }`) dies on wake with
   `bad function term ...`. The SDK now precomputes await arguments so every
   pump closure body is a single native call on captured values
   (`aion_flow` `timer.sleep`, `signal.receive`, `child.await`) — also the
   documented re-execution-safety contract for await fun bodies.
3. **Engine-side automatic activity retry is unbuilt.** The SDK encodes the
   declared `RetryPolicy` into the dispatch config and the wire carries a
   one-based attempt, but no engine component consumes the policy; every
   dispatch is stamped attempt 1
   (`crates/aion/src/runtime/nif_activity_dispatch.rs`,
   `FIRST_DELIVERY_ATTEMPT`). Workflow-driven retry loops are the supported
   pattern today.
4. **In-VM dispatcher failures are always recorded `Terminal`.** The
   `ActivityDispatcher` seam records every failure as
   `ActivityErrorKind::Terminal` and leaves the `retryable:`/`terminal:`
   prefix in the recorded message
   (`crates/aion/src/runtime/handle/delivery.rs`, `activity_failure`); only
   the Gleam SDK interprets the prefix. Durable history therefore
   misclassifies transient failures.
5. **Example behavioral tests used to skip silently when archives were not
   built (fixed).** These suites previously skipped when `.aion` archives
   were absent, so a green run did not imply the Gleam SDK path was
   exercised — the beamr 0.5.0 bump was validated green while every
   Gleam-workflow behavioral test was skipping. Every gate now rebuilds its
   archives from committed Gleam source on each run, and a missing `gleam`
   toolchain fails the gate instead of skipping.
