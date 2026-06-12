# aion-server

The Aion server **library**: HTTP, gRPC, WebSocket, and worker-protocol
endpoints for Aion workflows. The crate wraps the transport-agnostic engine
with API handlers, namespace isolation, observability, shutdown handling,
dashboard assets, and remote-worker task dispatch.

This crate ships no binary. To **run** an Aion server, install the CLI and
use the `server` subcommand:

```sh
cargo install aion-cli --locked
aion server --config aion.toml
```

Configuration (required keys, defaults, `AION_*` environment overrides) is
documented in the repository's
[operations guide](https://github.com/ablative-io/aion/blob/main/docs/guides/operations.md).

## Install (embedders)

```toml
[dependencies]
aion-server = "0.4.0"
```

## Key public types

- `ServerConfig` describes operator-facing addresses, storage, auth, and runtime settings.
- `ServerState` owns the shared engine and API state.
- `ServerError` and `StreamFailure` report server and streaming failures.
- `NamespaceResolver`, `NamespaceGuard`, and `ScopedEngine` enforce tenant scoping.
- `HeartbeatTracker`, `InFlightActivity`, and `LostWorkerReport` support worker liveness.
