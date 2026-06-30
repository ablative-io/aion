---
type: audit
title: Code-Quality Smell Test — Aion stack (failover, WASM, push, dashboard)
date: 2026-06-29
auditors: 4 × Opus, reviewed & synthesized by Frodo
standard: CLAUDE.md (no lazy code, no silent failures, no shortcuts, no god files, no arbitrary limits/defaults, no zombie code)
---

# Code-Quality Smell Test — 2026-06-29

Commissioned by Tom: before we build the full-strength dashboard, smell-test
everything recently built across the stack — no hacky workarounds, ground-up
means we make every choice properly. Four rigorous Opus auditors, one per area,
each judged against the full CLAUDE.md bar and ran the lint/typecheck gates.

**Tally: 9 Critical, 12 Major, 13 Minor.**

| Area | Gates | Critical | Major | Minor | Verdict |
|---|---|---|---|---|---|
| Aion failover (LSUB) | clippy ✓ fmt ✓ | 0 | 2 | 3 | **Strong** — invariants hold |
| Liminal push/dedup | clippy ✓ fmt ✓ | 1 | 2 | 3 | Strong, one durability bug |
| Haematite WASM | native ✓, wasm-runtime ✗ | 4 | 4 | 4 | Codec solid; browser I/O **not done** |
| Dashboard scaffold | tsc ✗ biome ✗ tests ✗ | 4 | 4 | 3 | Red across gates; reconcile, don't rewrite |

The headline: the **backend failover work is production-grade**; the **WASM
browser substrate and the dashboard scaffold are both incomplete** and need a
build-it-properly pass before the dashboard's Phase-2 (in-browser replay) or
even Phase-1 (build on the scaffold) can proceed.

---

## Aion failover (LSUB) — STRONG, no Criticals

Clippy + fmt clean. Single-writer, history-level exactly-once dedup, type-erased
rows, fail-loud config all hold. The typed disconnect-vs-timeout classification,
channel-name injectivity ("patient-records-grade"), and config validation are
exemplary.

- **M1 — hardcoded timing values violate the "NO ASSUMED DEFAULTS" rule.**
  `worker/liminal_transport.rs:87` `PUSH_REPLY_TIMEOUT = 30s` (dispatch reply
  deadline); `aion-worker/src/runtime/liminal.rs:108` `RECV_POLL = 100ms`
  (serve-loop poll); `run.rs:495` `drain_timeout_ms: 30_000`. All must come from
  the builder/config, REQUIRED when `transport = liminal`. Conservative values,
  not correctness bugs — but the rule is the rule.
- **M2 — `dispatch` discards the completion-delivery bool → result dropped on
  re-stage.** `worker/liminal_transport.rs:567` throws away `deliver()`'s `bool`.
  When the workflow isn't resident mid-failover (`Ok(false)`), the row is marked
  `Done` and the worker's already-computed result is discarded; recovery
  re-dispatches and **re-executes** the activity. At-least-once + history dedup
  keep it *correct*, but a non-idempotent activity runs twice. Shared with the
  gRPC path (`bridge.rs:208`), so pre-existing, not an LSUB regression — but it
  now matters operationally. Fix: on `Ok(false)` treat as undelivered (retry,
  don't terminalize), or capture the computed result on re-stage.
- **Minor:** m1 Drop-path poison-swallow leaks a dead worker in the registry
  (`registry.rs:712` — mirror the `into_inner()` recovery the notifier path
  already uses); m2 `transport⇒listen_address` coupling enforced at boot not in
  `validate_outbox`; m3 dispatcher/reconciler spawned fire-and-forget, handles
  dropped, so shutdown signals but never awaits the drain (`run.rs:323,341`).

## Liminal push / worker-registration / dedup — STRONG, one durability bug

Clippy + fmt clean. Typed death signal is genuine (not string-matched),
cancel-on-close funnels through one point, synchronous registration ack is
well-designed, the drain thread can't panic-and-vanish, wire codec is disciplined.

- **C1 (CRITICAL) — dedup claim leaks `InFlight` forever on publish failure.**
  `liminal-server/src/server/connection/services.rs:519-535`. The publish path
  claims the idempotency key, then `publish_with_delivery(...)?` propagates an
  error leaving the claim dangling as `InFlight`. Since `claim_delivery` returns
  `false` for both `Completed` and `InFlight`, **a transient publish failure
  permanently suppresses all future re-publishes of that key** (`delivered:
  false`, no fan-out) — the opposite of what a retrying outbox needs. Fix:
  compensate the claim on the `Err` branch (tombstone/try-claim) so a failed
  publish releases the key. The `InFlight` arm must be recoverable, not a black
  hole.
- **M1 — channel name reused as conversation correlation id.**
  `liminal-sdk/src/remote/handles.rs:241-252`. Every request-reply on a channel
  hashes to the same `conversation_wire_id`; concurrent calls on separate
  transports can cross replies. Correlation key must be per-request-unique.
- **M2 — two production files over the 500-LOC bar.**
  `connection/process.rs` (~562) and `connection/supervisor.rs` (~521). Both
  split cleanly (frame-dispatch helpers; push-correlation registry).
- **Minor:** n1 hardcoded `CONNECTION_SCHEDULER_THREADS = 4` (no-defaults rule);
  n2 clock-fault swallowed to `0` in one path vs surfaced as `ConfigError` in
  the sibling; n3 push-id counter overflow unhandled-by-contract (safe, wants a
  one-line note).

## Haematite WASM — codec SOLID, browser I/O NOT DONE

Native clippy clean; `wasm` lib clean; **`wasm-runtime --lib` fails to compile**
(C1). No CI runs `wasm-pack test` — the 4 browser-gated tests are compile-only;
the OPFS WAL, IndexedDB WAL, WebSocket transport, and shard runtime have **zero**
executable tests. Byte-parity tests are real but prove *framing*, not I/O.

- **C1 (CRITICAL) — `WasmShardRuntime` does not compile on its own target.**
  `wasm/runtime.rs:218` calls `is_instance_of` without `use wasm_bindgen::JsCast`.
  The module + Cargo.toml present it as "compile-checked" and reference a
  `vendor/`/`[patch.crates-io]` that **does not exist**. R3 was never built.
- **C2 (CRITICAL) — IndexedDB WAL acks writes before attempting them, drops
  every persistence error.** `store/opfs/browser.rs:273-282` — `append`/`truncate`
  `spawn_persist` then `let _ = staged.persist().await`. A WAL — the durability
  primitive — returns `Ok(())` and discards quota/abort/blocked errors.
- **C3 (CRITICAL) — WebSocket transport has no `onerror`/`onclose`.**
  `wasm/transport.rs:233-256` registers only `onmessage`. A dropped/refused/closed
  socket is invisible; async teardown is lost.
- **C4 (CRITICAL) — inbound frame-validation failures swallowed.**
  `wasm/transport.rs:242-247` drops rejected frames silently (no counter/log);
  the `NonBinaryMessage` error variant is defined but never constructed (zombie).
- **Major:** M1 "proven by byte-parity" overstates what's verified (framing only);
  M2 unbounded in-memory WAL buffer rewritten whole on every append (O(n²));
  M3 read/durable-state inconsistency on reload with no detection marker;
  M4 `BatchWriteProposal` decode missing the hostile-count clamp its siblings use.
- **Solid:** byte-parity discipline is real (reuses native `WalEntry` codec
  verbatim); `sync_codec` decode hardening careful; `usize_from_f64` rigorous;
  typed `DatabaseError::Fenced` + `replicate_write_routed` clean; no god files.

## Dashboard scaffold — RED across all gates; reconcile, don't rewrite

`tsc --noEmit` FAIL (29 errors, 15 in source); `biome check` FAIL; `bun test`
31 pass / 6 fail / 4 load errors. Architecture and core modules are genuinely
good, but the code has drifted out of sync with the regenerated Rust types and
carries a half-applied refactor.

- **C1 (CRITICAL) — timeline engine crashes on 11 of 29 event variants.**
  `features/workflow-detail/lib/timeline.ts:23-54` handles 18; the other 11
  (`Schedule*`, `SearchAttributesUpdated`, `SignalSent`, `WithTimeoutCompleted`,
  `WorkflowContinuedAsNew`, `WorkflowReopened`) fall to `assertNever` and **throw
  at runtime**. (Note: the union is **29** variants, not the 25 long assumed.)
- **C2 (CRITICAL) — does not typecheck.** `types/generated/index.ts` references
  `TimerIdKind` (:14) and `WithTimeoutOutcome` (:368) — names never defined
  (truncated codegen). `StatusBadge.tsx:13` missing the `ContinuedAsNew` status.
- **C3 (CRITICAL) — half-applied live-feed refactor.** `features/live-feed/
  index.ts` exports `ConnectionIndicator` twice and re-exports three names that
  don't exist; 4 test files fail to load.
- **C4 (CRITICAL) — live data path is dead-wired.** `useLiveWorkflowEvents` and
  `useLiveListUpdates` are referenced by **zero** components; detail/list views
  never subscribe. The MVP's headline feature is unwired.
- **Major:** M1 WS errors + retry-give-up silent (`console.warn` only — violates
  "nothing happens silently"); M2 no error boundary anywhere (one bad event →
  white screen); M3 shared socket closed on last unsubscribe (route churn
  re-handshakes); M4 Biome reformats the generated file CLAUDE.md forbids editing.
- **Solid (keep):** the `AionEventWebSocketManager` (injectable, backoff,
  stale-socket guards, resync cursors); the `AW_*_CONTRACT` pinning pattern;
  `mergeEventsBySequence` history+live dedup; skeletons-not-spinners, no `any`,
  no `@ts-ignore`, a11y basics; centralized config.

---

## What this means for the plan

- **The failover work is solid enough to build the Phase-1 ops console on now**
  (modulo M1/M2 hardening + promoting failover transitions from logs to events —
  see VISION §8). The dashboard's exactly-once visualization rests on Liminal C1
  and Aion M2 being fixed, or its "one terminal per ordinal" story will mislead.
- **Phase 2 (in-browser replay engine) is gated** on a genuine build-it-properly
  pass on the Haematite WASM stack — it does not currently compile clean and has
  no exercised browser I/O. VISION §8 now states this honestly.
- **Building on the scaffold is preceded by a named reconciliation pass** (29-variant
  timeline behind a compile-time guard, regenerate types, finish live-feed,
  error boundary, surface WS failures). Days, not a rewrite. VISION §11 records it.

No fixes were applied — this is the findings pass Tom asked for. Each item above
is concrete enough to dispatch as a fix brief.
