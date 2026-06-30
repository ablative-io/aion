# Multi-host Aion: meshed servers + remote workers

The failover demo (`scripts/demo/demo-failover.sh`, see
[`FAILOVER-DEMO.md`](FAILOVER-DEMO.md)) runs everything on one laptop over
loopback. This document explains how to spread the same cluster across machines
and point a remote worker at it.

It describes what the code does **today**. The cross-host transports run
**plaintext and dev-open by default** — there is no TLS or token auth wired into
the demo configs. Run a multi-host mesh only on a trusted network (a LAN you
control, or a Tailscale tailnet). See [Security posture](#security-posture).

---

## 1. Meshing aion servers across machines

### What a node binds

Each generated `nodeN.toml` (from `scripts/demo/gen-cluster-config.sh`) binds
four listeners. With one node per host you open these ports on that host's
address:

| Purpose                         | Config key                         | Default port (node *i*) |
| ------------------------------- | ---------------------------------- | ----------------------- |
| HTTP/JSON API + ops console     | `[server] listen_address`          | `8090 + i`              |
| gRPC API + worker pull protocol | `[server] grpc_address`            | `50051 + i`             |
| haematite replication bind      | `[store.cluster] bind_address`     | `7700 + i`              |
| liminal push listener           | `[outbox] liminal_listen_address`  | `8190 + i`              |

Peers reference each other by `[[store.cluster.peers]] address` (the `7700+`
replication endpoint) and `grpc_address` (the `50051+` endpoint). `node_id` and
the `members` list use `node-<i>@<host>`. Every one of these interpolates the
per-node host, so the whole mesh becomes routable simply by generating with
real addresses.

### Generating routable configs

The generator already resolves a per-node host. Three modes:

```bash
# (a) single laptop (the default) — every node is 127.0.0.1, distinct ports
scripts/demo/gen-cluster-config.sh --out cfg --package /path/fleet.aion --nodes 3

# (b) one shared IP for all nodes (e.g. a single remote box, distinct ports)
scripts/demo/gen-cluster-config.sh --out cfg --package /path/fleet.aion \
    --nodes 3 --mode shared --host 100.64.0.5

# (c) a true multi-host mesh — one routable IP per node index
DEMO_HOSTS="100.64.0.1 100.64.0.2 100.64.0.3" \
  scripts/demo/gen-cluster-config.sh --out cfg --package /path/fleet.aion \
    --nodes 3 --mode shared
```

`DEMO_HOSTS` (space-separated, one entry per node index) overrides everything
and is the multi-host path: node 0 binds the first address, node 1 the second,
and so on. Loopback mode (the default, no `--mode`/`DEMO_HOSTS`) is unchanged.

### Running the mesh

The generator only writes configs; it does not copy files or launch processes
on remote machines. Per host:

1. Copy that host's `nodeN.toml` **and** the `.aion` package
   (`workflow_packages = [...]` is an absolute path that must exist on the host)
   to the machine.
2. Ensure the per-node `data_dir` (`<out-dir>/dataN`, an absolute path) exists
   and is writable on that host.
3. Open the four ports above for that node on its routable address.
4. Start the server:

   ```bash
   aion server --config nodeN.toml
   ```

Stagger boots a few seconds apart. Simultaneous connects can trip a benign
beamr boot race (a `SimultaneousAbort` mis-mapped to a fatal error); the
single-host launcher staggers by `DEMO_BOOT_STAGGER` (default 3s) for the same
reason.

### Tailscale

Tailscale flattens addressing: every machine gets a stable `100.x.y.z` tailnet
IP reachable from every other node and worker regardless of NAT, so you can put
those IPs straight into `DEMO_HOSTS` and skip port-forwarding. It does **not**
add application auth — the aion transports are still dev-open; Tailscale only
gives you a private, authenticated **network** to run them over.

---

## 2. Pointing a remote worker at the mesh

The demo workers — `examples/norn-fan-worker`, `spike/liminal-fan-worker`,
`spike/greet-worker` — and the `aion-worker` SDK support two transports. Both
take fully caller-supplied addresses; the only baked-in default is the loopback
fallback `127.0.0.1:50061` when no `--address` is given.

### Transport A — liminal server-push (`--address`, used by the demo)

The fan workers take repeatable `--address HOST:PORT` candidates pointing at
each node's `[outbox] liminal_listen_address` (the `8190+` listener), and dial
them via `aion_worker::serve_with_redial`. Pass **one `--address` per candidate
node** so the worker re-dials and migrates to the new owner on failover:

```bash
norn-fan-worker \
  --address 100.64.0.1:8190 \
  --address 100.64.0.2:8190 \
  --address 100.64.0.3:8190 \
  --identity fleet-worker
```

`liminal-fan-worker` and `greet-worker` take the same `--address` flag. On this
path the SDK's `WorkerConfig.endpoint` is unused (the fan workers set it to the
placeholder `"unused-direct-address"`).

### Transport B — gRPC pull (`WorkerConfig.endpoint`)

The SDK session dials `WorkerConfig.endpoint` directly via tonic
(`GeneratedClient::connect`). Set it to the node's `grpc_address` as a full URI:

```
http://100.64.0.1:50051
```

This is the field a custom worker binary built on `aion-worker` configures
through `WorkerConfig::builder().endpoint(...)`.

### Reachability summary

A remote worker needs to reach, per node it targets, **either**:

- `50051 + i` (gRPC pull), **or**
- `8190 + i` (liminal push) — preferred for the failover demo, because the
  redial candidate list is what migrates the worker on owner death.

---

## Security posture

This is the honest current state, not a recommendation for production:

- **No TLS.** The worker SDK dials a bare endpoint with no client-side TLS
  (`GeneratedClient::connect` on the plain endpoint). The server only serves
  TLS if `[tls]` (`certificate_chain_path` + `private_key_path`) is configured,
  and the demo generator emits no `[tls]` section. Inter-node replication, gRPC
  peer traffic, and liminal push are therefore plaintext across hosts.
- **Dev-open auth.** The demo emits no `[auth]` section, so auth is disabled.
  In that mode the server **trusts the worker-supplied** `x-aion-namespaces`
  header (and `x-aion-deploy` for deploy) — a remote worker effectively
  self-declares its own authorization. Bearer-token validation exists but only
  engages when `[auth] enabled = true` with a `jwks_url`.
- **Deploy is enabled in the demo configs.** The generator now emits
  `[deploy] enabled = true` with explicit size ceilings so the ops console's
  "deploy package" action works against a demo node. With dev-open auth that
  means any caller that can reach the node may deploy.

To harden a real remote-worker close you would: add `[tls]` to the server config
and give the worker a TLS tonic channel (`Session::from_channel`); set
`[auth] enabled = true` with a `jwks_url` and have the worker carry a Bearer
token; and stop trusting `x-aion-namespaces` in dev mode. None of that is wired
into the demo today — keep the mesh on a trusted network.

### Ops console CORS across hosts

`[server] cors_allowed_origins` is secure-by-default: only the listed origins
may call the node's HTTP API cross-origin, and it defaults to the local Vite dev
server (`http://localhost:5173`, `http://127.0.0.1:5173`). An ops console served
from a routable host would be blocked until you add its origin. Set it at
generation time:

```bash
DEMO_HOSTS="100.64.0.1 100.64.0.2 100.64.0.3" \
DEMO_OPS_CONSOLE_ORIGINS="http://100.64.0.9:5173" \
  scripts/demo/gen-cluster-config.sh --out cfg --package /path/fleet.aion \
    --nodes 3 --mode shared
```

`DEMO_OPS_CONSOLE_ORIGINS` is a space-separated origin list and is applied to
every node's `cors_allowed_origins`. The legacy `DEMO_DASHBOARD_ORIGINS` name is
still honoured as a fallback.

---

## Reference: real flags and config keys

- Generator: `scripts/demo/gen-cluster-config.sh` (`--out`, `--package`,
  `--nodes`, `--mode loopback|shared`, `--host`), env `DEMO_HOSTS`,
  `DEMO_OPS_CONSOLE_ORIGINS` (legacy `DEMO_DASHBOARD_ORIGINS` honoured as a
  fallback); emitter `emit_cluster_config` in
  `scripts/demo/lib.sh`.
- Per-node config keys: `[server] listen_address` / `grpc_address` /
  `cors_allowed_origins`, `[store.cluster] bind_address` / `node_id` /
  `members` / `[[store.cluster.peers]]`, `[outbox] liminal_listen_address`,
  `[deploy] enabled` / `max_archive_bytes` / `max_inflated_bytes`.
- Optional hardening keys (not emitted by the demo): `[tls]`
  `certificate_chain_path` / `private_key_path`; `[auth]` `enabled` /
  `jwks_url` / `jwks_refresh_seconds`. Env overrides exist for deploy
  (`AION_DEPLOY_ENABLED`, `AION_DEPLOY_MAX_ARCHIVE_BYTES`,
  `AION_DEPLOY_MAX_INFLATED_BYTES`).
- Worker flags: `--address HOST:PORT` (repeatable; liminal push) on
  `norn-fan-worker` / `liminal-fan-worker` / `greet-worker`;
  `WorkerConfig.endpoint` (`http://HOST:50051`; gRPC pull) for SDK-built
  workers. Default fallback when omitted: `127.0.0.1:50061`.
