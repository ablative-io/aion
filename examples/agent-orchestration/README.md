# Aion agent orchestration

This example demonstrates Aion as a durable orchestrator for AI-agent workflows: the same dev-review loop pattern Meridian currently solves with script orchestration (YAML/Rhai), but expressed as a typed Gleam workflow and executed with Aion event history.

The workflow accepts a task brief, dispatches a simulated `develop` activity, dispatches a simulated `review` activity, loops back through `develop` when review asks for revisions, and returns the final `DevOutput` when review lands the change. The demo worker intentionally returns `revise` on the first review and `land` on the second review so the iteration is easy to observe.

## Why this matters for AI-agent pipelines

Script-based agent orchestration is easy to start with, but it gets fragile when a step is expensive or long-running. A real dev-agent activity may run for 30 minutes. If the process crashes after that dev step, a script runner usually has to reconstruct state manually or risks re-running the expensive agent.

Aion replaces that with durable typed workflows:

- **Type safety:** the orchestration logic lives in `src/orchestrator.gleam` with typed `TaskInput`, `DevOutput`, `ReviewOutput`, and `WorkflowError` values.
- **Durability:** activity completions are recorded in Aion history. On replay, a completed `develop` result is read from history and the workflow resumes at `review` instead of invoking `develop` again.
- **Observability:** the dashboard and `describe` API show the ordered event history: develop attempt 1, review revise, develop attempt 2, review land, workflow complete.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- Python 3.11 or newer
- `curl`

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/agent-orchestration
gleam build
cd ../..
```

The workflow source lives in `examples/agent-orchestration/src/orchestrator.gleam`. It exposes `run`, accepts JSON shaped like:

```json
{
  "title": "Add audit log export",
  "description": "Implement a CSV export for audit events.",
  "requirements": [
    "Add a typed export endpoint",
    "Include tests for empty and populated logs"
  ]
}
```

## 2. Package `orchestrator.aion`

```sh
cargo run --manifest-path examples/agent-orchestration/packager/Cargo.toml
```

This reads the BEAM files produced by `gleam build`, builds a manifest with:

- entry module: `orchestrator`
- entry function: `run`
- input schema: task brief object with `title`, `description`, and `requirements`
- output schema: object with `code_diff` and `commit_message`
- activities: `develop`, `review`

It writes:

```text
examples/agent-orchestration/orchestrator.aion
```

The repo-root `dev-config.toml` is the local development config. If you want the server to preload this package at startup, add `examples/agent-orchestration/orchestrator.aion` to its `workflow_packages` array after building the package.

## 3. Start the Aion dev server

In terminal 1:

```sh
cargo run -p aion-server -- --config dev-config.toml
```

Leave this process running. Open the dashboard:

```sh
open http://127.0.0.1:8080/
```

If you are not on macOS, open `http://127.0.0.1:8080/` in your browser.

## 4. Start the Python activity worker

In terminal 2, create a virtual environment and install the local worker SDK:

```sh
python3 -m venv .venv-aion-agent
. .venv-aion-agent/bin/activate
python -m pip install --upgrade pip
python -m pip install -e sdks/python/aion-worker
```

Then run the worker:

```sh
python examples/agent-orchestration/worker.py
```

The worker connects to `127.0.0.1:50051`, registers both `develop` and `review`, and logs each attempt. You should see logs for:

1. `develop attempt 1`
2. `review attempt 1 verdict=revise`
3. `develop attempt 2 applying reviewer findings`
4. `review attempt 2 verdict=land`

## 5. Start a workflow instance

In terminal 3:

```sh
TASK_JSON='{"title":"Add audit log export","description":"Implement a CSV export for audit events.","requirements":["Add a typed export endpoint","Include tests for empty and populated logs"]}'
TASK_BYTES=$(python3 - <<'PY' <<<"$TASK_JSON"
import sys
print(",".join(str(byte) for byte in sys.stdin.read().encode("utf-8")))
PY
)

START_RESPONSE=$(curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: agent-orchestration-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_type\": \"orchestrator\",
    \"input\": {
      \"content_type\": \"application/json\",
      \"bytes\": [$TASK_BYTES]
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

## 6. Observe the durable dev-review loop

Describe the workflow and include history:

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/describe \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: agent-orchestration-user' \
  -H 'x-aion-namespaces: default' \
  --data "{
    \"namespace\": \"default\",
    \"workflow_id\": { \"uuid\": \"$WORKFLOW_ID\" },
    \"run_id\": { \"uuid\": \"$RUN_ID\" },
    \"include_history\": true
  }"
```

In the event history and dashboard, observe the sequence:

1. workflow started with the task brief,
2. `develop` scheduled and completed for attempt 1,
3. `review` scheduled and completed with `verdict: revise`,
4. `develop` scheduled and completed for attempt 2 with the finding appended,
5. `review` scheduled and completed with `verdict: land`,
6. workflow completed with the final `DevOutput`.

This is the durability boundary that matters for AI agents. If the dev process takes a long time and the server crashes after `develop` completes, Aion replays the recorded `develop` completion and continues at `review` without re-running the dev agent.

## Clean up

Stop the worker and server with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf .venv-aion-agent examples/agent-orchestration/orchestrator.aion examples/agent-orchestration/build
```
