# Aion batch orchestrator

This example demonstrates parent-child orchestration with the typed `aion_flow` SDK. The parent workflow accepts a list of work items, starts one child workflow for each item, registers a `batch_progress` query for live inspection, awaits every child, and returns a final summary with per-item outcomes.

Use this pattern when each item needs its own workflow execution boundary: independent failure handling, independent retry/cancellation history, and a clear per-item audit trail. Use fan-out helpers only for related activity fan-out inside one workflow execution. They are not a substitute for child workflows when you want each item to have an isolated failure domain.

## What is in the example

- `definition()` defines the parent `batch-orchestrator` workflow with `workflow.define()`.
- `child_definition()` defines the single-item `batch-orchestrator-item` child workflow.
- The parent calls `child.spawn()` once per work item and stores each child handle with the original item id.
- The child runs one typed `process-batch-item` activity. This is a deterministic stub: ids or payloads containing `fail` return a terminal activity failure.
- The parent calls `child.await()` for each handle. Successful children become `succeeded` item outcomes, and child failures become `failed` item outcomes instead of crashing the parent.
- The parent registers a read-only `batch_progress` query. The current public SDK function is `query.handler(...)`; it wraps the engine's query registration and reply path internally.

> Note: the public SDK currently exposes `query.handler`, not separate `query.register` and `query.reply` functions. This example stays on the public SDK surface and does not import internal FFI modules.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- `jq`, optional but useful for extracting workflow ids from CLI JSON

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/batch-orchestrator
gleam build
cd ../..
```

The source lives in `examples/batch-orchestrator/src/batch_orchestrator.gleam` and consumes only the public `aion_flow` modules: `aion/workflow`, `aion/child`, `aion/query`, `aion/activity`, `aion/codec`, and `aion/error`.

## 2. Package and load the workflow

This example currently provides the standalone Gleam project and source requested by DX-025. It does not include a Rust packager subcrate. To run it against `aion-server`, package the compiled module using the same manifest shape as the sibling examples and load the resulting `.aion` archive in `dev-config.toml`. The generated `manifest.toml` is committed so dependency resolution is reproducible, matching the other Gleam examples.

A package for this example should expose:

- parent workflow type: `batch_orchestrator`
- entry module: `batch_orchestrator`
- entry function: `run`
- input shape: `{ "items": [{ "id": String, "payload": String }] }`
- output shape: summary with `total_processed`, `success_count`, `failure_count`, and `items`
- activity name: `process-batch-item`
- query name: `batch_progress`

Then start the dev server in terminal 1:

```sh
cargo run -p aion-server -- --config dev-config.toml
```

Leave the server running.

## 3. Start a batch

In terminal 2, start a workflow with several independent items. One payload intentionally contains `fail` so you can see a child failure recorded as an item outcome while the parent still completes.

```sh
START_RESPONSE=$(cargo run -q -p aion-cli -- \
  --subject batch-user \
  start batch_orchestrator \
  --input '{"items":[{"id":"item-1","payload":"alpha"},{"id":"item-2","payload":"beta"},{"id":"item-3","payload":"please-fail"},{"id":"item-4","payload":"delta"}]}')
printf '%s\n' "$START_RESPONSE"

WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | jq -r .workflow_id)
RUN_ID=$(printf '%s' "$START_RESPONSE" | jq -r .run_id)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

If you do not have `jq`, copy the `workflow_id` and `run_id` strings from the JSON output into shell variables manually.

## 4. Query live progress

While the parent is still awaiting children, query the `batch_progress` handler:

```sh
cargo run -q -p aion-cli -- \
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

Use `--run-id "$RUN_ID"` when you need to target a specific run rather than the latest run for a workflow id.

Queries are read-only. A query handler should return already-known workflow state; it must not schedule activities, await children, send signals, or mutate counters.

## 5. Inspect the final summary

After all children complete or fail, describe the workflow:

```sh
cargo run -q -p aion-cli -- \
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
