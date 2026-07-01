<!-- STATUS: DRAFT design blueprint (2026-07-01). Produced by a source-grounded
synthesis pass over the worker-deployment RESEARCH; every load-bearing seam cites
file:line, verified directly against the aion/beamr trees (not trusted from the
research doc). NOT yet approved to build — it folds into #146 (haematite as
cluster source-of-truth) and realises CONTROL-PLANE.md §1.3 / Phase 3. It carries
GATING SPIKES (negative controls) that must pass before build, and OPEN DECISIONS
for Tom (final section). Review first. -->

# Worker Deployment — aion's Second Moat: dynamic, multi-instance, multi-type, multi-node compute placement (DESIGN)

> Status: **design pass, read-only analysis. No production code changed by this doc.**
> Companion to [CONTROL-PLANE.md](../aion/docs/design/CONTROL-PLANE.md) §1.3 / Phase 3
> and [HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md](../aion/docs/design/HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md) (#146).
> Grounds the research field-survey (`scratchpad/research/worker-deployment.md`) and the
> gap investigation (`scratchpad/roadmap-invest/worker-deploy.md`) in aion/beamr's actual code.
>
> **Owner's hard constraints, honoured throughout and non-negotiable:**
> NO hardcoded/baked-in worker binary; NO single-node sidestep; genuinely DYNAMIC —
> deploy *multiple instances* of *multiple different* worker types onto *different nodes*.
> This doc tackles the artifact / placement / supervision / drain problems head-on, on
> the substrate that already exists.

---

## TL;DR (read this first)

1. **Deploy = a durable write, not a new subsystem.** aion already has four of the
   five pieces every competitor had to bolt on: (a) a content-addressed durable
   artifact store on every node (#117), (b) a supervising actor VM with distributed
   spawn + hot code load already running on every node (beamr), (c) a durable
   cluster source-of-truth for desired state + placement (haematite, #146), (d)
   built-in `(namespace, task_queue, node)` routing, and (e) durable-execution
   *in-flight-work awareness* so autoscale/drain can respect running workflows. The
   fifth piece — the placement control plane — is a haematite state write.
   "Deploy a worker" becomes: **register a content-addressed WORKER PACKAGE in the
   store → upsert a durable `WorkerDeployment` desired-state record → the target
   node's already-running beamr supervisor materialises the artifact by content hash
   and SPAWNS N supervised worker processes IN-VM → those processes dial the local
   registration path and appear in existing routing with zero new routing code.**

2. **The recommended model is the union no competitor has in one binary:**
   OTP's in-VM supervised distributed spawn + hot code load, Nomad's desired-state
   bin-pack placement + drain, and durable-execution's in-flight-work awareness —
   reusing substrates aion already ships rather than a container runtime or external
   scheduler.

3. **Two grounded corrections bound the thesis (both verified in code, not
   assumed):**
   - **Isolation:** the default and *only currently available* isolation is
     **BEAM per-process**, NOT wasm. beamr has **no `wasmtime`/`wasmi`/`wasi`
     dependency** (verified: `grep` over every `Cargo.toml` in the beamr tree
     returns empty) — its "wasm" is the beamr VM compiled to wasm, not a sandbox
     for arbitrary user modules. "wasm worker isolates" is a genuine FUTURE driver
     tier, not a current capability. Design the artifact envelope to admit it;
     do not ship on the assumption it exists.
   - **Artifact distribution:** #117 gives content-addressed identity + a durable
     content-hash store, but distribution is **reload-from-shared-store-on-boot**,
     not an on-demand peer lazy-pull, and worker-package **puts are non-quorum
     today** (`crates/aion-store-haematite/src/store.rs:271` — "packages, routes,
     and the outbox stay local (Design B)"). Cross-node artifact-availability-
     before-spawn is real work (§5.4), and the *placement/assignment* records (not
     necessarily the artifact bytes) must go through the quorum path.

4. **Headline capability: in-flight-aware autoscale/drain.** Because aion owns the
   durable execution state, it knows the true in-flight activity count per worker
   instance, so it can **never scale an instance with running work to zero** — the
   exact footgun KEDA's Temporal scaler hits (kedacore/keda#7368), which aion
   structurally avoids. This is the clearest place aion is better than the
   Temporal+KEDA stack, and it should be the demo beat.

5. **Gated on / shares substrate with #146.** Node inventory, quorum placement
   writes, and cross-node artifact availability all ride the #146 durable-cluster-
   state work. This is the same lineage as [NAMESPACE-REGISTRY-PHASE-1.md] and the
   cluster-formation design — build it in that sequence, not ahead of it.

---

## 1. The one-paragraph honest summary

The best-in-class model for aion is **not** to copy Nomad (ship OS binaries +
fork/exec) or k8s (ship container images + kubelet). It is to exploit the one
structural asset none of them have: aion **already runs a supervising actor VM
(beamr) on every node and already ships code as content-addressed artifacts**, and
**already has a durable consensus store (haematite) for cluster desired-state**. So
"deploy a worker" is a content-addressed artifact upload + a durable desired-state
upsert + an in-VM supervised spawn — no container runtime, no external scheduler,
no new transport, and `kill -9` survivability already proven (#157; ROADMAP.md:34-35
"Cross-node `kill -9` failover WORKS and is proven"). The genuine cut-above is the
**union**: OTP's supervised distributed spawn + Nomad's desired-state placement +
durable-execution's in-flight-work awareness, in one binary. The two corrections
that keep it honest and bound the design: (1) the isolation aion actually has is
**BEAM per-process fault/state isolation**, not a hostile-code wasm sandbox (beamr
has no `wasmtime`/`wasmi` — verified), so untrusted-code isolation is a future
driver tier; and (2) content-addressed **distribution** today is reload-from-shared-
store with **non-quorum package puts**, so cross-node artifact-availability-before-
spawn is real work that must be built, and the *placement records* must be quorum.

---

## 2. The recommended model

Design tenets, straight from the constraints: **no hardcoded binary** — the worker
artifact is uploaded/registered content-addressed like a package; **no single-node
sidestep** — placement is cross-node desired-state from the design's core (Phase 3),
with the single-node reconciler (Phase 2) built as the *honest primitive*, not a
hardcoded shortcut; **dynamic/multi-instance/multi-type/multi-node** — the model is
`desired-state × placement × N`.

### 2.1 The four moving parts

1. **Content-addressed worker package** (the artifact). Reuses #117 end-to-end.
   `.aion`-family package with a manifest declaring `kind: worker`, `isolation`,
   `entrypoint`, served `(namespace, task_queue, activity_types)`, and `resources`.
   Content hash **is** the version id (already true — `content_hash` over exact
   `.beam` bytes in canonical order, `crates/aion-package/src/hash.rs`; verified on
   load via `PackageError::IntegrityMismatch`), landing in the existing
   `PackageStore` (`crates/aion-store/src/package.rs`, `PackageRecord { workflow_type,
   content_hash, archive, deployed_at }`; haematite impl
   `crates/aion-store-haematite/src/store.rs`).

2. **`WorkerDeployment` desired-state record** (the control-plane object). Durable
   haematite state, mirroring the `NamespaceRecord` model
   ([NAMESPACE-REGISTRY-PHASE-1.md] §2) and Temporal's Worker Deployment concept but
   as *real placement*:

   ```
   WorkerDeployment {
     name,                         // stable id, e.g. "billing-activities"
     namespace,                    // tenant boundary (existing routing axis)
     task_queue,                   // routing axis (existing)
     artifact: ContentHash,        // -> PackageStore worker package (§3)
     isolation: Beam | OsProcess | Wasm,   // §3; only Beam materialisable today
     replicas: DesiredCount,       // N instances (multi-instance)
     placement: PlacementRule,     // §4 (multi-node): Any | Pinned | Spread | PerNode
     version_behavior: Pinned | AutoUpgrade,   // §6 drain/rollout
     resources: { cpu, mem, ... }, // §4 bin-packing + §5.3 limits
     autoscale: { min, max, metric } | None,   // §6
     created_at, updated_at, status
   }
   ```

   Deploy = artifact upload (existing `POST /deploy/packages` path — mounted only
   when `deploy.enabled`, `crates/aion-server/src/api/http/router.rs:137-141`;
   handler `crates/aion-server/src/api/handlers/deploy.rs:57,102`; operator-gated by
   `DeployGuard`) extended for `kind: worker`, **plus** a `WorkerDeployment` upsert.
   This is desired-state; reconciliation (§2.3) makes reality match. **Multi-type
   falls out for free:** many `WorkerDeployment`s with different artifacts/task_queues
   coexist as independent supervised subtrees.

3. **Node inventory as durable haematite state** (the placement substrate). Folds
   directly into #146's `cluster/member/<node_id>` per-member detail record
   (HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:135-137, explicitly a Phase-2 slot). Each
   node records `{ node_id, labels, capacity{cpu,mem}, allocated, last_seen }`.
   beamr/liminal liveness already *senses* membership; haematite is the *authority*
   (#146's core principle: "beamr is a SENSOR, not an authority",
   HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:46-52).

4. **A haematite-backed reconciler**, per node — NOT a new scheduler service. It
   reads desired `WorkerDeployment`s + node inventory, computes an assignment
   (deployment → {node → instance_count}), writes per-node assignments as durable
   state, and on each node converges reality to its owned slice by driving beamr
   spawn/kill. It survives failover because it is *just state* + a reconcile loop.

### 2.2 Why this is genuinely a cut above (the union, grounded)

| Capability | Nomad | k8s | Temporal | OTP | aion (this design) |
|---|---|---|---|---|---|
| Desired-state + bin-pack placement | ✅ | ✅ | ❌ (deploys nothing) | ❌ | ✅ (§4) |
| In-VM supervised spawn (no container/OS-proc tax) | ❌ | ❌ | ❌ | ✅ | ✅ (beamr, §2.3) |
| Content-addressed artifact identity | opt-in checksum | digest | n/a | release bundle | ✅ (#117, §3) |
| Hot code versioning (drain in-flight) | ❌ | ❌ | ✅ (Worker Versioning) | ✅ (hot load) | ✅ (§6) |
| **In-flight-work-aware autoscale/drain** | ❌ | ❌ (keda#7368) | partial (Pinned) | ❌ | **✅ (§6 — the moat)** |
| Single self-contained binary | server+client | no | no | no (no control plane) | ✅ |

No competitor has the whole row. The deploy operation is a write because the hard
parts (get verified bytes onto the node; keep N instances alive; notice node death)
are already solved substrates.

### 2.3 Multi-instance / multi-type / multi-node execution via beamr supervision

The per-node **reconciler + supervisor** (an aion-server component driving beamr):

1. Reads its node's desired assignments from haematite (owned-shard-scoped, §5.6).
2. For each assigned `WorkerDeployment`: ensures the **artifact is present locally**
   — materialise from the content-addressed store by hash and **verify the hash**,
   the exact pattern `reload_persisted_packages` already uses at engine build
   (`crates/aion/src/loader/persistence.rs`; re-verifies content hash against the
   store key before registering) — then `hot_load_module` it into beamr
   (`crates/beamr/src/scheduler/module_management.rs:18`).
3. Spawns the **desired N supervised worker processes** at the artifact's entrypoint
   via beamr `spawn` / `remote_spawn` (`crates/beamr/src/native/context/mod.rs`;
   distribution protocol `SPAWN_REQUEST=29`/`SPAWN_REPLY=31`,
   `crates/beamr/src/distribution/control.rs:388-390`) **under a supervisor process**
   that traps exits and **restarts** crashed instances (beamr supervision +
   `propagate_exit`, `crates/beamr/src/scheduler/supervision_integration.rs:43`; a
   trapping process receives `{'EXIT', Pid, Reason}` instead of dying — the death
   rule `reason == Kill || (reason != Normal && !trap_exit)`,
   `crates/beamr/src/supervision/link.rs:171` — is the restart hook). Each spawned worker runs the existing register-and-serve
   loop (`Worker::run`, `crates/aion-worker/src/worker.rs:143`), dialing the **local**
   `stream_worker` path (`crates/aion-server/src/api/worker_grpc.rs`) with the
   deployment's `(namespace, task_queue, node, activity_types)` — appearing in the
   existing registry/routing with **zero new registration code**
   (`accept_registration`, `crates/aion-server/src/worker/registry.rs:323`, which
   does auth/mint/placement gating then delegates the `WorkerHandle` insert to
   `register_namespaces`, `:539-559`).
4. Reconciles continuously: desired N vs actual → spawn/kill to converge; reports
   actual state back to haematite for the ops console (existing WS3
   `WorkerConnected`/`WorkerDisconnected` feed, `registry.rs:245,574,781`).

- **Multi-instance:** `replicas = N` → N supervised processes. BEAM processes are
  cheap; this scales to thousands per node.
- **Multi-type:** many deployments, many artifacts, many task_queues — each an
  independent supervised subtree.
- **Multi-node:** placement fans the same deployment across nodes; each node's
  reconciler independently converges its slice. Cross-node is native (beamr
  `remote_spawn` even lets a control node spawn onto a peer, though the
  reconciler-per-node model is cleaner and failover-friendlier).

---

## 3. The driver-tier artifact/isolation decision (decision-with-consequences)

**Decision:** the worker artifact is a **content-addressed worker package** with
**one envelope**; the manifest's `isolation` field selects a **driver**. **Tier-1
BEAM (in-VM, default)** is what ships first and uses everything that already exists.
**Tier-2 content-addressed OS-process (cgroups)** is the driver that makes the
*existing native-Rust `aion-worker` SDK* deployable. **Tier-3 wasm sandbox** is a
future driver, blocked on a real wasm runtime being added to beamr.

The envelope is **one thing**, so adding a tier is a *driver*, not a *migration* —
exactly the "reserve the field, build the physical variant later" discipline
CONTROL-PLANE.md §4.3 applies to namespace placement.

### 3.1 Tier-1 — BEAM/Gleam worker package (RECOMMENDED default)

- **What it is:** the worker's activity/handler code compiled to `.beam`, packaged
  `.aion`-style, content-hashed, in the existing `PackageStore`; `hot_load_module`ed
  into the running VM; N supervised worker processes spawned at the exported
  entrypoint.
- **Consequence — pro:** zero new artifact subsystem, zero new transport, in-VM
  supervision + hot-versioning for free, content-addressed identity + integrity for
  free, `kill -9` survivable (#157).
- **Consequence — con (the crux, §5.1):** today's worker is a **Rust binary linking
  activity-handler closures at compile time** (`WorkerBuilder::register_activity`,
  `crates/aion-worker/src/worker.rs:47`) — you **cannot** `hot_load_module` a Rust
  closure into beamr. So Tier-1 fits **pure-BEAM/Gleam-authored activities cleanly**
  and should be the flagship; **native-Rust-handler workers need Tier-2**.

### 3.2 Tier-2 — content-addressed OS-process worker package (the driver that makes today's SDK deployable)

- **What it is:** a worker package that carries (or references by content hash) a
  platform-compiled worker binary; a **local-spawn placement driver** fetches it
  from the content-addressed store and `fork/exec`s it as a supervised child that
  dials the existing `stream_worker` path (Nomad `exec`-style, but content-addressed
  + store-distributed instead of URL-fetched).
- **Consequence — pro:** supports arbitrary native/Rust workers (i.e. the SDK that
  exists **right now**), real OS isolation + hard CPU/mem limits via **cgroups**,
  no compile-into-VM constraint.
- **Consequence — con:** per-platform binaries; weaker in-VM supervision (beamr
  supervises a port/OS process, not an in-VM actor); heavier. This is the honest
  escape hatch for "my worker is a big native binary," and it is genuinely dynamic +
  multi-node — just not the elegant path. **It is what makes the current SDK a
  first-class deployable, so it is not optional long-term — but it is Phase 5,
  after Tier-1 proves the model.**

### 3.3 Tier-3 — wasm-sandbox worker (FUTURE driver, NOT available today)

- **What it would be:** worker code compiled to a wasm component, run in a real
  `wasmtime`/`wasmi` sandbox with WASI + capability security, distributed as a
  content-addressed wasm artifact. The Cloudflare-grade answer for **untrusted /
  multi-tenant** worker code.
- **Consequence — blocking fact (verified):** beamr has **no wasm sandbox for
  arbitrary user modules** — no `wasmtime`/`wasmi`/`wasi` dependency exists anywhere
  in its Cargo tree (verified by direct grep). Its `scheduler/wasm.rs` is the beamr
  *VM compiled to wasm*; `scheduler/wasm_native.rs`, despite the name, is the
  cooperative native-process runtime, not wasm-module execution. **Adding a real
  wasm runtime to beamr is its own scoped, genuinely-hard project** — SPIKE-FIRST,
  not a checkbox. The envelope reserves `isolation: Wasm` so a namespace policy can
  *demand* it once the driver exists.

### 3.4 Rejected: bare OS binary via URL (Nomad-style) as the primitive

Violates content-addressed identity, throws away #117 and in-VM supervision, and is
the model the whole thesis beats. Fine as the *mechanism inside* Tier-2's cgroup
driver; never as the model.

**Net:** default = Tier-1 BEAM package + BEAM-process isolation (uses everything
that exists, fits pure-BEAM/Gleam activities). Tier-2 = content-addressed OS-process
for the native-Rust SDK (cgroups). Tier-3 = wasm when a runtime is added. One
envelope, three drivers.

---

## 4. Placement — reuse `(ns, tq, node)` + bin-pack over durable node facts

**Do not build a new scheduler.** The routing substrate exists; placement is the
front half that decides *which node materialises which deployment*.

- **`PlacementRule`** generalises the existing `(ns, tq, node)` model:
  - `Any` — bin-pack anywhere feasible (Nomad-style: maximise resource use without
    exhausting a dimension; honour `resources`).
  - `Pinned(node|label)` — reuses the **existing** placement admission:
    `enforce_pinned_placement` (`crates/aion-server/src/worker/registry.rs:415`)
    already reads `NamespacePlacement::Pinned { nodes }` (`:424`) from the durable
    namespace record and admits workers by node locality (`worker_matches_node`,
    `:860`).
  - `Spread(N per node | across zones)`.
  - `PerNode` — a worker instance on every node serving the namespace (the natural
    default for "every node can do this work").
- **The reconciler is a haematite-backed control loop, not a service.** It reads
  desired `WorkerDeployment`s + node inventory, computes `deployment → {node →
  count}`, and writes per-node desired assignments as **quorum** durable state
  (§5.6). Survives failover because it is state. Bin-packing selects nodes for
  `Any`/`Spread`.
- **Composition, not bolt-on:** a deployed worker just needs to register with the
  `(ns, tq, node)` the deployment specified; it then slots into the existing
  round-robin dispatch (`workers_for`, `registry.rs:627`) with no new routing code.
  The registry already carries node affinity and pinned-placement admission; this
  design adds the *front half* that creates the worker process so it can dial that
  path.

---

## 5. The honest hard problems (tackled head-on)

Each states the problem (grounded), the approach, and **what must be BUILT**.
Anything the research flagged as unproven is marked **SPIKE-FIRST**.

### 5.1 Artifact build + native-handler loading (the crux)

- **Problem (grounded):** a worker today is a **Rust binary linking activity-handler
  closures compiled in** (`WorkerBuilder::register_activity`, worker.rs:47) — you
  cannot `hot_load_module` a Rust closure into beamr.
- **Approach:** (a) **BEAM/Gleam-authored activities** are the first-class Tier-1
  deployable (author activities in Gleam → `.beam` → package) — the flagship;
  (b) **native-Rust handlers ⇒ Tier-2 OS-process** (package the compiled binary
  content-addressed, `fork/exec` it — it already knows how to dial in);
  (c) wasm-compiled handlers ⇒ Tier-3 once a runtime exists.
- **Versioning:** the content hash **is** the version id already (#117); map it to a
  Temporal-style Build ID for `WorkerDeployment.version_behavior`. **Reject inventing
  a second versioning scheme.**
- **BUILD:** the worker-package manifest + builder extension; a Gleam-activity
  authoring path (or confirm one exists); the Tier-2 packaging+fork/exec driver
  (Phase 5).

### 5.2 Isolation honesty

- **Problem (verified):** BEAM per-process isolation is **fault/state** isolation
  (own heap, mailbox, capability set, namespace id; messages copied between heaps) —
  excellent for cooperative, operator-trusted multi-tenant workers, **not** a
  memory-isolation sandbox against hostile native code. The wasm tier that would
  provide that **does not exist in beamr** (no `wasmtime`/`wasmi`).
- **Approach:** ship Tier-1 for trusted/operator workers now; reserve the
  `isolation` field so policy can *demand* a stronger tier; treat "add a real wasm
  runtime to beamr" as a separate scoped project.
- **BUILD:** the `isolation` field + policy hook now; the wasm runtime later
  (SPIKE-FIRST, §7).

### 5.3 Resource limits

- **Problem (grounded):** beamr has **reduction-counting** (fair CPU scheduling) and
  per-process heaps, but not hard cgroup-style CPU/mem *caps* per worker.
- **Approach:** Tier-1 = per-process heap caps + reduction fairness + per-namespace
  **keyed backpressure/quotas** (CONTROL-PLANE.md §4.2 — generous defaults, NOT low
  hard-fails; avoid Temporal's `namespaceRPS=2400` mistake). Real CPU/mem hard limits
  ⇒ Tier-2 (cgroups) / Tier-3 (wasm fuel + memory limits) — another honest driver
  boundary. Bin-packing (§4) needs the `resources` declaration regardless.
- **BUILD:** the `resources` field consumed by bin-packing now; cgroup enforcement
  in the Tier-2 driver (Phase 5).

### 5.4 Cross-node artifact availability (the #117 gap — availability-before-spawn)

- **Problem (grounded, load-bearing):** package records are **plain local haematite
  commits**, non-quorum — `crates/aion-store-haematite/src/store.rs:271`: "packages,
  routes, and the outbox stay local (Design B: the survivor ... rebuilt from
  replicated history)". Distribution today is **reload-from-shared-store-on-boot**
  (`reload_persisted_packages`), and there is **no on-demand peer-to-peer lazy-pull-
  by-hash RPC** in the tree. So before a node can spawn a worker, the artifact bytes
  must already be on that node.
- **Approach (head-on):**
  (a) **Now:** route **worker-package puts through haematite's replication/sync** so
      the artifact is durably present cluster-wide before placement targets a node
      (acceptable: worker artifacts are desired-state, not hot per-run blobs).
  (b) **Long-term substrate:** build the **content-addressed lazy-pull-by-hash RPC**
      the thesis assumed but the code lacks — a node missing a hash fetches it from a
      peer/owner by content hash, verifying on receipt. **This is the one genuinely
      missing substrate piece; name it as such.** **SPIKE-FIRST** (§7).
  (c) **Always:** the reconciler **gates spawn on artifact-present** (materialise-
      and-verify-hash first, like `reload_persisted_packages`), so a not-yet-
      replicated artifact *delays* placement rather than crashing it.
- **BUILD:** (a) the replicated-put path for worker packages; (c) the spawn gate;
  (b) the lazy-pull RPC when (a) hits its limits.

### 5.5 Secrets

- **Problem:** content hash is **public identity** and artifacts are **replicated** ⇒
  a secret baked into an artifact is a leaked, replicated secret.
- **Approach:** **never** put secrets in the artifact. A per-namespace secret store
  (haematite-encrypted or an external-provider ref); the reconciler **injects secrets
  at spawn time** as process capabilities/env, scoped by the namespace grant.
  Rotation = re-spawn (cheap for BEAM processes) or capability refresh, never a
  re-deploy.
- **BUILD:** the per-namespace secret store + spawn-time injection (Phase 5).

### 5.6 Split-brain / double-placement + supervision + drain

- **Problem:** two nodes both spawning a deployment's Nth replica during a partition;
  keeping N alive across crash and node death.
- **Supervision (keep-alive):** beamr supervisor restart on crash; node death →
  liminal liveness marks node down (#146 sensor) → haematite reconciler re-places the
  replicas onto survivors (Nomad-style reschedule). The `kill -9` substrate is
  **proven** (#157; ROADMAP.md:34-35).
- **Split-brain (head-on):** reuse the **same shard-ownership/quorum machinery that
  fixed #157** — per-node assignments are **owned-shard-scoped durable state**
  (`owned_shards`, `crates/aion-store-haematite/src/store.rs:206`); a node only
  spawns for assignments it **owns**; placement writes go through the **quorum path**
  so a healed partition converges to one assignment and the reconciler kills surplus.
  **Note (grounded):** package *puts* are currently non-quorum (§5.4) — the
  *placement/assignment* records **must** be quorum even if the artifact bytes ride
  replication.
- **BUILD:** the owned-shard-scoped assignment record + quorum write + kill-surplus
  reconcile branch.

### 5.7 Drain respecting in-flight work + in-flight-aware autoscale (the moat)

- **Drain (the Temporal-Pinned mechanism, done in-VM):** to drain a node or roll a
  version, mark target instances **draining** — they **stop accepting new tasks**
  (deregister from routing / return "no capacity" on poll) but **keep serving
  in-flight activities to completion**, then exit. `version_behavior: Pinned` means
  existing workflows finish on the old artifact version (old code retained via beamr
  `ModuleVersions.old`, `crates/beamr/src/module.rs:220`, until `purge_module`,
  `module_management.rs:70`); new starts route to the new version. This is exactly
  Temporal Pinned/Auto-Upgrade + OTP hot-code purge, unified.
- **Autoscale that respects in-flight work (the explicit keda#7368 fix):** scale on
  task-queue backlog **AND** in-flight execution count, and **never scale an instance
  with running activities to zero** — an instance with running work is not idle by
  definition. Because aion **owns the durable execution state**, it knows the true
  in-flight count (unlike KEDA, which sees only queue depth). Scale-down = mark
  surplus instances draining, let them finish, then remove. **This is the single
  clearest place aion is structurally better than the k8s/KEDA stack, and the demo
  beat.**
- **BUILD:** the draining state on the worker/registry; the in-flight-count read the
  autoscaler keys on; the never-scale-below-in-flight guard.

---

## 6. Gating spikes (negative controls before build)

Named spikes that must pass **before** the corresponding phase is committed. Each is
a **negative control**: it is designed to *fail loudly* if the substrate assumption
is false, so we learn cheaply.

- **SPIKE-A — in-VM supervised spawn of a real registering worker.** Drive beamr
  `hot_load_module` + `spawn` under a supervisor to start a trivial worker process
  that dials the *local* `stream_worker` path and appears in the registry; then
  `kill` the beamr process and assert the supervisor **restarts** it and it
  re-registers. *Fails if:* the register-and-serve loop can't be driven from an
  in-VM spawn, or supervision restart doesn't re-register. **Gates Phase 2.**
- **SPIKE-B — cross-node artifact availability.** Put a worker package on node A;
  assert node B can **materialise + hash-verify** it *without* a manual copy, via the
  replicated-put path (§5.4a). *Fails if:* Design-B local puts never reach B ⇒
  forces the lazy-pull RPC (§5.4b) earlier than hoped. **Gates Phase 3.**
- **SPIKE-C — quorum assignment under partition.** Two nodes, partition, both try to
  own the Nth replica; assert exactly one wins via the quorum/owned-shard path and
  the loser spawns nothing; heal and assert convergence + surplus-kill. Reuses the
  #157 harness (`lsub5b_osproc_kill9_failover` / `ss5_failover_demo`). *Fails if:*
  assignment writes aren't actually fenced ⇒ double-placement. **Gates Phase 3.**
- **SPIKE-D — in-flight-count truth + never-scale-below.** Start N instances with
  live activities; issue a scale-to-below-in-flight command; assert **no instance
  with running work is killed** and drain completes first. This is the keda#7368
  negative control. *Fails if:* the engine's in-flight count isn't readable per
  instance at scale-decision time. **Gates Phase 4.**
- **SPIKE-E (Tier-3 only, deferred) — real wasm runtime in beamr.** Stand up a
  `wasmtime`/`wasmi` sandbox executing one trivial user module with a fuel + memory
  cap. *Fails if:* the integration cost is larger than a driver — which is the honest
  expectation, hence Tier-3 is its own project. **Gates Phase 5's wasm driver only.**

---

## 7. Phased build decomposition

Sequenced to compose with #146 (cluster source-of-truth), #157 (failover proven),
[NAMESPACE-REGISTRY-PHASE-1.md] (Phase 1 of the control plane), and CONTROL-PLANE.md
Phase 2. Naming mirrors the house `CSOT-N` convention. **Each phase lands green on
its own** (the project's "land the safe part" rule) and is never a throwaway
sidestep. **Explicit dependency: WD-3 onward is gated on / shares the #146
durable-cluster-state substrate** (node inventory, quorum writes) with the
cluster-formation design.

- **WD-0 — Artifact envelope + decision lock.** Define the content-addressed
  worker-package manifest (`kind: worker`, `isolation`, `entrypoint`, served
  `(ns, tq, activity_types)`, `resources`); extend the #117 builder + deploy path to
  accept it. Lock §3 (Tier-1 BEAM default; Tiers 2/3 as drivers). Reserve the
  `isolation` field everywhere. *Small, foundational, no new subsystem, no spawn yet.*
- **WD-1 — `WorkerDeployment` desired-state record (haematite).** The durable object
  (§2.1), `GET/POST /worker-deployments`. Deploy = artifact upload + deployment
  upsert; **nothing spawns yet**. Proves the control-plane object end-to-end
  (list/route/observe). Folds into #146 / mirrors [NAMESPACE-REGISTRY-PHASE-1.md].
- **WD-2 — Single-node reconciler + beamr supervised spawn (Tier-1).** *Gated on
  SPIKE-A.* The per-node reconciler: materialise artifact from content-addressed
  store → verify hash → `hot_load_module` → spawn N **supervised** worker processes
  that dial the local `stream_worker` path; restart-on-crash via beamr supervision.
  This makes "deploy a worker" TRUE and multi-instance/multi-type on one node —
  **via the real content-addressed + supervised mechanism, not a hardcoded-binary
  sidestep** (satisfies the constraint: the honest primitive, just not yet
  multi-node).
- **WD-3 — Node inventory + cross-node placement + artifact availability.** *Gated on
  SPIKE-B, SPIKE-C; depends on #146.* Node-capacity records (#146
  `cluster/member/<node_id>`); bin-pack/spread/per-node placement over inventory;
  route worker-package puts through haematite replication (§5.4a) so artifacts are
  present before placement; **quorum the assignment records** (§5.6). Now genuinely
  **multi-node dynamic deployment** — the second moat's core landing.
- **WD-4 — Drain + versioning + in-flight-aware autoscale.** *Gated on SPIKE-D.*
  Draining instances (finish in-flight, stop new); `Pinned`/`AutoUpgrade` rollout via
  beamr `ModuleVersions`/`purge_module`; autoscale on backlog **AND** in-flight count
  with never-scale-below-in-flight (the explicit keda#7368 fix). Node-death
  re-placement on the #157 substrate. **The operational moat vs Temporal+KEDA.**
- **WD-5 — Driver Tiers 2/3 + secrets.** OS-process driver (cgroup limits,
  native-Rust workers, §5.1b); per-namespace secret injection at spawn (§5.5);
  wasm-sandbox driver **iff** SPIKE-E clears and a real wasm runtime is added to beamr
  (§3.3) — scoped as its own hard project, giving hostile-code isolation for
  untrusted multi-tenant workers.
- **Cross-cutting substrate — content-addressed lazy-pull-by-hash RPC (§5.4b).** The
  one piece the thesis assumed but the code lacks. Build it when WD-3's replication
  approach hits its limits (SPIKE-B tells you when).

---

## 8. Open decisions for Tom

1. **Confirm the driver-tier default: Tier-1 BEAM/Gleam as flagship.** This means the
   *cleanest, first-class* deployable is a **Gleam-authored activity worker**, and
   the **existing native-Rust `aion-worker` SDK becomes deployable only via Tier-2
   (OS-process, Phase 5 / WD-5)**. Alternative: promote Tier-2 to co-flagship in WD-2
   so the current SDK deploys sooner — at the cost of leading with the heavier,
   per-platform, weaker-supervision path. **Recommendation: keep Tier-1 as the
   flagship** (it uses everything that exists and is the elegant story), but confirm
   you're comfortable that "deploy the Rust worker you have today" waits for WD-5.

2. **Confirm in-flight-aware autoscale/drain is the headline.** This design treats
   §5.7 (never-scale-below-in-flight; the keda#7368 fix aion can make because it owns
   execution state) as *the* differentiating beat and the demo. **Recommendation:
   yes — it is the one capability no competitor structurally has.** Confirm so WD-4
   is scoped as a marquee phase, not a tail.

3. **Artifact distribution posture for WD-3: replicated-put now vs lazy-pull-first.**
   §5.4 recommends (a) replicated worker-package puts now, (b) the lazy-pull-by-hash
   RPC as the proper long-term substrate. If you expect large artifacts or a wide
   cluster soon, we may want to build the lazy-pull RPC *first* rather than after
   replication hits its limits. **Recommendation: replicated-put now** (artifacts are
   desired-state, not hot blobs), lazy-pull when SPIKE-B says replication is
   insufficient.

4. **Sequencing vs #146 and the Sydney demo.** WD-3+ is gated on #146's durable node
   inventory + quorum writes. Confirm the order: **Sydney failover demo (#118) →
   #146 substrate → NAMESPACE-REGISTRY Phase 1 → WD-0..2 (single-node, honest
   primitive) → WD-3+ (multi-node moat).** WD-0..2 can proceed in parallel with #146
   because they don't need cross-node state; WD-3 blocks on it.

5. **Gleam-activity authoring path — does it exist, or is it WD-0 work?** Tier-1's
   flagship assumes activities can be authored in Gleam → `.beam` today. If that path
   isn't first-class yet, it becomes part of WD-0 scope (and a small spike). Flagging
   as an unknown to confirm before committing WD-2's timeline.

---

## Appendix — grounding (the code this design read and verified directly)

Every citation below was opened and confirmed in this pass (line numbers current as
of 2026-07-01; the research doc's claims were re-verified, not trusted).

- **Content-addressed artifact (#117):** `crates/aion-package/src/hash.rs`
  (`content_hash` over exact `.beam` bytes, canonical order; `IntegrityMismatch` on
  load); `crates/aion-store/src/package.rs` (`trait PackageStore`, `PackageRecord`);
  `crates/aion-store-haematite/src/store.rs` (`impl PackageStore`; **:271** "packages,
  routes, and the outbox stay local (Design B)" — the non-quorum-put caveat; **:206**
  `owned_shards`); `crates/aion/src/loader/persistence.rs` (`reload_persisted_packages`
  — materialise + re-verify hash; no lazy-pull RPC in tree).
- **Deploy path:** `crates/aion-server/src/api/http/router.rs:137-141` (mounted only
  when `deploy.enabled`); `crates/aion-server/src/api/handlers/deploy.rs:57,102`
  (`load_package` → `engine.load_package`); `DeployGuard` operator gate.
- **Worker SDK reality:** `crates/aion-worker/src/worker.rs:28` (`WorkerBuilder`),
  **:47** (`register_activity` — compile-time closure registration), **:143**
  (`Worker::run` register-and-serve loop). The worker is a native Rust binary.
- **Registry + routing:** `crates/aion-server/src/worker/registry.rs:77`
  (`PoolAddress`), **:155** (`WorkerHandle`), **:323** (`accept_registration` — no
  spawn/launch/provision verb exists), **:415/:424** (`enforce_pinned_placement` reads
  `NamespacePlacement::Pinned { nodes }`), **:627** (`workers_for` round-robin),
  **:665** (`all_workers` read-only), **:860** (`worker_matches_node`),
  **:245/:574/:781** (WS3 `WorkerConnected`/`WorkerDisconnected` feed).
  gRPC handshake: `crates/aion-server/src/api/worker_grpc.rs` (`stream_worker`, first
  message `RegisterWorker`; server never dials out).
- **beamr distributed supervised spawn:** `crates/beamr/src/distribution/control.rs:388-390`
  (`SPAWN_REQUEST=29`/`SPAWN_REPLY=31`; `handle_spawn_request` at `:558` →
  `spawn_with_options`); `crates/beamr/src/native/context/mod.rs:237`
  (`RemoteSpawnFacility::remote_spawn`, wired to BIFs in
  `native/process_bifs/mod.rs:357`); supervision death rule at
  `supervision/link.rs:171`, EXIT-message-to-trapping-process at `:145`, scheduler
  restart path `scheduler/supervision_integration.rs:43` (`propagate_exit`) →
  `:366` (`process_exit_signal`); connection-down → `NoConnection` exits for remote
  links at `supervision_integration.rs:307`.
- **beamr hot code loading:** `crates/beamr/src/module.rs:220` (`ModuleVersions {
  current, old }`); `crates/beamr/src/scheduler/module_management.rs:18`
  (`hot_load_module`), **:40** (`hot_load_module_in`), **:70** (`purge_module`).
- **beamr wasm CORRECTION (verified):** **no `wasmtime`/`wasmi`/`wasi` dependency in
  any beamr `Cargo.toml`** (direct grep, empty result). `scheduler/wasm.rs` = beamr VM
  compiled to wasm; `scheduler/wasm_native.rs` = cooperative native-process runtime,
  not user-wasm execution. beamr canNOT sandbox arbitrary user wasm today.
- **#146 substrate this folds into:** `docs/design/HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md`
  (**:46-52** beamr-is-a-sensor; **:130-137** `cluster/members` + `cluster/member/<node_id>`
  per-member detail = the node-inventory seam; **:426-441** CSOT phasing this mirrors).
- **#157 failover proven:** `docs/ROADMAP.md:34-35` ("Cross-node `kill -9` failover
  WORKS and is proven"); harnesses `lsub5b_osproc_kill9_failover`, `ss5_failover_demo`.
- **Control-plane thesis + KEDA footgun:** `docs/design/CONTROL-PLANE.md` §1.3
  (second moat), §4.2 (generous keyed quotas, not low hard-fails), §7 Phase 3;
  kedacore/keda#7368 (scale-to-zero-with-work-in-flight).
