# Aion approval gate

This example demonstrates a human-in-the-loop approval gate written with the Gleam `aion_flow` SDK. The workflow:

1. accepts a document id and configurable approval deadline,
2. starts a named durable timer for that deadline,
3. waits for an `approval_decision` signal payload shaped like `{ "decision": "approved" }` or `{ "decision": "rejected" }`,
4. publishes the document when the signal approves it, and
5. archives the document when the signal rejects it or the durable timeout expires first.

Durable timers matter because approval deadlines can outlive a single process. If the engine restarts while the workflow is waiting, Aion replays the completed history and resumes at the pending signal/timer wait instead of re-running completed workflow steps. When the timeout has already fired, replay observes the timer-fired event and continues immediately down the archive path.

> SDK note: the current public SDK exposes `workflow.receive(...)`, `workflow.start_timer(...)`, and `workflow.with_timeout(...)`. `workflow.race(...)` currently races activities only, so this example uses `with_timeout` as the compiling signal-vs-deadline primitive while still recording a named durable timer for observability.

## Prerequisites

Install these tools before starting:

- [Gleam CLI](https://gleam.run/getting-started/installing/) with Erlang/OTP available on your `PATH`
- Rust toolchain and Cargo (`rustup` is recommended)
- `jq`, optional but useful for extracting fields from the CLI's JSON output

All commands below are copy-pasteable from the repository root unless noted.

## 1. Build the Gleam workflow

```sh
cd examples/approval-gate
gleam build
cd ../..
```

The workflow source lives in `examples/approval-gate/src/approval_gate.gleam`. It exposes `definition()` for the named `approval-gate` workflow and keeps `run` as the entry function. `run` accepts JSON shaped like:

```json
{"document_id":"doc-123","timeout_minutes":5}
```

The workflow result records the business outcome and action taken:

```json
{
  "decision": "approved",
  "action_taken": "published doc-123",
  "reason": "approval signal approved the document"
}
```

Rejected and timed-out runs complete successfully too; they return `"decision": "rejected"` or `"decision": "timed_out"` and an archive action. Workflow errors are reserved for infrastructure failures such as malformed signal payloads, activity dispatch failure, or timer engine failure.

## 2. Package or load the workflow

This brief creates the standalone Gleam workflow source and manifest. To run it against the Aion server, package the compiled BEAM modules the same way the other examples do: create an `.aion` package whose manifest uses:

- entry module: `approval_gate`
- entry function: `run`
- workflow name: `approval-gate`
- input schema: object with required `document_id` string and `timeout_minutes` integer fields
- output schema: object with `decision`, `action_taken`, and `reason` string fields
- activities: `publish_document` and `archive_document`
- signals: `approval_decision` with payload `{ "decision": "approved" }` or `{ "decision": "rejected" }`

If you add the package to `dev-config.toml`, make sure `workflow_packages` includes the produced `examples/approval-gate/approval-gate.aion` path before starting the server.

## 3. Start the Aion dev server

In terminal 1:

```sh
cargo run -p aion-server -- --config dev-config.toml
```

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP `127.0.0.1:8080`, uses the in-memory store, and defaults to the `default` namespace. The CLI global flags map to client metadata and routing:

- `--endpoint` selects the gRPC server endpoint and defaults to `127.0.0.1:50051`.
- `--namespace` selects the workflow namespace and defaults to `default`.
- `--subject` identifies the caller and defaults to `cli-user`; this guide uses `approval-user`.
- `--pretty` formats the otherwise compact JSON output for humans.

## 4. Provide activity workers

The Gleam file includes deterministic local stub runners so the module can be built and read standalone. A server-backed run still needs workers registered for the activity names in the package:

- `publish_document`
- `archive_document`

A worker can return JSON matching the activity output codec:

```json
{"action_taken":"published doc-123"}
```

or:

```json
{"action_taken":"archived doc-123 because approval timed out before a signal arrived"}
```

No external approval service is required; a person or script can send the approval signal through `aion-cli`.

## 5. Start a workflow instance

In terminal 2:

```sh
START_RESPONSE=$(cargo run -q -p aion-cli -- \
  --subject approval-user \
  start approval_gate --input '{"document_id":"doc-123","timeout_minutes":5}')
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

## 6. Send an approval signal

Approve the document before the timeout expires:

```sh
cargo run -q -p aion-cli -- --subject approval-user \
  signal "$WORKFLOW_ID" approval_decision --payload '{"decision":"approved"}'
```

A successful signal request prints an acknowledgement JSON object. The workflow then executes `publish_document` and completes with `decision = "approved"` and `action_taken` beginning with `published`.

To observe the rejection branch, start another workflow and send:

```sh
cargo run -q -p aion-cli -- --subject approval-user \
  signal "$WORKFLOW_ID" approval_decision --payload '{"decision":"rejected"}'
```

That run executes `archive_document` and completes with `decision = "rejected"`.

### Client SDK alternative

A client can send the same signal payload through the Aion client handle instead of the CLI. Use the typed helper when you have a client-side payload type, or the raw payload escape hatch for JSON already in hand:

```rust
handle
    .signal("approval_decision", serde_json::json!({ "decision": "approved" }))
    .await?;
```

## 7. Observe the result and history

List workflows:

```sh
cargo run -q -p aion-cli -- --subject approval-user list --pretty
```

Describe a workflow and include its event history:

```sh
cargo run -q -p aion-cli -- --subject approval-user describe "$WORKFLOW_ID" --pretty
```

For an approved run, expect history entries for workflow start, timer start, signal receipt, `publish_document` scheduling/completion, and workflow completion. For rejected or timed-out runs, expect `archive_document` instead.

## 8. Observe the timeout path

Start a short-deadline workflow and do not send a signal:

```sh
START_RESPONSE=$(cargo run -q -p aion-cli -- \
  --subject approval-user \
  start approval_gate --input '{"document_id":"doc-timeout","timeout_minutes":1}')
WORKFLOW_ID=$(printf '%s' "$START_RESPONSE" | jq -r .workflow_id)
printf 'workflow_id=%s\n' "$WORKFLOW_ID"
```

Wait for the durable deadline to expire, then describe the run:

```sh
cargo run -q -p aion-cli -- --subject approval-user describe "$WORKFLOW_ID" --pretty
```

The result should include:

```json
{
  "decision": "timed_out",
  "action_taken": "archived doc-timeout because approval timed out before a signal arrived",
  "reason": "approval timed out before a signal arrived"
}
```

## Clean up

Stop the server and any workers with `Ctrl-C`, then remove local artifacts if desired:

```sh
rm -rf examples/approval-gate/build examples/approval-gate/approval-gate.aion
```
