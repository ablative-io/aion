# Errors reference

Aion uses one stable error taxonomy across HTTP, gRPC, and the CLI. Every
API failure carries a wire **code** (a stable string), a human message, and
— where the server can name a fix — a hint.

## How the CLI renders errors

```
error[<code>]: <context>: <server detail message>
  server error type: <type>
  hint: <actionable advice>
```

For example, querying a completed workflow:

```
error[not_running]: failed to query workflow: workflow <id> is not running
```

The `hint:` line appears only when the error class has actionable guidance.
Local (non-API) failures print as a plain `error: <cause chain>`. Exit
codes: `1` for errors, `2` for CLI usage mistakes.

## The error codes

| Code | HTTP | gRPC | Meaning |
|---|---|---|---|
| `not_found` | 404 | `NotFound` | Workflow, run, or deploy version unknown. Also returned for another tenant's workflow — a foreign probe is byte-identical to a miss. |
| `namespace_denied` | 403 | `PermissionDenied` | Caller not authorized for the requested namespace. |
| `invalid_input` | 400/413 | `InvalidArgument` | Malformed body, identifier, or archive; oversized deploy uploads (413) name the violated config key (`deploy.max_archive_bytes` / `deploy.max_inflated_bytes`). |
| `not_running` | 409 | `FailedPrecondition` | The operation needs a live workflow but the run is terminal (completed/failed/cancelled/timed out) or otherwise unable to answer. |
| `unknown_query` | 400 | `InvalidArgument` | No query handler registered under that name (wrong name, or the workflow has not reached its registration code yet). |
| `query_timeout` | 408 | `DeadlineExceeded` | No handler reply within the server's `runtime.query_timeout_ms` — the workflow is busy between yield points. |
| `query_failed` | 500 | `Internal` | The query handler ran and reported an application-level failure. |
| `sequence_conflict` | 409 | `FailedPrecondition` | A durable write lost an optimistic sequence race. Indicates a double-writer bug — report it. |
| `lagged` | 429 | `ResourceExhausted` | An event-stream consumer fell behind the bounded channel and was disconnected. Reconnect; per-workflow streams resume with `resume_from_seq`. |
| `deploy_denied` | 403 | `PermissionDenied` | No deploy grant. Supply the `deploy` token claim, or in development the `x-aion-deploy: true` header (the CLI sends it automatically). |
| `version_pinned` | 409 | `FailedPrecondition` | Deploy unload/route refused: the version is route-active or pinned by a live run, a recoverable run, or an in-flight start. The message names the holder. |
| `unauthenticated` / `unavailable` | 401/503 | `Unauthenticated`/`Unavailable` | Missing/invalid credentials; server draining or unreachable. |
| `backend` | 500/503 | `Internal`/`Unavailable` | Storage, serialization, or internal runtime failure; also deploy mutations refused during drain (503, with an explicit message). |

## Terminal-state behavior (what "done" means to each operation)

Once a run reaches a terminal status — `Completed`, `Failed`, `Cancelled`,
`TimedOut`, or `ContinuedAsNew` — it is history, not a live process:

| Operation on a terminal run | Result |
|---|---|
| `aion query <id> <name>` | `error[not_running]` — **expected**, not a bug. Queries are live reads answered by the running workflow; a terminal run answers nothing. Read its recorded state with `describe`. |
| `aion describe <id>` | Works — describe reads recorded history and works forever. |
| `aion list` | The run appears with its terminal status. |
| `aion signal <id> <name>` | Refused — a terminal run rejects signals; nothing is appended after the terminal event. |
| `aion cancel <id>` | No effect — the run already has its terminal event, and status is a projection of history. |
| Event stream (`/events/stream`) | A replayed or live terminal event closes a per-workflow stream at that run boundary. |

`not_running` can also appear on a race: the workflow completed between
your request and its delivery (the server's `error_type` distinguishes
`QueryNotRunning` from `QueryReplyDropped`).

## Quick diagnosis table

| Symptom | Likely cause | Fix |
|---|---|---|
| `error[not_running]` on query | Run already terminal | `aion describe <id>` instead |
| `error[unknown_query]` | Handler not registered yet, or name typo | Check the name; the workflow registers handlers as it reaches them |
| `error[query_timeout]` | Workflow busy between yield points | Wait; raise `runtime.query_timeout_ms` if legitimate |
| `error[deploy_denied]` | No deploy grant | Token `deploy` claim, or dev header (automatic from the CLI) |
| `error[version_pinned]` on unload | Version routed or in use | `aion route` away first; wait for pinning runs to finish |
| `error[not_found]` on deploy route/unload | `(type, hash)` not loaded | `aion versions` to see what is actually loaded |
| 404 on `/deploy/*` / gRPC `Unimplemented` | Deploy surface dark | Set `[deploy] enabled = true` + both size ceilings |
| Workflow stuck after start | No worker serving the activity type | Start a worker registering that type on the right endpoint/queue |
| Server refuses to start, names a config key | Required key missing | Set the named key (or its `AION_*` env override) |
