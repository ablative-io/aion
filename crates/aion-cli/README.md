# aion-cli

Command-line client for operating Aion durable workflows over gRPC. The binary wraps the Rust caller SDK with subcommands for starting, signaling, querying, canceling, listing, describing, and subscribing to workflow executions.

## Install

```toml
[dependencies]
aion-cli = "0.1.0"
```

## Key public surfaces

- `aion start` starts a workflow with a JSON payload.
- `aion signal` sends a named signal to a workflow.
- `aion query` reads workflow state with an optional timeout.
- `aion cancel`, `aion list`, and `aion describe` operate lifecycle and visibility APIs.
- `aion subscribe` streams workflow or firehose events.

## Minimal usage

```sh
aion --endpoint http://127.0.0.1:50051 \
  start examples.echo --payload '{"message":"hello"}'
```

## Error reporting

Every operational failure prints one report to stderr and exits with code 1;
stdout stays reserved for the JSON result document. Failures that carry the
client taxonomy render as

```text
error[<class>]: <operation>: <server detail message>
  server error type: <ErrorType>   # when the wire carried one
  hint: <actionable next step>     # for classes with a known remedy
```

where `<class>` is aligned with the wire error codes: `not_found`,
`already_exists`, `query_failed`, `query_timeout`, `unknown_query`,
`not_running`, `cancelled`, `unavailable`, `unauthenticated`,
`namespace_denied`, `invalid_input`, and `backend`. Local failures without a
taxonomy class render their full cause chain on one `error:` line.
