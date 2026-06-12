# Aion worker conformance harnesses

This directory contains the shared scenario source for the Rust, Python, and TypeScript worker SDK conformance harnesses. `scenarios.json` is the source of truth for scenario ids, fixture activity names, harness control signals, and expected normalized observations. Do not copy or hardcode those scenarios into a language-specific worker test.

The suite is fake-endpoint-only. It never connects to an Aion server, never exercises a live engine, and speaks only the AW-owned `WorkerProtocol.StreamWorker` contract from `crates/aion-proto/proto/worker.proto`.

## Scenario catalogue

`scenarios.json` defines exactly eight worker protocol scenarios:

1. `register` — worker sends `RegisterWorker` with the conformance namespace and activity type set.
2. `receive-complete` — worker receives one `ActivityTask` and sends `ActivityResult.result`.
3. `fail-retryable` — worker sends `ActivityResult.error.kind = ACTIVITY_ERROR_KIND_RETRYABLE`.
4. `fail-terminal` — worker sends `ActivityResult.error.kind = ACTIVITY_ERROR_KIND_TERMINAL`.
5. `heartbeat` — worker sends `Heartbeat` for an in-flight task and then completes it.
6. `cancellation` — harness delivers a cooperative cancellation control signal through the runner seam and verifies the handler observes it without forced termination. The current worker proto has no cancellation frame, so this is intentionally not modeled as a `ServerToWorker` message.
7. `reconnect-and-re-report` — harness drops the stream after receiving a result but before acknowledging it; on reconnect, the worker must re-register and re-report the unacknowledged `ActivityResult` before any new task is dispatched.
8. `backpressure` — harness offers five tasks with `maxConcurrency = 2` and verifies observed peak concurrency is exactly two while all five tasks eventually complete.

Every scenario uses AW proto names (`RegisterWorker`, `ActivityTask`, `ActivityResult`, `ActivityErrorKind`, `Heartbeat`) and stores payload expectations as JSON values plus the `application/json` `Payload.content_type` tag. `workflow_id` and `activity_id` are represented with their AW wrapper shapes (`WorkflowId.uuid` and `ActivityId.sequence_position`) so the catalogue stays aligned with `worker.proto` while runner output can still normalize SDK-native id types before comparison.

## Fake endpoint

The fake worker endpoint is specified in [`fake_engine/README.md`](fake_engine/README.md). It implements the server side of `WorkerProtocol.StreamWorker`, loads a single scenario, spawns a conformance worker process, records observable worker messages, and compares them with `scenarios.json`.

A normalized recording has this shape:

```json
{
  "sdk": "rust",
  "scenario": "receive-complete",
  "registrations": [
    {"namespace": "conformance", "activity_types": ["conformance.echo"]}
  ],
  "reports": [
    {
      "workflow_id": {"uuid": "wf"},
      "activity_id": {"sequence_position": 1},
      "outcome": "result",
      "content_type": "application/json",
      "json": {}
    }
  ],
  "failures": [],
  "heartbeats": [],
  "re_reports": [],
  "peak_concurrency": 1
}
```

The recorder may keep richer timing metadata internally, but cross-SDK assertions compare only the normalized fields defined in `scenarios.json`.

## Running the suite

The harness owns process spawning and port allocation; workers receive the fake endpoint URL and scenario id in environment variables. No `AION_WORKER_TEST_URL` or live server is required.

Recommended command entry points are documented in [`fake_engine/runners.md`](fake_engine/runners.md). The runner contract is:

```text
AION_WORKER_CONFORMANCE sdk=<sdk> scenario=<scenario-id> result=<json>
```

`result` is the normalized recording emitted by the fake endpoint after assertion. A missing local toolchain or SDK surface emits one logged skip line and exits successfully for that SDK:

```text
SKIP sdk=python reason="aion-worker Python activity codec unavailable"
```

Skips are useful for local development environments, but a scenario is not declared conformant unless every available SDK that claims support produces the same normalized observable behaviour and satisfies `scenarios.json`.

## Cross-SDK equivalence rule

A scenario is conformant only when Rust, Python, and TypeScript produce identical normalized observations for the same scenario and each observation satisfies the scenario expectation. Differences in classification, missing heartbeats, dropped re-reports, mismatched content-type tags, task ordering that violates `mustOccurBefore`, or peak concurrency outside the expected value are divergences.

Report divergences with SDK, scenario, field path, expected observable, and actual observable, for example:

```text
DIVERGENCE sdk=typescript scenario=fail-terminal path=failures[0].wire_kind expected="ACTIVITY_ERROR_KIND_TERMINAL" actual="ACTIVITY_ERROR_KIND_RETRYABLE"
```

## Adding a new SDK

To add another worker SDK to the suite:

1. Implement a thin conformance worker that connects to the fake endpoint and registers the fixture activities listed in `scenarios.json` under `fixtures.registered_activity_types`.
2. Use the SDK's public API only. Do not call harness internals from the worker process and do not patch SDK behaviour in this conformance directory.
3. Normalize observations through the fake endpoint recorder, not through SDK-specific logs.
4. Add the SDK command, toolchain probe, skip message, and activity implementation notes to `fake_engine/runners.md`.
5. Run every scenario from `scenarios.json`; new SDKs may not filter scenarios or change expectations.
6. Add payload parity coverage as described in [`payload_parity.md`](payload_parity.md).

If the SDK lacks a required public surface, the runner must log a clean skip that names the unavailable surface. Do not add protocol fields or SDK shims here to make the conformance suite pass.
