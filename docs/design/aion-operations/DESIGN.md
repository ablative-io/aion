---
type: design
cluster: aion-operations
title: Aion Operations — Production Deployment, Observability, and Runtime Completion
---

# Aion Operations — Production Deployment, Observability, and Runtime Completion

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

This cluster makes Aion deployable. Every prior cluster built one slice of
the engine or its network surface in isolation — the event model, the store,
the scheduler, the signal router, the worker protocol. Each passes its own
test suite. None of them together constitute a system an operator can run in
production.

Production means: a binary starts, advertises health, accepts traffic,
drains gracefully on shutdown, emits structured telemetry an operator can
alert on, and survives restarts without losing or double-executing work.
It means a workflow that suspends for a signal can receive that signal when
it resumes — not just when it happens to be resident. It means an operator
can configure the deployment topology without editing source code.

When this cluster is done, `aion-server` is a production binary that a
platform team can deploy behind a load balancer, point Prometheus at, wire
into their alerting stack, and trust with real workloads. The gap between
"tests pass" and "production-ready" is closed.

## Problem

Five categories of work remain between the current state and production
readiness:

**1. Non-resident signal delivery is unfinished.** The `SignalResumeHandoff`
struct exists and is tested (AT-006), but it is not wired into the engine's
signal flow. `ConcreteSignalRouter` (AX-005) only handles resident targets.
A signal sent to a suspended workflow is rejected with `WorkflowNotFound`
instead of being recorded and queued for delivery on resume. This is a
correctness gap — Temporal's signal semantics guarantee delivery regardless
of workflow residency.

**2. No graceful shutdown or drain.** The engine has a `shutdown()` method
that closes the scheduler. The server has no coordinated drain: in-flight
HTTP requests are dropped, connected workers lose their streams mid-task,
and activities executing on workers at shutdown time are not awaited or
re-queued. A rolling deploy loses work.

**3. No structured observability.** The server emits no metrics, no
structured traces, and no health probes. An operator cannot distinguish a
healthy idle server from a crashed one. Activity latency, queue depth,
workflow throughput, store latency — none are measurable without reading
logs (which are also unstructured).

**4. No runtime configuration.** Store connection strings, listen addresses,
TLS settings, namespace configuration, and scheduler thread counts are
either hardcoded or passed as builder arguments in test code. There is no
configuration file, no environment variable mapping, and no validation at
startup. Operators cannot deploy without editing Rust.

**5. No authentication or authorization boundary.** Namespace isolation
exists structurally (the `NamespaceGuard` in AW), but there is no
authentication layer. Any client that can reach the port can start workflows
in any namespace. For multi-tenant deployments this is a security gap.

## Solution

A single cluster (`AO`, aion-operations) addressing all five categories.
The work lives primarily in `aion-server` (the deployable binary) with one
engine-level fix (non-resident signal wiring).

### D1 — Non-resident signal delivery completes the AT-006 contract

The engine's `signal()` path gains residency-aware routing:

- **Resident**: record + deliver (current `ConcreteSignalRouter` path).
- **Non-resident**: record + defer via `SignalResumeHandoff` (the struct
  already exists, tested, and correct). On workflow resume (AE transition
  from `Suspended` to `Resident`), the engine invokes
  `SignalResumeHandoff::deliver_deferred()` to flush queued signals in FIFO
  order.
- **Terminal**: return typed `SignalRouterError::Terminal` and record nothing.
- **Unknown**: return `EngineError::WorkflowNotFound` and record nothing.

This is an engine-level change in `crates/aion/src/signal/router.rs` and
the residency transition code in `crates/aion/src/engine/`. It does not
touch the server.

### D2 — Graceful shutdown with activity drain

On SIGTERM/SIGINT:

1. Server stops accepting new connections and new workflow starts.
2. Connected workers are sent a "drain" signal — finish in-flight tasks but
   accept no new ones.
3. The server awaits completion of all in-flight activities up to a
   configurable drain timeout.
4. After drain (or timeout), the engine shuts down, flushing any pending
   event writes.
5. The process exits with code 0 on clean drain, code 1 on timeout.

Workers that disconnect during drain have their in-flight tasks surfaced as
retryable failures (existing `LostWorkerReport` path). On restart, those
activities are re-dispatched via the engine's retry policy.

### D3 — Structured observability via metrics and health probes

**Metrics** (Prometheus exposition format, `/metrics` endpoint):

- `aion_workflows_started_total` (counter, labels: namespace, workflow_type)
- `aion_workflows_completed_total` (counter, labels: namespace, status)
- `aion_activities_dispatched_total` (counter, labels: namespace, activity_type)
- `aion_activities_completed_total` (counter, labels: namespace, outcome)
- `aion_activity_duration_seconds` (histogram, labels: namespace, activity_type)
- `aion_store_operation_duration_seconds` (histogram, labels: operation)
- `aion_connected_workers` (gauge, labels: namespace)
- `aion_inflight_activities` (gauge, labels: namespace)
- `aion_signals_delivered_total` (counter, labels: namespace, residency)
- `aion_schedules_fired_total` (counter, labels: namespace)

**Health probes** (`/health/live`, `/health/ready`):

- **Liveness**: process is running and the scheduler is responsive (not
  deadlocked). Returns 200 or 503.
- **Readiness**: store is reachable, runtime is initialized, at least one
  worker is connected (configurable). Returns 200 or 503.

**Structured logging** via `tracing` with JSON output:

- Every engine operation emits a span with `workflow_id`, `namespace`, and
  operation-specific fields.
- Activity dispatch/completion emits `activity_type`, `worker_id`, duration.
- Errors include the typed error variant name, not just a message string.

### D4 — Runtime configuration via file and environment

A single TOML configuration file (`aion.toml`) with environment variable
overrides following the `AION_` prefix convention:

```toml
[server]
listen_address = "0.0.0.0:7233"
grpc_address = "0.0.0.0:7234"

[store]
backend = "libsql"
url = "file:aion.db"

[runtime]
scheduler_threads = 4

[drain]
timeout_seconds = 30

[auth]
enabled = false
# When enabled, tokens are validated against this JWKS endpoint
jwks_url = ""

[metrics]
enabled = true

[namespaces]
default = "production"
```

Environment variables: `AION_SERVER_LISTEN_ADDRESS`, `AION_STORE_URL`, etc.
CLI flags override both. Precedence: CLI > env > file > defaults.

### D5 — Authentication boundary (optional, feature-gated)

When `auth.enabled = true`:

- Every request must carry a Bearer token (HTTP Authorization header, gRPC
  metadata).
- The token is validated against the configured JWKS endpoint.
- The token's claims include a `namespace` field. Requests are scoped to
  that namespace — a token for namespace A cannot operate on namespace B.
- Worker connections authenticate the same way. A worker's token determines
  which namespace it serves.

When `auth.enabled = false` (default during development), no authentication
is required. The namespace is taken from the request metadata (existing
`NamespaceGuard` behaviour).

This is feature-gated behind `cfg(feature = "auth")` to keep the dependency
tree lean for embedded/testing use.

## Goals

1. A signal sent to a non-resident workflow is recorded and delivered when
   the workflow resumes, matching Temporal's signal delivery semantics.
2. A rolling deploy (SIGTERM → drain → restart) loses zero in-flight work.
3. A Prometheus scrape of `/metrics` returns all listed metrics with correct
   labels and values.
4. `/health/live` and `/health/ready` return correct status codes within
   100ms.
5. `aion-server` starts from a TOML config file with zero code changes.
6. With auth enabled, a request without a valid token is rejected with 401.
7. All of the above pass under `cargo clippy --workspace -- -D warnings`.

## Non-Goals

- **Horizontal scaling / clustering.** Aion is single-writer-per-workflow by
  design. Multi-node coordination (sharding, routing, consensus) is a future
  cluster if ever needed. This cluster makes one node production-grade.
- **Custom metrics backends.** Prometheus exposition is the interface.
  Operators use their own Prometheus → Grafana / Datadog / etc. pipeline.
- **OAuth2 provider implementation.** The server validates tokens, it does
  not issue them. Token issuance is the operator's identity provider.
- **Log aggregation.** Structured JSON to stdout. Operators pipe to their
  own aggregation (Loki, Datadog, ELK). No built-in log shipping.
- **Rate limiting.** Deferred to a future cluster. Namespace isolation
  provides tenant separation; per-tenant rate limits are an operational
  policy decision.

## Structure

```
crates/aion/src/
├── signal/
│   ├── router.rs             — Residency-aware routing (modify, AO-001)
│   └── resume.rs             — SignalResumeHandoff (exists, wire in AO-001)
├── engine/
│   ├── api.rs                — Resume transition trigger (modify, AO-001)
│   └── ...
└── ...

crates/aion-server/src/
├── config/
│   ├── mod.rs                — pub mod + re-exports
│   ├── file.rs               — TOML parsing and validation (AO-003)
│   └── env.rs                — Environment variable overlay (AO-003)
├── observability/
│   ├── mod.rs                — pub mod + re-exports
│   ├── metrics.rs            — Prometheus registry and endpoint (AO-004)
│   ├── health.rs             — Liveness and readiness probes (AO-004)
│   └── tracing.rs            — Structured logging setup (AO-004)
├── auth/
│   ├── mod.rs                — pub mod + re-exports (AO-006)
│   ├── middleware.rs         — Tower middleware for token extraction (AO-006)
│   └── jwks.rs               — JWKS validation (AO-006)
├── shutdown.rs               — Graceful drain coordinator (AO-002)
├── state.rs                  — (modify: config-driven initialization, AO-003)
├── main.rs                   — (modify: config loading, signal handling, AO-002/003)
└── ...
```

## Constraints

- **CO1: No execution logic in the server.** The signal wiring (AO-001) is
  an engine change. The server never makes durability decisions.
- **CO2: No arbitrary defaults for operator-configurable values.** Thread
  counts, timeouts, drain durations, and listen addresses come from config.
  No hardcoded "sensible defaults" that surprise operators.
- **CO3: Feature-gated auth.** The `auth` module is behind
  `cfg(feature = "auth")`. The base binary compiles and runs without auth
  dependencies (jsonwebtoken, reqwest for JWKS).
- **CO4: No new dependencies in the engine crate.** Observability and config
  live in `aion-server`. The `aion` engine crate stays lean — no metrics,
  no tracing, no config crate dependencies.
- **CO5: Structured output only.** No println, no eprintln, no unstructured
  log lines. Every output goes through `tracing` with structured fields.
