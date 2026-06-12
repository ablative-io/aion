# Aion batch orchestrator

This example demonstrates parent-child orchestration with the typed `aion_flow` SDK. The parent workflow accepts a list of work items, starts one child workflow for each item, registers a `batch_progress` query for live inspection, awaits every child, and returns a final summary with per-item outcomes.

Use this pattern when each item needs its own workflow execution boundary: independent failure handling, independent retry/cancellation history, and a clear per-item audit trail. Use fan-out helpers only for related activity fan-out inside one workflow execution. They are not a substitute for child workflows when you want each item to have an isolated failure domain.

## What is in the example

- `src/batch_orchestrator.gleam` defines the parent `batch-orchestrator` workflow: it spawns one child per work item, registers the `batch_progress` query, awaits every child, and aggregates outcomes.
- `src/batch_orchestrator_item.gleam` defines the single-item child workflow. It is a separate entry module because **the engine resolves a spawned child's workflow type against loaded packages by entry module name** — the same way `start` resolves a top-level type. The parent spawns children of type `batch_orchestrator_item`, so that module must be packaged and loaded in its own right.
- `workflow.toml` declares both workflows. Packaging produces two archives that share one content-hash version; the engine must load **both**.
- The child runs one typed `process-batch-item` activity, served by the Python worker in `worker/worker.py`. The worker stub is deterministic: ids or payloads containing `fail` raise a terminal failure, everything else is processed successfully. The Gleam module carries a local stub with the same contract for the pure-Gleam test double.
- The parent calls `child.await()` for each handle. Successful children become `succeeded` item outcomes, and child failures become `failed` item outcomes instead of crashing the parent.
- The parent registers a read-only `batch_progress` query with `query.handler(...)`; child awaits are yield points, so pending queries are answered while the parent waits.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.11+ for the activity worker
- `jq`, optional but useful for extracting workflow ids from CLI JSON

All commands below are copy-pasteable from the repository root unless noted.

Install the CLI once from the checkout (the crate is aion-cli; the binary is `aion`):

```sh
cargo install --path crates/aion-cli --locked
```

## 1. Build the Gleam workflows

```sh
cd examples/batch-orchestrator
gleam build
cd ../..
```

The source consumes only the public `aion_flow` modules: `aion/workflow`, `aion/child`, `aion/query`, `aion/activity`, `aion/codec`, and `aion/error`.

> Archives are local build artifacts. Rebuild them after updating the repository: an archive built against an older `aion_flow` fails at query registration on the current engine (`VM execution error: undefined function aion_flow_ffi:register_query/3` — the registration NIF is `register_query/2` now).

## 2. Package and load both workflows

```sh
aion package examples/batch-orchestrator
```

This reads the example's [`workflow.toml`](workflow.toml) and the BEAM files produced by `gleam build` (pass `--build` to compile and package in one step; see [`docs/packaging.md`](../../docs/packaging.md) for the full reference) and writes **two** archives:

- `examples/batch-orchestrator/batch-orchestrator.aion` — parent type `batch_orchestrator`
- `examples/batch-orchestrator/batch-orchestrator-item.aion` — child type `batch_orchestrator_item`

Both share the same beam set and content-hash version; only the entry module differs. The generated `manifest.toml` is committed so dependency resolution is reproducible, matching the other Gleam examples.

Start the dev server in terminal 1 with **both** packages loaded — the parent spawns children by type, and an engine that has only the parent archive fails every spawn with an unknown child workflow type:

```sh
AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY=1024 \
AION_RUNTIME_QUERY_TIMEOUT_MS=10000 \
aion server \
  --workflow-package examples/batch-orchestrator/batch-orchestrator.aion \
  --workflow-package examples/batch-orchestrator/batch-orchestrator-item.aion
```

(When using `--config dev-config.toml` instead, list both archives in `workflow_packages`.)

Leave the server running.

## 3. Start the Python activity worker

The child workflow's `process-batch-item` activity executes on a connected worker. In terminal 2, create a virtual environment and install the local worker SDK:

```sh
python -m venv .venv && source .venv/bin/activate
python -m pip install -e sdks/python/aion-worker
```

Then run the worker:

```sh
python examples/batch-orchestrator/worker/worker.py
```

It registers exactly one activity, `process-batch-item`, and serves tasks until interrupted. The standard worker environment variables apply (`AION_WORKER_ENDPOINT`, `AION_TASK_QUEUE`, `AION_WORKER_IDENTITY`, `AION_WORKER_CONCURRENCY`, `AION_WORKER_NAMESPACE`, `AION_WORKER_SUBJECT`), defaulting to the local dev server.

Leave the worker running.

## 4. Start a batch

In terminal 3, start a workflow with several independent items. One payload intentionally contains `fail` so you can see a child failure recorded as an item outcome while the parent still completes.

```sh
START_RESPONSE=$(aion \
  --subject batch-user \
  start batch_orchestrator \
  --input '{"items":[{"id":"item-1","payload":"alpha"},{"id":"item-2","payload":"beta"},{"id":"item-3","payload":"please-fail"},{"id":"item-4","payload":"delta"}]}')
printf '%s\n' "$START_RESPONSE"

WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | jq -r .workflow_id)
RUN_ID=$(printf '%s' "$START_RESPONSE" | jq -r .run_id)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

If you do not have `jq`, copy the `workflow_id` and `run_id` strings from the JSON output into shell variables manually.

## 5. Query live progress

While the parent is still awaiting children, query the `batch_progress` handler (a four-item batch finishes in well under a second, so use a larger batch when you want to observe intermediate progress):

```sh
aion \
  --subject batch-user \
  query "$WORKFLOW_ID" batch_progress --pretty
```

The query response is structured as:

```json
{
  "total": 4,
  "completed": 2,
  "failed": 1,
  "pending": 1
}
```

Use `--run-id "$RUN_ID"` when you need to target a specific run rather than the latest run for a workflow id. Querying an already-completed workflow returns an error, because queries are answered by the live workflow process at yield points.

Queries are read-only. A query handler should return already-known workflow state; it must not schedule activities, await children, send signals, or mutate counters.

> **Known issue (beamr VM, refined 2026-06-12):** the query path itself is fully functional — live `batch_progress` queries are answered while the parent is parked in `child.await`, repeated queries re-enter the same await cleanly, and the query path appends no history. The previously documented `invalid operand for instruction pointer` crash on await re-entry after a serviced query no longer reproduces on the current engine (the raw-protocol engine e2e suites exercise live query + re-entry at the signal, sleep, activity, child-await, and collect yield points ungated). What remains broken is independent of queries: when the parent decodes a child terminal payload, the `gleam_json`/`gleam_stdlib` code in the rebuilt archive hits beamr 0.4.9 VM gaps (`VM execution error: bad argument` on the success-decode path; `undefined function erlang:integer_to_list/2` raising `{invalid_byte, 0}` from `gleam_json_ffi:decode/1` on the error-decode path) — a never-queried run crashes identically. Those VM defects are being fixed on the separate beamr track. End-to-end tests for this example live in `crates/aion/tests/example_query_reentry.rs` behind the `beamr_query_reentry_fixed` cargo feature (off by default); enable it once the upstream fixes land.

## 6. Inspect the final summary

After all children complete or fail, describe the workflow:

```sh
aion \
  --subject batch-user \
  describe "$WORKFLOW_ID" --pretty
```

The completed workflow result has this shape:

```json
{
  "total_processed": 4,
  "success_count": 3,
  "failure_count": 1,
  "items": [
    { "item_id": "item-1", "status": "succeeded", "detail": "processed item item-1" },
    { "item_id": "item-2", "status": "succeeded", "detail": "processed item item-2" },
    { "item_id": "item-3", "status": "failed", "detail": "deterministic failure for item item-3" },
    { "item_id": "item-4", "status": "succeeded", "detail": "processed item item-4" }
  ]
}
```

The important behavior is that `item-3` failing does not fail the parent and does not prevent siblings from being awaited. Spawn failures, if the engine cannot create a child execution at all, are also captured as failed item outcomes so the parent can report the whole batch.

## How the child failure round-trips

The child entry function (`batch_orchestrator_item.run`) encodes both its success and its failure payload to JSON text. The engine records those exact payloads as the child terminal, and the awaiting parent decodes them with the same codecs the child module exports (`item_result_codec`, `item_error_codec`). That is why the parent can report `deterministic failure for item item-3` verbatim: the typed error crossed the parent-child boundary as data, not as a crash.
