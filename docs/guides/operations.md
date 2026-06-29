# Operations guide

Running, configuring, deploying to, and recovering an Aion server. The
server is started by the one user binary:

```sh
aion server --config aion.toml
```

There is no separate server binary — `aion-server` on crates.io is the
server **library** that `aion server` embeds. Aion is **single-node**: one
server process owns the store; there is no clustering.

## Configuration

The server starts from built-in defaults, then applies config-file values,
then `AION_*` environment overrides. When `--config` is omitted it
auto-discovers `aion.toml` in the working directory; with neither, it runs
on development defaults — but the two always-required keys below must then
come from the environment.

### Required keys (startup fails naming the missing key)

| Key | Why required |
|---|---|
| `runtime.query_timeout_ms` | The server always mounts `/workflows/query`; the reply deadline is an explicit operator decision. |
| `websocket.event_broadcast_capacity` | The server always mounts `/events/stream`; channel capacity must be sized for global event volume. |
| `deploy.max_archive_bytes` | Required when `deploy.enabled = true`; upload-size ceiling for `.aion` archives. |
| `deploy.max_inflated_bytes` | Required when `deploy.enabled = true`; decompressed-contents ceiling. Must be **>= `max_archive_bytes`** (a DEFLATE bomb under the upload ceiling can inflate ~1000:1). |

Validation failures are loud and name both the TOML key and its environment
override, e.g.:

```
runtime.query_timeout_ms is required and has no default: ... set
runtime.query_timeout_ms (or AION_RUNTIME_QUERY_TIMEOUT_MS) to a positive
number of milliseconds
```

### Full key reference

```toml
# Workflow package archives loaded at startup (see "Boot-time loads" below).
workflow_packages = []                  # default: []; .aion files in the
                                        # working directory are auto-discovered

[server]
listen_address = "127.0.0.1:8080"       # default; HTTP/JSON + dashboard
grpc_address = "127.0.0.1:50051"        # default; gRPC API + worker protocol
                                        # both need explicit non-zero ports

[store]
backend = "haematite"                   # default; durable ablative event store.
                                        # "libsql" = lightweight embedded backend
                                        # (libsql-backend feature); "memory" =
                                        # ephemeral, loses state on stop
data_dir = "aion-data"                  # default ("aion-data"); REQUIRED when
                                        # backend = "haematite": database dir,
                                        # created on start
shard_count = 1                         # default; shards on a fresh haematite db
# url = "aion.db"                       # REQUIRED when backend = "libsql":
                                        # embedded libSQL file, created on start

[runtime]
scheduler_threads = 1                   # default; must be > 0
query_timeout_ms = 10000                # REQUIRED, no default

[drain]
timeout_seconds = 30                    # default; graceful shutdown bound

[auth]
enabled = false                         # default; dev mode trusts headers
jwks_url = ""                           # REQUIRED when enabled = true
jwks_refresh_seconds = 300              # default

[metrics]
enabled = true                          # default; serves GET /metrics

[namespaces]
default = "default"                     # default namespace name

[websocket]
outbound_buffer_bound = 32              # default; per-connection buffer
event_broadcast_capacity = 1024         # REQUIRED, no default

[worker]
heartbeat_window = 30000                # default (ms); heartbeat cadence
                                        # advertised to workers in RegisterAck
                                        # (see "Workers and activity duration")

[deploy]
enabled = false                         # default: surface dark — /deploy/*
                                        # not mounted, gRPC DeployService
                                        # answers Unimplemented
max_archive_bytes = 16777216            # REQUIRED when enabled; no default
max_inflated_bytes = 67108864           # REQUIRED when enabled; no default;
                                        # must be >= max_archive_bytes
```

(`[tls]` keys exist but are currently unsupported and rejected by
validation; `[dashboard]` and `[namespace]` select dashboard asset source
and namespace resolver mode — see [`docs/API.md`](../API.md) for the
namespace/auth trust model.)

### Environment overrides

Every key: `AION_<SECTION>_<KEY>`, uppercase, underscores. Examples:

```sh
AION_SERVER_GRPC_ADDRESS=127.0.0.1:60051
AION_STORE_BACKEND=haematite            # default; "libsql" or "memory" to opt out
AION_STORE_DATA_DIR=aion-data           # haematite database directory
# AION_STORE_BACKEND=libsql AION_STORE_URL=aion.db   # lightweight embedded opt-out
AION_RUNTIME_QUERY_TIMEOUT_MS=10000
AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY=1024
AION_DEPLOY_ENABLED=true
AION_DEPLOY_MAX_ARCHIVE_BYTES=16777216
AION_DEPLOY_MAX_INFLATED_BYTES=67108864
```

Unknown `AION_*` variables are ignored. `AION_LOG` sets the tracing filter
(takes precedence over `RUST_LOG`); logs are JSON on stdout — pipe through
`jq` for interactive reading.

## Loading workflow code: two paths, one model

### Boot-time loads (config / flag / auto-discovery)

At startup the server loads, in order: the `workflow_packages` config array,
`.aion` files auto-discovered in the working directory (alphabetical), and
repeatable `--workflow-package <path>` flags — deduplicated by canonical
path. Last load wins the route per workflow type.

Boot-time loads are **not persisted**: they come from your config/filesystem
on every boot, and they win the route at boot. They are the right tool for
fixed, config-managed deployments.

### Runtime deploys (the deploy surface)

With `[deploy] enabled = true`, packages can be deployed to a **running**
server:

```sh
aion deploy my-flow.aion                # load + atomic route flip
aion versions [--workflow-type my_flow] # read model: every loaded version
aion route my_flow <content-hash>       # rollback / roll-forward
aion unload my_flow <content-hash>      # remove a non-routed, unpinned version
```

Runtime-deployed packages **persist in the event store** and are reloaded at
startup **before workflow recovery runs**, so recovered workflows always
find the exact code version they started on. (This is the key difference
from boot-time loads, which re-resolve from config each boot.)

Semantics worth knowing:

- **Deploy is idempotent.** Re-deploying a resident archive succeeds with
  `freshly_loaded: false`; `route_changed` reports whether routing moved.
  A deploy pipeline may retry blindly.
- **Versions are immutable and content-hashed.** Each package version
  deploys as modules named `<module>$<sha256>`; the hash covers compiled
  code only.
- **Running workflows are pinned.** A run executes the version it started
  on for its whole life, recorded in its history. New starts follow the
  route.
- **Unload is refused (`version_pinned`, HTTP 409) while anything pins the
  version**: it is route-active, a live instance runs it, a recoverable run
  recorded it, or an in-flight start holds it. Route away first; runs
  pinning an old version must finish before it can be unloaded.
- **A package load is engine-global.** Routing is per workflow *type*,
  startable from every namespace; the deploy surface has no namespace field.

**Authorization** is a deployment-wide `deploy` grant: a boolean `deploy`
claim in the bearer token when auth is enabled, or the development
`x-aion-deploy: true` header (the CLI sends it automatically; `--token`
overrides the `AION_TOKEN` environment variable). Denials are
`deploy_denied` (HTTP 403).

### Audit and metrics

Every deploy mutation emits one structured audit log line with `operation`
(`deploy.load` / `deploy.route` / `deploy.unload`), `subject`,
`grant_source`, `transport`, `workflow_type`, `content_hash`, `outcome`
(`loaded` / `idempotent` / `rerouted` / `unloaded`), and for loads
`freshly_loaded` / `route_changed`. Denials log at `warn`.

Metrics (Prometheus text at `GET /metrics` when `[metrics] enabled`):

- `aion_deploy_operations_total{operation, outcome}`
- `aion_deploy_denied_total{transport}`
- `aion_loaded_workflow_versions{workflow_type}` (gauge)

## Persistence and recovery

Status is a projection of event history; there is no mutable run state to
lose. With a durable store — the **haematite** event store (the default,
first-class ablative backend) or the lightweight embedded **libsql** store:

1. Every workflow event (start, activity scheduled/completed, timer fired,
   signal, terminal) is appended durably as it happens.
2. On startup — including after `kill -9` — the server reloads
   runtime-deployed packages from the store, then replays each active run's
   history. Workflow code re-executes from the top; every recorded
   side-effect call returns its recorded result instead of acting again, so
   the run lands exactly where it was: same await, same pending timers, same
   registered query handlers.
3. Durable timers are re-armed from history; a workflow sleeping across the
   restart still wakes on schedule.

The **memory** store keeps the same semantics within a process lifetime but
loses everything on stop — development only.

Recovery caveats to plan around:

- An activity that was *executing on a worker* during the crash is
  re-dispatched (its completion was never recorded). Make activities
  idempotent.
- Boot-time-loaded packages must still be present at their configured paths;
  a recovered run pinned to a version the server cannot load cannot resume.

## Workers and activity duration

**The engine imposes no activity timeout.** An activity runs as long as its
worker is working on it — agent-style activities legitimately run for over
an hour. A running activity is bounded only by:

1. **The workflow's own `timeout_seconds`** — the deadline the workflow
   author declared for the whole run.
2. **Worker liveness.** A worker whose gRPC stream ends — process death,
   network disconnect, expired token — is declared lost immediately: every
   activity in flight on it is failed back to the engine as a *retryable*
   lost-worker error, and the workflow's retry policy decides re-dispatch.
   Nothing hangs waiting on a dead worker.

`[worker] heartbeat_window` is the heartbeat cadence the server advertises
to workers in `RegisterAck` (workers report progress with explicit
`heartbeat(details)` calls), and the grace period an expired-token stream
gets to re-authenticate. It does **not** cap activity duration: a connected
worker that simply has not heartbeated is not failed. Window-based expiry
for hung-but-connected workers is on the roadmap together with engine-level
heartbeat timeouts (see `docs/ROADMAP.md`).

## Shutdown and drain

On SIGINT/SIGTERM the server drains: it stops accepting mutations
(in-flight deploy mutations are refused with a `backend`-class error and an
explicit message; the versions read model keeps serving), asks workers to
finish in-flight tasks (`DrainRequest`), and waits up to
`[drain] timeout_seconds`. `kill -9` skips all of that and is still safe
with a durable store (haematite or libsql) — that is the point of event
sourcing — but in-flight activities will re-dispatch on recovery.

## Operating runs

```sh
aion list [--status running|completed|failed|cancelled|timed-out|continued-as-new]
aion describe <workflow-id> [--run-id <id>] [--raw] --pretty
aion start <workflow-type> --input '<json>'
aion signal <workflow-id> <signal-name> --payload '<json>'
aion query <workflow-id> <query-name>
aion cancel <workflow-id> [--reason '<text>']
```

Global CLI flags: `--endpoint` (default `127.0.0.1:50051`), `--namespace`
(default `default`), `--subject` (default `cli-user`), `--token`,
`--pretty`. Errors print as `error[<code>]: <message>` with a `hint:` line
when there is an actionable next step — see the
[errors reference](../errors.md).

Health endpoints: `GET /health/live`, `GET /health/ready`. Event streaming:
`GET /events/stream` (WebSocket; see [`docs/API.md`](../API.md) for
subscription shapes and resumption cursors).
