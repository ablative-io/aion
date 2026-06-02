# Aion-Clients — Checklist

## Shared Behavioural Contract

- [ ] **C1** — CLIENT-CONTRACT.md defines the seven operations (connect, start, signal, query, cancel, list, describe, subscribe) language-neutrally, each with inputs, outputs, and the errors it can raise.
- [ ] **C2** — The contract specifies start idempotency: a retried identical start (same idempotency key + request) returns the original handle; a conflicting reuse raises AlreadyExists.
- [ ] **C3** — The contract specifies run targeting: signal/query/cancel/describe target the latest run by default and a specific RunId when given one.
- [ ] **C4** — The contract specifies query as a synchronous deadline-bounded round-trip yielding QueryTimeout when the deadline elapses and QueryFailed when the handler errors.
- [ ] **C5** — The contract specifies cancellation as a recorded request (cooperative stop), not an immediate kill, and subscribe resumption from the last delivered per-event sequence number with no gaps or duplicates.
- [ ] **C6** — The contract defines the shared error taxonomy: NotFound, AlreadyExists, QueryFailed, QueryTimeout, Cancelled, Unavailable, Unauthenticated, InvalidArgument, Server.

## Rust Client (aion-client)

- [ ] **C7** — ClientBuilder connects to an aion-server endpoint with auth credential and TLS configuration and produces a reusable Client; the Client depends on aion-proto and aion-core, not on aion/aion-store/beamr by default.
- [ ] **C8** — Client exposes start, signal, query, cancel, list, and describe over the network transport, returning idiomatic Result<_, ClientError>.
- [ ] **C9** — start returns a WorkflowHandle bundling WorkflowId + RunId and offering signal/query/cancel/describe/subscribe as methods; a bare-ID constructor for WorkflowHandle also exists.
- [ ] **C10** — Typed payload helpers serialise serde::Serialize inputs to a JSON Payload and deserialise results to serde::DeserializeOwned, with a raw-Payload escape hatch on every payload-bearing operation.
- [ ] **C11** — subscribe returns a Stream of decoded events that resumes from the last delivered sequence number across transient disconnects and yields an Err item on terminal failure rather than ending silently.
- [ ] **C12** — ClientError is a closed enum covering NotFound, AlreadyExists, QueryFailed, QueryTimeout, Cancelled, Unavailable, Unauthenticated, InvalidArgument, and Server, implementing std::error::Error via thiserror.
- [ ] **C13** — An embedded constructor binds the same Client/WorkflowHandle surface to an in-process aion engine behind a feature flag, pulling aion only when that feature is enabled.

## Python Client (aion-client-python)

- [ ] **C14** — Client connects to an aion-server endpoint with auth + TLS config and exposes start, signal, query, cancel, list, and describe; start returns a WorkflowHandle with per-workflow methods.
- [ ] **C15** — Payload helpers accept any JSON-serialisable value (and optionally a model / target type for results) with a raw bytes escape hatch on every payload-bearing operation.
- [ ] **C16** — subscribe yields an async iterator of decoded events that resumes from the last delivered sequence number across transient disconnects and raises on terminal failure rather than ending silently.
- [ ] **C17** — Errors form an exception hierarchy mapping the shared taxonomy (NotFound, AlreadyExists, QueryFailed, QueryTimeout, Cancelled, Unavailable, Unauthenticated, InvalidArgument, Server) with a common base; mypy --strict passes.

## TypeScript Client (aion-client-typescript)

- [ ] **C18** — Client connects to an aion-server endpoint with auth + TLS config and exposes start, signal, query, cancel, list, and describe; start returns a WorkflowHandle with per-workflow methods.
- [ ] **C19** — Payload helpers are generic over input/result types with JSON serialisation and a raw-Payload escape hatch on every payload-bearing operation; the public API is typed while the wire stays Payload.
- [ ] **C20** — subscribe returns an AsyncIterable of decoded events that resumes from the last delivered sequence number across transient disconnects and rejects/throws on terminal failure rather than ending silently.
- [ ] **C21** — Errors are a discriminated union / class hierarchy mapping the shared taxonomy; tsc --noEmit (strict) and eslint pass.

## Gleam Client (aion_client)

- [ ] **C22** — aion_client connects to an aion-server endpoint with auth + TLS config and exposes connect, start, signal, query, cancel, list, and describe; start returns a workflow handle value usable in per-workflow operations.
- [ ] **C23** — Payload helpers take a gleam/json encoder for input and a gleam/dynamic decoder for the result, staying statically typed, with a raw-Payload escape hatch on every payload-bearing operation.
- [ ] **C24** — subscribe returns an event stream that resumes from the last delivered sequence number across transient disconnects and surfaces terminal failure through the stream rather than ending silently.
- [ ] **C25** — Errors are a Gleam custom type mapping the shared taxonomy; gleam check, gleam format --check, and gleam test pass.

## Cross-Language Conformance

- [ ] **C26** — A shared conformance scenario set encodes the start -> signal -> query -> list -> describe -> cancel -> subscribe happy path plus the idempotency-conflict, query-timeout, not-found, and disconnect-resume cases.
- [ ] **C27** — Each SDK has a conformance harness that runs the shared scenarios against a real aion-server fixture and asserts the observable results match the contract.
- [ ] **C28** — The four SDKs produce identical observable behaviour for every shared scenario (same effect, same returned values, same error variant).

## Packaging and Publishing

- [ ] **C29** — aion-client (crates.io), aion-client-python (PyPI), aion-client-typescript (npm), and aion_client (Hex) each carry complete package metadata, a README, and a type-checked public surface.
- [ ] **C30** — Each package ships a runnable example exercising all seven operations against a server, referenced from its README.
