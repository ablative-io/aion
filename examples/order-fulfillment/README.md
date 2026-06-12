# Aion order-fulfillment saga (flagship example)

This is the flagship end-to-end Aion example: one realistic order-processing
saga that exercises the engine's durable-workflow machinery together, the way
a production adopter would.

In a single business flow, the `order_fulfillment` workflow demonstrates:

1. **Activity with retry** — `charge_payment` fails transiently on its first
   attempt; the workflow retries it over a durable backoff sleep. Each
   attempt is a fresh recorded dispatch carrying the attempt number in the
   activity input.
2. **Signal raced against a durable timeout** — after payment, the saga waits
   for a human `approval_decision` signal inside `workflow.with_timeout`.
   Approve in time and the order ships; reject (or let the deadline fire)
   and the saga compensates.
3. **Child workflow** — on approval, the parent starts the `order_shipping`
   child workflow (its own `[[workflow]]` entry and archive) and awaits its
   recorded terminal.
4. **Query** — an `order_status` query is answerable at every stage:
   `received` → `charging` → `awaiting_approval` → `shipping` → `completed`,
   or `compensating` → `cancelled` on the compensation path. The handler is
   re-registered as the saga advances, and replay re-registers it
   automatically after a restart.
5. **Saga compensation** — rejection, approval timeout, and shipping failure
   all run `refund_payment` and complete the order with business status
   `cancelled`. A compensated saga is a *successful* workflow run.
6. **Durability** — kill the engine after payment and before approval,
   restart it over the same store, and replay restores the exact recorded
   history; the post-restart approval signal drives the run to completion
   without re-executing the recorded charge attempts.
7. **Versioned deploy** — deploy a v2 of the saga while a v1 run waits for
   approval: the pinned v1 run refuses unload, completes on v1, and new
   starts land on v2.

The CI-runnable proof of all seven lives in
[`crates/aion/tests/order_saga_e2e.rs`](../../crates/aion/tests/order_saga_e2e.rs),
and a full walkthrough in
[`docs/examples/order-saga.md`](../../docs/examples/order-saga.md).

## Layout

```text
src/order_types.gleam        shared domain types + codecs (both workflows)
src/order_fulfillment.gleam  the parent saga
src/order_shipping.gleam     the shipping child workflow
workflow.toml                two [[workflow]] entries -> two archives
schemas/                     input/output JSON schemas per entry
worker/worker.py             Python activity worker for the server path
```

## Build and package

Install the CLI once from the checkout if you have not
(`cargo install --path crates/aion-cli --locked`; the binary is `aion`),
then:

```sh
aion package examples/order-fulfillment --build
```

This compiles the Gleam project and writes both archives:

```text
examples/order-fulfillment/order-fulfillment.aion
examples/order-fulfillment/order-shipping.aion
```

Both archives share one beam set (and therefore one content hash); only the
entry module differs. **Load both** — the engine resolves a spawned child's
workflow type by entry module name against its loaded packages, so loading
only the parent leaves every spawn failing with an unknown child workflow
type.

## Run the engine-level end-to-end proof

```sh
cargo test -p aion-rs --test order_saga_e2e
```

The suite rebuilds both archives from the committed Gleam source on every
run; a missing `gleam` toolchain fails the gate loudly rather than skipping.

## Run against a live server + Python worker

Start the dev server with the deploy surface enabled (terminal 1) — the
repo-root `dev-config.toml` keeps `[deploy]` dark by default, so commission
it from the environment:

```sh
AION_DEPLOY_ENABLED=true \
AION_DEPLOY_MAX_ARCHIVE_BYTES=16777216 \
AION_DEPLOY_MAX_INFLATED_BYTES=67108864 \
aion server --config dev-config.toml
```

Install and start the worker with the transient charge failure enabled
(terminal 2):

```sh
python3 -m venv .venv-aion-order
. .venv-aion-order/bin/activate
python -m pip install -e sdks/python/aion-worker
SIMULATE_TRANSIENT_CHARGE_FAILURE=true python examples/order-fulfillment/worker/worker.py
```

Deploy both archives and start an order (terminal 3):

```sh
aion deploy examples/order-fulfillment/order-fulfillment.aion
aion deploy examples/order-fulfillment/order-shipping.aion

ORDER_JSON='{"order_id":"o1","item":"widget","quantity":2,"amount_cents":4999,"approval_timeout_ms":300000}'
ORDER_BYTES=$(printf '%s' "$ORDER_JSON" | python3 -c 'import sys; print(",".join(str(b) for b in sys.stdin.read().encode()))')
curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: demo' -H 'x-aion-namespaces: default' \
  --data "{\"namespace\":\"default\",\"workflow_type\":\"order_fulfillment\",\"input\":{\"content_type\":\"application/json\",\"bytes\":[$ORDER_BYTES]}}"
```

Query the order status while it waits for approval (substitute the returned
ids):

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/query \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: demo' -H 'x-aion-namespaces: default' \
  --data '{"namespace":"default","workflow_id":{"uuid":"<workflow-id>"},"query_name":"order_status"}'
```

Approve (or reject) the order:

```sh
DECISION_JSON='{"decision":"approve","approver":"cfo"}'
DECISION_BYTES=$(printf '%s' "$DECISION_JSON" | python3 -c 'import sys; print(",".join(str(b) for b in sys.stdin.read().encode()))')
curl -sS -X POST http://127.0.0.1:8080/workflows/signal \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: demo' -H 'x-aion-namespaces: default' \
  --data "{\"namespace\":\"default\",\"workflow_id\":{\"uuid\":\"<workflow-id>\"},\"signal_name\":\"approval_decision\",\"payload\":{\"content_type\":\"application/json\",\"bytes\":[$DECISION_BYTES]}}"
```

Describe with history to see the recorded saga (charge failure + retry,
signal, child workflow, completion):

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/describe \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: demo' -H 'x-aion-namespaces: default' \
  --data '{"namespace":"default","workflow_id":{"uuid":"<workflow-id>"},"include_history":true}'
```

## Known engine limitations exercised here

- **64-byte engine-to-Gleam payload ceiling — fixed in beamr 0.6.0 (aion
  0.4.0).** Older beamr releases (0.4.6–0.5.0) killed a Gleam-compiled
  workflow receiving an await payload over 64 bytes; beamr 0.6.0 fixed the
  refc-binary BIFs and realistic payloads now flow end-to-end. This
  example's ids and messages predate the fix and are deliberately short.
- **Engine-side automatic retry is not wired yet.** `charge_payment`
  declares a `RetryPolicy` and the policy rides the dispatch config
  verbatim, but no engine component consumes it: every wire delivery is
  stamped attempt 1. This saga therefore drives its own bounded retry loop —
  which also keeps retry counts replay-deterministic.
- **In-VM dispatcher failures are recorded as `Terminal`.** The
  `ActivityDispatcher` seam records every failure as
  `ActivityErrorKind::Terminal` with the `retryable:`/`terminal:` prefix
  left in the message; the Gleam SDK parses the prefix into the typed
  classification.
