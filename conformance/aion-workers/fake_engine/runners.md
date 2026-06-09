# Worker conformance runners

This file defines the thin worker processes that the fake endpoint launches for Rust, Python, and TypeScript. Each runner is intentionally small: it connects to the fake `WorkerProtocol.StreamWorker` endpoint, registers the same fixed activities, serves only the selected scenario, and lets the fake endpoint record all observable behaviour.

## Shared environment contract

The fake endpoint starts each runner with:

| Variable | Meaning |
| --- | --- |
| `AION_WORKER_CONFORMANCE_ENDPOINT` | Local fake endpoint URL, for example `http://127.0.0.1:43127`. |
| `AION_WORKER_CONFORMANCE_SCENARIO` | Scenario id from `../scenarios.json`. |
| `AION_WORKER_CONFORMANCE_NAMESPACE` | Namespace to pass in `RegisterWorker.namespace`; normally `conformance`. |
| `AION_WORKER_CONFORMANCE_MAX_CONCURRENCY` | Operator-configured concurrency limit for the worker; the backpressure scenario sets `2`. |
| `AION_WORKER_CONFORMANCE_CONTROL` | Optional runner control channel for harness-only controls such as cooperative cancellation or blocked-task release. |

No runner reads `AION_WORKER_TEST_URL`, `AION_SERVER_URL`, or any live engine configuration.

## Fixture activities

Every runner registers exactly these activity types unless the selected SDK cannot yet expose the required public API and logs a skip:

| Activity type | Behaviour |
| --- | --- |
| `conformance.echo` | Decode JSON input and return the same JSON value with the same JSON content-type tag. |
| `conformance.fail-retryable` | Fail with an explicitly retryable SDK error mapped to `ACTIVITY_ERROR_KIND_RETRYABLE`. |
| `conformance.fail-terminal` | Fail with an explicitly terminal SDK error mapped to `ACTIVITY_ERROR_KIND_TERMINAL`. |
| `conformance.heartbeat-slow` | Call the SDK activity context heartbeat API with `{"phase":"halfway","percent":50}` and then complete. |
| `conformance.cancel-observing` | Poll the SDK cooperative cancellation API and return a terminal cancellation failure after the harness cancellation signal is observed. |
| `conformance.blocking-echo` | Block at a harness-controlled barrier so the fake endpoint can observe peak concurrency, then echo the input. |

The activities are deterministic and contain no side effects outside the runner process.

## Rust runner

Toolchain probe:

```sh
command -v cargo >/dev/null
```

Reference command from the repository root:

```sh
AION_WORKER_CONFORMANCE_ENDPOINT=$AION_WORKER_CONFORMANCE_ENDPOINT \
AION_WORKER_CONFORMANCE_SCENARIO=$AION_WORKER_CONFORMANCE_SCENARIO \
AION_WORKER_CONFORMANCE_NAMESPACE=conformance \
AION_WORKER_CONFORMANCE_MAX_CONCURRENCY=2 \
  cargo test -p aion-worker --test worker_conformance_fake_engine
```

The Rust runner uses the public `aion-worker` activity API: `ActivityRegistry::register_activity`, typed JSON payload helpers, `ActivityFailure::retryable`, and `ActivityFailure::terminal`. The fake endpoint self-test uses Rust as the reference runner because it is the canonical worker-side implementation for this cluster.

If Cargo or the Rust test binary is unavailable, emit:

```text
SKIP sdk=rust reason="cargo or worker_conformance_fake_engine unavailable"
```

## Python runner

Toolchain probe:

```sh
command -v python >/dev/null && python -m pytest --version >/dev/null
```

Reference quality gates from `sdks/python/aion-worker`:

```sh
ruff check .
mypy --strict aion_worker tests
pytest tests/conformance
```

Reference conformance command once the Python activity surface from AR-008 is present:

```sh
AION_WORKER_CONFORMANCE_ENDPOINT=$AION_WORKER_CONFORMANCE_ENDPOINT \
AION_WORKER_CONFORMANCE_SCENARIO=$AION_WORKER_CONFORMANCE_SCENARIO \
AION_WORKER_CONFORMANCE_NAMESPACE=conformance \
AION_WORKER_CONFORMANCE_MAX_CONCURRENCY=2 \
  pytest tests/conformance/test_worker_protocol.py
```

The Python runner should use decorator-registered activities, `RetryableError`, `TerminalError`, the SDK activity context heartbeat API, and the SDK cooperative cancellation API. In checkouts where the high-level activity API or JSON codec is not yet available, the runner may use the current lower-level dispatcher/session API only if it still exercises the SDK's public transport and classification behaviour. Otherwise it must log a clean skip:

```text
SKIP sdk=python reason="aion-worker Python activity API unavailable"
```

## TypeScript runner

Toolchain probe:

```sh
command -v npm >/dev/null
```

Reference quality gates from `sdks/typescript/aion-worker`:

```sh
npm run typecheck
npm run lint
npm test
```

Reference conformance command once the TypeScript activity surface from AR-010 is present:

```sh
AION_WORKER_CONFORMANCE_ENDPOINT=$AION_WORKER_CONFORMANCE_ENDPOINT \
AION_WORKER_CONFORMANCE_SCENARIO=$AION_WORKER_CONFORMANCE_SCENARIO \
AION_WORKER_CONFORMANCE_NAMESPACE=conformance \
AION_WORKER_CONFORMANCE_MAX_CONCURRENCY=2 \
  npm test -- --runInBand worker-protocol.conformance
```

The TypeScript runner should use generic typed activities, `RetryableError`, `TerminalError`, async context heartbeat, and cooperative cancellation. In checkouts where only the lower-level `ActivityDispatcher` API exists, the runner may use it to exercise transport/classification. If the required SDK surface or local npm tooling is unavailable, log:

```text
SKIP sdk=typescript reason="aion-worker TypeScript activity API unavailable"
```

## Three-way equivalence

After each SDK has produced a normalized recording, the harness compares:

- `registrations[*].namespace` and sorted `activity_types`.
- Report count, `workflow_id.uuid`, `activity_id.sequence_position`, result JSON, and content type.
- Failure count, `workflow_id.uuid`, `activity_id.sequence_position`, `wire_kind`, message predicate, details content type, and details JSON.
- Heartbeat count minimums and heartbeat progress payloads.
- Reconnect replay order, especially `re_report` before `dispatch:<sequence_position>`.
- `cancellation_observed` and `forced_termination`.
- `peak_concurrency`.

A drift in any SDK fails the suite with a per-scenario, per-language diff:

```text
DIVERGENCE sdk=python scenario=fail-retryable path=failures[0].wire_kind expected="ACTIVITY_ERROR_KIND_RETRYABLE" actual="ACTIVITY_ERROR_KIND_TERMINAL"
```

Unavailable toolchains produce `SKIP` lines and are omitted from local equivalence comparison, but CI should install all three toolchains before declaring AR-011 complete.
