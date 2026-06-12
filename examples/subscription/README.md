# Subscription lifecycle example

This example defines a long-lived subscription workflow with the typed `aion_flow` Gleam SDK. It demonstrates three core lifecycle patterns:

1. **Durable billing timers** — the workflow waits for `billing_period_seconds` before each billing cycle. The demo uses seconds so it is easy to run locally; a production subscription system would keep calendar/billing policy outside this example and pass an appropriate duration into the workflow.
2. **Plan-change signals** — while the workflow is waiting for the next billing period it listens for a typed `plan_change` signal. Upgrade and downgrade signals update the in-memory subscription state, and the next `bill_subscriber` activity receives the latest plan.
3. **Continue-as-new history rotation** — after `max_cycles` cycles in one run, the workflow carries the current subscription state into a fresh execution so the old event history stays bounded.

## Why continue-as-new matters

Long-lived workflows accumulate history: timer-scheduled, timer-fired, signal-received, activity-scheduled, and activity-completed events are all recorded so replay can rebuild deterministic state. A subscription that runs for years would eventually have a very large history if it stayed in one execution forever. Large histories make replay slower and harder to inspect.

Continue-as-new solves this by ending the current run with a terminal `ContinuedAsNew` event and starting a replacement run with the **same workflow id** and a **new run id**. The workflow author explicitly passes the state that must survive the rotation. In this example that carried input includes:

- subscriber id and email,
- the current plan after any upgrade or downgrade signals,
- the next absolute billing cycle number,
- the billing period configuration,
- the per-run cycle counter reset to `0`.

The replacement run replays only its own fresh history, while the old run remains available as a bounded record of up to `max_cycles` billing cycles.

> Implementation note: the Rust engine already supports continue-as-new, but this worktree does not yet expose a public `workflow.continue_as_new()` binding in `aion_flow`. The example calls the intended SDK API so the source documents the lifecycle pattern and will compile once that binding lands.

## Lifecycle walkthrough

For each cycle the workflow:

1. waits for `billing_period_seconds` using a durable timer;
2. receives any `plan_change` signal that arrives before the timer fires;
3. updates the plan for subsequent billing when the signal payload has `"direction": "upgrade"` or `"direction": "downgrade"`;
4. executes the stub `bill_subscriber` activity with the current plan and absolute cycle number;
5. increments `current_cycle` and `cycles_in_run`;
6. calls `workflow.continue_as_new(...)` when `cycles_in_run >= max_cycles`, carrying the next cycle state into the replacement run.

Durable timers survive engine restart. If the engine restarts while the workflow is asleep, replay observes the recorded timer events and resumes at the correct point in the billing cycle.

## Build

From the repository root:

```sh
cd examples/subscription
gleam build
```

As noted above, the build currently requires the `aion_flow` continue-as-new SDK binding. Until that binding exists, the source-level example is intentionally blocked at the `workflow.continue_as_new(...)` call.

## Start input

Use short periods for local runs. This sample bills every 10 seconds and rotates history after 3 cycles in the current run:

```json
{
  "subscriber_id": "sub_123",
  "subscriber_email": "ada@example.com",
  "plan": "starter",
  "current_cycle": 1,
  "billing_period_seconds": 10,
  "max_cycles": 3,
  "cycles_in_run": 0
}
```

`current_cycle` is the absolute cycle number across all runs. `max_cycles` is cycles per run, not total subscription lifetime.

## Running and signaling

Install the CLI once from the checkout (the crate is aion-cli; the binary is `aion`):

```sh
cargo install --path crates/aion-cli --locked
```

Package the example with `aion package examples/subscription` (after `gleam build`, or pass `--build`; see [`docs/packaging.md`](../../docs/packaging.md)), which reads the example's [`workflow.toml`](workflow.toml) and writes `examples/subscription/subscription.aion`. Once the archive is loaded by the server and a worker exposing the `bill_subscriber` activity is registered, start the `subscription` workflow with the JSON input above using the same CLI flow as the other examples.

Send an upgrade signal during a billing period:

```sh
aion --subject subscription-user signal "$WORKFLOW_ID" plan_change \
  --payload '{"direction":"upgrade","plan":"pro"}'
```

Send a downgrade signal during a billing period:

```sh
aion --subject subscription-user signal "$WORKFLOW_ID" plan_change \
  --payload '{"direction":"downgrade","plan":"starter"}'
```

Omit `--run-id` for normal use. The CLI targets the latest run, which is important after continue-as-new because the workflow id remains stable while run ids rotate.

## Inspecting history rotation

After the configured number of cycles, describe the workflow and inspect history. The old run should end with a continue-as-new event, and the replacement run should have the same workflow id, a new run id, fresh event history, and input whose `current_cycle` is the next cycle to bill.
