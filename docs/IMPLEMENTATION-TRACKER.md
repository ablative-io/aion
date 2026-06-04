# Aion Implementation Tracker

117 briefs across 12 clusters, dispatched in 42 waves across 6 phases.
Peak parallelism: 10 briefs (Wave 17, engine core).

## Team Assignments

| Owner | Clusters | Briefs | Phases |
|-------|----------|--------|--------|
| **Lemmy** | AC, AP, AS, AF, AN | 40 | 1, 2, 4 |
| **Charge** | AE, AD, AT | 36 | 3 |
| **Max Power** | AW, AR, AL, AU | 41 | 5, 6 |

## Dispatch Command

Each brief is executed via the `onatopp-dev-norn` workflow. The dispatcher
constructs the command from the cluster's design directory.

```sh
meridian workflow run onatopp-dev-norn \
  --workspace 6152a6a8-9a90-4111-b2c7-54502a5d04e4 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --worktree \
  --input "brief=$(cat <BRIEF_PATH>)" \
  --input "design_content=$(cat <DESIGN_DIR>/design.json)" \
  --input "checklist_content=$(cat <DESIGN_DIR>/checklist.json)" \
  --input "stories_content=$(cat <DESIGN_DIR>/stories.json)" \
  --input "notify=<DISPATCHER_NAME>"
```

### Cluster Path Reference

| Cluster | Design Dir | Brief Pattern |
|---------|-----------|---------------|
| AC | `docs/design/aion-core` | `briefs/AC-NNN.json` |
| AP | `docs/design/aion-package` | `briefs/AP-NNN.json` |
| AS | `docs/design/aion-store-libsql` | `briefs/AS-NNN.json` |
| AE | `docs/design/aion-engine` | `briefs/AE-NNN.json` |
| AD | `docs/design/aion-durability` | `briefs/AD-NNN.json` |
| AT | `docs/design/aion-time-signals` | `briefs/AT-NNN.json` |
| AF | `docs/design/aion-flow` | `briefs/AF-NNN.json` |
| AN | `docs/design/aion-nif` | `briefs/AN-NNN.json` |
| AW | `docs/design/aion-server` | `briefs/AW-NNN.json` |
| AR | `docs/design/aion-workers` | `briefs/AR-NNN.json` |
| AL | `docs/design/aion-clients` | `briefs/AL-NNN.json` |
| AU | `docs/design/aion-dashboard` | `briefs/AU-NNN.json` |

---

## Phase 0 — Scaffold (prerequisite)

- [x] Run `tools/scaffold.py` to generate workspace skeleton
- [x] Commit scaffold output so `cargo check` passes for every worktree

---

## Phase 1 — AC (aion-core) · Lemmy · 7 briefs

The keystone. Everything else depends on AC being right.

### Wave 1
- [x] **AC-001** — aion-core crate scaffold, identifiers, payload · _landed 736dc1f_

### Wave 2
- [x] **AC-004** — Error taxonomy (activity/workflow errors) · _landed d7137b5_

### Wave 3
- [x] **AC-002** — Event model · _landed 113dbab_

### Wave 4
- [x] **AC-003** — Workflow status, filters, summaries · _landed c758486_

### Wave 5
- [x] **AC-005** — EventStore trait, StoreError · _landed 967223b_

### Wave 6
- [x] **AC-006** — InMemoryStore reference implementation · _landed 17ba2c9_

### Wave 7
- [x] **AC-007** — Shared EventStore behavioural test suite · _landed 3049bc4_

---

## Phase 2 — AP + AS (parallel tracks) · Lemmy · 15 briefs

AP (aion-package) and AS (aion-store-libsql) are independent of each other.
Both depend on AC types.

### Wave 8 (2 parallel)
- [x] **AP-001** — Crate scaffold, error taxonomy, beam set types · _landed 9d776ba_
- [x] **AS-001** — Crate scaffold, config, error mapping · _landed fb5b644_

### Wave 9 (3 parallel)
- [x] **AP-002** — Manifest model and format versioning · _landed 0208d9e_
- [x] **AP-003** — Content-hash versioning (canonical beam set) · _landed db3c9eb_
- [x] **AS-002** — libSQL connection and idempotent schema · _landed 349bf27_

### Wave 10 (3 parallel)
- [x] **AP-004** — Content-hash module namespacing scheme · _landed 48acb73_
- [x] **AP-005** — PackageBuilder (deterministic write path) · _landed 2b75207_
- [x] **AS-003** — LibSqlStore struct and EventStore wiring · _landed 73e071f_

### Wave 11 (3 parallel)
- [x] **AP-006** — Package::load (read path, integrity check) · _landed b9b0053_
- [x] **AS-004** — Atomic append with expected-sequence guard · _landed bfdf2c8_
- [x] **AS-006** — Durable timers (schedule/expired) · _landed e7cd99e_

### Wave 12 (2 parallel)
- [x] **AP-007** — Version record and round-trip conformance · _landed a727e26_
- [x] **AS-005** — read_history, list_active, query · _landed 7271697_

### Wave 13
- [x] **AS-007** — Conformance suite and persistence tests · _landed 354af1c_

### Wave 14
- [x] **AS-008** — Embedded-replica sync · _landed 29e9bb7_

---

## Phase 3 — AE + AD + AT (engine core) · Charge · 36 briefs

All three clusters share the `aion` crate. AE-001 is the common root.
Cross-cluster bridge: AE-008 depends on AD-002 (Recorder).

### Wave 15
- [x] **AE-001** — aion crate scaffold, EngineError taxonomy · _landed a8ff4db_

### Wave 16 (4 parallel)
- [ ] **AE-002** — beamr runtime embedding, RuntimeHandle · _depends: AE-001_
- [ ] **AE-004** — Active-execution registry, WorkflowHandle · _depends: AE-001_
- [ ] **AD-001** — Durability module scaffold, error taxonomy · _depends: AE-001_
- [ ] **AT-001** — Engine seam, module scaffolding · _depends: AE-001_

### Wave 17 (10 parallel — peak)
- [ ] **AE-003** — NIF registration surface · _depends: AE-002_
- [ ] **AE-005** — Module loading, content-hash namespacing · _depends: AE-002_
- [ ] **AE-006** — Three-level supervision tree · _depends: AE-002_
- [ ] **AD-002** — Recorder, single-writer append path · _depends: AD-001_
- [ ] **AD-003** — History cursor, correlation keys · _depends: AD-001_
- [ ] **AD-006** — Determinism context (recorded-now, seeded RNG) · _depends: AD-001_
- [ ] **AT-002** — Durable timer service (schedule, wheel, fire) · _depends: AT-001_
- [ ] **AT-005** — Signal router (record-then-deliver) · _depends: AT-001_
- [ ] **AT-007** — Query service (non-recording dispatch) · _depends: AT-001_
- [ ] **AT-008** — Child workflow spawning (linked process) · _depends: AT-001_

### Wave 18 (8 parallel)
- [ ] **AE-007** — In-VM activity dispatch · _depends: AE-002, AE-003, AE-006_
- [ ] **AE-008** — Workflow start lifecycle · _depends: AE-004, AE-005, AE-006, AD-002_
- [ ] **AE-011** — EngineBuilder and build() · _depends: AE-003, AE-004, AE-005, AE-006_
- [ ] **AD-004** — Command, Resolution, Resolver · _depends: AD-003_
- [ ] **AT-003** — Timer recovery (startup sweep, periodic tick) · _depends: AT-002_
- [ ] **AT-004** — Named cancellable timers, anonymous sleeps · _depends: AT-002_
- [ ] **AT-006** — Non-resident signal delivery, terminal handling · _depends: AT-005_
- [ ] **AT-009** — Concurrency correlation, cancellation · _depends: AT-008_

### Wave 19 (6 parallel)
- [ ] **AE-009** — Cancel, complete, fail lifecycle · _depends: AE-008, AD-002_
- [ ] **AE-010** — Suspend and resume lifecycle · _depends: AE-008_
- [ ] **AD-005** — Non-determinism violation detection · _depends: AD-004, AD-002_
- [ ] **AD-007** — Live executor seam, resume-live handoff · _depends: AD-004, AD-002_
- [ ] **AT-010** — all (fan-out, ordered collect, fail-fast) · _depends: AT-009_
- [ ] **AT-011** — race (first result, cancel rest) · _depends: AT-009_

### Wave 20 (3 parallel)
- [ ] **AE-012** — Engine API (start, cancel, result, list, shutdown) · _depends: AE-008, AE-009, AE-011_
- [ ] **AD-008** — Replay driver, activity-result caching · _depends: AD-004, AD-006, AD-007_
- [ ] **AT-012** — map (dynamic fan-out) · _depends: AT-010_

### Wave 21 (3 parallel)
- [ ] **AE-013** — Delegated signal, query, subscribe · _depends: AE-012_
- [ ] **AD-009** — Recovery on startup · _depends: AD-008, AD-002_
- [ ] **AD-010** — End-to-end record-then-replay round-trip · _depends: AD-008, AD-006, AD-005_

### Wave 22
- [ ] **AE-014** — End-to-end integration tests · _depends: AE-012, AE-007_

---

## Phase 4+5 — AF + AN + AW (parallel tracks) · Lemmy (AF/AN) + Max Power (AW) · 31 briefs

AF, AN, and AW are independent of each other. All depend on Phase 3 (engine core).
These three tracks run concurrently.

### Wave 23 (3 parallel)
- [ ] **AF-001** — Package scaffold, @external binding layer · _Lemmy_
- [ ] **AN-001** — aion-nif scaffold, error taxonomy, FFI seam · _Lemmy_
- [ ] **AW-001** — aion-proto scaffold, tonic-build wiring · _Max Power_

### Wave 24 (3 parallel)
- [ ] **AF-002** — Codec and Duration primitives · _Lemmy · depends: AF-001_
- [ ] **AN-002** — FromTerm/IntoTerm conversion · _Lemmy · depends: AN-001_
- [ ] **AW-002** — Common wire types, proto conversions · _Max Power · depends: AW-001_

### Wave 25 (5 parallel)
- [ ] **AF-003** — Error taxonomy (ActivityError, engine failures) · _Lemmy · depends: AF-002_
- [ ] **AN-003** — Payload and JSON bridge · _Lemmy · depends: AN-002_
- [ ] **AW-003** — Workflow-management service definition · _Max Power · depends: AW-002_
- [ ] **AW-004** — Event-streaming wire shape · _Max Power · depends: AW-002_
- [ ] **AW-005** — Remote-worker protocol definition · _Max Power · depends: AW-002_

### Wave 26 (3 parallel)
- [ ] **AF-004** — Activities (typed Activity, RetryPolicy) · _Lemmy · depends: AF-003_
- [ ] **AN-004** — Nif descriptor, deterministic_nif declaration · _Lemmy · depends: AN-002, AN-003_
- [ ] **AW-006** — aion-server scaffold, config, shared state · _Max Power · depends: AW-003, AW-004, AW-005_

### Wave 27 (3 parallel)
- [ ] **AF-005** — Workflow core (run, now, random, entry) · _Lemmy · depends: AF-004_
- [ ] **AN-005** — activity_nif (side-effectful path) · _Lemmy · depends: AN-004_
- [ ] **AW-007** — Namespace isolation and guard · _Max Power · depends: AW-006_

### Wave 28 (5 parallel)
- [ ] **AF-006** — Timers (sleep, start_timer, cancel_timer) · _Lemmy · depends: AF-005_
- [ ] **AN-006** — NifSet registration, illustrative set · _Lemmy · depends: AN-004, AN-005_
- [ ] **AW-008** — Workflow-management handler layer · _Max Power · depends: AW-007_
- [ ] **AW-010** — WebSocket event streaming · _Max Power · depends: AW-007, AW-004_
- [ ] **AW-011** — Remote-worker registry, task dispatch · _Max Power · depends: AW-007, AW-005_

### Wave 29 (3 parallel)
- [ ] **AF-007** — Signals (SignalRef, typed receive/send) · _Lemmy · depends: AF-006_
- [ ] **AW-009** — gRPC and HTTP transports · _Max Power · depends: AW-008_
- [ ] **AW-012** — Worker heartbeats, lost-worker handling · _Max Power · depends: AW-011_

### Wave 30 (2 parallel)
- [ ] **AF-008** — Queries (handler registration, reply) · _Lemmy · depends: AF-007_
- [ ] **AW-013** — Dashboard hosting (static assets) · _Max Power · depends: AW-009, AW-010_

### Wave 31
- [ ] **AF-009** — Child workflows (ChildHandle, spawn, await) · _Lemmy · depends: AF-008_

### Wave 32
- [ ] **AF-010** — Concurrency (all, race, map) · _Lemmy · depends: AF-009_

### Wave 33
- [ ] **AF-011** — Test harness (simulated time, mocking) · _Lemmy · depends: AF-010_

### Wave 34
- [ ] **AF-012** — Type-safety verification, end-to-end example · _Lemmy · depends: AF-011_

---

## Phase 6 — AR + AL + AU (parallel tracks) · Max Power · 28 briefs

Workers, clients, and dashboard. All depend on AW wire types.
Three independent tracks running concurrently.

### Wave 35 (3 parallel)
- [ ] **AR-001** — aion-worker (Rust) scaffold, config, session
- [ ] **AL-001** — Shared client contract, conformance scenarios
- [ ] **AU-001** — Dashboard app scaffold

### Wave 36 (6 parallel)
- [ ] **AR-002** — Task loop and concurrency · _depends: AR-001_
- [ ] **AL-002** — aion-client (Rust): scaffold, operations, errors · _depends: AL-001_
- [ ] **AL-004** — aion-client-python: Client, handle, payloads · _depends: AL-001_
- [ ] **AL-005** — aion-client-typescript: Client, handle, generics · _depends: AL-001_
- [ ] **AL-006** — aion_client (Gleam): operations, payloads · _depends: AL-001_
- [ ] **AU-002** — Generated wire types, typed REST client · _depends: AU-001_

### Wave 37 (6 parallel)
- [ ] **AR-003** — Dispatch and failure classification · _depends: AR-002_
- [ ] **AL-003** — aion-client (Rust): WorkflowHandle, payloads, stream · _depends: AL-002_
- [ ] **AU-003** — Aion event WebSocket manager · _depends: AU-002_
- [ ] **AU-004** — Workflow list view (filter, paginate) · _depends: AU-002_
- [ ] **AU-005** — Workflow detail (event history timeline) · _depends: AU-002_
- [ ] **AU-006** — Namespace selection, query scoping · _depends: AU-002_

### Wave 38 (7 parallel — peak)
- [ ] **AR-004** — Heartbeat and cancellation · _depends: AR-003_
- [ ] **AR-005** — Reconnect and re-report · _depends: AR-003_
- [ ] **AL-007** — Cross-language conformance harnesses · _depends: AL-001..006_
- [ ] **AL-008** — Packaging, metadata, READMEs, examples · _depends: AL-003..006_
- [ ] **AU-007** — App shell (routing, providers, layout) · _depends: AU-003..006_
- [ ] **AU-008** — Shared components (badge, icon, states) · _depends: AU-001, AU-003_
- [ ] **AU-009** — Live updates (list, timeline, resync) · _depends: AU-003..005_

### Wave 39
- [ ] **AR-006** — Typed Activity, Worker builder · _depends: AR-002..005_

### Wave 40 (2 parallel)
- [ ] **AR-007** — aion-worker-python: packaging, session, loop · _depends: AR-006_
- [ ] **AR-009** — aion-worker-typescript: packaging, session, loop · _depends: AR-006_

### Wave 41 (2 parallel)
- [ ] **AR-008** — aion-worker-python: @activity, context, errors · _depends: AR-007_
- [ ] **AR-010** — aion-worker-typescript: defineActivity, context · _depends: AR-009_

### Wave 42
- [ ] **AR-011** — Cross-SDK conformance suite · _depends: AR-006, AR-008, AR-010_

---

## Summary

| Phase | Clusters | Owner | Briefs | Waves | Peak Parallel |
|-------|----------|-------|--------|-------|---------------|
| 0 | Scaffold | — | — | 1 | — |
| 1 | AC | Lemmy | 7 | 7 | 1 |
| 2 | AP + AS | Lemmy | 15 | 7 | 3 |
| 3 | AE + AD + AT | Charge | 36 | 8 | 10 |
| 4+5 | AF + AN + AW | Lemmy + Max | 31 | 12 | 5 |
| 6 | AR + AL + AU | Max Power | 28 | 8 | 7 |
| **Total** | **12** | **3** | **117** | **42** | **10** |

**Critical path:** AC (7 waves, serial) → Phase 3 engine core (8 waves, heavy parallel) → AF linear tail (12 waves).

**Aggressive optimization:** Phase 6 can overlap with late Phase 4+5 — AL starts after AW-002 (Wave 24), AR after AW-005 (Wave 25), AU after AW-010 (Wave 28). This compresses the tail by up to 6 waves.
