# Getting started with Aion

This guide takes a fresh checkout to a running workflow using the hello-world example. It keeps the path short; the full walkthrough lives in [`examples/hello-world/README.md`](examples/hello-world/README.md).

## Prerequisites

Install these before starting:

- Rust toolchain and Cargo (`rustup` is recommended)
- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP on your `PATH`
- Python 3.11 or newer, optional unless you run the Python worker used below
- `curl`

## 1. Clone and enter the repository

```sh
git clone https://github.com/ablative/aion.git
cd aion
```

If you already have the repository, start from its root directory.

## 2. Build the Gleam workflow package

```sh
cd examples/hello-world
gleam build
cd ../..
cargo run --manifest-path examples/hello-world/packager/Cargo.toml
```

This writes `examples/hello-world/hello-world.aion`.

## 3. Start the Aion dev server

The server CLI accepts TOML config files via `--config`. Create a local config that listens on HTTP `127.0.0.1:8080`, gRPC `127.0.0.1:50051`, and preloads the hello-world package:

```sh
cat > aion-hello-world.toml <<'EOF'
workflow_packages = ["examples/hello-world/hello-world.aion"]

[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "memory"

[namespaces]
default = "default"
EOF
cargo run -p aion-server -- --config aion-hello-world.toml
```

Leave this process running in terminal 1. The dashboard/static UI at `http://127.0.0.1:8080/` is under development; use the HTTP API commands below (or Aion CLI commands where available) to observe workflows for now.

### Server environment variables and JSON logs

The server starts from built-in defaults, then applies config-file values, then `AION_` environment variable overrides, then CLI flags. Supported server environment variables are:

| Variable | Description | Default |
|---|---|---|
| `AION_SERVER_LISTEN_ADDRESS` | HTTP listen address for the server API/static assets. | `127.0.0.1:8080` |
| `AION_SERVER_GRPC_ADDRESS` | gRPC listen address for the worker/client protocol. | `127.0.0.1:50051` |
| `AION_STORE_BACKEND` | Store backend selection, such as `memory` for the in-memory dev store. | `memory` |
| `AION_STORE_URL` | Backend connection URL/path when the selected store needs one. | unset |
| `AION_RUNTIME_SCHEDULER_THREADS` | Number of scheduler runtime threads. | `1` |
| `AION_DRAIN_TIMEOUT_SECONDS` | Graceful shutdown drain timeout in seconds. | `30` |
| `AION_AUTH_ENABLED` | Enables or disables server auth. | `false` |
| `AION_AUTH_JWKS_URL` | JWKS endpoint used when auth is enabled with JWKS validation. | unset |
| `AION_AUTH_JWKS_REFRESH_SECONDS` | JWKS refresh interval in seconds. | `300` |
| `AION_METRICS_ENABLED` | Enables or disables metrics endpoints/export. | `true` |
| `AION_NAMESPACES_DEFAULT` | Default namespace used when one is not otherwise configured. | `default` |
| `AION_LOG` | Tracing filter for server logs; takes precedence over `RUST_LOG`. Example: `AION_LOG=debug`. | `info` |

Server logs are emitted as JSON on stdout. For interactive development, pipe them through `jq` for readability, for example:

```sh
AION_LOG=debug cargo run -p aion-server -- --config aion-hello-world.toml | jq .
```

## 4. Start the Python activity worker

In terminal 2, from the repository root:

```sh
python3 -m venv .venv-aion-hello
. .venv-aion-hello/bin/activate
python -m pip install --upgrade pip
python -m pip install -e sdks/python/aion-worker
python examples/hello-world/worker.py
```

The worker connects to `127.0.0.1:50051`, registers the `greet` activity on the default task queue, and serves until you stop it with `Ctrl-C`.

## 5. Start a workflow

In terminal 3, from the repository root:

```sh
START_RESPONSE=$(curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
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

The byte array is the UTF-8 JSON payload `{"name":"Ada"}`.

Capture the returned ids:

```sh
WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | python3 -c 'import json, sys; print(json.load(sys.stdin)["workflow_id"]["uuid"])')
RUN_ID=$(printf '%s' "$START_RESPONSE" | python3 -c 'import json, sys; print(json.load(sys.stdin)["run_id"]["uuid"])')
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

## 6. Observe the run

List workflows:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/list \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: hello-world-user' \
  -H 'x-aion-namespaces: default' \
  --data '{"namespace":"default"}'
```

Describe the run and include history:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/describe \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: hello-world-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_id\": { \"uuid\": \"$WORKFLOW_ID\" },
    \"run_id\": { \"uuid\": \"$RUN_ID\" },
    \"include_history\": true
  }"
```

You should see events for workflow start, `greet` scheduling/completion, and workflow completion.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-hello aion-hello-world.toml examples/hello-world/hello-world.aion examples/hello-world/build
```

## Where to go next

- [`examples/`](examples/) — working examples, including hello-world.
- [`examples/hello-world/README.md`](examples/hello-world/README.md) — detailed end-to-end walkthrough.
- [`docs/API.md`](docs/API.md) — API and transport overview.
- [`gleam/aion_flow/README.md`](gleam/aion_flow/README.md) — Gleam workflow authoring SDK guide.
- [`gleam/aion_client/README.md`](gleam/aion_client/README.md) — Gleam caller SDK guide.
- [`docs/IMPLEMENTATION-TRACKER.md`](docs/IMPLEMENTATION-TRACKER.md) — implementation status details.
