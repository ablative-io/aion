# Node affinity — the third routing dimension (DESIGN)

> ✅ IMPLEMENTED (reconciled 2026-07-02). NODE-1..5 shipped. Verified: the `node`
> field is on the worker registration wire (`crates/aion-proto-generated/proto/worker.proto`,
> "optional locality affinity ... round-robin among them"), a worker serves a SET of
> namespaces (`repeated string namespaces`), and the dispatch pool key is
> `(namespace, task_queue, node)` in `aion-server/src/worker/registry.rs` with
> `worker_matches_node`/`ClaimScope` selection. Namespace-default placement over this axis
> (Prefer/Pinned) is Control-Plane Phase 2 (LANDED — see CONTROL-PLANE-PHASE-2.md). Design
> record retained below.
>
> Status (original): design locked 2026-06-28 (conversation with owner). Builds directly on the
> NSTQ rethread (namespace × task_queue, landed `6c0276fc`). This adds `node` as the
> third routing dimension and widens a worker to serve a SET of namespaces.

## 1. The model (locked)

Routing identity is a hierarchy of **free-form string** dimensions — NO preset/hardcoded
categories anywhere (`local`/`remote`/`gpu` are example operator values, not an enum):

```
namespace      correctness/isolation domain  (a workflow belongs to one; a WORKER serves a SET)
   └── task_queue   pool/flavour lane within  (free string; this is where local/remote/gpu live)
          └── node  locality affinity          (OPTIONAL; absent = any worker in the pool)
```

- **namespace** — domain/isolation tag, supplied at workflow start, recorded durably. No
  manifest declaration, no set-and-validate ceremony (it buys nothing). A worker advertises
  the SET of namespaces it serves; a workflow's dispatch only reaches workers whose set
  includes the workflow's namespace.
- **task_queue** — the routing lane within a namespace (free string; defaults to `"default"`).
- **node** — OPTIONAL locality affinity. **A node is a locality, NOT a worker process** —
  many worker processes may share a node id. Pinning to node `N` selects *any* worker on
  `N` (round-robin among them), not one exact process. Absent affinity = any worker in the
  `(namespace, task_queue)` pool.

## 2. Semantics (locked)

- **Pin = require** (the correctness primitive): a pinned step dispatches only to workers on
  that node; if none are available it waits / fails over per existing rules. "Prefer" (soft
  fallback to any) is a deliberate *later* addition — NOT built now (no zombie code), but the
  durable/wire shape is additive so it can be added without re-threading or re-migration.
- **node id** — a free-form string a worker advertises. Default = machine hostname; override
  with an explicit `--node` flag (alongside `--namespace` / `--task-queue`).
- **"good for all time"** — model the routing address as an extensible type; carry node via the
  established replay-safe patterns: additive nullable libSQL column (`ensure_*_column`) +
  `#[serde(default)]` on the history event. Old rows/histories decode to "no affinity"
  deterministically.

## 3. How it threads (mirror the NSTQ spine; node is OPTIONAL throughout)

- **Worker registration** advertises (set of namespaces, task_queue, node id). Registry indexes
  a worker under EACH of its namespaces; tracks node per worker.
- **Selection**: `(namespace, task_queue)` → candidate workers; if the dispatch pins a node,
  filter to workers on that node; round-robin within the result.
- **Durable carry**: optional `node` on the OutboxRow (additive column); recorded on
  `ActivityScheduled` in history (replay-safe `#[serde(default)]`).
- **SDK**: per-activity `activity.node(...)` selection (optional), threaded through the same
  resolve seam as task_queue.
- **Liminal**: a pinned dispatch publishes to a node-specific sub-channel
  `f(namespace, task_queue, node)`; unpinned stays `f(namespace, task_queue)`. The single
  channel-deriving function gains the optional node arg so dispatcher and subscriber can't drift.

## 4. Decomposition (spike-/smallest-first, full clippy bar, no shims)

- **NODE-1** — worker serves a SET of namespaces + node dimension core: worker config (`--node`
  default hostname; namespaces as a set), proto `RegisterWorker` (repeated namespaces + node),
  registry (multi-namespace index + node tracking + pin-aware selection), dispatch threads
  optional node. Routing address type stays a named, extensible struct.
- **NODE-2** — OutboxRow gains optional `node` (additive libSQL migration; haematite serde default).
- **NODE-3** — record node affinity on `ActivityScheduled` (replay-safe); recovery re-derives it.
- **NODE-4** — Gleam SDK `activity.node(...)` selection (optional), threaded via the resolve seam.
- **NODE-5** — liminal channel gains optional node sub-channel; corrects the channel fn + tests.

Dependencies: NODE-1 → NODE-2 → NODE-3; NODE-4 after NODE-3; NODE-5 after NODE-2.
NODE-4 and NODE-5 are disjoint and run in parallel.
