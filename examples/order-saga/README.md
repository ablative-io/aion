# Aion order fulfillment saga

This example demonstrates the canonical durable workflow saga pattern in Aion. The workflow processes an order through four forward activities:

1. `charge_payment`
2. `reserve_inventory`
3. `ship_order`
4. `confirm_order`

If a later step fails after earlier steps have completed, the workflow schedules compensating activities in reverse order. For example, when `ship_order` fails, the workflow first runs `release_inventory` and then `refund_payment`, and finally returns a structured `SagaFailed` error describing the failed step and compensation results.

You will:

1. build a Gleam workflow that uses `aion_flow`,
2. package the compiled BEAM modules into `order-saga.aion`,
3. start the Aion dev server with the repo-root `dev-config.toml`,
4. start a Python activity worker for all six saga activities,
5. run a happy-path workflow instance,
6. run a compensation-path workflow instance, and
7. inspect the durable event history for both paths.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.11 or newer
- `curl`

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/order-saga
gleam build
cd ../..
```

The workflow source lives in `examples/order-saga/src/order_saga.gleam`. It exposes `run`, accepts JSON shaped like:

```json
{"order_id":"order-1001","item":"widget","quantity":2,"amount":5000}
```

The `amount` field is an integer amount in cents. The workflow dispatches forward activities through `aion_flow` and records compensating activity decisions in the same durable workflow history.

## 2. Package `order-saga.aion`

```sh
cargo run --manifest-path examples/order-saga/packager/Cargo.toml
```

This reads the BEAM files produced by `gleam build`, builds a manifest with:

- entry module: `order_saga`
- entry function: `run`
- input schema: object with required fields `order_id`, `item`, `quantity`, and `amount`
- output schema: successful `OrderConfirmation` or failed `SagaFailed` shape
- activities: `charge_payment`, `reserve_inventory`, `ship_order`, `confirm_order`, `release_inventory`, `refund_payment`

It writes:

```text
examples/order-saga/order-saga.aion
```

The repo-root `dev-config.toml` is the local development config. If you want the server to preload this package at startup, add `examples/order-saga/order-saga.aion` to its `workflow_packages` array after building the package.

## 3. Start the Aion dev server

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP `127.0.0.1:8080`, uses the in-memory store, and defaults to the `default` namespace.

In terminal 1:

```sh
cargo run -p aion-server -- --config dev-config.toml
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
python examples/order-saga/worker.py
```

The worker connects to `127.0.0.1:50051`, registers all six activities, and logs each activity as it runs. Leave this process running for the happy-path demo.

## 5. Happy path demo

In terminal 3:

```sh
ORDER_JSON='{"order_id":"order-1001","item":"widget","quantity":2,"amount":5000}'
ORDER_BYTES=$(python3 - <<'PY' <<<"$ORDER_JSON"
import sys
print(",".join(str(byte) for byte in sys.stdin.read().encode("utf-8")))
PY
)

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
WORKFLOW_ID=$(python3 - <<'PY' <<<"$START_RESPONSE"
import json, sys
print(json.load(sys.stdin)["workflow_id"]["uuid"])
PY
)
RUN_ID=$(python3 - <<'PY' <<<"$START_RESPONSE"
import json, sys
print(json.load(sys.stdin)["run_id"]["uuid"])
PY
)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

The worker logs should show the forward sequence only:

```text
Charging payment ...
Reserving inventory ...
Shipping order ...
Confirming order ...
```

The completed workflow result should be shaped like:

```json
{"order_id":"order-1001","shipment_id":"ship-order-1001","confirmation_id":"conf-order-1001"}
```

## 6. Compensation path demo

Stop the worker from terminal 2 with `Ctrl-C`, then restart it with shipping failure enabled:

```sh
SIMULATE_SHIPPING_FAILURE=true python examples/order-saga/worker.py
```

In terminal 3, start a second workflow:

```sh
ORDER_JSON='{"order_id":"order-1002","item":"widget","quantity":2,"amount":5000}'
ORDER_BYTES=$(python3 - <<'PY' <<<"$ORDER_JSON"
import sys
print(",".join(str(byte) for byte in sys.stdin.read().encode("utf-8")))
PY
)

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

Capture the ids again:

```sh
WORKFLOW_ID=$(python3 - <<'PY' <<<"$START_RESPONSE"
import json, sys
print(json.load(sys.stdin)["workflow_id"]["uuid"])
PY
)
RUN_ID=$(python3 - <<'PY' <<<"$START_RESPONSE"
import json, sys
print(json.load(sys.stdin)["run_id"]["uuid"])
PY
)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

The worker logs should show the forward steps until shipping fails, followed by reverse compensation:

```text
Charging payment ...
Reserving inventory ...
Shipping failed intentionally ...
Compensating inventory reservation ...
Compensating payment charge ...
```

The workflow error payload should be shaped like:

```json
{
  "type": "saga_failed",
  "failed_step": "ship_order",
  "reason": "simulated shipping failure for order order-1002",
  "completed_steps": ["charge_payment", "reserve_inventory"],
  "compensations": [
    {"step": "release_inventory", "status": "released", "detail": "released 2 x widget from res-order-1002"},
    {"step": "refund_payment", "status": "refunded", "detail": "refunded 5000 from pay-order-1002"}
  ]
}
```

## 7. Observe the event history

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

1. `charge_payment`
2. `reserve_inventory`
3. `ship_order`
4. `confirm_order`

In the compensation-path history, observe that `ship_order` fails after payment and inventory have completed, then the workflow records the compensating activities in reverse order:

1. `release_inventory`
2. `refund_payment`

That event history is the durable saga guarantee: after a failure, recovery/replay can see exactly which forward steps completed and which compensations were scheduled or completed.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-order-saga examples/order-saga/order-saga.aion examples/order-saga/build
```
