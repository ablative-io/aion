# Fake worker endpoint

`fake_engine/` is the language-neutral conformance harness for Aion worker SDKs. It plays the server side of the AW-owned `WorkerProtocol.StreamWorker` bidirectional gRPC stream and records the observable behaviour of a worker process. The harness is a fake worker endpoint only; it does not start or connect to a live `aion-server` or engine.

## Protocol surface implemented

The fake endpoint implements only the worker protocol needed by `../scenarios.json`:

- Accept a `StreamWorker` connection.
- Require the first worker message to be `WorkerToServer.register` containing `RegisterWorker.namespace` and `RegisterWorker.activity_types`.
- Send `ServerToWorker.task` messages containing `ActivityTask.workflow_id`, `activity_id`, `activity_type`, and `input`.
- Record `WorkerToServer.result` messages containing either `ActivityResult.result` or `ActivityResult.error`.
- Record `WorkerToServer.heartbeat` messages and preserve heartbeat progress payload content type and bytes.
- Send `ServerToWorker.drain` when a scenario needs graceful shutdown after all expected observations are collected.
- Close the stream intentionally to simulate disconnects.

The fake endpoint does not define new wire behaviour. Scenario controls that are not present in `worker.proto`, such as cooperative cancellation and result acknowledgement, are harness actions outside the gRPC message stream. They are used to coordinate conformance runner fixtures and to test SDK state-machine behaviour without changing AW.

## Scenario execution model

For each scenario, the harness:

1. Loads `../scenarios.json` and selects one scenario id.
2. Allocates a local gRPC listen address.
3. Spawns the SDK conformance worker process with the endpoint URL, scenario id, namespace, max concurrency, and any runner control channel details in environment variables.
4. Waits for `RegisterWorker` and validates the advertised activity set.
5. Dispatches scenario tasks according to `given.tasks` and any `dispatchAfter` dependencies.
6. Applies `given.signals` as harness controls, for example stream disconnects, runner-seam cancellation, or release of blocked fixture activities.
7. Records worker messages until every expected report, failure, heartbeat, re-report, and peak-concurrency condition is observed or a scenario timeout expires.
8. Emits one normalized result line for the SDK and scenario.

Workers are black boxes from the harness perspective. The only SDK-specific contract is the process command documented in `runners.md`; the harness asserts wire observations, not SDK APIs.

## Recording schema

Recordings are normalized before assertion and cross-SDK comparison:

```json
{
  "sdk": "rust",
  "scenario": "reconnect-and-re-report",
  "registrations": [
    {
      "namespace": "conformance",
      "activity_types": ["conformance.echo"],
      "after_reconnect": false
    }
  ],
  "reports": [
    {
      "workflow_id": {"uuid": "wf-reconnect"},
      "activity_id": {"sequence_position": 1},
      "outcome": "result",
      "content_type": "application/json",
      "json": {"message": "before disconnect"},
      "re_report": true
    }
  ],
  "failures": [
    {
      "workflow_id": {"uuid": "wf-fail-terminal"},
      "activity_id": {"sequence_position": 1},
      "outcome": "error",
      "wire_kind": "ACTIVITY_ERROR_KIND_TERMINAL",
      "message": "terminal fixture failure",
      "details": {"content_type": "application/json", "json": {}}
    }
  ],
  "heartbeats": [
    {
      "workflow_id": {"uuid": "wf-heartbeat"},
      "activity_id": {"sequence_position": 1},
      "content_type": "application/json",
      "json": {"phase": "halfway", "percent": 50}
    }
  ],
  "reReports": [
    {"activity_id": {"sequence_position": 1}, "before": "dispatch:2"}
  ],
  "peak_concurrency": 1,
  "cancellation_observed": false,
  "forced_termination": false
}
```

Normalization rules:

- Sort `activity_types` lexicographically for comparison.
- Compare `workflow_id.uuid` and `activity_id.sequence_position` using the AW wrapper shapes from `common.proto`.
- Decode JSON payload bytes only when `content_type` is the JSON baseline; otherwise preserve raw bytes and fail expectations requiring `payload_equals`.
- Map numeric proto enum values to `ACTIVITY_ERROR_KIND_RETRYABLE` or `ACTIVITY_ERROR_KIND_TERMINAL` strings before diffing.
- Preserve message order for assertions that include `mustOccurBefore` or reconnect replay ordering.

## Reconnect and acknowledgements

The current `worker.proto` has no acknowledgement frame. For `reconnect-and-re-report`, the fake endpoint treats the first observed result for activity sequence position `1` as received but unacknowledged, closes the stream immediately, waits for the worker to reconnect and re-register, and expects the same `ActivityResult` to be sent again before it dispatches activity sequence position `2`. The engine-side de-duplication contract is outside this harness; this suite only verifies the SDK does not drop locally-computed unacknowledged work across reconnect.

## Cancellation

The current `worker.proto` has no cancellation frame. The `cancellation` scenario is driven by a harness control signal delivered through the conformance runner seam that represents the SDK's cooperative cancellation surface. The harness records whether the cancellation-observing fixture activity saw the cancellation flag and verifies the worker was not forcibly terminated. When a language SDK has no cancellation seam yet, its runner must log a clean skip for that scenario rather than invent a wire message.

## Backpressure and peak concurrency

The fake endpoint records peak concurrency by combining task dispatch timing with runner fixture controls. The `backpressure` scenario sends five blocking tasks while the worker is configured with `maxConcurrency = 2`. The first two handlers block at a runner barrier; the harness verifies exactly two are in flight before releasing the barrier and allowing all five tasks to complete. A peak below two means the worker did not use its configured capacity; a peak above two means it violated backpressure.

## Self-test

The harness self-test runs entirely against the Rust SDK conformance runner and the fake endpoint:

```sh
cargo test -p aion-worker --test worker_conformance_fake_engine
```

The self-test must not read `AION_WORKER_TEST_URL`. It validates that the fake endpoint can load every scenario, accept the Rust runner registration, record completions/failures/heartbeats, inject the reconnect stream drop, verify re-report-before-new-task ordering, and observe peak concurrency for the backpressure scenario.

If the Rust runner or the test binary is not available in a local checkout, the harness prints a single skip line naming the missing binary or toolchain. CI for AR-011 is expected to treat the Rust self-test as required.

## Failure reporting

Every mismatch includes the SDK, scenario id, field path, expected value, and actual value:

```text
DIVERGENCE sdk=rust scenario=reconnect-and-re-report path=reports[1].must_occur_before expected="dispatch:2" actual="dispatch observed before re-report"
```

The same diff format is used for per-SDK scenario validation and three-way equivalence comparisons.
