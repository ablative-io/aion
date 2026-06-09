# Aion data pipeline

This example demonstrates a fan-out/fan-in workflow written with the typed
Gleam `aion_flow` SDK and served by a Python activity worker. The workflow takes
a list of URLs, fans out simulated fetch activities, processes each fetched item
in parallel, and then fans in to a single aggregate activity.

The activity worker uses stubs only: `fetch_url` returns simulated content and
never makes real HTTP requests.

## Pattern overview

Fan-out/fan-in is useful when one workflow input naturally becomes many
independent pieces of work. The workflow records every durable activity in
history, so replay is deterministic: each activity result is either scheduled
once or loaded from history with the same correlation and ordering.

```text
{ "urls": [...] }
        |
        v
workflow.all([fetch_url(url1), fetch_url(url2), ...])
        |  fan out: N fetch_url activity events, ordered results
        v
workflow.map(fetched_items, process_item_activity)
        |  parallel transform: N process_item activity events
        v
workflow.run(aggregate_results(processed_items))
        |  fan in: exactly one aggregate_results activity event
        v
{ "total_urls": N, "total_words": ..., "summaries": [...] }
```

Use the primitives this way:

- `workflow.all` (the public SDK wrapper around `concurrency.all`) when you
  already have a deterministic list of activities and need all results before
  continuing.
- `workflow.map` (the public SDK wrapper around `concurrency.map`) when you have
  a list of values and want to build one homogeneous activity per value.
- `workflow.run` for a single durable activity, such as the final fan-in step
  that receives all processed items and returns the workflow output.

The workflow source lives in `examples/data-pipeline/src/data_pipeline.gleam`.
It exposes `definition()` for the named `data-pipeline` workflow and keeps `run`
as the entry function. Input JSON is shaped like:

```json
{
  "urls": [
    "https://example.com/alpha",
    "https://example.com/beta",
    "https://example.com/gamma"
  ]
}
```

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP on
  your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.10 or newer
- `jq`, optional but useful for extracting fields from the CLI's JSON output

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/data-pipeline
gleam build
cd ../..
```

The workflow imports only the public `aion_flow` SDK modules plus Gleam standard
JSON helpers. The durable phases are deliberately separated in code as
`fetch_all`, `process_all`, and `aggregate` so the event history shows distinct
fetch, process, and aggregation stages.

## 2. Start the Aion dev server

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP
`127.0.0.1:8080`, uses the in-memory store, and defaults to the `default`
namespace.

In terminal 1:

```sh
cargo run -p aion-server -- --config dev-config.toml
```

Leave this process running. Use `aion-cli` over the gRPC endpoint
(`127.0.0.1:50051`) to start and inspect workflows.

## 3. Start the Python activity worker

In terminal 2, create a virtual environment and install the local worker SDK:

```sh
python3 -m venv .venv-aion-data-pipeline
. .venv-aion-data-pipeline/bin/activate
python -m pip install --upgrade pip
python -m pip install -e sdks/python/aion-worker
```

Then run the worker:

```sh
python examples/data-pipeline/worker/worker.py
```

The worker reads these environment variables:

| Variable | Description | Default |
|---|---|---|
| `AION_WORKER_ENDPOINT` | gRPC endpoint for the Aion server, formatted as `host:port`. | `127.0.0.1:50051` |
| `AION_TASK_QUEUE` | Non-empty task queue where the worker registers and polls for activities. | `default` |
| `AION_WORKER_IDENTITY` | Non-empty worker identity reported to the server. | `data-pipeline-python-worker` |
| `AION_WORKER_CONCURRENCY` | Positive integer maximum concurrent activity tasks handled by the worker. | `8` |
| `AION_WORKER_NAMESPACE` | Namespace used for worker registration. | `default` |
| `AION_WORKER_SUBJECT` | Subject used for worker registration. | `worker` |

The worker connects to the server and registers exactly these activity names:

- `fetch_url`
- `process_item`
- `aggregate_results`

Leave this process running.

## 4. Start a workflow instance

In terminal 3:

```sh
START_RESPONSE=$(cargo run -q -p aion-cli -- \
  --subject data-pipeline-user \
  start data_pipeline --input '{"urls":["https://example.com/alpha","https://example.com/beta","https://example.com/gamma"]}')
printf '%s\n' "$START_RESPONSE"
```

Capture the identifiers for inspection:

```sh
WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | jq -r .workflow_id)
RUN_ID=$(printf '%s' "$START_RESPONSE" | jq -r .run_id)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

If you do not have `jq`, copy the `workflow_id` and `run_id` strings from the
JSON output into shell variables manually.

## 5. Observe the fan-out/fan-in history

Describe the workflow and include history:

```sh
cargo run -q -p aion-cli -- --subject data-pipeline-user \
  describe "$WORKFLOW_ID" --pretty
```

For the three-URL input above, expect the event history to show:

1. three `fetch_url` activity events from the `workflow.all` fan-out,
2. three `process_item` activity events from the `workflow.map` transform, and
3. exactly one `aggregate_results` activity event from the final fan-in.

The completed workflow output has this shape:

```json
{
  "total_urls": 3,
  "total_words": 33,
  "summaries": [
    "https://example.com/alpha: 11 words processed",
    "https://example.com/beta: 11 words processed",
    "https://example.com/gamma: 11 words processed"
  ]
}
```

The exact word counts reflect the worker stub's simulated content string.
