# Aion hello world

This example takes you from a fresh checkout to a running Aion workflow. You will:

1. build a Gleam workflow that uses `aion_flow`,
2. package the compiled BEAM modules into `hello-world.aion`,
3. start the Aion dev server with the repo-root `dev-config.toml` and the built package preloaded,
4. start a Python activity worker for the `greet` activity,
5. start a workflow instance with the `aion` CLI, and
6. inspect and operate the run with the `aion` CLI while the dashboard UI is under development.

Install the CLI once from the checkout (the crate is `aion-cli`; the binary is `aion`):

```sh
cargo install --path crates/aion-cli --locked
```

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.10 or newer
- `jq`, optional but useful for extracting fields from the CLI's JSON output

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/hello-world
gleam build
cd ../..
```

The workflow source lives in `examples/hello-world/src/hello_world.gleam`. It imports the public `aion/workflow`, `aion/activity`, `aion/codec`, and `aion/error` modules from the `aion_flow` SDK. The module's public surface is the packaged entry function `run` plus the `HelloInput`, `GreetingOutput`, and `WorkflowError` types. `run` accepts JSON shaped like `{ "name": "Ada" }`, creates a typed `greet` activity with `activity.new()`, executes it with `workflow.run()`, unwraps the worker's typed greeting payload, and returns the greeting string.

## 2. Package `hello-world.aion`

```sh
aion package examples/hello-world
```

This reads the example's [`workflow.toml`](workflow.toml) and the BEAM files produced by `gleam build` (pass `--build` to compile and package in one step; see [`docs/packaging.md`](../../docs/packaging.md) for the full reference), and builds a manifest with:

- entry module: `hello_world`
- entry function: `run`
- input schema: object with required string field `name`
- output schema: string
- activities: `greet`

It writes:

```text
examples/hello-world/hello-world.aion
```

## 3. Start the Aion dev server

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP `127.0.0.1:8080`, uses the in-memory store, and defaults to the `default` namespace. Pass the package path on the command line so a fresh checkout preloads this example without editing TOML.

In terminal 1:

```sh
aion server --config dev-config.toml \
  --workflow-package examples/hello-world/hello-world.aion
```

Leave this process running. The dashboard/static UI at `http://127.0.0.1:8080/` is under development; use the `aion` CLI over the gRPC endpoint (`127.0.0.1:50051`) to inspect workflows for now. The CLI global flags map to client metadata and routing:

- `--endpoint` selects the gRPC server endpoint and defaults to `127.0.0.1:50051`.
- `--namespace` selects the workflow namespace and defaults to `default`.
- `--subject` identifies the caller and defaults to `cli-user`; this tutorial uses `hello-world-user`.
- `--pretty` formats the otherwise compact JSON output for humans.

## 4. Start the Python activity worker

In terminal 2, create a virtual environment and install the local worker SDK:

```sh
python3 -m venv .venv-aion-hello
. .venv-aion-hello/bin/activate
python -m pip install --upgrade pip
python -m pip install -e sdks/python/aion-worker
```

Then run the worker:

```sh
python examples/hello-world/worker.py
```

The worker example reads these environment variables:

| Variable | Description | Default |
|---|---|---|
| `AION_WORKER_ENDPOINT` | gRPC endpoint for the Aion server, formatted as `host:port`. | `127.0.0.1:50051` |
| `AION_TASK_QUEUE` | Non-empty task queue where the worker registers and polls for activities. | `default` |
| `AION_WORKER_IDENTITY` | Non-empty worker identity reported to the server. | `hello-world-python-worker` |
| `AION_WORKER_CONCURRENCY` | Positive integer maximum concurrent activity tasks handled by the worker. | `4` |

For example, to point the worker at another server:

```sh
AION_WORKER_ENDPOINT=127.0.0.1:50051 AION_WORKER_CONCURRENCY=4 python examples/hello-world/worker.py
```

The worker connects to `127.0.0.1:50051`, registers `greet`, and returns the JSON object decoded by `GreetingOutput` in `hello_world.gleam`:

```json
{"greeting":"Hello, <name>! Welcome to Aion."}
```

Leave this process running.

## 5. Start a workflow instance

The registered workflow type is `hello_world` — the package manifest's entry module name from step 2, which the server also logs when it loads the package. The archive file is named `hello-world.aion`, but the workflow type uses the underscore module name.

In terminal 3:

```sh
START_RESPONSE=$(aion --subject hello-world-user \
  start hello_world --input '{"name":"Ada"}')
printf '%s\n' "$START_RESPONSE"
```

The CLI prints JSON by default:

```json
{"workflow_id":"<workflow-id>","run_id":"<run-id>"}
```

Capture the workflow id for the next commands:

```sh
WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | jq -r .workflow_id)
RUN_ID=$(printf '%s' "$START_RESPONSE" | jq -r .run_id)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

If you do not have `jq`, copy the `workflow_id` and `run_id` strings from the JSON output into shell variables manually.

## 6. Observe the result

List workflows:

```sh
aion --subject hello-world-user list --pretty
```

Describe the workflow and include history:

```sh
aion --subject hello-world-user describe "$WORKFLOW_ID" --pretty
```

You should see workflow events for start, `greet` scheduling/completion, and workflow completion. For machine-readable output, omit `--pretty`; the compact JSON is parseable by `jq`.

## 7. Signal or cancel a workflow

The hello-world workflow normally completes quickly and does not define a meaningful signal handler, so the required hello-world path is complete after `describe`. For workflows that are still running and listen for signals, send JSON payloads with the same CLI:

```sh
aion --subject hello-world-user \
  signal "$WORKFLOW_ID" example_signal --payload '{"note":"hello from the CLI"}'
```

A successful signal request prints an acknowledgement JSON object. Workflows that need cooperative shutdown can also be cancelled while they are still running:

```sh
aion --subject hello-world-user \
  cancel "$WORKFLOW_ID" --reason 'tutorial cleanup'
```

## HTTP API reference

The tutorial above uses the CLI as the primary interaction method. If you need to compare against the HTTP API directly, the dev server also listens on `http://127.0.0.1:8080/`; HTTP callers must provide JSON request bodies and the equivalent metadata headers (`x-aion-subject` and `x-aion-namespaces`). See [`docs/API.md`](../../docs/API.md) for the transport overview.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-hello examples/hello-world/hello-world.aion examples/hello-world/build
```
