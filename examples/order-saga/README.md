# Aion order fulfillment saga

This example demonstrates the durable workflow **saga** pattern in Aion. A saga breaks a business transaction into ordered steps and records compensating actions that undo already-completed work when a later step fails. That is useful for external systems such as inventory, payments, and shipping where a single database transaction cannot cover every side effect.

The workflow runs three forward activities in order:

1. `reserve_inventory`
2. `charge_payment`
3. `ship_order`

It declares three compensating activities:

1. `release_inventory`
2. `refund_payment`
3. `cancel_shipment`

If `charge_payment` fails, inventory has already been reserved, so the workflow runs `release_inventory` and returns a structured `SagaFailed` error. If `ship_order` fails, inventory and payment have both completed, so the workflow runs `refund_payment` and then `release_inventory` in reverse order. `cancel_shipment` is registered and ready for workflows that need to compensate a completed shipment; this three-step example has no later forward step after shipping, so the mandatory failure paths do not call it.

## Flow

Happy path:

```text
OrderInput
  -> reserve_inventory
  -> charge_payment
  -> ship_order
  -> Ok({"order_id":"...","shipment_id":"..."})
```

Charge failure compensation path:

```text
OrderInput
  -> reserve_inventory succeeds
  -> charge_payment fails
  -> release_inventory
  -> Error({"failed_step":"charge_payment", "compensations":["release_inventory"]})
```

Shipping failure compensation path:

```text
OrderInput
  -> reserve_inventory succeeds
  -> charge_payment succeeds
  -> ship_order fails
  -> refund_payment
  -> release_inventory
  -> Error({"failed_step":"ship_order", "compensations":["refund_payment", "release_inventory"]})
```

## Source layout: declare once, generate the rest

Each activity is declared exactly once (ADR-014) — its name, tier, and typed
input/output — in `src/aion_order_saga_activities.gleam`'s `manifest()`, next to
the activity bodies. Everything that must agree byte-for-byte is generated from
that declaration by `aion generate` and carries a do-not-edit header:

| File | Authored by |
| --- | --- |
| `src/aion_order_saga_activities.gleam` (`manifest()` + bodies) | you |
| `src/order_saga.gleam` (workflow orchestration) | you |
| `worker/handlers.py` (Python activity bodies) | you |
| `schemas/*.json` (value-type schemas) | you |
| `src/aion_order_saga_io.gleam` (types + codecs) | generated |
| `src/aion_order_saga_codecs.gleam` (typed codecs) | generated |
| `src/aion_order_saga_activity_wrappers.gleam` (`activity.new` wrappers) | generated |
| `worker/worker.py` (worker plumbing → `handlers.py`) | generated |
| `test/aion_order_saga_wire_compat_test.gleam` (wire goldens) | generated |
| the `activities` list in `workflow.toml` | generated |

Run `aion generate examples/order-saga` after editing a declaration or schema,
and `aion generate examples/order-saga --check` in CI — it regenerates every
file in memory and fails if any on-disk copy has drifted, so a hand-edit to
generated output is a build error.

You will:

1. build a Gleam workflow that uses `aion_flow`,
2. package the compiled BEAM modules into `order-saga.aion`,
3. start the Aion dev server with the repo-root `dev-config.toml`,
4. start a Python activity worker for all six saga activities,
5. run a happy-path workflow instance,
6. run charge-failure and shipping-failure compensation instances, and
7. inspect the durable event history.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.11 or newer
- `curl`

All commands below are copy-pasteable from the repository root unless noted.

Install the CLI once from the checkout (the crate is aion-cli; the binary is `aion`):

```sh
cargo install --path crates/aion-cli --locked
```

## 1. Generate the plumbing and build the Gleam workflow

```sh
aion generate examples/order-saga
cd examples/order-saga
gleam build
cd ../..
```

`aion generate` reads the declarations from `manifest()` and (re)writes the
generated files listed above; the committed copies are already current, so this
is a no-op on a clean checkout. `gleam build` then compiles the workflow.

The workflow source lives in `examples/order-saga/src/order_saga.gleam`. It exposes `run`, accepts JSON shaped like:

```json
{"order_id":"order-1001","item":"widget","quantity":2,"amount":5000}
```

The `amount` field is an integer amount in cents. The workflow dispatches forward and compensating activities through `aion_flow` and records those decisions in durable workflow history.

## 2. Package `order-saga.aion`

```sh
aion package examples/order-saga
```

This reads the example's [`workflow.toml`](workflow.toml) and the BEAM files produced by `gleam build` (pass `--build` to compile and package in one step; see [`docs/packaging.md`](../../docs/packaging.md) for the full reference), and builds a manifest with:

- entry module: `order_saga`
- entry function: `run`
- input schema: object with required fields `order_id`, `item`, `quantity`, and `amount`
- output schema: successful `Shipment` or failed `SagaFailed` shape
- activities: `reserve_inventory`, `charge_payment`, `ship_order`, `release_inventory`, `refund_payment`, `cancel_shipment`

It writes:

```text
examples/order-saga/order-saga.aion
```

The repo-root `dev-config.toml` is the local development config. If you want the server to preload this package at startup, add `examples/order-saga/order-saga.aion` to its `workflow_packages` array after building the package.

## 3. Start the Aion dev server

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP `127.0.0.1:8080`, uses the in-memory store, and defaults to the `default` namespace.

In terminal 1:

```sh
aion server --config dev-config.toml
```

Leave this process running. The dashboard/static UI at `http://127.0.0.1:8080/` is under development; use the HTTP API observe commands below (or Aion CLI commands where available) to inspect workflows for now.

## 4. Start the Python activity worker

In terminal 2, create a virtual environment and install the local worker SDK:

```sh
python3 -m venv .venv-aion-order-saga
. .venv-aion-order-saga/bin/activate
python -m pip install --upgrade pip
python -m pip install -e sdks/python/aion-worker
```

Then run the worker for the happy path:

```sh
python examples/order-saga/worker/worker.py
```

The worker connects to `127.0.0.1:50051`, registers exactly `reserve_inventory`, `charge_payment`, `ship_order`, `release_inventory`, `refund_payment`, and `cancel_shipment`, and logs each activity as it runs. Leave this process running for the happy-path demo.

## 5. Happy path demo

In terminal 3:

```sh
ORDER_JSON='{"order_id":"order-1001","item":"widget","quantity":2,"amount":5000}'
ORDER_BYTES=$(printf '%s' "$ORDER_JSON" | python3 -c 'import sys; print(",".join(str(byte) for byte in sys.stdin.read().encode("utf-8")))')

START_RESPONSE=$(curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: order-saga-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_type\": \"order_saga\",
    \"input\": {
      \"content_type\": \"application/json\",
      \"bytes\": [$ORDER_BYTES]
    }
  }")
printf '%s\n' "$START_RESPONSE"
```

Capture the workflow id for observation:

```sh
WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | python3 -c 'import json, sys; print(json.load(sys.stdin)["workflow_id"]["uuid"])')
RUN_ID=$(printf '%s' "$START_RESPONSE" | python3 -c 'import json, sys; print(json.load(sys.stdin)["run_id"]["uuid"])')
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

The worker logs should show the forward sequence only:

```text
Reserving inventory ...
Charging payment ...
Shipping order ...
```

The completed workflow result should be shaped like:

```json
{"order_id":"order-1001","shipment_id":"ship-order-1001"}
```

## 6. Charge failure compensation demo

Stop the worker from terminal 2 with `Ctrl-C`, then restart it with charge failure enabled:

```sh
SIMULATE_CHARGE_FAILURE=true python examples/order-saga/worker/worker.py
```

In terminal 3, start a second workflow:

```sh
ORDER_JSON='{"order_id":"order-1002","item":"widget","quantity":2,"amount":5000}'
ORDER_BYTES=$(printf '%s' "$ORDER_JSON" | python3 -c 'import sys; print(",".join(str(byte) for byte in sys.stdin.read().encode("utf-8")))')

START_RESPONSE=$(curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: order-saga-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_type\": \"order_saga\",
    \"input\": {
      \"content_type\": \"application/json\",
      \"bytes\": [$ORDER_BYTES]
    }
  }")
printf '%s\n' "$START_RESPONSE"
```

The worker logs should show inventory reservation, the simulated charge failure, and only inventory release:

```text
Reserving inventory ...
Payment failed intentionally ...
Compensating inventory reservation ...
```

The workflow error payload should be shaped like:

```json
{
  "type": "saga_failed",
  "failed_step": "charge_payment",
  "reason": "simulated charge failure for order order-1002",
  "completed_steps": ["reserve_inventory"],
  "compensations": [
    {"step": "release_inventory", "status": "released", "detail": "released 2 x widget from res-order-1002"}
  ]
}
```

## 7. Shipping failure compensation demo

Restart the worker with shipping failure enabled:

```sh
SIMULATE_SHIPPING_FAILURE=true python examples/order-saga/worker/worker.py
```

In terminal 3, start another workflow:

```sh
ORDER_JSON='{"order_id":"order-1003","item":"widget","quantity":2,"amount":5000}'
ORDER_BYTES=$(printf '%s' "$ORDER_JSON" | python3 -c 'import sys; print(",".join(str(byte) for byte in sys.stdin.read().encode("utf-8")))')

START_RESPONSE=$(curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: order-saga-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_type\": \"order_saga\",
    \"input\": {
      \"content_type\": \"application/json\",
      \"bytes\": [$ORDER_BYTES]
    }
  }")
printf '%s\n' "$START_RESPONSE"
```

The worker logs should show the forward steps until shipping fails, followed by reverse compensation:

```text
Reserving inventory ...
Charging payment ...
Shipping failed intentionally ...
Compensating payment charge ...
Compensating inventory reservation ...
```

The workflow error payload should be shaped like:

```json
{
  "type": "saga_failed",
  "failed_step": "ship_order",
  "reason": "simulated shipping failure for order order-1003",
  "completed_steps": ["reserve_inventory", "charge_payment"],
  "compensations": [
    {"step": "refund_payment", "status": "refunded", "detail": "refunded 5000 from pay-order-1003"},
    {"step": "release_inventory", "status": "released", "detail": "released 2 x widget from res-order-1003"}
  ]
}
```

## 8. Observe the event history

List workflows:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/list \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: order-saga-user' \
  -H 'x-aion-namespaces: default' \
  --data '{"namespace":"default"}'
```

Describe the latest workflow and include history:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/describe \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: order-saga-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_id\": { \"uuid\": \"$WORKFLOW_ID\" },
    \"run_id\": { \"uuid\": \"$RUN_ID\" },
    \"include_history\": true
  }"
```

In the happy-path history, observe durable scheduling and completion events for:

1. `reserve_inventory`
2. `charge_payment`
3. `ship_order`

In the charge-failure history, observe that `charge_payment` fails after inventory has completed, then the workflow records:

1. `release_inventory`

In the shipping-failure history, observe that `ship_order` fails after inventory and payment have completed, then the workflow records reverse compensation:

1. `refund_payment`
2. `release_inventory`

That event history is the durable saga guarantee: after a failure, recovery/replay can see exactly which forward steps completed and which compensations were scheduled or completed.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-order-saga examples/order-saga/order-saga.aion examples/order-saga/build
```
