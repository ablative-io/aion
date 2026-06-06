# Aion client conformance harnesses

This directory contains the shared scenario source for the four caller SDK conformance harnesses. `scenarios.json` is the source of truth for scenario ids, steps, expected success shapes, and expected error taxonomy variants. Do not copy or hardcode those scenarios into a language-specific test.

## Server fixture

AL-007 consumes a live `aion-server` fixture; it does not start one. Bring up a server with the conformance workflows registered:

- `conformance.echo` starts, accepts the `record` signal, answers the `state` query, records cancellation requests, and emits lifecycle/signal events.
- `conformance.slow-query` answers the `slow` query after longer than the scenario deadline.

The server binary expects a config file (`aion-server <config.json>`). Use a fixture config with non-zero gRPC/HTTP listen ports, auth enabled with a bearer token if required, the `conformance` namespace available, worker connectivity, and websocket/event streaming enabled.

## Environment

All harnesses use the same runtime gate:

```sh
export AION_SERVER_URL=http://127.0.0.1:8080
export AION_AUTH_TOKEN=dev-token   # omit or leave empty only when the fixture does not require auth
```

When `AION_SERVER_URL` is unset, each harness prints one `SKIP sdk=<sdk> ...` line and passes without `ignore` attributes, feature-gated skips, or per-language scenario filtering.

## Running each harness

Run all four harnesses against the same live server URL and token.

### Rust

From the repository root:

```sh
AION_SERVER_URL=$AION_SERVER_URL AION_AUTH_TOKEN=$AION_AUTH_TOKEN \
  cargo test -p aion-client --test conformance
```

### Python

From `sdks/python/aion-client`:

```sh
AION_SERVER_URL=$AION_SERVER_URL AION_AUTH_TOKEN=$AION_AUTH_TOKEN \
  pytest tests/conformance
```

### TypeScript

From `sdks/typescript/aion-client`:

```sh
AION_SERVER_URL=$AION_SERVER_URL AION_AUTH_TOKEN=$AION_AUTH_TOKEN \
  npm run build && npm test -- --test-name-pattern conformance
```

The package test script executes built `dist/**/*.test.js`; the conformance test is compiled from `test/conformance/conformance.test.ts`.

### Gleam

From `gleam/aion_client`:

```sh
AION_SERVER_URL=$AION_SERVER_URL AION_AUTH_TOKEN=$AION_AUTH_TOKEN \
  gleam test
```

## Normalised observable output

Each harness emits one line per exercised scenario step:

```text
AION_CONFORMANCE sdk=<sdk> scenario=<scenario-id> step=<step-id> result=<json>
```

`result` is a normalised observable value, either:

```json
{"ok":{"kind":"handle","workflowId":"...","runId":"..."}}
```

or:

```json
{"error":"AlreadyExists"}
```

Error names are the shared contract taxonomy only: `NotFound`, `AlreadyExists`, `QueryFailed`, `QueryTimeout`, `Cancelled`, `Unavailable`, `Unauthenticated`, `InvalidArgument`, and `Server`.

## Cross-SDK equivalence rule

A scenario is conformant only when all four SDKs produce the same normalised observable result for every shared scenario step and each result satisfies the `scenarios.json` expectation. Do not declare conformance passed for a scenario if one SDK skips a step, emits a different error variant, returns a different normalised value, or cannot execute the live operation.

Report divergence with the SDK, scenario, step, expected observable, and actual observable, for example:

```text
DIVERGENCE sdk=typescript scenario=idempotency-conflict step=conflicting-reuse expected={"error":"AlreadyExists"} actual={"ok":{"kind":"handle",...}}
```

The language harness assertions include the scenario id and step id on mismatch; the `AION_CONFORMANCE` lines provide the step-by-step JSON needed for manual or scripted cross-SDK comparison. At the time of AL-007, the Gleam SDK has no checked-in live HTTP/WebSocket transport, so its live run emits the SDK's current `Unavailable` observable for non-connect operations. Treat those lines as a divergence, not as conformance passed, until the Gleam transport follow-up lands.
