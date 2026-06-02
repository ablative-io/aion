# Aion-Workers — Checklist

## Rust SDK — Protocol Session and Configuration

- [ ] **C1** — aion-worker is a workspace member with unsafe_code = deny and a lib.rs containing only declarations and re-exports; it depends on aion-proto and aion-core but not on aion, aion-server, or aion-store.
- [ ] **C2** — WorkerConfig carries the engine endpoint, task-queue name, worker identity, max concurrency, and a TLS/credentials passthrough; no field has a hardcoded default concurrency or tunable.
- [ ] **C3** — WorkerError is a closed thiserror enum covering connection, handshake, registration, decode/encode, and transport failures, and is Send + Sync.
- [ ] **C4** — A WorkerSession trait abstracts the transport (connect+handshake, register activity types, receive-task stream, report result/failure, send heartbeat) so execution machinery never touches generated gRPC stubs directly.
- [ ] **C5** — A gRPC-backed WorkerSession implementation connects to the engine's worker endpoint, performs the handshake declaring task queue and identity, and registers a set of activity-type names per the aion-proto contract.
- [ ] **C6** — Registering an activity-type name for which no handler exists is rejected by the SDK before serving begins, not at dispatch time.

## Rust SDK — Task Loop and Dispatch

- [ ] **C7** — The worker receives tasks via the server-streamed receive call; each task exposes activity type, ActivityId, attempt number, and the input Payload with its content-type tag.
- [ ] **C8** — The worker loop serves up to the configured max-concurrency activities at once and applies backpressure on the receive stream when the pool is full (no unbounded spawning).
- [ ] **C9** — Dispatch decodes the input Payload to the handler's input type using the content-type tag (JSON baseline) and encodes the handler's output back to a Payload without changing the content type.
- [ ] **C10** — On success the worker reports completion for the task's ActivityId carrying the encoded output Payload; on failure it reports a failure carrying the error and its retryable/terminal classification.
- [ ] **C11** — ActivityFailure carries an explicit retryable-vs-terminal classification (not a string or bool); a handler returns Result<Output, ActivityFailure>.

## Rust SDK — Context, Heartbeat, Cancellation, Reconnect

- [ ] **C12** — ActivityContext exposes the ActivityId, attempt number, a heartbeat method, and cancellation observation (is_cancelled and an awaitable cancelled signal).
- [ ] **C13** — ctx.heartbeat() sends a heartbeat frame carrying an optional opaque Payload of progress detail; the SDK never heartbeats on the handler's behalf.
- [ ] **C14** — When the engine signals cancellation for an in-flight activity, the SDK flips the ActivityContext cancellation flag; it never forcibly terminates the handler's task.
- [ ] **C15** — On session drop the SDK reconnects with bounded exponential backoff and re-registers its activity types before serving new tasks.
- [ ] **C16** — The SDK tracks results computed locally but not yet acknowledged, and on reconnect re-reports those un-acked results before serving any new task.

## Rust SDK — Typed Activities and Worker Surface

- [ ] **C17** — An Activity is definable as an async handler taking a typed input (Serialize/DeserializeOwned) and an &ActivityContext, returning Result<Output, ActivityFailure>, and registered by activity-type name.
- [ ] **C18** — A Worker builder accepts a WorkerConfig and registered activities; Worker::run() connects, serves, and returns on a graceful shutdown signal after draining in-flight activities.

## Python SDK — Protocol and Loop

- [ ] **C19** — aion-worker-python is a PyPI-packaged project (pyproject.toml) depending on grpcio and protobuf, with __init__.py exposing only re-exports.
- [ ] **C20** — A session module performs connect+handshake (task queue + identity) and activity-type registration over generated gRPC stubs, behind a session abstraction the loop depends on.
- [ ] **C21** — An asyncio worker loop receives tasks from the server stream, dispatches up to the operator-configured concurrency, and reports completion or failure; backpressure is applied when the pool is full.
- [ ] **C22** — The Python SDK reconnects with bounded backoff, re-registers, and re-reports un-acked results before serving new tasks.

## Python SDK — Typed Activities, Context, Errors

- [ ] **C23** — An @activity(name=...) decorator registers an async (or pool-run sync) function as an activity; type hints on its arguments and return drive Payload encode/decode (JSON baseline).
- [ ] **C24** — A context object passed to the handler exposes heartbeat(detail) and cancellation observation (is_cancelled() and an awaitable cancelled()).
- [ ] **C25** — RetryableError and TerminalError base classes classify failures; an unclassified exception escaping a handler defaults to retryable and is logged as unclassified (never silent).
- [ ] **C26** — A Worker object registers decorated activities and await worker.run() connects, serves, and shuts down gracefully draining in-flight activities; a thread/process-pool escape hatch handles blocking/CPU-bound handlers.

## TypeScript SDK — Protocol and Loop

- [ ] **C27** — aion-worker-typescript is an npm package (package.json with ESM+CJS builds and bundled type declarations) depending on @grpc/grpc-js, with index.ts exposing only re-exports and strict tsconfig.
- [ ] **C28** — A session module performs connect+handshake (task queue + identity) and activity-type registration over @grpc/grpc-js, behind a session abstraction the loop depends on.
- [ ] **C29** — A worker loop receives tasks from the server stream, dispatches up to the operator-configured concurrency as concurrent promises, and reports completion or failure; backpressure is applied when the pool is full.
- [ ] **C30** — The TypeScript SDK reconnects with bounded backoff, re-registers, and re-reports un-acked results before serving new tasks.

## TypeScript SDK — Typed Activities, Context, Errors

- [ ] **C31** — defineActivity<I, O>(name, handler) registers a typed activity; the generic types drive Payload encode/decode (JSON baseline) at the boundary.
- [ ] **C32** — A ctx object passed to the handler exposes heartbeat(detail) and cancellation observation (isCancelled() and an awaitable cancelled()).
- [ ] **C33** — RetryableError and TerminalError classes classify failures; an unclassified throw escaping a handler defaults to retryable and is logged as unclassified (never silent).
- [ ] **C34** — A Worker class registers activities and await worker.run() connects, serves, and shuts down gracefully draining in-flight activities.

## Cross-SDK Conformance

- [ ] **C35** — A language-agnostic scenario set defines the protocol behaviours every SDK must exhibit: register, receive+complete, receive+fail-retryable, receive+fail-terminal, heartbeat, cancellation, reconnect-and-re-report, and backpressure under full concurrency.
- [ ] **C36** — A fake worker-endpoint harness drives the scenarios, and each of the three SDKs is run against it producing identical observable wire behaviour (same completion/failure classification, same re-report on reconnect).
- [ ] **C37** — JSON payload round-trip parity is verified across all three SDKs: the same input value encodes to an equivalent Payload and decodes back to an equal value in each language.
