# aion-server

Deployable HTTP, gRPC, WebSocket, and worker endpoint for Aion workflows. The crate wraps the transport-agnostic engine with API handlers, namespace isolation, observability, shutdown handling, dashboard assets, and remote-worker task dispatch.

## Install

```toml
[dependencies]
aion-server = "0.1.0"
```

## Key public types

- `ServerConfig` describes operator-facing addresses, storage, auth, and runtime settings.
- `ServerState` owns the shared engine and API state.
- `ServerError` and `StreamFailure` report server and streaming failures.
- `NamespaceResolver`, `NamespaceGuard`, and `ScopedEngine` enforce tenant scoping.
- `HeartbeatTracker`, `InFlightActivity`, and `LostWorkerReport` support worker liveness.

## Minimal usage

```rust
use aion_server::ServerConfig;

let config = ServerConfig::default();
println!("serving API on {}", config.http_addr);
```
