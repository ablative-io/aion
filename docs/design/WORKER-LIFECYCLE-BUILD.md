# Worker Lifecycle Through the Server — Build Design

**Status: design pass for the operator's ruling — not yet build-approved.**
Authored 2026-07-19 from a full-repo ground survey (evidence anchors
throughout are file:line at aion main `ca7825e6`). Prior art:
`WORKER-AUTHORING-STORY.md` (ruled direction: lifecycle through the server,
Aion-native, launchd ruled out, lifecycle FIRST in the worker-story sequence)
and `WORKER-DEPLOYMENT.md` (draft blueprint, treated as prior art only).

## The one-sentence goal

The operator deploys, starts, stops, and supervises workers from the ops
console; the server owns the processes; no external process manager exists
anywhere in the story.

## Ground truth (what exists today)

- Workers are **independently launched processes that connect inward**. The
  server registers and routes already-connected streams
  (`api/worker_grpc.rs:43-107`) and stores only ephemeral stream state —
  worker ID, namespaces, task queue, transport, node
  (`worker/registry.rs:147-168`). No PID, no artifact/version, no desired
  state, no restart count. Registry state dies with the server process
  (`state.rs:219-229`).
- **SDK reconnect supervises a connection, not a process**
  (`aion-worker/src/worker.rs:167-206`). Nothing anywhere spawns, restarts,
  or converges worker processes; the control plane marks compute management
  as its remaining phase (`CONTROL-PLANE.md:217-243`).
- **Deploy ships workflow code, not workers.** Format-v1 `.aion` manifests
  model workflow entries only (`aion-package/src/manifest.rs:68-140`);
  generated worker source is written to a local `worker/` tree and built,
  placed, and launched by hand (`codegen/activity_project.rs:317-373`).
- Death detection: heartbeat expiry bounded at `window + window/4` (default
  37.5s) for in-flight tasks; stream teardown fails tracked tasks as
  retryable worker loss (`worker/heartbeat.rs:507-529`,
  `api/worker_grpc.rs:195-241`). **An idle wedged process has no
  process-level heartbeat to expire** (`runtime/loop_.rs:232-240`).
- The console shows worker counts and searchable live entities; it exposes
  **zero lifecycle actions** (`FailoverView.tsx:74-88`, router has no worker
  lifecycle route: `api/http/router.rs:135-188`).
- The wire does not carry the SDK's `identity`
  (`protocol/session.rs:67-88`); console worker IDs are per-server-process
  stream ordinals, not durable identities.

## Constraints already ruled

- Aion-native supervision; **no launchd, ever** (operator ruling 2026-07-14;
  `WORKER-AUTHORING.md:142-143`).
- ADR-001: no invented numeric limits/defaults — every bound is an operator
  decision or a signed number.
- ADR-003: worker loss is a real, retry-policy-mediated failure — the
  lifecycle layer must not silently mask it.
- ADR-022: cluster-control authority stays distinct from deploy and
  workflow-command authority.
- Single-node first (house doctrine). Cross-node placement and artifact
  availability ride the control plane's compute phase, not this build.

## The design in one paragraph

A durable **`WorkerDeployment`** record (store-backed, surviving restart)
carries what the operator declared: artifact reference, launch contract,
namespace/queue binding, desired state (`Running{replicas}` | `Stopped`).
A server-owned **reconciler** converges actual toward desired: it
materializes the artifact, spawns OS processes with a standardized launch
contract, adopts their registrations via a wire-carried instance identity,
restarts crashes with operator-configured backoff, and distinguishes
"stopped because told to" from "gone because it died". Every lifecycle
transition is a **retained, durable status** (deployments never vanish from
view on disconnect) published over the existing cluster stream. The
**control API** exposes deploy/start/stop/restart/scale/drain under
cluster-control authority, and the console grows the management surface
last, on top of commands and state that already work.

## Build units (dependency order)

**W-0 — Durable deployment record + wire identity.**
The `WorkerDeployment` store entity (haematite-backed like namespace
records): deployment ID, artifact ref, launch contract, namespace set, task
queue, node affinity, desired state, and status history. Extend
`RegisterWorker` with an optional instance-identity field (proto is
currently identity-blind) so a registration can be associated with the
deployment instance that spawned it — old workers registering without it
stay first-class (the connect-inward path remains supported forever).
*Exit: records survive server restart; a hand-started worker carrying an
identity is associated; nothing spawns yet.*

**W-1 — Worker artifact kind in the package format.**
An additive worker-kind entry in the `.aion` format (kind, executable
entrypoint or script, platform, declared activity types) with a loader that
old servers refuse loudly and new servers accept alongside format-v1
workflow entries; an upload path that lands artifact bytes in the store.
The draft's rejection of bare-binary-URLs stands: the artifact is bytes we
store, not a location we hope stays valid.
*Exit: `aion deploy worker.aion` (or a worker section of an existing
archive) persists a deployable artifact the server can enumerate.*

**W-2 — Launch contract + materializer + spawner.**
One standardized launch contract: the server materializes artifact bytes to
a run directory and spawns with the `AION_*` environment (endpoint, queue,
identity/instance, concurrency, reconnect values — the contract generated
Rust mains already require, promoted to the norm). Exit capture and stdout/
stderr retention per instance. Custom binaries (today's liminal workers,
norn workers) are supported by an explicit `command`+`args`+`env` launch
contract on the deployment record — declared, never inferred.
*Exit: the server can start one instance of a deployed artifact and observe
its exit; the shell worker (`aion worker shell --manifest`) is the first
launchable family, because the distribution already owns its whole
contract.*

**W-3 — Reconciler (the supervision core).**
The desired-vs-actual convergence loop: crash → restart with
operator-configured backoff and intensity caps (ADR-001: numbers are config,
not invention); `Stopped` intent → no restart, record retained;
server restart → rebuild actual from store + re-adopted registrations, then
converge. Distinguishes crash-loop (escalating status, never silent) from
clean exit. This unit also closes the idle-wedge blind spot: instance-level
liveness (process alive + periodic idle heartbeat on the stream) rather
than only in-flight-task heartbeats.
*Exit: kill -9 a worker instance → it returns; stop it via desired state →
it stays down and stays visible; crash-loop is visible and bounded.*

**W-4 — Lifecycle control API + targeted drain.**
HTTP/gRPC commands: create/update deployment, start, stop, restart, scale,
and **targeted drain** (today's only drain is a global server-shutdown
broadcast: `worker/registry.rs:766-788`). Drain marks the instance
not-accepting (visible to placement), waits for in-flight completion with an
operator-supplied timeout, then stops. Authority per ADR-022:
cluster-control, separate from deploy and workflow-command grants.
*Exit: every lifecycle mutation is a first-class, authorized API command
with a durable status outcome.*

**W-5 — Status read model + lifecycle events.**
The cluster stream grows deployment-level state: desired vs actual,
instance states (starting/running/draining/stopped/crashed/restarting),
restart counts, last-seen, exit info. Disconnection stops REMOVING entities
from view (`useClusterStream.ts:149-161` removes today); stopped and
crashed deployments remain inspectable. Events are retained-status-backed,
so a console that reconnects sees truth, not an edge-triggered residue.
*Exit: the read model alone answers "what is running, what should be
running, what died, and why".*

**W-6 — Console management surface.**
Deploy/start/stop/restart/scale/drain from the ops console; deployment
detail view (status history, exit info, logs tail); the studio's "no worker
on queue" guidance gains a "start one" action wired to W-4. Built last, on
commands and state that already work — the console is the management
surface, not the mechanism.

## Deferred (explicitly not this build)

Cross-node placement and artifact distribution (control-plane compute
phase); wasm/driver isolation tiers; BEAM-hot-load worker tier; autoscale.
The draft's open questions on those stay open and do not block W-0..W-6.

## For the operator's ruling

1. **The phase plan above** (W-0..W-6, single-node, shell-worker-first).
2. **Artifact posture**: worker kind as an additive `.aion` entry
   (recommended) vs a separate artifact object. The recommendation keeps
   one deploy verb, one store, one format lineage.
3. **The launch-contract promotion**: `AION_*` env as the standard contract,
   explicit command/args/env for custom binaries — declared, never inferred.
