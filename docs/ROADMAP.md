# Aion Roadmap — current as of 2026-06-30

> **This is the canonical forward roadmap.** It reconciles three planning
> surfaces: the live task tracker, the strategic design docs
> ([`CONTROL-PLANE.md`](./design/CONTROL-PLANE.md),
> [`HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md`](./design/HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md),
> [`DURABLE-AGENTS-AS-INFRASTRUCTURE.md`](./DURABLE-AGENTS-AS-INFRASTRUCTURE.md)),
> and the still-open detail from prior roadmaps. The machine ledger
> (`docs/design/roadmap.json`) remains authoritative for fine-grained dispatch
> status. Task numbers (`#NNN`) refer to the working task tracker.

## North Star — the No. 1 outcome

A **self-contained, multi-tenant, compute-aware durable-execution platform**:
the only single binary (embedded haematite, no Cassandra/ES/Postgres) that ships
a first-class multi-tenant control plane AND places/supervises worker compute —
the unclaimed quadrant vs Temporal/Inngest/Restate/DBOS/Hatchet
([`CONTROL-PLANE.md`](./design/CONTROL-PLANE.md) §1). Three pillars + two freebies:

| Pillar | State |
|---|---|
| **1. One binary, no external deps** | ✅ shipped — single `aion` binary, embedded haematite, embedded ops console |
| **2. Namespaces as a real tenant boundary** | 🔜 the control-plane build (Track B) |
| **3. Manage/place compute** | 🔜 the second moat (Track B Phase 3) |
| *Freebie:* visibility without Elasticsearch | ✅ haematite-durable + real-time socket ops console |
| *Freebie:* no-determinism authoring for agents | ⚠️ to verify against our engine (Track B, decision) |

## Where we are (the foundation — shipped & proven)

- **Durable execution**: workflows, durable timers, signals, queries, outbox
  fan-out, continue-as-new, package deploy. Published stack on crates.io (0.8.0).
- **Distribution & failover**: active-active haematite (quorum + epoch fence +
  shard election), cross-node request routing, liminal `namespace × task_queue
  × node` dispatch. **Cross-node `kill -9` failover WORKS and is proven** — both
  the parked-timer and fan-out OS-process kill-9 gates pass (#157, #148, #109).
  *(This supersedes the prior roadmap's "do not claim multi-node scale-out" —
  hard-kill cross-node failover is now demonstrable. Horizontal throughput
  scale-out at large node counts remains unproven; see Track D.)*
- **Ops console out-of-box**: embedded-by-default (no flag), auth-off operator
  mode, deploy-granted by default, live namespace list, real-time socket feed,
  properly named "Ops Console" (#154/#155/#156).
- **Routing**: namespace/task_queue/node split + affinity (NSTQ #84-89, NODE
  #91-100, LSUB #101-112).

---

## Track A — Demo & proof (prove the thesis live)

The near-term credibility work. Pillar 1 is provable *today*.

- **#118 Sydney failover demo** — laptop → Tailscale → dogfood; the visceral
  "kill it, watch it recover, correct result" beat. *Now unblocked by #157.*
  Needs Tom (hardware/Tailscale/login). **Proves pillar 1 live.**
- **#121 L2-min: real Norn agent surviving kill-9** — a real agent activity on
  the cluster, surviving a hard kill. Builds on #118.
- **#133 GOAL: all 5 demo capabilities LIVE-verified in the ops console** —
  deploy ✅ (#136), start ✅ (#137), remote worker ✅ (#139); **#138 target
  namespace + task_queue from the UI** is the one unverified capability.
- **Proof portfolio** (from prior roadmap §2, still open): `docs/CLAIMS.md`
  claims→receipt ledger; **chaos gate** (kill the server at random points,
  assert byte-identical history + zero re-executed activities); recorded
  asciinema demos; published benchmark numbers (`benchmarks/million-processes`
  exists, never published); honest Temporal side-by-side (same saga, both
  systems, LOC + infra footprint).

## Track B — Control plane (pillars 2 & 3) — [`CONTROL-PLANE.md`](./design/CONTROL-PLANE.md)

The strategic build. Design done; phased.

- **Decision (now): determinism/authoring model** — verify whether our engine is
  replay-deterministic (Temporal footgun) or memoization-friendly (Inngest-style,
  an agent win). Shapes the whole agent story. (`CONTROL-PLANE.md` §5.)
- **Decision (now): default shard count** — raise from 1 (validate value vs perf
  audit #47); the immutable-`shard_count` trap means single-node deployments
  can't grow to a cluster without this. (`CONTROL-PLANE.md` §6.)
- **Phase 1 — Namespace registry as durable haematite state**: minted-on-use
  (worker registration upserts), `{name, created_at, last_seen, origin, config,
  placement}`, `GET /namespaces` returns the live set, loud "namespace created"
  event, `auto_create=open|closed` policy. Folds into #146.
- **Phase 2 — Namespace as a real boundary**: auth-consistent checks at every
  hop (CVE-2025-14986 lesson), per-namespace keyed backpressure (not hard-fail
  limits).
- **Phase 3 — Compute management (second moat)**: beamr-supervised worker fleets
  placed per namespace, kept alive across kill-9, autoscaling that respects
  in-flight work. Subsumes the "distributed worker deploy" direction
  (registry → leasing → remote deploy).
- **Phase 4 — Observability scoped by namespace**: the socket ops console per
  namespace (near-free given what's built).

## Track C — Correctness, hardening & CI

The "stop regressions silently rotting" track. **The keystone gap.**

- **Public full-gate CI** (from prior roadmap §2.2; STILL the biggest hole —
  only `ops-console-embed.yml` exists). fmt + clippy `-D` + `cargo test -p`
  every crate, on fresh clones, with `gleam` on the runner for example builds.
- **CI-harden the kill-9 gates** — the `#[ignore]d` parked-timer + fan-out
  kill-9 e2e tests pass now but aren't in CI; that's *exactly* how #157 rotted
  unseen. Add a slow/nightly lane so it can't silently regress.
- **#113** beamr's rare ~3% parallel lib-test flake — hunt it (will surface in
  CI once the gate exists).
- **#144 task_queue-on-start CONSUMPTION seam** — activities default to the
  recorded start-time queue (closes a routing half-gap).
- **Known flakes** (prior roadmap §6): `payload_binary…spawn` race,
  `examples_e2e` gleam `Incompatible locked version` race, SDK harness
  `with_timeout` zero-deadline limitation.

## Track D — Distribution & scale maturity

- **#146 → BUILD: dynamic membership / quorum denominator** — design landed;
  build it so quorum stops sizing in dead nodes (the fault-tolerance ceiling).
- **#147 Cluster auto-discovery** — mDNS-first cut for a laptop mesh.
- **#116 Aion fan-out/affinity + pluggable-storage maturity** (initiative) —
  haematite first-class distributed backend; storage backend pluggability.
- **Horizontal throughput scale-out** — unproven at large node counts; the
  honest gap to close before claiming it (ties to Track A's Temporal side-by-side).

## Track E — Authoring, SDK & DX (from prior roadmap §3)

- **CLI JSON ergonomics**: `@file` convention for `--input`/`--payload`,
  client-side schema validation before dispatch, `aion input <type>` skeleton
  emitter, polymorphic/inline-or-file input (directory form needs design).
- **`aion dev`** — watch mode: rebuild + repackage + hot-redeploy on change.
- **Ops-console run timeline** — per-run event timeline view (now an ops-console
  feature, not "dashboard").
- **Elixir SDK** — BEAM-native polyglot authoring (the strategic counter to
  Temporal's client-runtime polyglot; we never build client-side determinism
  cores).
- **Declarative DSL + visual builder** — on top of the typed SDK.
- **WASM workflow runtime** — long-term polyglot path (#125 haematite browser-IO
  remediation + banked beamr items are prerequisites).
- **#125 Haematite WASM browser-I/O remediation** — gates ops-console Phase 2 /
  browser-side workflows.
- **#130 Ops-console professional disciplines (ADR-015..019)**.

## Track F — Engine semantics (decisions + wiring, prior roadmap §1/§7)

- **Parent-close policy** — DECIDED (Tom, per-spawn `RequestCancel | Terminate
  | Abandon`, required arg); **implementation still queued** (no
  `ParentClosePolicy` in the engine yet). Propagate on all parent terminals,
  recursively; recovery re-arms pending propagations.
- **Worker heartbeats** — CONFIRMED WANTED. `HeartbeatTracker` exists but
  `fail_expired_workers` is not driven by any loop; wire it + `heartbeat(details)`
  queryable live state + heartbeat timeouts for hung activities.
- **Per-activity author-declared timeouts** — the `config` JSON seam already
  reaches the dispatch wait; thread an author timeout onto it (pairs with
  heartbeat timeouts).
- **Engine-side retry from `RetryPolicy`** — not wired; decide whether
  engine-side retry is wanted or the workflow-driven bounded-loop pattern is the
  permanent answer.
- **No-worker dispatch should PARK, not fail** — an activity with no connected
  worker currently fails the run; a durability engine should wait.
- **mint-or-resume on crash** (stacked-dev worker) — resume the same norn
  session on a kill-9 mid-`dev` instead of always minting.

## Track G — Hygiene, decisions & meridian

- **#150 DECISION (needs Tom): AD-012 reopen operation** — keep / finish / drop
  the WIP branch.
- **#149 Branch-thicket cleanup** — prune stale merged branches across all 4 repos.
- **#151 Refactor** — split the oversized `aion-server config/mod.rs` into modules.
- **#153 cargo-install UX** — `cargo install` from crates.io currently yields an
  API-only binary (no embedded console for the published-crate path); smooth it.
- **Meridian integration** (prior roadmap §5, Tom's prior focus) — `examples/
  stacked-dev` is the contract; remaining: multi-reviewer verdict wire
  (branch→run mapping), a Meridian worker serving the eight activity names, CLI
  contract discipline (schema'd machine output).

---

## Near-term sequence (recommended)

1. **#118 Sydney failover demo** (with Tom) — proves pillar 1 live; everything
   downstream rides on it being real.
2. **CI keystone** (Track C): full-gate CI + CI-harden the kill-9 gates — stop
   the silent-rot class of bug that produced #157.
3. **Control-plane decisions** (Track B): verify determinism model + decide the
   shard-count default — both are cheap and unblock the doc → build.
4. **Control-plane Phase 1**: the durable namespace registry (the spine that
   makes the ops console genuinely multi-namespace and retires the `default`
   stopgap).
5. Then iterate Track B Phases 2-4, with Track D/E/F items interleaved by need.

## Decisions needed from Tom

- **#150** AD-012 reopen operation: keep / finish / drop.
- **Shard-count default** value (Track B) — accept "generous power-of-two,
  validated vs #47", or pick a number.
- **Determinism authoring stance** (Track B) — once verified, confirm whether we
  lean into the no-determinism-for-agents positioning.
- **Engine-side retry** (Track F) — wanted, or is workflow-driven the answer?

## Coverage map (every open item → track)

| Item | Track |
|---|---|
| #118 Sydney demo, #121 Norn-on-cluster, #133/#138 demo-capabilities, proof portfolio | A |
| Control-plane Phases 1-4, determinism decision, shard-count decision, #146-build (registry side) | B |
| Public CI, CI-harden kill-9 gates, #113 flake, #144 consumption seam, known flakes | C |
| #146-build (dynamic membership), #147 auto-discovery, #116 fan-out/storage, scale-out proof | D |
| CLI ergonomics, `aion dev`, run timeline, Elixir SDK, DSL, WASM runtime, #125, #130 | E |
| Parent-close, heartbeats, per-activity timeouts, engine retry, no-worker-park, mint-or-resume | F |
| #150 AD-012, #149 branches, #151 config split, #153 install UX, Meridian integration | G |

*Last reconciled 2026-06-30 (post #157 failover fix + CONTROL-PLANE.md). When an
item lands, update this file and the ledger.*
