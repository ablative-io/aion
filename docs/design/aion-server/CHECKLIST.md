# Aion-Server — Checklist

## Wire Contract Scaffold (aion-proto)

- [ ] **C1** — aion-proto is a workspace member depending on aion-core plus external crates only (prost, tonic, serde) — no aion, aion-server, or store-backend dependency.
- [ ] **C2** — build.rs compiles the .proto files via tonic-build and the generated code is included through a thin lib.rs that contains only declarations and re-exports.
- [ ] **C3** — aion-proto sets unsafe_code = "deny" and inherits the workspace clippy lints; cargo check -p aion-proto passes.

## Common Wire Types and Error Taxonomy

- [ ] **C4** — common.proto and convert.rs define the proto<->aion-core mapping for WorkflowId, RunId, ActivityId, TimerId, Payload, and WorkflowStatus with lossless round-trip.
- [ ] **C5** — The wire Event/WorkflowFilter/WorkflowSummary reuse aion-core's types via a thin envelope (namespace, request id, serialised core payload); no wire clone of those types exists.
- [ ] **C6** — WireError is a closed taxonomy with stable codes covering not-found, namespace-denied, sequence-conflict, unknown-query, query-timeout, not-running, lagged, and backend categories.
- [ ] **C7** — WireError maps from aion's EngineError, aion-store's StoreError, and query/signal/namespace failures, with a documented mapping and no Debug-formatted internals on the wire.

## Workflow Management Wire API

- [ ] **C8** — workflow.proto defines a service with StartWorkflow, Signal, Query, Cancel, ListWorkflows, and DescribeWorkflow RPCs and their request/response messages.
- [ ] **C9** — Each request carries a namespace; inputs and results are carried as aion-core Payload, and ListWorkflows carries a WorkflowFilter and returns WorkflowSummary values.
- [ ] **C10** — Workflow-management wire messages have serde types deriving Serialize + Deserialize and round-trip losslessly to/from their proto form.

## Event Streaming Wire Shape

- [ ] **C11** — events.proto defines a SubscriptionRequest with per-workflow, filtered (by type/status/namespace), and firehose variants, plus the streamed Event envelope.
- [ ] **C12** — The streamed Event envelope carries the aion-core Event unmodified plus the owning namespace, and round-trips losslessly.

## Worker Protocol Wire Shape

- [ ] **C13** — worker.proto defines a bidirectional-streaming service with RegisterWorker, ActivityTask, ActivityResult, and Heartbeat messages.
- [ ] **C14** — ActivityTask carries the activity type, input Payload, and the WorkflowId/ActivityId correlation; ActivityResult carries a result Payload or an ActivityError; Heartbeat carries liveness and optional progress.
- [ ] **C15** — RegisterWorker carries the worker's namespace and the activity types it implements.

## Server Scaffold, Config, and State

- [ ] **C16** — aion-server is a workspace member (binary) depending on aion, aion-proto, and a store backend, with a thin main.rs where anyhow is the only error type at the top level.
- [ ] **C17** — Config types (store DSN, listen ports, TLS, auth, dashboard asset path, namespace mode, worker heartbeat window, WebSocket buffer bound) derive Deserialize only — never Serialize — and no operational value is a hardcoded default.
- [ ] **C18** — ServerError is a thiserror taxonomy used throughout the server library modules; no unwrap/expect in library code and lock/stream poison is handled explicitly.
- [ ] **C19** — Shared server state holds the engine handle and the NamespaceResolver and is constructed once at startup.

## Namespace Isolation

- [ ] **C20** — A NamespaceResolver authorises a caller for a namespace and scopes an operation to it, abstracting whether namespaces map to separate engine/store instances or one shared engine with a namespace key.
- [ ] **C21** — Namespace authorisation and scoping happen at the adapter boundary before any Engine call; no cross-namespace operation (start/signal/query/cancel/list/subscribe/worker dispatch) reaches the engine.

## Workflow API Handlers (gRPC + HTTP)

- [ ] **C22** — A single handler layer translates each workflow-management request into exactly one (or a small fixed composition of) Engine call(s); it performs no retry, scheduling, sequencing, or other durable decision.
- [ ] **C23** — A tonic gRPC service and an axum HTTP/JSON facade both delegate to the shared handler layer; the transport is a thin skin.
- [ ] **C24** — Handler outcomes (including failures) are serialised as the wire response or a mapped WireError; engine internals never cross the wire.

## WebSocket Event Streaming

- [ ] **C25** — A SubscriptionRequest is validated for namespace scope and mapped onto the engine's EventFilter, then engine.subscribe is called to obtain the stream tail.
- [ ] **C26** — A per-connection task forwards events from the engine stream to the socket until the client closes or the subscribed workflow terminates, then drops the subscription with no leak.
- [ ] **C27** — Each connection has a bounded outbound buffer; a consumer that cannot keep up is closed with a typed lag WireError, and the engine broadcast is never back-pressured by a subscriber.

## Remote-Worker Protocol (server side)

- [ ] **C28** — The server accepts a worker's bidirectional gRPC stream, records its RegisterWorker advertisement in a connected-worker registry keyed by namespace and activity type.
- [ ] **C29** — A scheduled Tier-3 activity is matched to a registered worker for that activity type in that namespace and pushed as an ActivityTask; no Temporal-style polling.
- [ ] **C30** — A returned ActivityResult is fed into the engine's activity contract so the engine records ActivityCompleted/ActivityFailed and resumes; the server does not record activity events or decide retries itself.
- [ ] **C31** — A worker that misses its heartbeat window or disconnects before reporting is marked lost and its in-flight task is surfaced to the engine as failed, leaving the retry decision to the engine.

## Dashboard Hosting

- [ ] **C32** — The server serves cluster AU's built static asset bundle from a configured (or embedded) path.
- [ ] **C33** — The dashboard is backed only by the same public gRPC/HTTP handlers and WebSocket feed every other client uses; there is no dashboard-specific API.
