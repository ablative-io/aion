# Aion API overview

Aion exposes the engine through the server (`aion server --config aion.toml`, built on the `aion-server` library crate) plus language-specific client and worker SDKs.

## Implementation status

- **Implemented:** engine crash/restart recovery for active workflow histories, durable timer recovery, HTTP/JSON workflow operations (including live workflow queries), gRPC worker/client APIs, and the WebSocket event-stream route described below.
- **In progress:** dashboard UX and cross-language SDK/conformance hardening. Prefer the `aion` CLI, HTTP, gRPC, or the Rust client when you need the most exercised surfaces.

## Authentication and caller metadata

HTTP and WebSocket routes use the same caller extraction path, and the gRPC API reads the same names as request metadata:

- In local development with auth disabled (`[auth] enabled = false`, the default), send `x-aion-subject` to identify the caller. If omitted, the server identifies the caller as `anonymous`.
- Send `x-aion-namespaces` as a comma-separated list of namespace grants. The default namespace mode is `SharedEngine`, in which a request is authorized only when its `namespace` value appears in this list — so the dev curl examples below must include the header. Deployments configured with `[namespace] mode = SingleTenant` instead authorize exactly the configured namespace for every caller, without consulting this header.
- **Trust model:** with auth disabled, both headers are taken at face value. Namespace grants are purely caller-asserted — any caller can self-grant any namespace simply by listing it in `x-aion-namespaces`. This is a development convenience, not tenant isolation. Real tenant isolation requires `[auth] enabled = true` with a JWKS endpoint (`[auth] jwks_url`, refreshed every `jwks_refresh_seconds` seconds): the server then validates the `Authorization: Bearer <token>` header against the JWKS keys and derives the caller identity from validated token claims. (A server binary compiled without the `auth` feature falls back, when `auth.enabled` is true, to comparing the bearer token against the configured `jwks_url` value as a static shared secret — also development-only.) WebSocket upgrades use the same headers as HTTP requests.
- The `aion` CLI follows the same model over gRPC metadata: it asserts exactly its `--namespace` flag value (default `default`) as its single namespace grant and sends `--subject` (default `cli-user`) as the caller.

Most examples use the default `default` namespace.

## HTTP/JSON routes

The public HTTP router currently mounts these routes:

| Method | Path | Request shape | Response shape | Status |
|---|---|---|---|---|
| `GET` | `/workflows` | Query parameters: `namespace` (required), optional `workflow_type`, `status`, `started_after`, `started_before`, `closed_after`, `closed_before`, `search_attributes`, `limit`, `offset`. | JSON array of visibility `WorkflowSummary` values. | Implemented |
| `GET` | `/workflows/count` | Same query parameters as `/workflows`. | `{ "count": <number> }` | Implemented |
| `POST` | `/workflows/start` | JSON body with `namespace`, `workflow_type`, optional `input`. `input` may be an ordinary JSON value or a `ProtoPayload` envelope with `content_type` and byte-array `bytes`. | `ProtoStartWorkflowResponse` (`workflow_id`, `run_id`). | Implemented |
| `POST` | `/workflows/signal` | `ProtoSignalRequest`: `namespace`, `workflow_id`, optional `run_id`, `signal_name`, optional `payload`. | Empty acknowledgement object. | Implemented |
| `POST` | `/workflows/query` | `ProtoQueryRequest`: `namespace`, `workflow_id`, optional `run_id`, `query_name`. | `ProtoQueryResponse` with result or typed error (see [Query semantics](#query-semantics)). | Implemented |
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

(`aion start` flattens this to `{"workflow_id":"<workflow-id>","run_id":"<run-id>"}` in its own output.)

For non-JSON binary payloads, use the envelope form with `bytes` as a JSON array of integers. For the hello-world tutorial, the `aion` CLI is the recommended path because it handles payload encoding for you.

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

`POST /workflows/list` returns the raw wire form — `{"summaries":[...]}` with serde-encoded envelopes whose payload `bytes` are JSON byte arrays. Prefer `GET /workflows` or `aion list` for human-readable output.

### Query semantics

`POST /workflows/query` (and the gRPC `WorkflowService.Query`) is a synchronous, deadline-bounded round trip into the live workflow process: the engine delivers the query to the workflow's registered handler and waits for its reply up to the server-configured deadline. The server **requires** `runtime.query_timeout_ms` in its configuration (env: `AION_RUNTIME_QUERY_TIMEOUT_MS`) — startup fails without it, because the query route is always mounted.

Query-semantic failures ride the `QueryResponse.error` oneof on a successful transport call (HTTP 200 / gRPC OK):

| `error.code` | Meaning | Typical cause |
|---|---|---|
| `unknown_query` | The workflow has no registered handler for the requested query name. | Wrong query name, or the workflow has not reached its registration code yet. |
| `query_timeout` | No handler reply arrived before the configured `runtime.query_timeout_ms`. | The workflow is busy between yield points, or parked in a non-yielding (blocking) call. |
| `not_running` | The workflow cannot answer a live query: it is terminal, suspended, or ended before answering (`error_type` distinguishes `QueryNotRunning` from `QueryReplyDropped`). | Querying a completed/failed/cancelled workflow, or racing its completion. |
| `query_failed` | The workflow's query handler ran and reported an application-level failure. | The handler raised or replied with an error. |

Namespace denial, unknown workflow ids, and backend faults remain transport-level errors with the usual status codes (403/404/500), exactly as for every other operation — a query against another tenant's workflow is byte-identical to one against a workflow that never existed.

## Operator deploy API

> **Operator surface, not a caller operation.** Deploy is deliberately outside the caller SDK contract (`CLIENT-CONTRACT.md`): client SDKs SHALL NOT expose it. The wire surface is a separate gRPC `DeployService` (`crates/aion-proto/proto/deploy.proto`) plus the `/deploy/*` HTTP routes below, driven by `aion deploy`/`versions`/`route`/`unload`.

The deploy surface is **dark by default**. It mounts only when commissioned in server config:

```toml
[deploy]
enabled = true                 # default false: routes not mounted (HTTP 404, gRPC Unimplemented)
max_archive_bytes = 16777216   # REQUIRED when enabled; no default — size for your packages
max_inflated_bytes = 67108864  # REQUIRED when enabled; no default — must be >= max_archive_bytes
```

`max_archive_bytes` (env override `AION_DEPLOY_MAX_ARCHIVE_BYTES`; `AION_DEPLOY_ENABLED` gates the mount) is enforced while reading the upload on both transports; oversized archives are refused with `413` / `InvalidArgument` naming the key.

`max_inflated_bytes` (env override `AION_DEPLOY_MAX_INFLATED_BYTES`) caps the total decompressed size of an uploaded archive's contents: `max_archive_bytes` bounds only the compressed upload, and a DEFLATE bomb under that cap can inflate ~1000:1. Extraction charges every inflated byte (manifest included) against this budget and refuses with the same `413` / `InvalidArgument` class, naming the key. Startup validation rejects a value below `max_archive_bytes` — an inflate ceiling under the upload ceiling would refuse archives the upload ceiling admits even stored uncompressed.

**Authorization** is a deployment-wide `deploy` grant, decided before any handler logic runs:

- JWT path (`[auth] enabled = true` with the `auth` feature): a boolean `deploy` claim in the same bearer token. Absent claim = no grant; existing data-operation tokens keep working unchanged.
- Development paths: the `x-aion-deploy: true` header/metadata entry, the dev analog of the claim (the `aion` CLI sends it automatically). The dev-token fallback checks the shared secret first, then the header.
- Denials are `403` / `PermissionDenied` with the dedicated `deploy_denied` wire code, and the message names the knob that carries the grant (header vs token claim).

The grant is engine-global on purpose: **a package load is engine-global**. Loading registers code into the shared VM and re-points routing for a workflow *type* that is startable from every namespace — there is no namespace field anywhere on the deploy surface, and no per-namespace isolation is implied.

| Method | Path | Body | Behavior |
|---|---|---|---|
| `POST` | `/deploy/packages` | raw `application/octet-stream` `.aion` archive | Load + atomic route flip. Response: `workflow_type`, `content_hash`, `deployed_entry_module`, `entry_function`, `freshly_loaded`, `route_changed`. Idempotent: re-POSTing a resident archive succeeds with `freshly_loaded = false`; `route_changed` reports whether routing moved (re-deploy after rollback). A deploy pipeline may retry blindly. |
| `GET` | `/deploy/versions` | — | The deploy read model: every loaded version with `route_active`, sorted `(type, loaded_at)`. Keeps serving during drain. |
| `POST` | `/deploy/route` | JSON `{"workflow_type", "content_hash"}` | Atomic, idempotent re-point (rollback / roll-forward) to an already-loaded version. |
| `POST` | `/deploy/unload` | JSON `{"workflow_type", "content_hash"}` | Unload a non-routed version after the engine verifies nothing pins it. |

Failure taxonomy (both transports; messages pass the engine's refusal prose through):

| Condition | Wire code | HTTP | gRPC |
|---|---|---|---|
| No deploy grant | `deploy_denied` | 403 | `PermissionDenied` |
| Version route-active or pinned (live run, in-flight start, recoverable run, recorded child) | `version_pinned` | 409 | `FailedPrecondition` |
| Malformed archive / hash mismatch / collision / same-hash-different-manifest | `invalid_input` | 400 | `InvalidArgument` |
| Unknown `(workflow_type, content_hash)` | `not_found` | 404 | `NotFound` |
| Archive exceeds `deploy.max_archive_bytes` | `invalid_input` (names the key) | 413 | `InvalidArgument` |
| Archive contents inflate past `deploy.max_inflated_bytes` | `invalid_input` (names the key) | 413 | `InvalidArgument` |
| Draining / shutting down (mutations only) | `backend` with explicit message | 503 | `Unavailable` |
| Deploy surface disabled | — (not mounted) | 404 | `Unimplemented` |

Every mutation emits one structured audit log line (`operation`, `subject`, `grant_source`, `transport`, `workflow_type`, `content_hash`, `outcome`, and `freshly_loaded`/`route_changed` for loads); denials log at `warn`. Metrics: `aion_deploy_operations_total{operation, outcome}`, `aion_deploy_denied_total{transport}`, and the `aion_loaded_workflow_versions{workflow_type}` gauge.

CLI (`--token` overrides the `AION_TOKEN` environment variable; without either, the development headers apply):

```bash
aion --endpoint 127.0.0.1:50051 --token "$TOKEN" deploy dist/order.aion
aion versions --workflow-type order
aion route order <content-hash>     # rollback / roll-forward
aion unload order <content-hash>
```

See [docs/packaging.md](packaging.md) for building the `.aion` archives this API consumes.

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

### Filtered-subscription selector semantics

`filtered` selectors are enforced server-side, before any frame is encoded:

- **`workflow_type`** delivers only events of workflows whose recorded type equals the selector. A workflow's type is its most recent recorded `WorkflowStarted` type (continue-as-new chains record each run's type on that run's `WorkflowStarted`, so the type follows the chain). The type is resolved from durable history via the same per-workflow read that verifies namespace ownership; a workflow whose history records no started run never matches a `workflow_type` selector — absence is not a wildcard.
- **`status`** matches per event kind: each terminal lifecycle event matches exactly its projected status (`WorkflowCompleted` → `Completed`, `WorkflowFailed` → `Failed`, `WorkflowCancelled` → `Cancelled`, `WorkflowTimedOut` → `TimedOut`, `WorkflowContinuedAsNew` → `ContinuedAsNew`), and every non-terminal event — including `WorkflowStarted` — matches `Running`. So `status: "Completed"` is a stream of completion events, and `status: "Running"` is the stream of in-flight activity (activities, timers, signals, starts) without terminal events.
- When both selectors are present they **AND** together: an event is delivered only when its workflow has the selected type *and* the event kind matches the selected status.

Filtered streams remain live-only (see resumption below): selectors choose which live events are delivered, they do not replay history.

The server requires `websocket.event_broadcast_capacity` in its configuration (env: `AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY`) — startup fails without it. It sizes the engine-global live-event broadcast channel; lag is filter-blind, so size it for global event volume across all namespaces.

### Subscription resumption (`resume_from_seq`)

Per-workflow subscriptions accept an optional `resume_from_seq` cursor — the **first** per-workflow sequence number the caller wants (SDKs send `last delivered seq + 1`):

```json
{"per_workflow":{"namespace":"default","workflow_id":{"uuid":"<workflow-id>"},"resume_from_seq":7}}
```

The server replays recorded history events with `seq >= resume_from_seq` in order, then splices into the live stream with no gaps and no duplicates. Sequence numbers start at 1; `resume_from_seq: 1` replays the full history; `head + 1` (one past the recorded head) replays nothing and tails live events only. Absent cursor = live tail from now (previous behaviour). A replayed terminal or `ContinuedAsNew` event closes the stream at that run boundary, exactly like a live one; callers walk continue-as-new chains by resubscribing with the next cursor.

Filtered and firehose subscriptions are **live-only** and accept no cursor: `seq` is per-workflow, so a cross-workflow cursor is structurally unrepresentable. SDKs must surface a non-resumable disconnect on those streams instead of silently reattaching with a gap.

Cursor error semantics (the namespace guard verdict always comes first — an unauthorized or foreign workflow probe yields the same `not_found` regardless of cursor, never a cursor error):

| Condition | Error frame |
|---|---|
| Workflow unknown or foreign-owned | `not_found` (identical for both; cursor never inspected) |
| `resume_from_seq: 0` | `invalid_input` (`resume_from_seq must be >= 1`) |
| `resume_from_seq > head + 1` | `invalid_input`, `error_type: "ResumeCursorAheadOfHistory"` |
| Cursor older than earliest retained event (compaction; cannot occur yet) | reserved: `not_found`, `error_type: "HistoryCompacted"` — restart without a cursor |

All stream errors are one terminal `{"error": {"code": ..., "message": ..., "error_type": ...}}` text frame followed by a close frame; any event frames already queued for the connection are delivered before the terminal error frame. A consumer that falls behind the live stream receives a terminal `lagged` error frame; reconnect with `resume_from_seq = last delivered seq + 1` to continue gap-free. Per-workflow streams additionally carry a server-side contiguity tripwire: if a delivered-sequence gap or regression is ever observed (unreachable under normal operation), the stream ends with a terminal `lagged` error frame carrying `error_type: "SequenceContiguityViolation"` instead of silently delivering a gapped stream — recover the same way, by reconnecting with `resume_from_seq = last delivered seq + 1`.

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

The worker protocol is a single bidirectional gRPC stream whose authoritative contract lives in [`crates/aion-proto/proto/worker.proto`](../crates/aion-proto/proto/worker.proto): the server acknowledges registration with a `RegisterAck` frame (carrying the assigned worker id, the authorized namespace, and the heartbeat window) before dispatching any task, tasks carry a one-based delivery `attempt`, every consumed activity result is acknowledged with a `ResultAck` (only that ack clears a worker's re-report backlog), and a server `DrainRequest` tells workers to finish in-flight work and reconnect without consuming their reconnect drop budget.

The hello-world quickstart uses the Python worker SDK and registers a `greet` activity.

## Workflow authoring SDK

Gleam workflow code is authored with [`aion_flow`](../gleam/aion_flow/README.md). It provides typed workflow definitions, activity calls, signals, timers, queries, child workflows, codecs, and a test harness.

## Examples

See [`../examples/`](../examples/) for working examples. Start with [`../examples/hello-world/README.md`](../examples/hello-world/README.md) for a complete workflow/package/server/worker run.
