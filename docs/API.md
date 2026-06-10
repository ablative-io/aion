# Aion API overview

Aion exposes the engine through the standalone `aion-server` plus language-specific client and worker SDKs.

## Implementation status

- **Implemented:** engine crash/restart recovery for active workflow histories, durable timer recovery, HTTP/JSON workflow operations, gRPC worker/client APIs, and the WebSocket event-stream route described below.
- **In progress:** dashboard UX and cross-language SDK/conformance hardening. Prefer `aion-cli`, HTTP, gRPC, or the Rust client when you need the most exercised surfaces.

## Authentication and caller metadata

HTTP and WebSocket routes use the same caller extraction path:

- In local development with auth disabled, send `x-aion-subject` to identify the caller. If omitted, the server identifies the caller as `anonymous`.
- Send `x-aion-namespaces` as a comma-separated allow-list when the server runs in shared-engine namespace mode. The default single-tenant dev config authorizes the configured namespace without this header.
- When auth is enabled, use the configured bearer-token/JWKS setup; WebSocket upgrades use the same headers as HTTP requests.

Most examples use the default `default` namespace.

## HTTP/JSON routes

The public HTTP router currently mounts these routes:

| Method | Path | Request shape | Response shape | Status |
|---|---|---|---|---|
| `GET` | `/workflows` | Query parameters: `namespace` (required), optional `workflow_type`, `status`, `started_after`, `started_before`, `closed_after`, `closed_before`, `search_attributes`, `limit`, `offset`. | JSON array of visibility `WorkflowSummary` values. | Implemented |
| `GET` | `/workflows/count` | Same query parameters as `/workflows`. | `{ "count": <number> }` | Implemented |
| `POST` | `/workflows/start` | JSON body with `namespace`, `workflow_type`, optional `input`. `input` may be an ordinary JSON value or a `ProtoPayload` envelope with `content_type` and byte-array `bytes`. | `ProtoStartWorkflowResponse` (`workflow_id`, `run_id`). | Implemented |
| `POST` | `/workflows/signal` | `ProtoSignalRequest`: `namespace`, `workflow_id`, optional `run_id`, `signal_name`, optional `payload`. | Empty acknowledgement object. | Implemented |
| `POST` | `/workflows/query` | `ProtoQueryRequest`: `namespace`, `workflow_id`, optional `run_id`, `query_name`. | `ProtoQueryResponse` with result or typed error. | Implemented |
| `POST` | `/workflows/cancel` | `ProtoCancelRequest`: `namespace`, `workflow_id`, optional `run_id`, `reason`. | Empty acknowledgement object. | Implemented |
| `POST` | `/workflows/list` | `ProtoListWorkflowsRequest`: `namespace`, optional encoded visibility `filter`. | `ProtoListWorkflowsResponse`. | Implemented |
| `POST` | `/workflows/describe` | `ProtoDescribeWorkflowRequest`: `namespace`, `workflow_id`, optional `run_id`, `include_history`. | Workflow summary plus optional history. | Implemented |
| `GET` | `/events/stream` | WebSocket upgrade; first client message must be a subscription JSON object. | WebSocket event frames. | Implemented |
| `GET` | `/schedules?namespace=...` | Namespace query parameter. | `ProtoListSchedulesResponse`. | Implemented |
| `POST` | `/schedules` | `ProtoCreateScheduleRequest`: `namespace`, encoded schedule `config`. | `201 Created` with `ProtoCreateScheduleResponse`. | Implemented |
| `GET` | `/schedules/{id}?namespace=...` | Path schedule id plus namespace query parameter. | `ProtoDescribeScheduleResponse`. | Implemented |
| `PUT` | `/schedules/{id}` | `ProtoUpdateScheduleRequest`; path id is used as the target schedule id. | `ProtoUpdateScheduleResponse`. | Implemented |
| `DELETE` | `/schedules/{id}?namespace=...` | Path schedule id plus namespace query parameter. | Empty acknowledgement object. | Implemented |
| `POST` | `/schedules/{id}/pause?namespace=...` | Path schedule id plus namespace query parameter. | `ProtoPauseScheduleResponse`. | Implemented |
| `POST` | `/schedules/{id}/resume?namespace=...` | Path schedule id plus namespace query parameter. | `ProtoResumeScheduleResponse`. | Implemented |
| `GET` | `/metrics` | No body; available when metrics are enabled. | Prometheus metrics text. | Optional |
| `GET` | `/health/live` | No body; available when health endpoints are installed. | Liveness response. | Optional |
| `GET` | `/health/ready` | No body; available when health endpoints are installed. | Readiness response. | Optional |

The `Proto*` JSON field names above are the serde names from `crates/aion-proto`. Prefer the CLI or generated/client SDK types for complex encoded `Payload` and `WireEnvelope` fields.

### Start example

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: docs-user' \
  -d '{"namespace":"default","workflow_type":"hello-world","input":{"name":"Ada"}}'
```

For non-JSON binary payloads, use the envelope form with `bytes` as a JSON array of integers. For the hello-world tutorial, `aion-cli` is the recommended path because it handles payload encoding for you.

## WebSocket event streaming

Connect to `ws://<server>/events/stream` and then send one JSON text or binary message that selects the subscription. The server ignores ping/pong frames while waiting, but closes the stream with an input error if the subscription is missing or malformed.

Supported subscription messages:

```json
{"per_workflow":{"namespace":"default","workflow_id":{"uuid":"<workflow-id>"}}}
```

```json
{"filtered":{"namespace":"default","workflow_type":"hello-world","status":"Completed"}}
```

```json
{"firehose":{"namespace":"default"}}
```

The same shapes may also be wrapped as `{ "subscription": { ... } }`. `filtered` accepts optional `workflow_type`, optional `status` (status name or numeric value), and optional `namespace_selector`. `firehose` accepts `namespace` or `namespace_selector`.

## Recovery and durable timers

Crash/restart recovery and durable timers are implemented in the current build. On server startup the engine replays durable histories for active workflows, reconciles visibility, and recovers timer state. Timer operations record durability events and are re-armed from stored history after restart, so workflow-visible time continues to come from recorded event timestamps rather than wall-clock reads inside workflow code.

## Client SDKs

Use client SDKs when application code needs to call Aion workflows over the server API:

- Gleam: [`../gleam/aion_client/README.md`](../gleam/aion_client/README.md) — SDK surface exists; live HTTP/WebSocket transport remains in progress for embedding applications.
- Rust: `crates/aion-client` — gRPC client surface with workflow operations and event subscription.
- Python and TypeScript SDK packages under [`../sdks/`](../sdks/) — in progress/hardening.

## Worker SDKs

Use worker SDKs to host activities outside the workflow VM and connect them to the server worker protocol:

- Rust: `crates/aion-worker`
- Python and TypeScript worker packages under [`../sdks/`](../sdks/) — in progress/hardening.

The hello-world quickstart uses the Python worker SDK and registers a `greet` activity.

## Workflow authoring SDK

Gleam workflow code is authored with [`aion_flow`](../gleam/aion_flow/README.md). It provides typed workflow definitions, activity calls, signals, timers, queries, child workflows, codecs, and a test harness.

## Examples

See [`../examples/`](../examples/) for working examples. Start with [`../examples/hello-world/README.md`](../examples/hello-world/README.md) for a complete workflow/package/server/worker run.
