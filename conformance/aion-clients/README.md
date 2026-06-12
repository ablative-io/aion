# Aion client conformance harnesses

This directory contains the shared scenario source for the caller SDK conformance harnesses. `scenarios.json` is the source of truth for scenario ids, steps, expected success shapes, and expected error taxonomy variants. Do not copy or hardcode those scenarios into a language-specific test.

Three SDKs have live harnesses: Rust (`crates/aion-client/tests/conformance.rs`), Python (`sdks/python/aion-client/tests/conformance/test_conformance.py`), and TypeScript (`sdks/typescript/aion-client/test/conformance/conformance.test.ts`). The Gleam SDK has the same runtime gate (it prints `SKIP sdk=gleam ...` when `AION_SERVER_URL` is unset) but no checked-in live HTTP/WebSocket transport, so it has no live harness yet; treat its non-connect observables as divergence, not conformance, until the Gleam transport follow-up lands.

## Scenarios covered

All seven scenarios run against a live server, each harness emitting the identical 34-step observable matrix:

- `happy-path` — start → signal → query → list → describe → cancel → subscribe over the full caller surface.
- `idempotency-conflict` — the same start idempotency key retried identically returns the original handle; conflicting reuse is `AlreadyExists`.
- `query-timeout` — a query exceeding the caller deadline is `QueryTimeout` (never `Cancelled`, never fire-and-forget).
- `not-found` — operations against a nonexistent workflow are `NotFound`.
- `namespace-denied` — an authenticated caller without a grant for the namespace gets `NamespaceDenied` (never `Unauthenticated`).
- `not-found-anti-leak` — probing a foreign-namespace workflow is byte-identical to probing a workflow that exists nowhere (`NotFound`), so cross-namespace existence never leaks.
- `disconnect-resume` — a forced transport drop mid-subscription resumes gap-free and duplicate-free from the wire cursor.

## Subscription contract under test

Three cross-SDK contract decisions are pinned by these scenarios (normative text in `docs/design/aion-clients/CLIENT-CONTRACT.md`, operation `subscribe`):

- **Initial attach**: an attach without a cursor is a live tail; an explicit `resume_from_seq = 1` replays the full recorded history (`0` is `invalid_input` on the wire). The harnesses attach with `resume_from_seq = 1` because the scenarios assert on events from the workflow's beginning.
- **Connect failure**: a failed attach is classified exactly like a mid-stream drop — `Unavailable` is retryable (per-workflow streams re-attach with their cursor, including on the initial attach), every other taxonomy error is terminal immediately.
- **Graceful end**: the server finishes a graceful subscription end with WebSocket close-1000, reason `subscription complete`; SDKs end iteration normally on close-1000 and treat any other socket end as a transient drop.

## Server fixture

The harnesses consume a live Aion server; they do not start one. The fixture workflows live in `fixture/`:

- `conformance_echo` (`fixtures.workflowType`) starts, accepts the `record` signal, answers the `state` query, accepts cooperative cancellation, and emits lifecycle/signal events.
- `conformance_slow_query` (`fixtures.slowQueryWorkflowType`) answers the `slow` query after longer than the scenario deadline.

Build the `.aion` archives:

```sh
aion package conformance/aion-clients/fixture --build
```

Bring up the server with both packages loaded. A working config (memory store, `conformance` as the default namespace):

```toml
workflow_packages = [
  "<repo>/conformance/aion-clients/fixture/conformance_echo.aion",
  "<repo>/conformance/aion-clients/fixture/conformance_slow_query.aion",
]

[server]
listen_address = "127.0.0.1:18084"   # HTTP + WebSocket
grpc_address = "127.0.0.1:18055"     # gRPC

[store]
backend = "memory"

[runtime]
query_timeout_ms = 10000

[namespaces]
default = "conformance"

[websocket]
event_broadcast_capacity = 1024
```

```sh
aion server --config /tmp/aion-conformance.toml
```

Caller identity rides the server's development-header extraction: each harness presents subject `conformance-harness` with a grant for `conformance` only — never `conformance-denied` — which is exactly the grant shape the `namespace-denied` and `not-found-anti-leak` scenarios pin.

## Environment

All harnesses use the same runtime gate. When `AION_SERVER_URL` is unset, each harness prints one `SKIP sdk=<sdk> ...` line and passes without `ignore` attributes, feature-gated skips, or per-language scenario filtering.

- `AION_SERVER_URL` — the SDK's primary endpoint: the gRPC address for Rust and Python (`http://127.0.0.1:18055`), the HTTP base URL for TypeScript (`http://127.0.0.1:18084`).
- `AION_STREAM_URL` — the HTTP/WebSocket listener carrying `/events/stream`. Required for Rust and Python (the gRPC endpoint and the stream listener are separate addresses; nothing is derived). Optional override for TypeScript, whose HTTP endpoint and WebSocket stream share one listener.
- `AION_AUTH_TOKEN` — bearer token; omit or leave empty when the fixture server does not require auth.

For plaintext `http` stream URLs each harness fronts the stream — and only the stream — with a local transparent TCP relay so `harness.forceDisconnect` can sever live sockets without touching the server; `https`/`wss` endpoints connect directly and forced disconnects are refused with a precise error.

## Running each harness

Run all harnesses against the same live server.

### Rust

From the repository root:

```sh
AION_SERVER_URL=http://127.0.0.1:18055 AION_STREAM_URL=http://127.0.0.1:18084 \
  cargo test -p aion-client --test conformance
```

### Python

From `sdks/python/aion-client`:

```sh
AION_SERVER_URL=http://127.0.0.1:18055 AION_STREAM_URL=http://127.0.0.1:18084 \
  uv run --extra dev --isolated -- python -m pytest tests/conformance
```

### TypeScript

From `sdks/typescript/aion-client`:

```sh
AION_SERVER_URL=http://127.0.0.1:18084 npm test
```

The package test script compiles and executes `dist/**/*.test.js`; the conformance test is compiled from `test/conformance/conformance.test.ts`.

### Gleam

From `gleam/aion_client`, `gleam test` runs the SDK's unit suite and prints the `SKIP sdk=gleam` gate line; there is no live harness until the Gleam HTTP/WebSocket transport lands.

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

Error names are the shared contract taxonomy only: `NotFound`, `AlreadyExists`, `QueryFailed`, `QueryTimeout`, `Cancelled`, `Unavailable`, `Unauthenticated`, `NamespaceDenied`, `InvalidArgument`, and `Server`.

Steps whose expectation carries `errorSameAs` additionally pin the SDK-observable error identity (taxonomy member, message, and detail) to be byte-identical to the error recorded by the referenced step. The `not-found-anti-leak` scenario uses this to prove a foreign-owned workflow probe is indistinguishable from a nonexistent-workflow probe.

## Cross-SDK equivalence rule

A scenario is conformant only when every live-harness SDK produces the same normalised observable result for every shared scenario step and each result satisfies the `scenarios.json` expectation. Do not declare conformance passed for a scenario if one SDK skips a step, emits a different error variant, returns a different normalised value, or cannot execute the live operation.

Report divergence with the SDK, scenario, step, expected observable, and actual observable, for example:

```text
DIVERGENCE sdk=typescript scenario=idempotency-conflict step=conflicting-reuse expected={"error":"AlreadyExists"} actual={"ok":{"kind":"handle",...}}
```

The language harness assertions include the scenario id and step id on mismatch; the `AION_CONFORMANCE` lines provide the step-by-step JSON needed for manual or scripted cross-SDK comparison.
