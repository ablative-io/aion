# Aion Repository Review

**Date:** 2026-07-23 · **Scope:** full repository — architecture, code quality, correctness, and design-doc/implementation drift · **Baseline:** `main` @ `e57e5778`

**Method.** Ten domain-sliced reviews (core/store, engine durability, engine services, store backends, package/loading, server/proto, workers/clients, AWL/toolchain, design-drift, cross-cutting standards) were run by independent Opus reviewers, and **every finding was then adversarially verified by a separate Opus verifier instructed to refute it against the source** (38 agents total). 28 findings survived verification (several with severity corrected downward); 0 were refuted outright. Deterministic checks (clippy, fmt, full test suite, grep audits for `#[allow]`, beamr-boundary, direct-append, file sizes) were run first-hand. Every finding below cites a verified `file:line`.

---

## Verdict

**The engineering core of Aion is genuinely strong — the five load-bearing invariants hold in the shipped code, error-handling discipline is well above typical, and test coverage exercises failure paths, not just happy paths. There are no critical findings. There are 15 major findings and 13 minor ones.** The majors cluster in three places:

1. **One real determinism bug in the engine** — workflow-visible `now()` is non-positional and diverges on replay (the sharpest finding in this review).
2. **The haematite backend and its contract surface** — non-atomic outbox read-modify-write, a two-commit append, and a shared conformance oracle that doesn't pin the behaviours that would catch either.
3. **Top-level documentation that has rotted badly behind the code** — the stock binary defaults to haematite while three docs say libSQL; README says "no clustering" while a 1,100-line multi-node failover supervisor ships default-on; CLAUDE.md omits ~9 crates including an entire second authoring language (AWL).

Nothing found threatens durability of already-recorded history. The single-writer append path, status-as-projection, type-erased events, and content-hash namespacing invariants are all structurally enforced and verified clean.

---

## Build & Test Health

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ clean |
| `cargo fmt --check` | ✅ clean |
| `cargo test --workspace --no-fail-fast` | ⚠️ 2,809 passed / **130 failed** / 7 ignored |

**All 130 failures are one environmental cause:** the `gleam` CLI is not on PATH on this machine, and the compile-proof/archive-gate/codec-proof tests **hard-error** instead of gating. No logic failure was observed anywhere in the suite. Two problems follow:

- CI (`.github/workflows/ci.yml:148-153`) installs gleam, and its comment says these cases would otherwise be "skipped" — they aren't skipped, they fail. The comment is wrong about the behaviour.
- CLAUDE.md's own rule says runtime-dependent tests must gate at runtime (env var check, logged skip, `Ok(())`) — never fail on a missing toolchain. 130 tests across 45 suites violate this; a contributor without gleam cannot get a green `cargo test --workspace`.

---

## Invariant Scorecard

| # | Invariant | Status | Evidence |
|---|---|---|---|
| 1 | Type-erased events | ✅ **Holds** | `Event` carries opaque `Payload` (bytes + `ContentType`); no generic parameter anywhere; validation on read |
| 2 | Determinism boundary | ⚠️ **One confirmed violation** | `now()` is non-positional (F-1 below). `random` is correctly seeded (WorkflowId+RunId+ordinal, ChaCha20); no wall-clock/entropy leaks found elsewhere |
| 3 | Single writer per workflow | ✅ **Holds, structurally** | All production `EventStore::append` calls are inside the Recorder (plus the `PublishingEventStore` store-decorator); enforced at type level by the `WriteToken` capability (`aion-store/src/store.rs:11-36`) |
| 4 | Status is a projection | ✅ **Holds** | `WorkflowStatus` has no `Default`, no public constructor; `status_from_events` is the sole producer; visibility status column is an explicitly-derived read index |
| 5 | Content-hash namespacing | ✅ **Holds** | Length-framed hashing prevents boundary-shift collisions; catalog immutability tripwire (`ManifestMismatch`); `$`-separator names rejected at both write and read |

Also verified: the **beamr boundary rule holds** — every non-test `beamr` import in `crates/aion` lives under the `runtime` module (workspace-wide grep; the only `loader/` hits are in a test file).

---

## Major Findings (15)

All CONFIRMED by an independent adversarial verifier. Ordered by severity of consequence.

### Correctness & security

**F-1 · Workflow-visible `now()` is non-positional and diverges on replay** — `crates/aion/src/runtime/nif_determinism.rs:216`
`now_from_context` returns `context.last_recorded_at()`, which `NifContext` sets from `history.last()` over the **full run segment** (`nif_context.rs:117-118`); `nif_timer.rs:341-346` does the same. But the durability layer models `now()` positionally — `DeterminismContext::advance_to_recorded_at` advances per applied event, and `replay_inspect.rs:145-148` claims per-step `now = event.recorded_at()` "is exactly the value the production now() NIF serves" (true only for the final step). Under full-history re-execution recovery, a `now()` call before a blocking point returns the timestamp of the *final* recorded event instead of the value the original run observed — e.g. `let t = now()` at workflow start returns `WorkflowStarted@t0` live but `SignalReceived@ts` after recovery. Any `now()` value used as data is silently corrupted across recovery. This is the direct contradiction of invariant 2 / DESIGN CO8. **Fix: serve the positional timestamp (the cursor already tracks it).**

**F-2 · Worker gRPC token expiry only enforced on heartbeat frames** — `crates/aion-server/src/api/worker_grpc.rs:405`
The only `token_expired` check sits inside the `Message::Heartbeat` arm; the `Result` arm and the inbound loop have none, and heartbeats are per-in-flight-activity (`heartbeat.rs:229-231` rejects heartbeats with no matching task), so **an idle worker emits none**. A worker whose short-lived JWT has expired keeps its authenticated, dispatch-eligible stream and registry entry indefinitely and continues receiving new activity dispatches. Credential rotation/revocation — the mechanism bounding a compromised worker token — is defeated by simply not heartbeating. The rest of the server's authn/authz was verified defense-in-depth clean; this is the one gap.

**F-3 · haematite outbox mutations are unguarded read-modify-write** — `crates/aion-store-haematite/src/store.rs:1880` (also 2446, 2500)
Claim/transition/settle are all scan-or-get → unconditional `put_routed` → commit, with no CAS, no status predicate, and no serialization (`self.blocking` is a bare `spawn_blocking`, `store.rs:1060-1069`). libSQL, the oracle it must match, does select-and-claim in one IMMEDIATE transaction with `WHERE status='pending'` guards on every update. Two same-node racers (dispatcher claim vs. cancel-settle; reconciler vs. worker complete) each read old state and last-write-wins, producing non-deterministic terminal outbox status — a cancelled ordinal can dispatch anyway. Bounded (history-level exactly-once holds via Recorder dedup; cross-node double-claim prevented by shard ownership) but a real concurrency divergence from the documented contract. haematite has a `cas` primitive; the outbox path just doesn't use it.

**F-4 · `.v1` timeout-identity packages are silently dropped on restart** — `crates/aion-package/src/package.rs:231`
`verified_content_hash` deliberately accepts `.v1` single-value identities "so a `.v1`-stamped deployment recovers on restart instead of being skipped" (`hash.rs:172-176`) — but `to_archive_bytes` re-stamps v1 manifests with the beams-only legacy hash (the `has_explicit_timeout_identity` predicate is false for v1), so the persisted record's store key no longer matches the recomputed hash on reload, and `reload_persisted_packages` skips it; routes fail with `UnknownVersion`. This is the exact regression class already fixed for `.v3` (see `deploy_persistence_e2e.rs:420`) left open for v1: a durability break for a form the loader explicitly promises to keep recoverable.

**F-5 · `not_owner` is retryable in the Rust client but terminal in Python/TS/Gleam** — `crates/aion-client/src/error.rs:319`
Rust maps wire code 13 to retryable `Unavailable` ("re-resolve + retry"); Python/TS/Gleam never decode codes 11–14 and collapse it to a terminal `Server` error. In an HA deployment, a routine wrong-shard-owner fence that Rust callers ride through hard-fails every other SDK and pages operators with false internal-fault alerts.

### Contract & oracle gaps

**F-6 · Cross-SDK error taxonomy has diverged three ways** — `crates/aion-client/src/error.rs:296`
CLIENT-CONTRACT.md mandates *exactly* 10 branchable failures and folds `unknown_query`/`not_running` into `InvalidArgument`; Python/TS/Gleam comply, but Rust exposes 13 variants and maps those codes distinctly. Proto code 14 (`invalid_state`, a caller-reachable reopen-precondition failure) is classified three different ways: Rust=`InvalidState`, Python/TS/Gleam=`Server`.

**F-7 · Client conformance suite can't catch F-5/F-6** — `conformance/aion-clients/scenarios.json:4`
The 7 scenarios assert only 4 of the 10+ taxonomy members, and the harness is live-only (skips without `AION_SERVER_URL`), so ordinary CI verifies none of the wire-code mapping. Every SDK passes CI while disagreeing on half the taxonomy.

**F-8 · `list_active` trait doc contradicts required behaviour** — `crates/aion-store/src/store.rs:101`
Doc says "non-terminal", but `is_terminal` is false for both `Running` and `Paused`, and the `list_paused` doc 10 lines below says list_active "filters `== Running`". Both shipping backends filter Running-only by convention. A third backend implementing the literal doc would respawn operator-paused workflows on restart, defeating the #204 pause guarantee — and would pass the conformance suite (next finding).

**F-9 · Conformance oracle omits Paused/list_paused and non-contiguous-append rejection** — `crates/aion-store/src/conformance.rs:27`
Zero hits for `paused` in the shared suite despite `list_paused` being a required trait method tied to GATE-2 kill-9 recovery; no scenario appends a mis-sequenced batch, though the reference store rejects them (`memory.rs:303-311`). Two safety-critical behaviours are pinned only by per-backend private tests. (Related minor: the outbox claim/complete lifecycle and concurrent-claim contention are also absent from the oracle — the mechanism by which F-3 went uncaught.)

**F-10 · Deadline-fire retry policy hardcoded** — `crates/aion/src/runtime/nif_timer_bridge.rs:514`
`MAX_ATTEMPTS=6 / 200ms / 30s` as consts, in a codebase where CLAUDE.md names retry policy explicitly as builder-supplied, and where the co-located child-terminal watcher already threads a builder-supplied `SignalDeliveryConfig` for exactly this purpose.

**F-11 · `PausedRuns` silently swallows RwLock poison on every path** — `crates/aion/src/lifecycle/pause.rs:66`
`insert`/`remove`/`replace_all`/`extend` drop the poison arm with no log; `snapshot()` returns an **empty** held set on poison. Once poisoned, pause-hold exclusion in the outbox dispatcher (`outbox_dispatcher.rs:452-453`) silently stops excluding paused runs — a durably-Paused workflow's activities dispatch anyway, unlogged, until a restart rebuilds the set. The rest of the crate maps poison to typed errors; this module is the deviation.

### Documentation drift (load-bearing)

**F-12 · Documented default store is libSQL; the shipped binary defaults to haematite** — `crates/aion-cli/Cargo.toml:87`
`default = ["haematite-backend", "liminal-transport", "norn"]` (comment: "the runtime default backend is haematite"). CLAUDE.md:17, README.md:108, and DESIGN-OVERVIEW.md:629 all say libSQL is the default. An operator provisioning/backing-up per the docs has the wrong mental model of where the system-of-record lives.

**F-13 · README "Honest limits" says "There is no clustering"** — `README.md:45`
Contradicted by `aion-server/src/cluster.rs` (1,116 lines, "automatic multi-node failover … no human in the loop"), wired into the production boot path (`run.rs:226`, `state.rs:858`), plus haematite distributed mode with quorum replication — all behind the **default-on** feature. The honesty section either undersells shipped capability or overstates its maturity; either way it's wrong on the single most load-bearing operational question.

**F-14 · CLAUDE.md architecture is stale by ~9 crates and 18 design clusters** — `CLAUDE.md:15`
Omits aion-awl/-lsp/-package (an entire second authoring language), aion-store-haematite (the now-default backend), aion-toolchain, aion-darwin-acl, aion-proto-generated, and the three integration crates; says "twelve clusters" (docs/design has 30); lists Python/TS SDKs as crates when they live under `sdks/`. As the authoritative per-instance contract, it actively misleads every agent that onboards from it.

**F-15 · AWL is a shipped first-class authoring surface absent from all onboarding docs** — `README.md:6`
`aion awl {check,fmt,emit,schema}`, `aion run <file.awl>`, and direct `.awl` deploy all ship in the CLI; README/GETTING-STARTED/CLAUDE.md have zero mentions (verified by grep). The documented authoring story is materially incomplete.

---

## Minor Findings (13)

| # | Finding | Location |
|---|---|---|
| m-1 | haematite `append_with_outbox` commits events and outbox rows at two separate durability points (libSQL: one tx; contract says both-or-neither is "load-bearing"). Crash window mitigated by collect-replay re-derivation; inverse ordering impossible | `aion-store-haematite/src/store.rs:1370` |
| m-2 | Conformance oracle omits outbox claim/complete/retry/fail lifecycle and concurrent-claim contention | `aion-store/src/conformance.rs:76` |
| m-3 | DESIGN.md/CLAUDE.md still describe 5 `WorkflowStatus` variants; shipped enum has 7 (`ContinuedAsNew`, `Paused`) | `aion-core/src/status.rs:25` |
| m-4 | `InMemoryStore::lock_namespaces` recovers poison in place; sibling `lock_state` maps it to a typed error | `aion-store/src/memory.rs:103` |
| m-5 | Unused `SignalResumeError::Deliver` variant — zombie code implying an error path that doesn't exist | `aion/src/signal/resume.rs:171` |
| m-6 | Determinism linter silently misses aliased imports (`import gleam/float as f`) and cross-module helpers, contradicting its "no silent miss" contract; `aion check-deterministic` gives false clean bills | `aion-package/src/structure/determinism.rs:298` |
| m-7 | Rust client `from_payload` skips the content-type tag check that Python/TS enforce | `aion-client/src/payload.rs:40` |
| m-8 | Mock agent harness swallows Mutex poison (`if let Ok`), inconsistent with its own sibling `intervene` path | `aion-integration-cli/src/mock.rs:139` |
| m-9 | Codegen index casts saturate to `u16::MAX`/`u32::MAX` instead of erroring — silent miscompile at absurd (>65k-field) sizes; functions already return `Result` | `aion-awl/src/mir/lower/codec_encode.rs:104` |
| m-10 | Quota-ceiling path folds a genuine store `Err` into "no override" with zero logging — a persistent registry fault silently admits every tenant at `platform_default` | `aion-server/src/worker/quota_cache.rs:91` |
| m-11 | The one production `#[allow(clippy::cast_*)]` in the engine (documented, correctness-bounded, but the rule admits no production allows) | `aion/src/runtime/nif_activity_retry.rs:147` |
| m-12 | `PLACEMENT_CACHE_TTL`/`QUOTA_CACHE_TTL`/`QUOTA_BROADCAST_CADENCE` hardcoded as consts with no config path, while sibling cadences are plumbed from config | `aion-server/src/run.rs:36` |
| m-13 | COMPONENT-ARCHITECTURE.md names three store crates that don't exist (postgres/sqlite/turso), omits haematite/AWL/clustering; DESIGN-OVERVIEW code samples import `aion_store_postgres` | `docs/design/workflow-engine/COMPONENT-ARCHITECTURE.md:428` |

**Standards additions from first-hand audit:** the no-god-files rule (>500 production LOC) is violated by at least `aion-store-haematite/src/store.rs` (~2,100 production code lines) and `aion-server/src/state.rs` (~1,080), with several others near or over the line (`worker/bridge.rs` ~800, `worker/registry.rs` ~600). And the gleam-less test hard-fail described under Build & Test Health is itself a standards violation (runtime gating rule).

---

## Domain Assessments (reviewer summaries, verified)

- **aion-core / aion-store** — High quality; all three assigned invariants upheld in code. Weakness is the contract/oracle layer (F-8, F-9), not the logic. The replay-safety discipline on event evolution (serde defaults + strip-and-decode tests per added field) is exemplary.
- **Engine durability** — Mission-grade. Single-writer is structural; the interleaving-aware `HistoryCursor` (async arrivals inside activity spans, retry trails, reopen supersession) is correct and thoroughly tested. The one substantive defect is F-1.
- **Engine services (time/signal/query/child/lifecycle)** — The strongest code in the repo per its reviewer. Record-before-deliver, terminal-under-lock, cancel-vs-fire coordination, and the WF-TIMEOUT deadline machinery (terminal re-check + append in one critical section; continue-as-new deadline-resurrection closed with a regression test) are all disciplined. Only standards nits (F-10, m-5).
- **Store backends** — libSQL is transactionally clean (one IMMEDIATE tx, status-predicated transitions, disciplined rollback; no defects found). All concerns are haematite-side (F-3, m-1) plus the oracle gap that hides them (m-2).
- **Package / loading / toolchain** — Security-conscious: extraction-bomb budgets with no default, zip-slip impossible (in-memory), reserved-namespace rejection, catalog immutability tripwire, per-submission throwaway workspaces. One real durability defect (F-4).
- **Server / proto** — Exceptionally well-tested. Multi-tenancy isolation verified defense-in-depth (the reviewer could not construct a cross-namespace path); byte-identical NotFound for foreign-vs-nonexistent prevents existence leaks; worker teardown is idempotent with a drop-guard sweep. One security gap (F-2).
- **Workers / clients / SDKs** — Reconnect/backoff/unacked-replay genuinely parity-engineered across Rust/Python/TS (identical formulas and guards, verified at matching line numbers). The weakness is error-taxonomy parity (F-5, F-6, F-7).
- **AWL / NIF / CLI / integrations** — Excellent. The NIF boundary catches every unwind, classifies panics as terminal activity errors, and never exposes raw terms; codec round-trips respect the D4/S5 omit-vs-null invariants; anyhow correctly confined to the binary.

---

## Recommendations (priority order)

1. **Fix `now()` to be positional** (F-1). This is the only confirmed invariant violation in the engine and it silently corrupts data across recovery. The cursor already carries the per-step timestamp; `replay_inspect` documents the intended semantics.
2. **Enforce token expiry on every worker-stream frame and on dispatch selection** (F-2), and deregister on expiry.
3. **Close the pause-hold poison hole** (F-11) — map poison to the crate's existing typed errors; never return an empty snapshot on poison.
4. **Use haematite's CAS primitive on the outbox path and unify the append commit** (F-3, m-1), then **extend the shared conformance oracle** to pin claim-lifecycle, concurrent-claim, Paused/list_paused, and non-contiguous-append rejection (F-9, m-2) so backends can't drift again.
5. **Fix the `.v1` restamp-on-persist bug** (F-4) the same way v3 was fixed, with a round-trip test.
6. **Reconcile the client error taxonomy** (F-5, F-6): decide the canonical taxonomy (10 vs 13), bring all four SDKs and CLIENT-CONTRACT.md to it, and add offline wire-code mapping fixtures to the conformance suite (F-7).
7. **Rewrite the top-level docs in one sweep** (F-12–F-15, m-3, m-13): CLAUDE.md crate list and cluster count, README default-store and clustering claims, AWL onboarding, `list_active` contract wording (F-8), status-variant enumeration. Several of these are one-line fixes with outsized onboarding impact.
8. **Make gleam-dependent tests gate-and-skip** per the repo's own rule, and fix the ci.yml comment.
9. Sweep the remaining minors: builder-thread the hardcoded policies (F-10, m-12), split the two god files, delete the zombie variant, log the quota-cache store error, add the Rust content-type check, harden the determinism linter or narrow its claim (m-6).

---

## Review Provenance

- 10 domain reviewers + 28 adversarial verifiers, all Opus, 38 agents, ~2.2M tokens, 492 tool calls.
- Every finding above was independently confirmed against source by a verifier whose default stance was "refuted"; 3 findings had severity corrected downward during verification and are reported at the corrected level.
- First-hand checks by the coordinating reviewer: clippy/fmt/test runs, `#[allow]` census, beamr-boundary grep, direct-append grep, file-size audit, spot re-verification of F-1, F-2, F-3, F-12, and the README/docs link integrity.
- Full machine-readable findings (evidence + verifier notes): workflow run `wf_2a112366-417`.
