# Aion API overview

Aion exposes the engine through the standalone `aion-server` plus language-specific client and worker SDKs.

## Server transports

`aion-server` is the deployable API boundary for local development and service deployments. It provides:

- HTTP/JSON workflow operations for starting, signalling, querying, cancelling, listing, and describing workflows.
- gRPC APIs, including the remote worker protocol used by worker SDKs.
- WebSocket event streams for live workflow updates at `/events/stream`.
- Static dashboard assets served from the same HTTP listener; the dashboard UI is under development, so use the HTTP API or CLI for workflow observation for now.

The root [Getting started guide](../GETTING-STARTED.md) shows the shortest copy-pasteable path for running the server with the hello-world package.

## Client SDKs

Use client SDKs when application code needs to call Aion workflows over the server API:

- Gleam: [`../gleam/aion_client/README.md`](../gleam/aion_client/README.md)
- Rust: `crates/aion-client`
- Python and TypeScript SDK packages under [`../sdks/`](../sdks/)

## Worker SDKs

Use worker SDKs to host activities outside the workflow VM and connect them to the server worker protocol:

- Rust: `crates/aion-worker`
- Python and TypeScript worker packages under [`../sdks/`](../sdks/)

The hello-world quickstart uses the Python worker SDK and registers a `greet` activity.

## Workflow authoring SDK

Gleam workflow code is authored with [`aion_flow`](../gleam/aion_flow/README.md). It provides typed workflow definitions, activity calls, signals, timers, queries, child workflows, codecs, and a test harness.

## Examples

See [`../examples/`](../examples/) for working examples. Start with [`../examples/hello-world/README.md`](../examples/hello-world/README.md) for a complete workflow/package/server/worker run.
