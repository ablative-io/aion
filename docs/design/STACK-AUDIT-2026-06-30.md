# Ablative Stack Audit — Synthesized Work-List (2026-06-30)

Source: four structured audits of the ablative stack (beamr / haematite / liminal / aion) covering three themes:
1. **Test coverage** of beamr + haematite (the durable/failover-critical seams)
2. **Making haematite the default / first-class storage backend** in aion
3. **Making default Cargo features select OUR stack** (beamr / haematite / liminal) instead of external alternatives (libSQL / gRPC-tonic)

This document leads with the **Top priorities** (highest-risk correctness/durability test gaps + the default-backend / default-features changes Tom asked for first), then gives the full synthesized work-list grouped by theme, then the complete detail from all four underlying audits.

---

## Top priorities (do these first)

These are the items that combine the highest risk with what Tom explicitly asked to lead on: the most failover-relevant **test gaps** and the **default-backend / default-features** flips.

### Tier 0 — start here (3 things)

1. **Flip aion default Cargo features to our stack.** `aion-server` and `aion-cli` ship `default = []` and hard-link external libSQL + default gRPC; a stock/published binary cannot even run on haematite (selecting it is a boot error). This is one coordinated change that unblocks *every* defaulting fix below.
   - `crates/aion-server/Cargo.toml`: `default = ["haematite-backend", "liminal-transport"]`; make `aion-store-libsql` `optional = true` behind a new `libsql-backend` feature.
   - `crates/aion-cli/Cargo.toml`: `default = ["haematite-backend", "liminal-transport"]`.
   - (Critical × 2 from "default features = ours" + the gating critical from "haematite as default backend".)

2. **Make haematite the runtime default backend (with a default `data_dir`).** Change `StoreConfig::default()` from `Memory` (coerced to LibSql) to `Haematite`, and supply a default `data_dir` (e.g. `aion-data`) so `validate()` does not fail on an empty config. Keep `backend = "memory" | "libsql"` as explicit opt-ins; flip `OutboxTransport` default `Grpc → Liminal`; set `aion-client` `default = ["embedded"]`.
   - `crates/aion-server/src/config/mod.rs:966-977` (default), `:716-718` + `config/env.rs:36-37` (Memory→LibSql coercion), `:767-772` (validate data_dir guard), `:513-516` (`OutboxTransport`).

3. **Close the single most failover-relevant TEST gap: beamr has no proactive liveness (net-tick/heartbeat).** An idle link to a silently-partitioned peer never produces a `nodedown`, so pg purge, remote-monitor DOWN, and aion ownership-lease expiry never fire. Add the integration test that drops a peer socket at OS level with no FIN/RST and asserts `connection_down` fires within a bounded interval — a failing test is the signal to build the tick mechanism **before** trusting beamr for unattended cross-node failover.
   - `crates/beamr/src/distribution/connection.rs:799-829` (mark_down only on read error/EOF), `sender.rs:176` (write-timeout needs outbound traffic).

### Also lead-tier (durability test gaps + default-config flips)

- **haematite disk dir-fsync barrier is never crash-injected** (HIGH) — `store/disk.rs:116`; a lost-rename window could drop a published node. There is an active `fsync-nodedir` worktree confirming this is live.
- **haematite WASM persistence (OPFS/IndexedDB) has zero tests** (HIGH) — `src/wasm/*`, `src/store/opfs/*`, `src/store/indexeddb.rs`.
- **ETF wire decoder parses untrusted peer bytes with no fuzz/proptest** (HIGH) — `crates/beamr/src/etf/decode.rs`; no proptest harness exists anywhere in beamr.
- **`aion new` scaffold hard-codes `backend = "libsql"`** (HIGH) — `crates/aion-cli/templates/shared/aion.toml:11-13`; flip to haematite + `data_dir`.

---

## Synthesized work-list by theme

Within each theme, items are ordered by severity (critical → low).

### Theme 1 — Test coverage (beamr + haematite)

Severity counts: **1 critical, 5 high, 5 medium, 3 low** (14 findings across both crates).

| # | Sev | Action | Location | Why it matters |
|---|-----|--------|----------|----------------|
| 1.1 | **critical** | Add a net-tick/heartbeat liveness test: drop a peer socket at OS level with no FIN/RST while the link is idle, assert `connection_down` fires within a bounded tick interval. If no tick mechanism exists, build one. | beamr `distribution/connection.rs:799-829`, `sender.rs:176` | Idle link to a silently-partitioned peer never emits `nodedown` ⇒ pg purge / remote-monitor DOWN / aion lease expiry never fire. The #1 failover hazard. |
| 1.2 | high | Add proptest/fuzz over the ETF decoder: arbitrary bytes never panic/OOM, always `Ok`/typed `DecodeError`; round-trip `Term→encode→decode==orig`; allocation-bomb header returns `Truncated` not a giant pre-alloc. | beamr `etf/decode.rs`, `distribution/etf.rs`, `distribution/atom_cache.rs` | Decoder ingests peer/attacker-controlled bytes; no fuzz/proptest exists anywhere in beamr. |
| 1.3 | high | Direct unit tests on `WasmScheduler.run_until_idle`: priority drain order, multi-process summary counts, exit-result collection mid-round, `tick_native_timers` interleaving. | beamr `scheduler/wasm.rs:429,683` | Newest/least-mature subsystem; its core run loop has 0 inline tests. |
| 1.4 | high | wasm-bindgen-test round-trips for haematite OPFS/IndexedDB stores: write N nodes, reopen, assert byte-identical reads; torn OPFS frame rejected like a bad WAL CRC. Add pure `frame.rs` encode/decode unit test (testable off-wasm). | haematite `src/wasm/*`, `store/opfs/*`, `store/indexeddb.rs`, `store/opfs/frame.rs` | WASM durable-history persistence layer is entirely unverified. |
| 1.5 | high | Crash-inject the disk directory-fsync barrier: snapshot the dir BEFORE `sync_dirty_dirs`, assert recovery behaves correctly when the directory entry is absent (simulate the lost-rename window), not just that the dirty set drains. | haematite `store/disk.rs:116`, `tests/disk_store.rs` | Published rename can be lost on power loss; currently only bookkeeping is asserted. Active `fsync-nodedir` worktree confirms this is live. |
| 1.6 | medium | proptest the haematite wire codec: round-trip over arbitrary Write/BatchWrite/election messages; decode of random/truncated bytes never panics, returns `Err`. | haematite `sync_codec/wire/decode.rs`, `sync_codec/tests/*` | Decoder handles peer-controlled bytes backing replication; only example-based tests today. |
| 1.7 | medium | proptest union-merge commutativity AND associativity: random `(key, stamp, value|tombstone)` sets split across 2/3 roots, all merge orderings ⇒ identical root hash + identical reads. | haematite `sync/handoff_merge_tests.rs` | Convergence primitive for active-active recovery; same bug class as the prolly-tree history-independence issue, one layer up. |
| 1.8 | medium | Fault-injection tests for native transport glue + sync protocol driver via an in-memory fake transport that drops/reorders/duplicates frames; assert convergence/retry. | haematite `sync/transport_glue.rs`, `sync/protocol.rs`, `sync/mod.rs` | Adversarial delivery handling is unproven; e2e tests use cooperative transports. |
| 1.9 | medium | Unit tests on `db/receiver.rs` apply paths: stale-epoch proposal must be fenced (not persisted), mid-batch failure leaves no partial durable state, duplicate proposal is idempotent. | haematite `db/receiver.rs:494,524,630,709` | Durability-critical replicated-write apply path has no co-located tests; e2e won't deterministically hit these. |
| 1.10 | medium | beamr restart-intensity: either test that a tight crash-loop is eventually capped (stop restarting after N in T, propagate shutdown) or document that the runtime offers no crash-loop backstop. | beamr `native/supervision.rs` (no `max_restarts`/`restart_intensity` in runtime) | A crash-looping aion worker can spin forever — real availability hazard. |
| 1.11 | medium | End-to-end reconnection with state re-convergence: A+B share pg membership + remote monitor; B drops, A purges, B re-dials; assert membership/monitors match a fresh join (not silently resurrected). | beamr `connection.rs:1611`, `tests/pg_distribution_e2e.rs` | Reconnection proven only at socket layer; aion failover needs a recovered node to converge to correct group state. |
| 1.12 | low | TTL sweep crash-recovery: start a sweep, simulate crash before commit, run `recover_view`, assert no TTL entry is half-deleted. | haematite `ttl/sweep/recover.rs`, `ttl/sweep/tests.rs` | Mutates durable state; mid-run interruption untested. |
| 1.13 | low | Group-commit all-or-nothing: buffer a multi-write group, inject error at the commit fsync, assert none observable after recovery (no torn group). | haematite `wal/buffer.rs`, `shard/actor.rs`, `db/receiver.rs:630` | Partial-batch durability property untested in a single dedicated test. |
| 1.14 | low | Unit test `ControlRouter.send_link/send_unlink/send_exit`: assert buffered `ControlMessage` payloads have correct node/pid encoding + ordering. | beamr `distribution/remote_link.rs:66-107` | Link-propagation path supervision-across-nodes rides on has zero direct tests. |

### Theme 2 — Make haematite the default / first-class backend

Severity counts: **2 critical, 1 high, 2 medium, 1 low** (6 findings). Implementation is already first-class; this is defaulting + feature-gating, not engineering. libSQL stays an option throughout.

| # | Sev | Action | Location | Why it matters |
|---|-----|--------|----------|----------------|
| 2.1 | **critical** | Turn `haematite-backend` ON by default (add to `aion-server` `default` and `aion-cli` default features; at minimum add to aion-cli `release`). Keep it a named feature for slim opt-out; keep libSQL linked so `backend="libsql"` always works. | aion-server `Cargo.toml`; aion-cli `Cargo.toml:62-78` | Feature is OFF by default ⇒ stock/published binary literally cannot run on haematite; selecting it is a boot error (`state.rs:1074`). No other defaulting change matters until this lands. |
| 2.2 | **critical** | Change `StoreConfig::default().backend` to `Haematite` with a default `data_dir` (e.g. `aion-data`); update Memory→LibSql coercion so explicit libSQL is honored but the unconfigured durable path is haematite. | aion-server `config/mod.rs:966-977`, `:716-718`, `config/env.rs:36-37`; mind `validate()` at `:767-772` requiring `data_dir`. | Runtime default is Memory (coerced to LibSql); haematite is never selected unless an operator writes `backend="haematite"`. |
| 2.3 | high | Change `aion new` scaffold `[store]` to `backend = "haematite"` + `data_dir = "aion-data"`, libSQL kept as a commented alternative. | aion-cli `templates/shared/aion.toml:11-13` (emitted by `new/template.rs:56-69`) | Every freshly scaffolded project ships durable-on-libSQL instead of haematite. |
| 2.4 | medium | Switch `dev-config.toml` to `backend = "haematite"` + `data_dir` (memory only as explicit ephemeral choice). | `dev-config.toml:24-25` | Default dev config steers users away from haematite. |
| 2.5 | medium | Rewrite operations guide `[store]` section: haematite as first-class durable default (data_dir/shard_count, single-node vs `[store.cluster]`), libSQL as lightweight single-file alt, memory as ephemeral-only; env example `AION_STORE_BACKEND=haematite`. | `docs/guides/operations.md:52-55, 104-105` | Docs currently present memory as default and libSQL for durability; never name haematite as the durable default. |
| 2.6 | low | No config change to the demo generator (already correct); once 2.1 lands it stops needing `--features haematite-backend`. Until then ensure the demo build passes the feature. | `scripts/demo/lib.sh:166-178` | Only place haematite is already the wired default. |

### Theme 3 — Default Cargo features = ours (beamr / haematite / liminal)

Severity counts: **2 critical, 3 high, 1 medium, 3 low** (9 findings). Problem is entirely at the aion integration layer; leaf crates (beamr, haematite, liminal-sdk) are already ablative-by-default. Note 3.1 / 3.2 overlap with Theme 2 (2.1) — same Cargo edits, do once.

| # | Sev | Action | Location | Why it matters |
|---|-----|--------|----------|----------------|
| 3.1 | **critical** | aion-server: `default = ["haematite-backend", "liminal-transport"]`; make `aion-store-libsql` `optional = true` behind a new `libsql-backend` feature. | aion-server `Cargo.toml` | Default build links external libSQL non-optionally and never links our store/messaging. |
| 3.2 | **critical** | aion-cli: add `default = ["haematite-backend", "liminal-transport"]`. | aion-cli `Cargo.toml` | Shipped `aion` binary has no default features ⇒ links libSQL + gRPC, never haematite/liminal. |
| 3.3 | high | Flip `OutboxTransport` runtime default `Grpc → Liminal` (gated so when `liminal-transport` is compiled the default is Liminal). | aion-server `config/mod.rs:513-516` | Cross-node outbox defaults to external tonic even when liminal is available. |
| 3.4 | high | aion-client: `default = ["embedded"]` so the in-process ablative engine is the out-of-box path; gRPC/tonic remains available but not the sole default. | aion-client `Cargo.toml` | Embeddable client defaults to external gRPC; ablative in-process path is OFF. |
| 3.5 | high | After 3.1, gate `aion-store-libsql` behind the new `libsql-backend` feature so the external store only appears when explicitly selected (mirrors how haematite is gated today). | aion-store-libsql `Cargo.toml` | External SQLite-derived engine is currently always in the default graph. |
| 3.6 | medium | Replace the "byte-identical default build" invariant on both crates with the ablative-default set; keep `auth`, `embed-dashboard`/`release`, and the new `libsql-backend` as opt-in extras. | aion-server + aion-cli `Cargo.toml` (features) | "Byte-identical-to-external" is the wrong invariant given Tom's intent. |
| 3.7 | low | haematite core `default = []` — confirmed correct (native engine always-on via target-cfg; wasm opt-in). No change. | haematite `crates/haematite/Cargo.toml` | Audited; ablative engine is already the default surface. |
| 3.8 | low | liminal / liminal-server / liminal-sdk — confirmed correct (`liminal-sdk default = ["std"]` carries the real TCP transport). No change; gap is purely aion not SELECTing liminal. | liminal-sdk `Cargo.toml` (+ liminal/, liminal-server/) | Audited; ablative messaging default is right. |
| 3.9 | low | beamr main crate `default = ["std","threads","net","fs","jit","embedded"]` — the model to follow. No change. | beamr `crates/beamr/Cargo.toml` | Audited; ablative runtime on by default. |

---

## Full audit detail (all four reports)

### Audit A — beamr test coverage

**Current state.** beamr is broadly and in places deeply tested; Tom's blanket "coverage is lacking" is too pessimistic for most subsystems but correct about specific high-risk seams. 37 integration test files under `crates/beamr/tests/` plus extensive inline `#[cfg(test)]` modules.

- **Scheduler — strongly tested.** `scheduler/tests.rs` (57 tests, 3268 lines) covers lost-wakeup gaps (`delivery_in_the_wait_park_gap_is_not_a_lost_wakeup`, `dirty_resume_in_the_suspend_park_gap_is_not_lost`, `timer_expiry_in_the_wait_park_gap`), dead-pid orphan guards, fair scheduling, telemetry, tombstones. `run_queue.rs` (8), `steal.rs` (4). `mod.rs` has 0 inline tests but is exercised through tests.rs.
- **Distribution — connection layer best-tested.** `connection.rs` (22 tests, 2042 lines): HS-0 silent-peer handshake hang, HS-2 simultaneous-install dedup, HS-3 retry-storm avoidance, HS-4 re-dial-after-drop, wrong-cookie rejection, connection-down notify-once. `handshake.rs` (11). `sender.rs` (7): per-node FIFO, dead-peer non-stall, wedged-peer write-timeout drain. `pg.rs` 0 inline but `pg_tests.rs` (5) + `pg_distribution_e2e.rs` (5 tokio). `control_lifecycle`/`control_monitor` each 8-test files. Cross-node round-trip + 3-node mesh convergence covered.
- **Supervision — well tested.** `native_supervision.rs` (722 lines): cross-runtime exit propagation, monitor DOWN normal/abnormal, trap_exit, factory restart. `supervision/link.rs` (11), `monitor_tests.rs` (8), `scheduler/supervision_tests.rs` (28).
- **GC — solid.** `gc/tests.rs` (22): minor/major survival of tuples/cons/closures/proc_bins/sub-binaries/fd-resources, constant-pool roots, virtual-binary pressure, exception-root rewriting, process isolation. `major.rs` (3), `minor.rs` (2).
- **Native process/actor.** `native_process.rs` source 2 inline tests; integration tests (517 + 285 lines) cover beam<->native round-trips, mailbox park/wake, self-send, self-tick timers. `actor.rs` source 0 inline but actor_tests/actor_cooperative_tests/actor_dynamic_tests exist.
- **WASM runtime port — thinnest.** `scheduler/wasm.rs` (715 lines) 0 inline; covered indirectly by `wasm_tests.rs` (9) + `wasm_native_tests.rs` (13). `run_until_idle`, work draining, priority ordering, exit-result collection under-exercised.
- **ETF decoder** has bounds-checked error variants + 13 decode unit tests, but **NO proptest/quickcheck/fuzz harness anywhere in the codebase.**

**Findings:** see Theme 1 rows 1.1 (critical), 1.2/1.3 (high), 1.10/1.11 (medium), 1.14 (low).

**Summary.** beamr is far better tested than "coverage is lacking" implies. Failover-relevant gaps are narrow and sharp, ranked by risk to aion: (1) CRITICAL no proactive net-tick/heartbeat liveness; (2) HIGH ETF decoder no fuzz; (3) HIGH wasm run loop no direct tests; (4) MEDIUM no restart-intensity cap + reconnection proven only at socket layer. Fix the liveness gap before trusting beamr for unattended cross-node failover.

### Audit B — haematite test coverage

**Current state.** Coverage broader/deeper than feared: ~430 inline test markers + 20 integration test files. Highest-risk subsystems genuinely well-tested:

- **WAL durability/recovery** (`wal/recovery.rs`) strongest: `recovery_tests.rs` + `recovery_persist_003_tests.rs` cover torn-tail writes, CRC mismatch as hard corruption, untrusted ~4GiB length prefixes, truncated tail after commit, corrupt-frame-after-commit, commit-after-recovery truncation. fsync points in `wal/durable.rs` explicit + `durable_tests.rs`.
- **Quorum + CAS + epoch-fencing** (`sync/consistency.rs`): ~33 unit tests covering majority size, ack dedup, timeout vs unavailable vs fenced vs cas-conflict, fence-wins-over-mismatch precedence. `tests/spike_fencing.rs` real-Database stale-owner fencing e2e.
- **Prolly-tree history-independence** (`tree/mutate_history_independence_tests.rs`): real proptest (300 cases) — insertion-order independence, insert/delete history independence, distinct-maps-distinct-roots, minimal-counterexample regression guard. The MEMORY bug, now guarded.
- **Causal commit stamp / step-3 epoch fence** (`db/owner_stamp.rs` + `tests/commit_stamp_e2e.rs`): identical-stamp-on-proposer-and-peer, recovered-epoch-cannot-restamp. `handoff_merge_tests.rs`: union-merge max-stamp-wins, tombstone precedence, commutativity.
- **Distribution e2e extensive:** three_node_split_brain, ss0_partition_divergence, concurrent_proposer, election_e2e, replicated_append, writer_coordinator, receiver_apply/batch_apply. `db/receiver.rs` no co-located unit tests but apply paths reached via these.
- **Disk store dir-fsync barrier** (`store/disk.rs sync_dirty_dirs`) asserted at state level (pending_dirty_dirs populated then drained).

Real gaps cluster in: WASM build (zero tests), native transport glue, property/fuzz breadth on wire codec + union merge, crash-injection (vs state-assertion) for the disk dir-fsync barrier.

**Findings:** see Theme 1 rows 1.4/1.5 (high), 1.6/1.7/1.8/1.9 (medium), 1.12/1.13 (low).

**Summary.** The four highest-risk durable-store subsystems (WAL torn-write/CRC recovery, quorum+CAS+epoch-fencing, prolly-tree history-independence, causal commit-stamp epoch fence) are all well covered, several with the exact regression guards MEMORY records. Genuine risk-ranked gaps: (1) WASM + OPFS/IndexedDB persistence ~no tests; (2) disk dir-fsync barrier bookkeeping-asserted but never crash-injected (matches live fsync-nodedir worktree); (3) property-shaped but example-only — wire-codec decoder + union-merge commutativity/associativity (crate has exactly one proptest file today); (4) native transport glue, sync protocol driver, db/receiver inbound-apply lack isolated adversarial-delivery + stale/malformed-proposal tests. Priority: WASM persistence round-trips + disk dir-fsync crash test first (durability), then union-merge + wire-codec proptests (distribution-correctness), then receiver/transport fault-injection.

### Audit C — haematite as default backend

**Current state.** Haematite is NOT default or first-class anywhere in the Aion stack today — it is a fully-wired, optional, OFF-by-default alternative gated behind a Cargo feature.

- Backend selection lives in aion-server. `StoreBackend` enum (`config/mod.rs:115-122`): Memory, LibSql, Haematite. Runtime default `StoreBackend::Memory` (`StoreConfig::Default` at `mod.rs:966-977`); any `--store-url`/`AION_STORE_URL` coerces Memory→LibSql (`mod.rs:716-718`; `env.rs:36-37`). Effective durable default is libSQL; haematite never selected unless operator writes `backend = "haematite"`.
- The haematite arm is compiled out by default. `aion-store-haematite` is `optional = true`, enabled only by `haematite-backend`, OFF in both aion-server (`default = []`) and aion-cli (default features, and NOT in `release`). When off, selecting `backend = "haematite"` is a hard boot error (`state.rs:1074-1078`). A stock `cargo build`/published `aion` literally cannot run on haematite.
- Every default-facing config selects non-haematite: `aion new` ships `backend = "libsql"`; `dev-config.toml` ships `backend = "memory"`; ops guide documents `backend = "memory"`. ONLY default-haematite path is the failover demo generator (`scripts/demo/lib.sh emit_cluster_config`), which itself needs `--features haematite-backend`.
- Good news: the store is genuinely first-class once enabled — one leaf `Arc<HaematiteStore>` as both EventStore + OutboxStore, single-node and distributed `[store.cluster]` boots, auto-failover, passing integration tests. The gap is purely defaulting/feature-gating.

**Findings:** see Theme 2 rows 2.1/2.2 (critical), 2.3 (high), 2.4/2.5 (medium), 2.6 (low).

**Summary.** Two blockers make haematite second-class: (1) `haematite-backend` feature off in aion-server + aion-cli defaults (absent from `release`) ⇒ stock binary cannot run on haematite, selecting it is a boot error; (2) every default config selects something else (runtime Memory→LibSql, scaffold libsql, dev memory, ops memory/libsql). Only the demo generator defaults haematite and it needs the non-default feature. To make haematite default/first-class without removing libSQL: turn `haematite-backend` on by default (or at least in `release`), change `StoreConfig::default` to Haematite with a default data_dir (mind validate() data_dir requirement), flip scaffold/dev/ops configs, keep `memory|libsql` as explicit opt-ins. The implementation is already first-class; the work is defaulting + feature-gating.

### Audit D — default features = ours

**Current state.** Only beamr's main crate has a rich default feature set; everything else is lean/empty or actively defaults to an external component.

1. `beamr/crates/beamr/Cargo.toml`: `default = ["std","threads","net","fs","jit","embedded"]` — correct, beamr runtime on by default.
2. **aion server/CLI default AWAY from the stack.** `aion-server` and `aion-cli` both `default = []`/no default; hard-link `aion-store-libsql` (external libSQL/SQLite) non-optionally; gate `aion-store-haematite` and `liminal-transport` behind OFF-by-default features. Runtime outbox defaults to `OutboxTransport::Grpc` (external tonic); in-process `aion-client` defaults to gRPC/tonic with `embedded` (ablative in-proc) off.
3. haematite, liminal, liminal-server, liminal-sdk, beamr-cli, beamr-wasm, aion-nif are `default = []` or no `[features]` — neutral; the leaf storage/transport repos can't fix the wiring, which lives in aion.

Net: a default `cargo build`/`cargo install` of aion produces a binary that links libSQL + gRPC and does NOT link haematite or liminal — the exact inverse of "default featured stuff to be all of ours."

**Findings:** see Theme 3 rows 3.1/3.2 (critical), 3.3/3.4/3.5 (high), 3.6 (medium), 3.7/3.8/3.9 (low).

**Summary.** beamr already does it right; leaf ablative crates (haematite `default=[]` native-always-on, liminal-sdk `default=["std"]` carrying the real transport) are correctly ablative-by-default. The problem is entirely at the aion integration layer. Corrected defaults: aion-server and aion-cli `default = ["haematite-backend", "liminal-transport"]`, make aion-store-libsql optional behind a new opt-in `libsql-backend` feature, flip `OutboxTransport` default to `Liminal`, set aion-client `default = ["embedded"]`. Two critical, two/three high, then medium/low confirmations.

---

## Cross-cutting notes

- **Overlap to do once:** Theme 2 (2.1) and Theme 3 (3.1/3.2) are the same `aion-server`/`aion-cli` `Cargo.toml` edits — land them as one coordinated change adding `default = ["haematite-backend", "liminal-transport"]` and making libSQL opt-in behind `libsql-backend`.
- **Ordering constraint:** the `validate()` data_dir guard (`config/mod.rs:767-772`) means the haematite default (2.2) MUST also ship a default `data_dir`, or an empty config fails validation.
- **Keep options alive:** every recommendation preserves libSQL (`backend="libsql"` / `libsql-backend`) and gRPC as explicit opt-ins — this is a defaulting change, not a removal.
- **Durability test gaps map to live work:** the disk dir-fsync crash test (1.5) and group-commit all-or-nothing test (1.13) correspond to active worktrees (`fsync-nodedir`, `group-commit`).
