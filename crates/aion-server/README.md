# aion-server

The Aion server **library**: HTTP, gRPC, WebSocket, and worker-protocol
endpoints for Aion workflows. The crate wraps the transport-agnostic engine
with API handlers, namespace isolation, observability, shutdown handling,
ops-console assets, and remote-worker task dispatch.

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

## Multi-node failover demo (haematite cluster, SS-5b)

A runnable, watchable demonstration of automatic multi-node failover on the
haematite backend: it boots a small in-process Aion cluster over a real beamr
loopback haematite cluster, starts a workflow, **kills the node that owns it**,
and shows a surviving node's cluster supervisor **automatically detect the death
and resume the workflow to completion** — with narrated output. No human triggers
the failover.

```sh
# `gleam` must be on PATH (the hello-world workflow is rebuilt from source).
cargo run -p aion-server --example failover_demo --features haematite-backend
```

You will see node-1 (owner of shard 1) die, node-0 detect the dropped
replication link, debounce, auto-adopt shard 1, and node-1's orphaned workflow
replay from its replicated haematite history to `✅ completed`, while the witness
node finishes uninterrupted.

This is the same machinery the real `aion` server runs: build the binary with
`--features haematite-backend` (forwarded from `aion-cli`) and a distributed
`[store.cluster]` config in which each peer declares its `owned_shards`, and the
**SS-5b cluster supervisor** is commissioned automatically — it polls each peer's
replication liveness and calls the engine's `adopt_shards` failover path when a
peer with owned shards stays down past the debounce. A single-node /
non-clustered boot never spawns it, so default behaviour is unchanged. The demo
is **in-process** (N engines in one OS process); see the example's file header
for what that does and does not exercise versus a separate OS process per node.

## Key public types

- `ServerConfig` describes operator-facing addresses, storage, auth, and runtime settings.
- `ServerState` owns the shared engine and API state.
- `ServerError` and `StreamFailure` report server and streaming failures.
- `NamespaceResolver`, `NamespaceGuard`, and `ScopedEngine` enforce tenant scoping.
- `HeartbeatTracker`, `InFlightActivity`, and `LostWorkerReport` support worker liveness.
