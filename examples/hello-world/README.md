# Aion hello world

This example takes you from a fresh checkout to a running Aion workflow. You will:

1. build a Gleam workflow that uses `aion_flow`,
2. package the compiled BEAM modules into `hello-world.aion`,
3. start the Aion dev server with the repo-root `dev-config.toml`,
4. start a Python activity worker for the `greet` activity,
5. start a workflow instance with `aion-cli`, and
6. inspect and operate the run from `aion-cli` while the dashboard UI is under development.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.11 or newer
- `jq`, optional but useful for extracting fields from the CLI's JSON output

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/hello-world
gleam build
cd ../..
```

The workflow source lives in `examples/hello-world/src/hello_world.gleam`. It exposes `run`, accepts JSON shaped like `{ "name": "Ada" }`, dispatches one `greet` activity through `aion_flow`, and returns the greeting string.

## 2. Package `hello-world.aion`

```sh
cargo run --manifest-path examples/hello-world/packager/Cargo.toml
```

This reads the BEAM files produced by `gleam build`, builds a manifest with:

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

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP `127.0.0.1:8080`, uses the in-memory store, and defaults to the `default` namespace. To preload this example at startup, make sure `workflow_packages` includes `examples/hello-world/hello-world.aion` after building the package.

In terminal 1:

```sh
cargo run -p aion-server -- --config dev-config.toml
```

Leave this process running. The dashboard/static UI at `http://127.0.0.1:8080/` is under development; use `aion-cli` over the gRPC endpoint (`127.0.0.1:50051`) to inspect workflows for now. The CLI global flags map to client metadata and routing:

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

The worker connects to `127.0.0.1:50051`, registers `greet`, and returns:

```json
{"greeting":"Hello, <name>! Welcome to Aion."}
```

Leave this process running.

## 5. Start a workflow instance

In terminal 3:

```sh
START_RESPONSE=$(cargo run -q -p aion-cli -- \
  --subject hello-world-user \
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
cargo run -q -p aion-cli -- --subject hello-world-user list --pretty
```

Describe the workflow and include history:

```sh
cargo run -q -p aion-cli -- --subject hello-world-user describe "$WORKFLOW_ID" --pretty
```

You should see workflow events for start, `greet` scheduling/completion, and workflow completion. For machine-readable output, omit `--pretty`; the compact JSON is parseable by `jq`.

## 7. Signal or cancel a workflow

The hello-world workflow completes quickly and does not define a meaningful signal handler, but the CLI can send any JSON signal payload to workflows that do listen for signals. To exercise the CLI command shape against the captured hello-world id, run:

```sh
cargo run -q -p aion-cli -- --subject hello-world-user \
  signal "$WORKFLOW_ID" example_signal --payload '{"note":"hello from the CLI"}'
```

A successful signal request prints an acknowledgement JSON object. Workflows that need cooperative shutdown can also be cancelled:

```sh
cargo run -q -p aion-cli -- --subject hello-world-user \
  cancel "$WORKFLOW_ID" --reason 'tutorial cleanup'
```

## HTTP API reference

The tutorial above uses the CLI as the primary interaction method. If you need to compare against the HTTP API directly, the dev server also listens on `http://127.0.0.1:8080/`; HTTP callers must provide JSON request bodies and the equivalent metadata headers (`x-aion-subject` and `x-aion-namespaces`). See [`docs/API.md`](../../docs/API.md) for the transport overview.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-hello examples/hello-world/hello-world.aion examples/hello-world/build
```
