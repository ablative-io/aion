# Aion-Operations — Checklist

## Non-Resident Signal Delivery

- [ ] **C1** — Engine signal() branches on workflow residency: Resident delivers immediately, NonResident records and defers, Terminal returns typed error, Unknown returns WorkflowNotFound.
- [ ] **C2** — SignalResumeHandoff::deliver_deferred() is invoked when a workflow transitions from Suspended to Resident, delivering all queued signals in FIFO order.
- [ ] **C3** — A signal to a non-resident workflow appears in the workflow's durable history as a SignalReceived event even before the workflow resumes.
- [ ] **C4** — On engine restart, in-memory SignalResumeHandoff is empty; AD replay re-delivers recorded signals from history during deterministic replay (no double-delivery).

## Graceful Shutdown and Drain

- [ ] **C5** — On SIGTERM or SIGINT, the server stops accepting new connections and new workflow starts within 100ms of signal receipt.
- [ ] **C6** — Connected workers receive a drain signal indicating no new tasks will be dispatched; workers finish in-flight tasks.
- [ ] **C7** — The server awaits completion of all in-flight activities up to the configured drain timeout before shutting down the engine.
- [ ] **C8** — Activities that exceed the drain timeout are surfaced as retryable failures via the existing LostWorkerReport path.
- [ ] **C9** — Clean drain exits with code 0; timeout-exceeded drain exits with code 1.
- [ ] **C10** — No event writes are lost during shutdown — the engine flushes all pending appends before the store connection closes.

## Observability — Metrics

- [ ] **C11** — GET /metrics returns Prometheus exposition format text with all registered metrics and their current values.
- [ ] **C12** — Workflow lifecycle metrics: aion_workflows_started_total and aion_workflows_completed_total counters with namespace and type/status labels.
- [ ] **C13** — Activity metrics: aion_activities_dispatched_total counter, aion_activities_completed_total counter (with outcome label), and aion_activity_duration_seconds histogram.
- [ ] **C14** — Infrastructure metrics: aion_connected_workers gauge, aion_inflight_activities gauge, aion_store_operation_duration_seconds histogram.
- [ ] **C15** — Signal and schedule metrics: aion_signals_delivered_total counter (with residency label), aion_schedules_fired_total counter.

## Observability — Health Probes

- [ ] **C16** — GET /health/live returns 200 when the process is running and the beamr scheduler is responsive; 503 otherwise.
- [ ] **C17** — GET /health/ready returns 200 when the store is reachable and the runtime is initialized; 503 otherwise.
- [ ] **C18** — Both probes respond within 100ms under normal operation (no blocking store queries in the probe path).

## Observability — Structured Logging

- [ ] **C19** — All server output uses tracing with a JSON subscriber; no println, eprintln, or unstructured log macro output exists in production paths.
- [ ] **C20** — Engine operations emit spans with workflow_id and namespace fields; activity dispatch adds activity_type and worker_id.
- [ ] **C21** — Error events include the typed error variant name as a structured field, not just a Display string.

## Runtime Configuration

- [ ] **C22** — aion-server loads configuration from aion.toml with all sections (server, store, runtime, drain, auth, metrics, namespaces) parsed and validated at startup.
- [ ] **C23** — Environment variables with AION_ prefix override TOML values (e.g. AION_STORE_URL overrides store.url).
- [ ] **C24** — CLI flags override both environment and file values; precedence is CLI > env > file > defaults.
- [ ] **C25** — Invalid configuration (missing required fields, malformed URLs, conflicting settings) produces a clear error message naming the field and exits with code 2 before any server initialization.
- [ ] **C26** — The default configuration (no file, no env) starts a development server with an in-memory store on localhost — usable for local testing without any config.

## Authentication and Namespace Authorization

- [ ] **C27** — When auth.enabled=true, requests without a valid Bearer token are rejected with HTTP 401 / gRPC UNAUTHENTICATED before reaching any handler.
- [ ] **C28** — The token's namespace claim scopes all operations; a token for namespace A cannot start, signal, query, cancel, or list workflows in namespace B.
- [ ] **C29** — Worker connections authenticate with the same token mechanism; the token's namespace determines which activity tasks the worker receives.
- [ ] **C30** — JWKS keys are cached with configurable refresh interval; a key rotation does not require server restart.
- [ ] **C31** — When auth.enabled=false (default), no authentication is required and the namespace is taken from request metadata (existing NamespaceGuard behaviour).
- [ ] **C32** — The auth module compiles only when cfg(feature='auth') is active; the base binary has no jsonwebtoken or JWKS dependencies.
