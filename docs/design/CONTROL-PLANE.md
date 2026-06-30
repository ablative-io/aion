# Aion Control Plane: Namespaces, Tenancy, and Compute Placement

> Status: **DESIGN** (2026-06-30). Companion to
> [`HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md`](./HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md)
> (#146). This doc defines the namespace/tenancy control-plane model, the design
> decisions behind it, the competitive white space it targets, and the phased
> build. It is the strategic spine for the "No. 1 outcome": a self-contained,
> multi-tenant, compute-aware durable-execution platform.

## 0. The one-line bet

**"Restate's single binary — with the real multi-tenant control plane Restate
doesn't have and everyone else hides in a SaaS — and it places your compute, not
just your tasks."**

Everyone in durable execution nailed the *execution*. Nobody nailed the
*control plane*. That is the opening.

## 1. Competitive landscape & the white space

Research (2024–2026) across Temporal, Inngest/Trigger.dev, Restate, DBOS,
Hatchet, Cadence, Conductor. Mapped on two axes — **self-contained ops** ×
**first-class multi-tenant control plane in the OSS artifact**:

| Engine | Single self-contained binary | Embedded durable log | Tenancy / namespace | Where tenancy lives |
|---|---|---|---|---|
| **Restate** (nearest mirror) | ✅ | ✅ Bifrost log | ❌ **none** | isolate = run another deployment |
| **DBOS** | ✅ library | ❌ (your Postgres) | ❌ OSS / ✅ Cloud | closed SaaS |
| **Hatchet** | ❌ engine+API+Postgres | ❌ (Postgres-as-log) | ✅ tenant+namespace | mostly Cloud-grade |
| **Cadence** | ❌ Cassandra+ES+4 svc | ❌ | ✅ domains (4000+ @ Uber) | heavyweight cluster |
| **Temporal** | ❌ DB+ES+4 svc | ❌ | ⚠️ logical label on shared shards | shared persistence |
| **aion (target)** | ✅ | ✅ haematite | ✅ ns › task_queue › node | **in the OSS binary** |

**The three things practitioners most wish these systems did differently —
independently surfaced by all three research streams — are exactly the three
aion is structurally built to own:**

1. **Ship as one deployable unit; kill the Cassandra/Elasticsearch/cluster tax.**
   Temporal's #1 complaint ("not a binary, a distributed system you operate";
   "the real cost is people — ~8 engineering-months on maintenance"). The entire
   Postgres-only competitor wave (Hatchet/DBOS/Inngest) exists because of this.
   → aion: **single binary, embedded haematite, no external store, no ES. Already true.**

2. **Make namespaces a REAL tenant boundary** — hard isolation + per-tenant
   quotas, not logical labels on shared shards. Temporal's *own docs* tell you
   NOT to use namespaces for multi-tenancy (shared shards → noisy-neighbor;
   CVE-2025-14986 was a cross-tenant confused-deputy bug in namespaces). Restate
   has *no* tenancy; DBOS/Hatchet gate the good model behind a SaaS.
   → aion: **the wedge** — ns›task_queue›node routing + haematite source-of-truth, in the OSS binary.

3. **Manage/scale/place workers; make safe deploys boring.** *All five* engines
   "manage dispatch, not compute" — BYO pollers or embedded; none place or
   supervise worker processes. Temporal even ships a worker autoscaler that can
   scale to zero with tasks in flight.
   → aion: **second moat** — beamr supervises + places compute.

Two near-free wins the research confirms are real pain elsewhere:

- **Visibility without Elasticsearch.** ES is a top-tier Temporal pain
  ("neither UI nor CLI works without it", version-pin hell). aion already has
  haematite-durable history + a real-time socket dashboard.
- **The activity boundary as the agent guarantee.** aion is replay-deterministic
  (Temporal-style — verified against the engine), but the load-bearing agent
  property is the **activity boundary**: every model/tool call is wrapped in an
  activity, recorded once on first execution, and *never re-run on recovery* (the
  recorded result is replayed). A `kill -9` mid-agent-run never re-invokes or
  re-bills a completed model call. That is the honest, still-strong positioning —
  not Inngest's "no replay rules" model, which is FALSE for our engine. (See §5,
  RESOLVED.)

## 2. What a namespace is (today vs target)

**Today:** a namespace is *not a managed object* — a routing+authorization label
carried on workflows and workers (`namespace × task_queue × node`). The server
runs one shared engine (`SharedEngine`), one configured `default`, and a
namespace "exists" implicitly the moment something uses it. Nothing validates
it; auth is the only gate. `GET /namespaces` returns just the configured default
(a stopgap so the UI selector isn't empty).

**Target:** the namespace is the **control-plane object the whole platform is
organized around** — the unit that carries access, placement, retention, quotas,
and observability scope. The ops console becomes "pick a namespace → here's
everything in it." It is **self-serve** (no admin priesthood) and
**self-contained** (state lives in haematite, not an external control-plane DB).

## 3. The namespace lifecycle model — minted-on-use, recorded durably

Creation is **dynamic and self-serve**, not a separate admin step:

- **Minted on first reference.** A worker registering for a namespace that does
  not exist yet *creates* it — an idempotent CAS **upsert** into a durable
  registry (two workers racing the same new namespace is fine). No
  pre-provisioning.
- **Recorded durably regardless.** Every namespace is recorded in a
  haematite-backed registry the moment it is first seen:
  `{ name, created_at, last_seen, origin, config, placement }`. This is what
  makes `GET /namespaces` return the *real, live* set (retiring the stopgap),
  gives the cluster observability, survives failover, and provides a future
  lockdown switch — all without an external system.
- **Governable when you want it.** A policy knob `auto_create = open | closed`
  (default **open**, matching zero-config philosophy). `closed` rejects unknown
  namespaces in prod. Openness now, governance later, no redesign.

### Where responsibility lands (the boundary question)

- **Server/cluster = registrar + source of truth.** Records durably, lists,
  routes, enforces policy. It does NOT pre-provision; it observes and records.
- **Worker = the minter.** A worker coming online with `(namespace, task_queue
  [, node])` brings the namespace into being (auth-scoped — see §4).
- **Workflow = lives in exactly one namespace, immutable once started** (NSTQ-6
  holds — per-*workflow* immutability; the *set* of namespaces is free to grow).

**Existence is anchored on STATE, not workers** (critical correction): a
namespace exists if it has durable state OR a live worker OR an explicit entry.
Worker-minting is *one* way it comes to exist — never the definition. This
avoids orphaned durable history in a namespace the registry has reaped.

## 4. The hard design decisions

These determine whether we beat the field or re-make its mistakes.

1. **Authorization-consistent end-to-end.** The CVE-2025-14986 lesson: a
   namespace must be checked at *every hop*, not just the outer request, or you
   get confused-deputy cross-tenant bugs. A worker's grant declares which
   namespaces it may mint/serve. Auth-off (single-tenant operator) → mint
   freely; auth-on → minting is a *scoped right*. Open-mint and tenant isolation
   only coexist if minting is auth-scoped.

2. **Per-tenant quotas as keyed backpressure, NOT low hard-fail limits.** Steal
   Inngest's per-key concurrency/throttle. AVOID Temporal's mistake (default
   `namespaceRPS=2400` that `RESOURCE_EXHAUSTED`s; users had to raise it 32×).
   Generous defaults, smooth backpressure, configurable per namespace.

3. **Logical isolation now; placement field reserved from day one.** Do NOT
   build physical per-namespace engines yet (resource blowup vs free-mint). But
   put a `placement` field in the registry entry immediately, so physical
   isolation / node-affinity is a later *policy*, not a *migration*.

4. **Shard-count is the immutable scale ceiling — choose it generously now.**
   See §6. A now-decision, not a later one.

5. **Determinism / authoring model — RESOLVED.** aion is replay-deterministic;
   the agent guarantee is the activity boundary (recorded-once, never-re-run). See §5.

## 5. RESOLVED: determinism / authoring model — replay-deterministic, with the activity boundary as the agent guarantee

**Verdict (verified by reading the engine, 2026-06-30):** aion is
**replay-deterministic** in the Temporal sense, **not** memoization-free in the
Inngest sense.

- On recovery the engine **re-runs the workflow function from the top**
  (`durability/recovery.rs` `spawn_workflow_with_policy`).
- Each primitive resolves against recorded history before any side effect
  (`runtime/nif_activity_dispatch.rs`): `ResolveOutcome::Recorded` short-circuits
  to the recorded result; only `ResumeLive` actually dispatches.
- A real command-stream mismatch raises `NonDeterminismError` and **terminates
  the run** (`durability/resolver.rs`) — the Temporal footgun is present.
- Workflow-visible `now()` / `random()` are engine-provided and seeded from
  recorded state (`runtime/nif_determinism.rs`), so they replay identically.

**The honest, still-strong positioning is the ACTIVITY BOUNDARY.** Every
model/tool call is wrapped in an activity, recorded once on first execution, and
**never re-run on recovery** (the recorded result is replayed). So a `kill -9`
mid-agent-run never re-invokes or re-bills a completed model call. That is the
real agent guarantee — and it is true of our engine.

**Design consequence:** keep **Temporal-style authoring guidance** — no
wall-clock, no RNG, no IO in the workflow body; all side effects go through
activities (and engine-provided `now()` / `random()` for time/entropy). Do **NOT**
adopt Inngest-style "no replay rules" positioning: it is FALSE for our engine and
would mis-sell the determinism contract. We claim recorded-once / never-re-run at
the activity boundary, not determinism-freedom.

## 6. Haematite shard-count: the Temporal `numHistoryShards` trap (VERIFIED)

**Finding (verified in haematite source, 2026-06-30):** haematite has Temporal's
exact `numHistoryShards` constraint.

- `shard_count` is chosen at `Database::create`, persisted (`write_config`), and
  read back **immutably** on `open` (`read_config`). Callers cannot change it.
- Routing is `BLAKE3(key) % shard_count` (`ids.rs:13`). Changing the modulus
  reshuffles every key → shard mapping.
- **No reshard / split / merge / resize path exists** anywhere in haematite.
- The **default single-node `aion server` boots with `shard_count = 1`**
  (`config/mod.rs:1148`) — so a default single-node deployment is *permanently
  pinned to 1 shard* and can never grow into a multi-node cluster without a full
  data migration. This directly undercuts aion's "start single-node, grow to a
  cluster" pitch.

**The architecture is RIGHT, only the default and immutability are wrong.**
haematite's `shard_count` is effectively a **virtual shard count**, and aion's
`acquire_shard` / owned-shards already does virtual-shard-range → node ownership
(nodes adopt ranges of shards — the failover machinery fixed in #157). That is
exactly how Temporal and well-designed systems scale: a fixed, large virtual
shard count, nodes own ranges, so adding nodes reassigns ranges with **no key
rehash**.

**Decisions:**

1. **Raise the default virtual shard count well above 1** even single-node — a
   generous power-of-two — so a single-node deployment can become a cluster by
   reassigning shard ranges (already supported) with zero rehash. The exact
   number is a trade-off (more shards = more parallelism + cluster headroom, but
   more per-commit fsync fan-out + memory on a laptop) — **validate against the
   perf audit (#47)**; the principle ("over-provision up front") is Temporal's
   own advice for the identical constraint.
2. **Document `shard_count` as the immutable scale ceiling** in config + docs
   ("choose for your max scale; it cannot change in place") — apply Temporal's
   lesson *proactively* so we never silently ship someone into the trap.
3. **In-place resharding (split/merge of the virtual count)** is a known
   haematite roadmap gap and the eventual proper fix — but NOT a blocker; the
   generous default makes it rarely needed (Temporal operates for years this way).
4. Namespace **placement** maps namespaces → node/shard-ranges against this
   generous virtual shard space, so placement has granularity. The namespace
   registry itself is unaffected (logical state in some shard).

## 7. Phased build

Grounded in what already exists (haematite source-of-truth #146, liminal
ns›tq›node routing, beamr supervision, #157 failover).

- **Phase 0 — This doc.** Plus verify §5 (determinism) and decide §6.1 (shard
  default) against #47.
- **Phase 1 — Namespace registry as durable haematite state.**
  Upsert-on-first-reference; `{name, created_at, last_seen, origin, config,
  placement}`; `GET /namespaces` returns the live set (retires the stopgap);
  loud "namespace created" event. Folds into #146.
- **Phase 2 — Namespace as a real boundary.** Auth-consistent checks at every
  hop (§4.1) + per-namespace keyed quotas (§4.2, generous backpressure).
- **Phase 3 — Compute management (second moat).** beamr-supervised worker
  fleets placed per namespace, kept alive across kill-9 (#157 substrate proven),
  autoscaling that respects in-flight work (do NOT repeat KEDA-scales-to-zero).
- **Phase 4 — Observability scoped by namespace.** The socket dashboard / ops
  console per namespace — near-free given what's built.
- **Cross-cutting now:** raise the default shard count (§6.1) — small change,
  outsized payoff (makes single-node→cluster actually true).

## 8. Sequencing vs the demo

The **Sydney failover demo (#118)** is now unblocked (#157 landed) and is
near-term. It is **not a detour** — it *proves pillar #1* ("single binary
survives kill-9"), which anchors this whole thesis. Order:

1. Get the Sydney failover demo over the line (proves the strategy live).
2. This design doc (done) + verify §5 + decide §6.1.
3. Build Phase 1 (the registry spine).

## 9. Things to steal (be fair to the competition)

- **Inngest**: zero-friction onboarding ("your deployment is the worker" as a
  first-class trivial-onboarding mode); event-native triggers + declarative
  flow-control (concurrency/throttle/debounce/batch as one-liners, with
  per-tenant keys). (NOT its no-replay-rules authoring — our engine is
  replay-deterministic; see §5.)
- **Restate**: **virtual objects** — keyed, single-writer stateful entities.
  This maps almost 1:1 onto a durable *agent* (keyed, single-writer, stateful,
  long-lived). Strong candidate primitive for the agent-native angle.
- **Hatchet**: explicit tenant + namespace + environment model; monitoring
  tables decoupled from queue tables (retention without bloating the hot path).
