# Norn Observability + Intervention — BUILD PROGRESS (living tracker)

**This is the "where are we / pick up here" doc.** It tracks the build of the Norn agent
observability + mid-run intervention control plane, slice by slice.

- **Design (source of truth for WHAT to build):** [`NORN-OBSERVABILITY-AND-INTERVENTION.md`](./NORN-OBSERVABILITY-AND-INTERVENTION.md) — the full design; the NOI-0..8 pipeline lives in its §9.1.
- **What it is:** a complete, durable, real-time control plane for agents, built as a **general integration SDK** — Norn is just the *first* adapter, and there is **zero Norn-specific code in aion's platform crates** (mechanically enforced by the `no-norn-in-platform` CI gate).
- **Transport:** stdio-duplex **JSON-RPC 2.0** (hand-rolled, zero new external deps). **Five neutral capability-gated intervention primitives** (`InjectMessage` / `Cancel` / `PauseResume` / `UpdateBudget` / `RespondToApproval`). Observability persists to a byte-disjoint haematite **`O`-keyspace**, never in the workflow replay log.

_Last updated: 2026-07-02 — NOI-7 landed (`32edd9c4`); ops-console transcript + intervention UI wired, delimiter bug fixed. Next: NOI-8 (2nd adapter, SDK generality proof)._

---

## Status — NOI-0 … NOI-8

Legend: ✅ landed & green on `main` · 🔨 building · ⬜ pending

| Slice | What | Repo | Status | Commit |
|---|---|---|---|---|
| — | **Neutral DATA types** — `ActivityEvent` envelope + 5-primitive `InterventionCommand` (in `aion-core`, beside `cluster_event.rs`; ts-rs for the dashboard) | aion | ✅ | `956a443e` |
| **NOI-0** | Durable `attempt` field on `ActivityStarted/Completed/Cancelled` (the whole design keys on `(workflow,activity,attempt)`) | aion-core | ✅ | `0381ab5c` |
| **NOI-1** | Norn `--protocol jsonrpc` channel: `initialize` + `run/*` result + `event/*` live notifications | norn | ✅ | `3533590` |
| **NOI-2** | Norn `intervene/*` write direction: inject / cancel + `-32601` capability gate | norn | ✅ | `58bec9f` |
| **NOI-3** | `aion-integrations` SDK crate — the harness-blind `AgentHarness`/`AgentSession` trait + reusable JSON-RPC-stdio helper | aion | ✅ | `185d92dd` |
| **NOI-4a** | `aion-integration-norn` adapter — first `AgentHarness` impl, drives a **real** `norn --protocol jsonrpc` process (e2e verified) | aion | ✅ | `7b80f23b` |
| **NOI-4b** | `aion-worker` harness-blind trait-driver + `aion-cli` composition root (default-on `norn` feature) + **`no-norn-in-platform` CI gate** (proven by planted-edge negative control) | aion | ✅ | `7ef85752` |
| **NOI-5** | Durable transcript spine **core**: byte-disjoint haematite **`O`-keyspace** + `ObservabilityStore` compare-and-append + **`ActivityEventPublisher` sequencer** (commit-allocated `store_seq`, retry loop, live tail + resume-by-seq). All negative controls green (concurrent monotonicity, failover dedup, O-vs-E replay-invisibility). | aion | ✅ | `ac48e9f3` |
| **NOI-5b** | Transport wiring (additive on the proven core): thread `Arc<dyn ObservabilityStore>` through **all 5** `ServerState` constructors (haematite → durable `O`-keyspace impl; other backends → in-memory impl; transcript sequencer served on every boot), add the `Transcript` subscription variant (proto oneof tag 5 + `StreamedActivityEvent` frame) + **namespace-gated** `serve_transcript_socket` (reuses the per-workflow anti-leak gate; durable-tail replay + live-splice, no gap/dup), and the additive worker→server publish seam (`ActivityContext.event_sender` + `emit_event`, no-op without a seam). E2E proof green (live delivery, resume-by-`store_seq`, ephemeral-never-persisted, denial). Generated-types diff + `verify-ops-console` + `no-norn-in-platform` clean. **Deferred:** the worker-runtime drain arm + the concrete liminal Channel-Publish event bus (the §5.1/§9 spike) — the handler-facing seam is in place, the runtime→liminal→server bus lands with NOI-6's transport. | aion | ✅ | `16d4f213` |
| **NOI-6** | Intervention routing: operator → server → liminal PUSH → worker → `session.intervene()`. Neutral `InterventionOutcome` ack (ts-rs). Server `InterventionRouter` gates on the worker's advertised `InterventionCapabilities` (stored on `WorkerHandle`, surfaced at liminal registration via the notifier) + resolves the owning worker via an `AttemptOwnerIndex` back-index + pushes over the liminal server-push (`InterventionRequest`/`InterventionReply` demuxed from `DispatchRequest` on the same channel). Worker `ControlRegistry` routes the pushed command to the live session's `spawn_agent` control channel → `session.intervene()` → ack. Namespace-gated `POST /workflows/intervene` (mirrors signal/cancel). **E2E over real liminal TCP green** (InjectMessage + Cancel applied), plus both negative controls (unadvertised gated at server & never sent; stale/nonexistent attempt = app-range too-late no-op, no panic). Deferred: the liminal-worker execute path does not yet drive `spawn_agent`/register sessions itself (the seam + registry are in place); harness-neutral (`no-norn-in-platform` green). | aion | ✅ | `0ca827a7` |
| **NOI-7** | Ops-console `TranscriptPanel` + `InterventionControls` (capability-gated). Includes the transcript target delimiter fix (NUL join/split unification — a green-but-dead path caught by adversarial review, confirmed via `od -c`) + round-trip regression test. Re-gated: typecheck, biome, 256 bun tests, `verify-ops-console`, `no-norn-in-platform`. | aion | ✅ | `32edd9c4` |
| **NOI-8** | Second observability-only adapter — the SDK **generality proof** (two implementations of one trait) | aion | ⬜ | — |

**Milestone reached:** through NOI-4, the entire aion↔Norn control plane works end-to-end through the neutral trait, the product runs Norn out-of-box, and the zero-Norn-in-platform invariant is CI-enforced. NOI-5..8 are the durable-persistence + UI back half.

---

## Build cadence / discipline (hard rules — learned the hard way)

- **Serial per repo.** NEVER run two file-mutating builds against the *same* repo at once — worktrees share one `.git`, and a ref race once moved `main` (recovered, nothing lost). Cross-repo parallel is fine; same-repo is serial. The back half is almost all aion, so it runs one slice at a time.
- **Explicit worktree per build** (`git worktree add`), never destructive `git reset` against a shared ref.
- **Re-gate every merge by hand on a forced recompile** (`cargo clean -p <crate>` + rebuild) — twice this caught phantom rust-analyzer "compile errors" that were actually fine.
- **No corners:** clippy `-D warnings`, zero new production `#[allow]`, additive-only, real negative controls.

---

## Needs Tom (open decisions, not blocking the NOI chain)

- ~~**4096 shard default** (#169b)~~ — ✅ DONE: haematite 0.4.0 published, aion default → 4096 (`af4bad09`).
- **AD-012 reopen** (#150) — ✅ decided: fold into the reopen/resume feature (Failed+Cancelled reopen + Paused resume; design done, ADR-022 namespace-auth); stale empty WIP branch to be deleted.
- Live-in-browser verifies (#138) + the Sydney hardware demo (#118) need Tom at the keyboard.
- ~~**liminal republish**~~ — ✅ DONE: `liminal-rs`/`liminal-sdk`/`liminal-server` 0.2.1 on haematite 0.4.0 published; aion re-pointed to a single haematite 0.4.0.

---

## Resume point

**NOI-7 landed** (`32edd9c4`, merged to main `4c2f6d5f`; re-gated: typecheck 0 errors, biome clean, 256
bun tests, `verify-ops-console` + `no-norn-in-platform` clean). The transcript target delimiter fix
(NUL join / space split mismatch — a green-but-dead path an adversarial review flagged; confirmed real
via `od -c` on the raw bytes) landed with it plus a round-trip regression test. **Next: NOI-8** (a
second observability-only adapter — the SDK generality proof, two implementations of one trait). Two
committed strategic design docs sit beside this one: `ZERO-CONFIG-CLUSTER-FORMATION.md` and
`WORKER-DEPLOYMENT.md`, both gated on #146 (CSOT-1 phase-1 substrate now landed on haematite main
`c41b35c`). Update after each merge.
