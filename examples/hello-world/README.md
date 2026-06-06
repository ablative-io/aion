# Aion hello world

This example takes you from a fresh checkout to a running Aion workflow. You will:

1. build a Gleam workflow that uses `aion_flow`,
2. package the compiled BEAM modules into `hello-world.aion`,
3. start the Aion dev server with the repo-root `dev-config.json`,
4. start a Python activity worker for the `greet` activity,
5. start a workflow instance with `curl`, and
6. inspect the run from the dashboard and HTTP API.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.11 or newer
- `curl`

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

The repo-root `dev-config.json` listens on gRPC `127.0.0.1:50051`, HTTP `127.0.0.1:8080`, uses bearer token `dev-token`, and preloads `examples/hello-world/hello-world.aion` at startup.

In terminal 1:

```sh
cargo run -p aion-server -- dev-config.json
```

Leave this process running.

Open the dashboard:

```sh
open http://127.0.0.1:8080/
```

If you are not on macOS, open `http://127.0.0.1:8080/` in your browser.

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

The worker connects to `127.0.0.1:50051`, registers `greet`, and returns:

```json
{"greeting":"Hello, <name>! Welcome to Aion."}
```

Leave this process running.

## 5. Start a workflow instance

In terminal 3:

```sh
START_RESPONSE=$(curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer dev-token' \
  -H 'x-aion-subject: hello-world-user' \
  -H 'x-aion-namespaces: default' \
  --data '{
    "namespace": "default",
    "workflow_type": "hello_world",
    "input": {
      "content_type": "application/json",
      "bytes": [123,34,110,97,109,101,34,58,34,65,100,97,34,125]
    }
  }')
printf '%s\n' "$START_RESPONSE"
```

The byte array is the UTF-8 JSON payload:

```json
{"name":"Ada"}
```

Capture the workflow id for the next commands:

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

## 6. Observe the result

List workflows:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/list \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer dev-token' \
  -H 'x-aion-subject: hello-world-user' \
  -H 'x-aion-namespaces: default' \
  --data '{"namespace":"default"}'
```

Describe the workflow and include history:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/describe \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer dev-token' \
  -H 'x-aion-subject: hello-world-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_id\": { \"uuid\": \"$WORKFLOW_ID\" },
    \"run_id\": { \"uuid\": \"$RUN_ID\" },
    \"include_history\": true
  }"
```

You should see workflow events for start, `greet` scheduling/completion, and workflow completion. The dashboard at `http://127.0.0.1:8080/` should show the same run.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-hello target/aion-dev.db examples/hello-world/hello-world.aion examples/hello-world/build
```
