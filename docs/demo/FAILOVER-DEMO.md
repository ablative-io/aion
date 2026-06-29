# Aion failover demo — backend + status surface

A turnkey, rehearsable, single-laptop demo: boot an N-node (default 3)
haematite+liminal Aion cluster, run a legible "AI-agent-task fleet" fan-out
workload with a provable EXACTLY-ONCE result, then violently `kill -9` the
work-owning node and watch the work complete exactly-once on a survivor.

All failover intelligence lives in shipped library code (the SS-5b
`ClusterSupervisor` + `Engine::adopt_shards` + #73 routing + the worker's
`serve_with_redial`). The demo scripts only boot processes, drive the CLI,
`kill -9`, and poll real observables. This is the bulletproof spine; the
`collect_four` fan-out fixture is the workload behind a clean seam, swappable
for a real Norn agent later (see "Swapping in a real agent" below).

There is no web UI here — this is the backend + a stable status surface a
dashboard can poll. The dashboard plan ships separately.

## One command

```bash
# Boot a 3-node cluster + start the fleet workload, leave it RUNNING:
scripts/demo/demo-failover.sh

# ...then, in another terminal, the one-keystroke VIOLENT kill + exactly-once
# assertion against a survivor:
scripts/demo/kill-owner.sh

# Or run the whole rehearsal end-to-end (boot -> start -> kill -> assert ->
# teardown) in one shot; exits non-zero unless exactly-once is proven:
scripts/demo/demo-failover.sh --auto-kill

# Cluster size (default 3; <3 cannot survive a kill — quorum needs >=3):
scripts/demo/demo-failover.sh --nodes 5
```

The launcher is idempotent: every run kills stale `aion server`/worker
processes, wipes the state dir (`/tmp/aion-failover-demo` by default), packages
the workload, regenerates configs, and boots fresh.

## What you see

1. **Build** — the cluster binary (`--features haematite-backend,liminal-transport`), the redial-capable worker, and the workload packager.
2. **Boot** — N nodes boot staggered (3s apart, to dodge the beamr
   simultaneous-connect boot race); each answers `/health/live` and commissions
   its SS-5b cluster supervisor.
3. **Workload** — one redial-capable worker registers across all nodes; the
   `collect_four` fan-out (4 agent-ish tasks) starts on the owner (shard 0). A
   per-task delay (`LIMINAL_FAN_DELAY_MS`, default 1500ms) keeps the fan-out in
   flight for ~tens of seconds so progress is visible.
4. **Kill** — `kill-owner.sh` `kill -9`s node 0 (owner of shard 0, host of the
   fleet). A survivor's `ClusterSupervisor` auto-adopts the dead shard, the
   worker redials the survivor and re-registers, and the survivor re-dispatches
   the pending ordinals.
5. **Proof** — read over a survivor's gRPC: the workflow reaches `Completed`
   with EXACTLY ONE `ActivityCompleted` terminal per ordinal (4 total). No
   sleeps are used as proof; every assertion reads a real observable.

## Ports

Per node index `i`, deliberately AVOIDING TCP 7000 (macOS AirPlay Receiver):

| Surface                       | Base    | node `i` |
|-------------------------------|---------|----------|
| Replication bind (haematite)  | `7700`  | `7700+i` |
| gRPC API + worker protocol    | `50051` | `50051+i`|
| HTTP/JSON API + health + dash | `8090`  | `8090+i` |
| liminal push listener         | `8190`  | `8190+i` |

Override the bases via `DEMO_BIND_BASE`, `DEMO_GRPC_BASE`, `DEMO_HTTP_BASE`,
`DEMO_LIMINAL_BASE`.

## Status surface — what a dashboard polls

Every node exposes these over its HTTP port (`8090+i`) and gRPC port
(`50051+i`). State is replicated, so a dashboard can poll any live node; after a
kill, poll a survivor. (The `aion` CLI shown drives the same gRPC API a
dashboard would call directly; the HTTP/JSON routes are in
`crates/aion-server/src/api/http/router.rs`.)

### Node liveness

```bash
curl -s http://127.0.0.1:8090/health/live    # 200 once the process is up
curl -s http://127.0.0.1:8090/health/ready    # 200 once it can serve traffic
```

Poll all `8090+i`; a node whose `/health/live` stops answering is down (the
killed owner). This is the node-liveness signal for the dashboard.

### Workload progress + exactly-once tally

```bash
# All runs and their status (JSON array):
aion list --endpoint http://127.0.0.1:50051

# A run's full history; the exactly-once tally is the count of
# "ActivityCompleted" events (climbs 0 -> 4 as the fleet progresses):
aion describe <WORKFLOW_ID> --endpoint http://127.0.0.1:50051

# HTTP/JSON equivalents (for a browser dashboard). ALL requests must carry the
# caller's authorized namespace as a header: `-H "x-aion-namespaces: default"`
# (anonymous callers declare their namespaces this way; without it the server
# replies namespace_denied). Request bodies are clean JSON with string ids:
#   GET  http://127.0.0.1:8090/namespaces                      -> ["default"]
#   GET  http://127.0.0.1:8090/workflows?namespace=default     (clean WorkflowSummary[])
#   GET  http://127.0.0.1:8090/workflows/count?namespace=default
#   POST http://127.0.0.1:8090/workflows/describe
#        (body: {"namespace":"default","workflow_id":"<uuid-string>","include_history":true})
```

The `summary.status` field is the run state (`Running` -> `Completed`); the
number of `history[].type == "ActivityCompleted"` events is the exactly-once
tally (must equal the fan-out arity, 4, with no dupes even across a kill).

### Live events (push, for a reactive dashboard)

```
ws://127.0.0.1:8090/events/stream
```

A WebSocket of live workflow/activity events — the dashboard's push channel for
progress + failover transitions instead of polling.

### Current shard owner + failover events

The current owner of a shard and the auto-adoption transition are emitted to the
node logs by shipped library code. The load-bearing failover line a dashboard
(or the demo) keys on:

```
adopted a downed peer's shards (SS-5b auto-failover)
```

found in `/tmp/aion-failover-demo/logs/node<i>.log`. Each node's configured
`owned_shards` (in its generated `node<i>.toml`) plus this log line tell you who
owns what before and after a kill. (A first-class "who owns shard N now" query
endpoint is a future enhancement; today shard ownership is read from config +
the adoption log + which nodes are live.)

### Metrics

```bash
curl -s http://127.0.0.1:8090/metrics   # Prometheus exposition
```

Activity durations, workflow counts, store latencies — per namespace/activity.

## Web dashboard (the `/failover` view)

A React dashboard ships in `apps/aion-dashboard`; its `/failover` route is the
visual front-end for this demo — node-liveness grid, owner/adoption strip,
fan-out bar, the headline exactly-once counter, and a live event log.

```bash
cd apps/aion-dashboard
# Defaults already target http://127.0.0.1:8090 + ws://127.0.0.1:8090 (demo node 0),
# so for the stock single-laptop demo only the namespace grant is required:
VITE_AION_NAMESPACES=default bun run dev
# open the printed URL at /failover   (Tier-1 bulletproof floor: /failover?mode=fallback)
```

Environment (parsed once at startup; all optional except the namespace grant):

| Var | Default | Purpose |
|-----|---------|---------|
| `VITE_AION_NAMESPACES` | (empty) | Comma list the caller is authorized for; sent as `x-aion-namespaces`. REQUIRED — without it every read is `namespace_denied`. |
| `VITE_AION_API_BASE` | `http://127.0.0.1:8090` | REST base URL (any live node). |
| `VITE_AION_WS_BASE` | derived from API base | WebSocket base for `/events/stream`. |
| `VITE_AION_SUBJECT` | (none) | `x-aion-subject` (dev auth). |
| `VITE_AION_BEARER_TOKEN` | (none) | `authorization: Bearer …` (JWT auth). |

The view uses only the status surface above (HTTP + WS, no gRPC): `/namespaces`,
`/health/live` per node, `/metrics`, `GET /workflows` + `/workflows/count`,
`POST /workflows/describe`, and the `/events/stream` firehose. After a kill it
fails its own reads over to a survivor automatically. The exactly-once counter is
derived from `ActivityCompleted` events deduped by history sequence, so a
WebSocket reconnect during the kill can never double-count.

### Single binary serves the dashboard (no separate web server)

The `aion` binary can serve the built dashboard itself at its **HTTP port** — the
same port as the API. No vite server, no static host: one process.

* **Dev (live, hot reload):** run the vite dev server (`bun run dev` above). It
  talks to the API via `VITE_*` config and **CORS** (configure
  `cors_allowed_origins` on the server to include the vite origin, e.g.
  `http://localhost:5173`).
* **Shipped (single binary):** build the bundle into the binary, then run it:

  ```bash
  cargo xtask build-dashboard                       # regen wire types + bun build + sync into dashboard-embed/
  cargo build -p aion-cli --release --features release
  ./target/release/aion server --open              # serves the dashboard at the HTTP port; --open launches a browser
  ```

  The dashboard is then at `http://<listen_address>/` (e.g. `http://127.0.0.1:8090/`).

The embed is gated by the `embed-dashboard` cargo feature (forwarded from aion-cli's
`release` feature). A **plain** `cargo build` (feature off) needs no bun and serves a
branded placeholder at `/` that documents the dev URL and build command — never a
blank page. See `apps/aion-dashboard/README.md` for the pipeline and CI guards.

## Components

| Path | Purpose | Lines |
|------|---------|------:|
| `scripts/demo/lib.sh` | shared helpers + the cluster-config **generator** (`emit_cluster_config`) | see file |
| `scripts/demo/gen-cluster-config.sh` | standalone generator CLI (loopback + real-IP modes) | see file |
| `scripts/demo/demo-failover.sh` | one-command **launcher** (build, gen, boot, workload, watch/kill) | see file |
| `scripts/demo/kill-owner.sh` | violent **kill** + exactly-once **assertion** | see file |
| `demo/fleet-packager/` | packages the fan-out workload into a `.aion` archive (the workload **seam**) | see crate |

The config generator also runs on its own for a real-IP / Tailscale deploy:

```bash
DEMO_HOSTS="100.0.0.1 100.0.0.2 100.0.0.3" \
  scripts/demo/gen-cluster-config.sh --out cfg --package fleet.aion \
    --nodes 3 --mode shared
```

## Swapping in a real agent (the seam)

The workload is the `collect_four` fan-out fixture, packaged by
`demo/fleet-packager`. Two seams make it replaceable without touching the
launcher, kill, or status surface:

1. **The workload archive** — `demo/fleet-packager/src/main.rs` embeds the
   fixture beam/erl and builds the `.aion`. Point those `include_bytes!` inputs
   (and the manifest entry module/function) at a real Norn-agent workflow
   archive; the launcher loads whatever `fleet.aion` it produces.
2. **The activity handlers** — `spike/liminal-fan-worker/src/main.rs` registers
   one handler per fan-out ordinal (currently a deterministic, no-network
   sleep+return). Replace those handlers with real agent activities; the redial
   loop, registration, and exactly-once dedup are unchanged.

Keep the workload on the FAN-OUT exactly-once shape (the reliable one); the
in-flight parked-resume path is owned by a separate durability track.

## Caveats

- **`<3` nodes cannot survive a kill.** haematite `quorum_size` needs a
  majority; a lone survivor can never re-elect. The launcher warns and the demo
  defaults to 3.
- **Start routing.** There is no cross-shard start routing yet, so the launcher
  retries `start` until it lands on the owner's own shard (mirrors the proven
  test harness). This is a known library gap, not a demo bug.
- **Boot stagger.** Nodes boot 3s apart to sidestep a documented beamr
  simultaneous-connect boot race (a benign `SimultaneousAbort` mis-mapped to
  fatal in haematite's `DistributionEndpoint::connect`). Pure launch ordering;
  no failover logic. The proper fix belongs in the haematite library.
- The `Killed: 9` lines bash prints on teardown are job-control noise for the
  SIGKILL'd background processes; they do not affect the exit code.

## Regression test

The same workload + kill-9 exactly-once path is covered as an `--ignored`
OS-process integration test:

```bash
cargo test -p aion-cli --features haematite-backend,liminal-transport \
  --test lsub5b_osproc_kill9_failover_e2e -- --ignored --nocapture
```
