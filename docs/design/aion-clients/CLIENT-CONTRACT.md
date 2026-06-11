# Aion client behavioural contract

This document is the language-neutral behavioural contract for the caller-side Aion SDKs. It defines the observable SDK surface for Rust, Python, TypeScript, and Gleam clients that connect to an `aion-server` deployment and drive workflows as callers.

The contract covers the caller SDK operation catalogue: `connect`, `start`, `signal`, `query`, `cancel`, `list`, `describe`, and `subscribe`. Project shorthand sometimes calls this the seven workflow operations plus `connect`; SDKs must expose and document every entry named here.

## Scope and AW boundary

The Aion client SDKs consume the AW-owned `aion-server` API and `aion-proto` wire types. They SHALL NOT define their own wire formats, endpoints, request fields, response fields, or WebSocket frame shapes. This contract names the authoritative `aion-proto` request and response messages and the `aion-core` domain values carried inside `WireEnvelope`; it does not restate those messages field-by-field as a new protocol.

Current AW alignment gaps are called out explicitly where the desired SDK behaviour depends on server/proto support that is not yet present in the checked-in proto. SDK implementations must not pretend those gaps are new client-owned fields. They should map to the AW-owned request/metadata/cursor once AW lands the protocol support.

## Common transport and domain types

- Unary workflow calls use `aion.WorkflowService` from `crates/aion-proto/proto/workflow.proto`.
- Event streaming uses the AW-owned WebSocket event stream with `aion.SubscriptionRequest` and `aion.StreamedEvent` from `crates/aion-proto/proto/events.proto`.
- Payload-bearing values use `aion.Payload` (`content_type` plus opaque bytes). Each SDK must offer typed JSON-oriented helpers and a raw `Payload` escape hatch for workflow inputs, signal payloads, query arguments where AW supports them, and query results.
- Identifiers are AW/core identifiers rendered per language: `WorkflowId` and `RunId` identify a workflow and a concrete run. `start` returns a `WorkflowHandle` bundling both IDs; handle methods call the same operations described below.
- `ListWorkflowsRequest.filter` carries an `aion-core` `WorkflowFilter` inside `WireEnvelope`. `ListWorkflowsResponse.summaries` and `DescribeWorkflowResponse.summary` carry `aion-core` `WorkflowSummary` values inside `WireEnvelope`. `DescribeWorkflowResponse.history` carries the optional event history as AW/core values inside `WireEnvelope` entries.
- `StreamedEvent.event` carries a serialized `aion-core` `Event`. The event envelope includes the per-workflow monotonic sequence number (`EventEnvelope.seq`) used for subscription resumption.

## Shared error taxonomy

Every SDK must expose exactly the following branchable client failures, rendered idiomatically in its language. SDKs SHALL NOT collapse distinct variants into one opaque error, and SHALL NOT swallow an error by returning success, ending a stream silently, or substituting a default value.

Retryability is "in principle" only: it describes whether a caller may reasonably retry after correcting nothing. It is not an automatic retry policy.

| Variant | Meaning | Maps from server/transport condition | Retryable in principle | Notes |
| --- | --- | --- | --- | --- |
| `NotFound` | The targeted workflow, run, subscription resource, or described entity does not exist or is not visible to the caller. | `WireErrorCode::NotFound`; gRPC `NotFound`; equivalent server not-found status. | No, unless the caller is racing creation or visibility. | Do not use for malformed IDs; those are `InvalidArgument`. |
| `AlreadyExists` | A caller-supplied start idempotency key was reused for a different start request. | AW start idempotency conflict response/status once exposed. | No for the conflicting request. | Retrying the original identical request must return the original handle instead. |
| `QueryFailed` | The workflow's query handler ran and reported an application-level failure. | `WireErrorCode::QueryFailed` (`query_failed`), carried in `QueryResponse.error`; equivalent AW query-handler failure distinct from timeout and invalid query name. | No unless workflow state changes or caller changes the query. | Unknown query names may map to `InvalidArgument` when AW reports them as invalid API input. |
| `QueryTimeout` | The caller's query deadline elapsed before a query result was available. | `WireErrorCode::QueryTimeout`; gRPC `DeadlineExceeded` for query; client-side caller deadline expiry. | Yes, if the caller is willing to wait longer or retry later. | The query is a synchronous round-trip, not fire-and-forget. |
| `Cancelled` | The operation was cancelled by the caller context or the server reports cancellation of the requested operation. | Client cancellation token/context; gRPC `Cancelled`; server cancellation status. | Maybe, if the caller did not intend to cancel or creates a new operation. | A successful `cancel` operation itself does not return this; it records a cancellation request. |
| `Unavailable` | The server or stream is temporarily unreachable. | Transport connect failure, connection drop, DNS/TLS/socket failure, gRPC `Unavailable`, transient WebSocket disconnect before successful resumption, or stream lag/reconnect exhaustion reported as transport unavailability. | Yes. | Subscription streams must surface terminal unavailability as an error item/stream error, not silent end. |
| `Unauthenticated` | The server rejects or cannot validate the caller credential. | Authentication failure status, gRPC `Unauthenticated`, failed bearer/mTLS credential validation, HTTP `401 Unauthorized`. | No until credentials are corrected or refreshed. | Credential failure only. Namespace authorization denial is `NamespaceDenied`, never this variant. |
| `NamespaceDenied` | The caller's credential was accepted, but the caller holds no grant for the requested namespace. | `WireErrorCode::NamespaceDenied`; gRPC `PermissionDenied`; HTTP `403 Forbidden`. | No until the caller's namespace grants or the request's namespace change. | Carries the server's detail message. A workflow that does not exist or is owned by another namespace surfaces as `NotFound` with an identical response either way, so callers cannot probe for cross-namespace existence. SDKs SHALL NOT collapse this variant into `Unauthenticated`, `InvalidArgument`, or `Server`. |
| `InvalidArgument` | The request is syntactically or semantically invalid as API input. | Malformed IDs, malformed payloads, invalid filters, unsupported query/signal names when AW reports invalid input, `WireErrorCode::UnknownQuery`, `WireErrorCode::NotRunning` when the requested operation cannot apply to the run, gRPC `InvalidArgument`. | No until the request is changed. | Do not hide validation errors behind transport failures. |
| `Server` | An unexpected server-side failure with diagnostic detail. | `WireErrorCode::Backend`; gRPC `Internal`/`Unknown`; any unexpected server error carrying detail that does not fit another variant. | Maybe. | Preserve the server's detail/message where available for diagnostics. |

## Hard-case semantics

### Start idempotency

`start` accepts an optional caller-supplied idempotency key. When the same key is retried with an identical start request, the SDK returns the original `WorkflowHandle` containing the original `WorkflowId` and `RunId`; it does not launch a second workflow. When the same key is reused with a different start request, the SDK raises `AlreadyExists`.

The current checked-in `StartWorkflowRequest` does not yet expose an idempotency-key field. This contract defines the SDK behaviour and requires SDKs to map the key to the AW-owned start request field or request metadata once AW exposes it; SDKs must not invent a private proto field.

### Run targeting

`signal`, `query`, `cancel`, and `describe` target the latest run of a workflow by default. If the caller supplies a `RunId`, the operation targets exactly that run. A `WorkflowHandle` returned by `start` supplies both IDs and therefore targets its concrete run unless the caller explicitly constructs a latest-run handle or omits the run in the language's bare-ID constructor.

The current server handlers require `RunId` for these operations. This contract states the observable SDK behaviour and depends on AW/server support for omitted-run latest resolution.

### Query timeout and failure

`query` is a synchronous request/response round-trip bounded by a caller deadline. If the deadline elapses before the server returns a query result, the SDK raises `QueryTimeout`. If the workflow query handler runs and returns an application error, the SDK raises `QueryFailed`. `query` must never be described or implemented as fire-and-forget.

### Cancellation as a cooperative request

`cancel` returns once the server has recorded the cancellation request for the targeted run. A successful return means the request was accepted and recorded; it does not assert that the workflow has already stopped or that a terminal cancelled event has already been observed. Workflow code stops cooperatively according to engine semantics.

### Subscribe resumption

`subscribe` returns a language-native async stream of decoded events. Each delivered event advances the resume cursor to that event's per-workflow `EventEnvelope.seq`. On transient disconnect, the SDK reconnects and resumes from the last delivered sequence number (`resume_from_seq` = last delivered + 1 on the wire), delivering a gap-free and duplicate-free stream to the caller. Terminal failures surface through the stream as a taxonomy error, rather than ending silently.

**Initial attach.** An initial attach with no cursor is a live tail: only events recorded from attach onward are delivered. An explicit starting cursor replays recorded history from that sequence number before splicing into the live stream; `resume_from_seq = 1` replays the full recorded history, and `0` is `InvalidArgument`. SDKs default to the live tail and expose the explicit cursor per idiom (Rust `Client::subscribe_workflow_from`, Python `WorkflowHandle.subscribe(from_seq=...)`); they never invent a cursor the caller did not supply. Conformance harnesses that assert on events from the workflow's beginning attach with `resume_from_seq = 1`.

**Connect failure.** A failed subscription attach is classified exactly like a mid-stream drop. `Unavailable` (transport-level connect failure: DNS, TLS, socket, abnormal close) is retryable — a per-workflow stream re-attaches with its cursor, on the initial attach as well as after delivered events, until the caller ends the stream; a live-only stream re-attaches only while nothing has been delivered, because reconnecting a gapped live tail would silently lose events. Every other taxonomy error (`Unauthenticated`, `NamespaceDenied`, `NotFound`, `InvalidArgument`, `Server`, ...) is terminal immediately.

**Graceful end.** When a subscription ends cleanly (the per-workflow terminal event was delivered, or the server's event stream ended without error), the server finishes the WebSocket close handshake with a normal-closure frame (close code 1000, reason `subscription complete`). SDKs treat close-1000 as "stream complete" and end iteration normally; any other socket end is a transient drop handled by the resumption rules above.

## Operation: connect

| Item | Contract |
| --- | --- |
| Transport mapping | No workflow RPC request/response message. `connect` establishes reusable client transport for `WorkflowService` unary RPCs and the WebSocket event stream. |
| Inputs | Server endpoint/base URL; authentication credential accepted by AW (for example bearer token or mTLS material); TLS configuration (CA roots, client certificate/key where applicable, server-name verification settings); optional namespace/default call options per SDK idiom. |
| Stream endpoint | The WebSocket event-stream listener is configured by an option named `stream_endpoint` (`streamEndpoint`/`with_stream_endpoint` per language idiom) holding the full URL of the server's `/events/stream` route. SDKs whose primary transport is gRPC (Rust, Python) never derive a stream URL from the gRPC endpoint — the listeners are separate addresses, and `subscribe` without a configured stream endpoint raises `InvalidArgument` with a precise message. The TypeScript SDK's HTTP endpoint and the WebSocket stream share one listener, so its stream endpoint defaults to the client endpoint. |
| TLS trust | Caller-supplied CA material (`ca_certificate_pem`) is trusted by BOTH client transports: the unary RPC channel and the `wss://` WebSocket event stream. A deployment behind a private CA streams events over the same trust configuration it uses for unary calls. Unparseable or empty CA material is `InvalidArgument` at connector construction. |
| Output | A reusable SDK client value capable of invoking `start`, `signal`, `query`, `cancel`, `list`, `describe`, and `subscribe`. Connection setup may be eager or lazy per language, but observable authentication/TLS failures must map to the taxonomy. |
| Errors | `Unauthenticated`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | SDKs must not invent an auth scheme. Credentials are carried exactly as AW/server requires. TLS failures are transport failures unless AW provides a more specific status. |

## Operation: start

| Item | Contract |
| --- | --- |
| Transport mapping | `WorkflowService.StartWorkflow(StartWorkflowRequest) -> StartWorkflowResponse`. Idempotency key maps to AW-owned start request metadata/field once available. |
| Inputs | Namespace/default namespace; workflow type name; workflow input as typed value encoded to `Payload` or raw `Payload`; optional caller-supplied idempotency key; optional caller deadline/cancellation context. |
| Output | `WorkflowHandle` containing the returned `WorkflowId` and `RunId`. The handle exposes per-workflow `signal`, `query`, `cancel`, `describe`, and `subscribe` methods. |
| Errors | `AlreadyExists`, `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | Retried identical start with the same idempotency key returns the original handle. Conflicting reuse raises `AlreadyExists`. Without a supplied key, normal server start semantics apply and callers must not assume retry safety. |

## Operation: signal

| Item | Contract |
| --- | --- |
| Transport mapping | `WorkflowService.Signal(SignalRequest) -> SignalResponse`. |
| Inputs | Namespace/default namespace; `WorkflowId`; optional `RunId` (latest run by default); signal name; signal payload as typed value encoded to `Payload` or raw `Payload`; optional caller deadline/cancellation context. |
| Output | Acknowledgement that the server accepted the signal for delivery to the targeted run. The operation does not return a workflow result. |
| Errors | `NotFound`, `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | `signal` may be fire-and-forget at the workflow API level after server acceptance, but errors before acceptance must be surfaced. Latest-run targeting is the SDK default when no `RunId` is supplied. |

## Operation: query

| Item | Contract |
| --- | --- |
| Transport mapping | `WorkflowService.Query(QueryRequest) -> QueryResponse`. Query arguments, if AW adds them, use `Payload`; the checked-in `QueryRequest` currently names the query only. |
| Inputs | Namespace/default namespace; `WorkflowId`; optional `RunId` (latest run by default); query name; optional query arguments when supported by AW, as typed payload or raw `Payload`; caller deadline; optional cancellation context. |
| Output | Query result as typed decoded value or raw `Payload`, according to the caller's chosen surface. |
| Errors | `NotFound`, `QueryFailed`, `QueryTimeout`, `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | `query` is synchronous and deadline-bounded. Deadline expiry maps to `QueryTimeout`; workflow handler errors map to `QueryFailed`. It is not fire-and-forget. |

## Operation: cancel

| Item | Contract |
| --- | --- |
| Transport mapping | `WorkflowService.Cancel(CancelRequest) -> CancelResponse`. |
| Inputs | Namespace/default namespace; `WorkflowId`; optional `RunId` (latest run by default); optional reason string; optional caller deadline/cancellation context. |
| Output | Acknowledgement that the server recorded the cooperative cancellation request. |
| Errors | `NotFound`, `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | Success does not mean the workflow has already stopped. The caller observes eventual terminal status through `describe`, `list`, or `subscribe`. |

## Operation: list

| Item | Contract |
| --- | --- |
| Transport mapping | `WorkflowService.ListWorkflows(ListWorkflowsRequest) -> ListWorkflowsResponse`; filter is an `aion-core` `WorkflowFilter` in `WireEnvelope`; summaries are `aion-core` `WorkflowSummary` values in `WireEnvelope`. |
| Inputs | Namespace/default namespace; filter dimensions: workflow type, workflow status, started-after time, started-before time, and parent workflow; pagination controls when AW exposes them; optional caller deadline/cancellation context. |
| Output | A page of workflow summaries plus pagination continuation information when AW exposes it. Each summary contains the AW/core summary projection, including workflow identity, type, status, start/end timestamps, and parent where available. |
| Errors | `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | The current checked-in `ListWorkflowsRequest` has `namespace` and `WireEnvelope filter`, and `ListWorkflowsResponse` has repeated summaries but no pagination fields. Pagination is required SDK/server behaviour for the public clients once AW exposes token/limit fields; SDKs must not invent private pagination fields meanwhile. |

## Operation: describe

| Item | Contract |
| --- | --- |
| Transport mapping | `WorkflowService.DescribeWorkflow(DescribeWorkflowRequest) -> DescribeWorkflowResponse`; summary and optional history are carried in `WireEnvelope` entries. |
| Inputs | Namespace/default namespace; `WorkflowId`; optional `RunId` (latest run by default); include-history flag; optional caller deadline/cancellation context. |
| Output | Workflow description containing the `WorkflowSummary` projection with current status, plus optional event history when requested and authorised. |
| Errors | `NotFound`, `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | Latest-run targeting is the SDK default when no `RunId` is supplied. History is optional and may be omitted by request or server policy; absence of requested history due to an error must be surfaced rather than silently treated as an empty history. |

## Operation: subscribe

| Item | Contract |
| --- | --- |
| Transport mapping | WebSocket event stream using `SubscriptionRequest` for subscription intent and `StreamedEvent` frames for delivered events. There is no unary `WorkflowService` subscribe RPC in the checked-in proto. |
| Inputs | Namespace/default namespace; subscription selector (`PerWorkflowSubscription`, `FilteredSubscription`, or `FirehoseSubscription` as AW exposes and authorises); typed event decoder or raw event payload surface; optional explicit starting cursor (`from_seq`); optional caller cancellation context; resumption cursor managed by the SDK from last delivered `EventEnvelope.seq`. |
| Initial attach | Absent cursor = live tail (events recorded from attach onward). Explicit cursor = history replay from that sequence number, then live splice; `resume_from_seq = 1` replays the full recorded history; `0` is `InvalidArgument`. See "Subscribe resumption" hard-case semantics. |
| Connect failure | An attach failure is classified exactly like a mid-stream drop: `Unavailable` is retryable (per-workflow streams always re-attach with their cursor; live-only streams re-attach only while nothing has been delivered); every other taxonomy error is terminal immediately. |
| Graceful end | The server finishes a graceful subscription end with WebSocket close-1000 (reason `subscription complete`); SDKs end iteration normally on close-1000 and treat any other socket end as a transient drop. |
| Output | A language-native async stream/iterator of decoded events. Normal caller cancellation ends the stream according to language idiom; terminal server/transport failures are emitted/surfaced as taxonomy errors. |
| Errors | `NotFound`, `Unauthenticated`, `NamespaceDenied`, `Unavailable`, `InvalidArgument`, `Server`, `Cancelled`. |
| Notes | On transient disconnect, the SDK resumes from the last delivered per-workflow sequence number and must not deliver gaps or duplicates. If resumption cannot be completed, the stream surfaces `Unavailable` or the more specific taxonomy variant supplied by AW. |
