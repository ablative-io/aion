# Aion API overview

Aion exposes the engine through the standalone `aion-server` plus language-specific client and worker SDKs.

## Implementation status

- **Implemented:** engine crash/restart recovery for active workflow histories, durable timer recovery, HTTP/JSON workflow operations, gRPC worker/client APIs, and the WebSocket event-stream route described below.
- **In progress:** dashboard UX and cross-language SDK/conformance hardening. Prefer `aion-cli`, HTTP, gRPC, or the Rust client when you need the most exercised surfaces.

## Authentication and caller metadata

HTTP and WebSocket routes use the same caller extraction path, and the gRPC API reads the same names as request metadata:

- In local development with auth disabled (`[auth] enabled = false`, the default), send `x-aion-subject` to identify the caller. If omitted, the server identifies the caller as `anonymous`.
- Send `x-aion-namespaces` as a comma-separated list of namespace grants. The default namespace mode is `SharedEngine`, in which a request is authorized only when its `namespace` value appears in this list — so the dev curl examples below must include the header. Deployments configured with `[namespace] mode = SingleTenant` instead authorize exactly the configured namespace for every caller, without consulting this header.
- **Trust model:** with auth disabled, both headers are taken at face value. Namespace grants are purely caller-asserted — any caller can self-grant any namespace simply by listing it in `x-aion-namespaces`. This is a development convenience, not tenant isolation. Real tenant isolation requires `[auth] enabled = true` with a JWKS endpoint (`[auth] jwks_url`, refreshed every `jwks_refresh_seconds` seconds): the server then validates the `Authorization: Bearer <token>` header against the JWKS keys and derives the caller identity from validated token claims. (A server binary compiled without the `auth` feature falls back, when `auth.enabled` is true, to comparing the bearer token against the configured `jwks_url` value as a static shared secret — also development-only.) WebSocket upgrades use the same headers as HTTP requests.
- `aion-cli` follows the same model over gRPC metadata: it asserts exactly its `--namespace` flag value (default `default`) as its single namespace grant and sends `--subject` (default `cli-user`) as the caller.

Most examples use the default `default` namespace.

## HTTP/JSON routes

The public HTTP router currently mounts these routes:

| Method | Path | Request shape | Response shape | Status |
|---|---|---|---|---|
| `GET` | `/workflows` | Query parameters: `namespace` (required), optional `workflow_type`, `status`, `started_after`, `started_before`, `closed_after`, `closed_before`, `search_attributes`, `limit`, `offset`. | JSON array of visibility `WorkflowSummary` values. | Implemented |
| `GET` | `/workflows/count` | Same query parameters as `/workflows`. | `{ "count": <number> }` | Implemented |
| `POST` | `/workflows/start` | JSON body with `namespace`, `workflow_type`, optional `input`. `input` may be an ordinary JSON value or a `ProtoPayload` envelope with `content_type` and byte-array `bytes`. | `ProtoStartWorkflowResponse` (`workflow_id`, `run_id`). | Implemented |
| `POST` | `/workflows/signal` | `ProtoSignalRequest`: `namespace`, `workflow_id`, optional `run_id`, `signal_name`, optional `payload`. | Empty acknowledgement object. | Implemented |
| `POST` | `/workflows/query` | `ProtoQueryRequest`: `namespace`, `workflow_id`, optional `run_id`, `query_name`. | `ProtoQueryResponse` with result or typed error. | Wire surface only — engine-side handler execution is not yet implemented; every query currently fails with a configuration error |
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
  -H 'x-aion-namespaces: default' \
  -d '{"namespace":"default","workflow_type":"hello_world","input":{"name":"Ada"}}'
```

A successful start returns the assigned identifiers as nested objects:

```json
{"workflow_id":{"uuid":"<workflow-id>"},"run_id":{"uuid":"<run-id>"}}
```

(`aion-cli start` flattens this to `{"workflow_id":"<workflow-id>","run_id":"<run-id>"}` in its own output.)

For non-JSON binary payloads, use the envelope form with `bytes` as a JSON array of integers. For the hello-world tutorial, `aion-cli` is the recommended path because it handles payload encoding for you.

### List and describe response shapes

`GET /workflows` returns a plain JSON array of visibility summaries. Each element carries a top-level `status`:

```json
[
  {
    "workflow_id": "<workflow-id>",
    "run_id": "<run-id>",
    "workflow_type": "hello_world",
    "status": "Completed",
    "start_time": "2026-01-01T00:00:00Z",
    "close_time": "2026-01-01T00:00:01Z",
    "search_attributes": { "aion.namespace": { "type": "String", "data": "default" } }
  }
]
```

`POST /workflows/describe` does **not** return a top-level `status`. It returns a `summary` envelope plus a `history` array of event envelopes; the workflow's projected status lives inside the decoded summary at `summary.payload.data.status`:

```json
{
  "summary": {
    "namespace": "default",
    "request_id": null,
    "payload": {
      "content_type": "application/json",
      "data": {
        "workflow_id": "<workflow-id>",
        "workflow_type": "hello_world",
        "status": "Completed",
        "started_at": "2026-01-01T00:00:00Z",
        "ended_at": "2026-01-01T00:00:01Z",
        "parent": null
      }
    }
  },
  "history": [ { "namespace": "default", "request_id": null, "payload": { "content_type": "application/json", "data": { "...": "event" } } } ]
}
```

`POST /workflows/list` returns the raw wire form — `{"summaries":[...]}` with serde-encoded envelopes whose payload `bytes` are JSON byte arrays. Prefer `GET /workflows` or `aion-cli list` for human-readable output.

## WebSocket event streaming

Connect to `ws://<server>/events/stream` and then send one JSON text or binary message that selects the subscription. The server ignores ping/pong frames while waiting, but closes the stream with an input error if the subscription is missing or malformed.

Supported subscription messages:

```json
{"per_workflow":{"namespace":"default","workflow_id":{"uuid":"<workflow-id>"}}}
```

```json
{"filtered":{"namespace":"default","workflow_type":"hello_world","status":"Completed"}}
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
