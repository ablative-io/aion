# Getting started with Aion

This guide takes a fresh checkout to a running workflow using the hello-world example. It keeps the path short; the full walkthrough lives in [`examples/hello-world/README.md`](examples/hello-world/README.md).

## Prerequisites

Install these before starting:

- Rust toolchain and Cargo (`rustup` is recommended)
- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP on your `PATH`
- Python 3.10 or newer for the Python activity worker used below
- `jq`, optional but useful for extracting fields from the CLI's JSON output

## 1. Clone and enter the repository

```sh
git clone https://github.com/ablative-io/aion.git
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

The repo-root `dev-config.toml` listens on HTTP `127.0.0.1:8080`, gRPC `127.0.0.1:50051`, uses the in-memory store, and defaults to the `default` namespace. Pass the package path on the command line so a fresh checkout preloads the hello-world workflow without editing TOML.

```sh
cargo run -p aion-server -- --config dev-config.toml \
  --workflow-package examples/hello-world/hello-world.aion
```

Leave this process running in terminal 1. The dashboard/static UI at `http://127.0.0.1:8080/` is under development; use `aion-cli` over the gRPC endpoint (`127.0.0.1:50051`) to operate workflows for now.

### Server environment variables and JSON logs

The server starts from built-in defaults, then applies config-file values, then `AION_` environment variable overrides, then CLI flags where a matching flag exists (`--listen-address`, `--store-url`, `--scheduler-threads`, and `--drain-timeout`). Supported server environment variables are:

| Variable | Description | Default |
|---|---|---|
| `AION_SERVER_LISTEN_ADDRESS` | HTTP listen socket address for the server API/static assets, formatted as `host:port` with a non-zero port. | `127.0.0.1:8080` |
| `AION_SERVER_GRPC_ADDRESS` | gRPC listen socket address for the worker/client protocol, formatted as `host:port` with a non-zero port. | `127.0.0.1:50051` |
| `AION_STORE_BACKEND` | Store backend selection; accepted values are `memory` or `libsql` (case-insensitive). | `memory` |
| `AION_STORE_URL` | Non-empty backend connection URL/path when the selected store needs one; setting it also selects `libsql` if the backend is still `memory`. | unset |
| `AION_RUNTIME_SCHEDULER_THREADS` | Positive integer number of scheduler runtime threads. | `1` |
| `AION_DRAIN_TIMEOUT_SECONDS` | Positive integer graceful shutdown drain timeout in seconds. | `30` |
| `AION_AUTH_ENABLED` | Enables or disables server auth; accepted booleans are `true`/`false`, `1`/`0`, `yes`/`no`, or `on`/`off` (case-insensitive). | `false` |
| `AION_AUTH_JWKS_URL` | Non-empty JWKS endpoint used when auth is enabled with JWKS validation. | unset |
| `AION_AUTH_JWKS_REFRESH_SECONDS` | Positive integer JWKS refresh interval in seconds. | `300` |
| `AION_METRICS_ENABLED` | Enables or disables metrics endpoints/export; uses the same boolean forms as `AION_AUTH_ENABLED`. | `true` |
| `AION_NAMESPACES_DEFAULT` | Non-empty default namespace used when one is not otherwise configured. | `default` |
| `AION_LOG` | Tracing filter for server logs; takes precedence over `RUST_LOG`. Example: `AION_LOG=debug`. | `info` |

Server logs are emitted as JSON on stdout. For interactive development, pipe them through `jq` for readability, for example:

```sh
AION_LOG=debug cargo run -p aion-server -- --config dev-config.toml \
  --workflow-package examples/hello-world/hello-world.aion | jq .
```

### Config auto-discovery

When `--config` is omitted, `aion-server` looks for `aion.toml` in the process working directory. If that file exists, the server loads and validates it; if it is absent, the server uses local development defaults for everything except `websocket.event_broadcast_capacity`, which has no default — supply it via `AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY` when running without a config file. To use auto-discovery from the repository root, copy the dev config and start the server. Include the package flag unless you also edit `aion.toml` to add the package path:

```sh
cp dev-config.toml aion.toml
cargo run -p aion-server -- \
  --workflow-package examples/hello-world/hello-world.aion
```

### Durable storage with libSQL

The quickstart uses the in-memory store, which loses all workflow state when the server stops. To make runs durable, switch the `[store]` section to the libSQL backend:

```toml
[store]
backend = "libsql"
url = "aion-dev.db"
```

`store.backend` accepts `memory` or `libsql`, and `store.url` is the embedded libSQL database file path, created on first start. `store.url` must be non-empty when `backend = "libsql"`. You can apply the same change without editing TOML: set `AION_STORE_BACKEND=libsql` and `AION_STORE_URL=aion-dev.db`, or pass `--store-url`, which also switches the backend from `memory` to `libsql` automatically:

```sh
cargo run -p aion-server -- --config dev-config.toml \
  --store-url aion-dev.db \
  --workflow-package examples/hello-world/hello-world.aion
```

With the libSQL store, you can stop the server after a workflow completes, start it again, and `list`/`describe` still return the recorded history — the engine replays durable histories on startup.

### Running on alternate ports

`[server].listen_address` (HTTP) and `[server].grpc_address` (gRPC) take `host:port` socket addresses with explicit non-zero ports. Override them in the config file or with the `AION_SERVER_LISTEN_ADDRESS` and `AION_SERVER_GRPC_ADDRESS` environment variables; only the HTTP address has a CLI flag (`--listen-address`), so use the environment variable or config file to move the gRPC listener. When the gRPC port changes, point the worker and CLI at the new endpoint:

```sh
AION_WORKER_ENDPOINT=127.0.0.1:60051 python examples/hello-world/worker.py
cargo run -q -p aion-cli -- --endpoint 127.0.0.1:60051 --subject hello-world-user list
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

The registered workflow type is `hello_world` — the package manifest's entry module name, which the server also logs when it loads the package at startup. (The archive file is named `hello-world.aion`, but the workflow type uses the underscore module name.)

In terminal 3, from the repository root:

```sh
START_RESPONSE=$(cargo run -q -p aion-cli -- \
  --subject hello-world-user \
  start hello_world --input '{"name":"Ada"}')
printf '%s\n' "$START_RESPONSE"
```

The CLI prints compact JSON by default, for example:

```json
{"workflow_id":"<workflow-id>","run_id":"<run-id>"}
```

Capture the returned ids with `jq` if it is installed:

```sh
WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | jq -r .workflow_id)
RUN_ID=$(printf '%s' "$START_RESPONSE" | jq -r .run_id)
printf 'workflow_id=%s\nrun_id=%s\n' "$WORKFLOW_ID" "$RUN_ID"
```

If you do not have `jq`, copy the `workflow_id` and `run_id` strings from the JSON output into shell variables manually.

## 6. Observe the run

List workflows:

```sh
cargo run -q -p aion-cli -- --subject hello-world-user list --pretty
```

Describe the run and include history:

```sh
cargo run -q -p aion-cli -- --subject hello-world-user describe "$WORKFLOW_ID" --pretty
```

You should see events for workflow start, `greet` scheduling/completion, and workflow completion.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-hello examples/hello-world/hello-world.aion examples/hello-world/build
```

If you copied `dev-config.toml` to `aion.toml` for config auto-discovery, or created a libSQL database file for the durable-store option, remove those too:

```sh
rm -f aion.toml aion-dev.db
```

## Where to go next

- [`examples/`](examples/) — working examples, including hello-world.
- [`examples/hello-world/README.md`](examples/hello-world/README.md) — detailed end-to-end walkthrough.
- [`docs/API.md`](docs/API.md) — API and transport overview.
- [`gleam/aion_flow/README.md`](gleam/aion_flow/README.md) — Gleam workflow authoring SDK guide.
- [`gleam/aion_client/README.md`](gleam/aion_client/README.md) — Gleam caller SDK guide.
- [`docs/IMPLEMENTATION-TRACKER.md`](docs/IMPLEMENTATION-TRACKER.md) — implementation status details.
