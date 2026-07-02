# Ledger gaps — untracked-but-real work (2026-07-02)

**Purpose.** A durable record of work that is *real* (designed and/or built, or a
live bug) but tracked in **no in-repo ledger**. The project roadmap ledger
(`docs/design/roadmap.json`) ends at **RM-029**, is dated **2026-06-21**, and does
**not** cover any of the items below. This file is a hand-off note so a human can
slot these into the ledger deliberately — it does **NOT** invent an RM- numbering
scheme or restructure `roadmap.json`.

Produced during the 2026-07-02 doc-accuracy reconciliation pass (the same pass that
flipped stale "not yet approved / not implemented" status headers on shipped
subsystems). Every code claim below was verified against `crates/` before recording.

Legend: **DESIGN** = has an in-repo design doc; **NO DOC** = no design doc found;
✅ built · ⏳ designed/partial · 🐞 live bug.

---

## 1. The control-plane / cluster wave (the "second moat")

Not in `roadmap.json`. Phases 0–2 of the control plane are BUILT (see
`CONTROL-PLANE.md` §7, reconciled this pass); the wave below is the remaining spine.

| Item | State | Design doc | Notes |
|------|-------|-----------|-------|
| **#146 — durable cluster membership / quorum denominator** | ⏳ designed | **DESIGN** `HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md` | Make membership/discovery/placement durable haematite state so quorum stops sizing in dead nodes (the fault-tolerance ceiling). Named in `ROADMAP.md` Track D but has no ledger line. |
| **#147 — cluster auto-discovery (mDNS-first)** | ⏳ designed | **DESIGN** `CLUSTER-AUTODISCOVERY.md` | Laptop-mesh zero-touch formation. In `ROADMAP.md` Track D, no ledger line. |
| **Worker deployment (Phase 3 "second moat")** | ⏳ designed | **DESIGN** `WORKER-DEPLOYMENT.md` | beamr-supervised worker fleets placed per namespace, kept alive across kill-9, autoscaling that respects in-flight work. `CONTROL-PLANE.md` §7 Phase 3. |
| **Zero-config cluster formation** | ⏳ designed | **DESIGN** `ZERO-CONFIG-CLUSTER-FORMATION.md` | The "works the moment you run it" formation story. No ledger line. |
| **NOI — Norn observability & intervention family** | ⏳ designed/partial | **DESIGN** `NORN-OBSERVABILITY-AND-INTERVENTION.md` + `NORN-OBSERVABILITY-PROGRESS.md` | Real harness-agnostic agent-event observability layer (real-time + haematite-durable). Progress doc exists but no roadmap ledger line. (Owned by an in-flight agent — do not edit those two docs.) |

---

## 2. Dashboard / ops-console ADRs with no roadmap line item

`docs/design/decisions.json` carries **ADR-020..031** — ~12 ratified dashboard/ops-console
decisions (this pass normalized their status from `accepted` → `decided` and re-rendered
`DECISIONS.md`, which now reads "Decided (31)"). **None** of ADR-020..031 has a matching
`roadmap.json` item. They are ratified decisions with no build-tracking line:

- ADR-020 live cluster map as first-class view
- ADR-021 clean-partial multi-shard adoption (this one DID land in code — `858c9209`)
- ADR-022 three-tier RBAC capability model
- ADR-023 control-action safety (idempotency-key + wait-for-effect + pinned blast-radius)
- ADR-024 dual-mode swimlane axis
- ADR-025 light-theme deferral / token architecture
- ADR-026 TLS via required terminating proxy for M1
- ADR-027 server keepalive frames drive freshness
- ADR-028 virtualization via @tanstack/react-virtual
- ADR-029 cluster-map last-known-state fallback
- ADR-030 audit to a durable sink before M2 command authority
- ADR-031 event-reference docs auto-generated from ts-rs, CI-guarded

**NO DOC** beyond the ADR entries themselves (ROADMAP.md Track E mentions "#130
Ops-console professional disciplines (ADR-015..019)" but not 020..031).

---

## 3. `SESSION-2026-06-15-FOLLOWUPS.md` orphan bugs (no ledger, no issue link in-repo)

These live only in `docs/SESSION-2026-06-15-FOLLOWUPS.md` "Bugs / issues still open"
and "Operational improvements needed". **NO DOC** (session notes only). Verified live
where checkable:

| # | State | Item | Verification |
|---|-------|------|--------------|
| **#8** | ✅ RESOLVED (#175, fixed, this change) | Remote-worker volatile temp-dir workspaces — stacked-dev workers cloned into OS temp (`mktemp -d` / `/tmp/stacked-dev-clones`), recorded the path in durable history; a reboot/temp-reaper purge lost unpushed dev-round commits on resume | Handler-side fix in both worker variants (`examples/stacked-dev-remote/worker`, `.meridian/workflows/stacked-dev/worker`): clones now live at `<AION_WORKSPACE_ROOT \|\| ~/.aion/clones>/<run_id>/repo` (root must be absolute — relative roots are CWD-dependent and refused); a colliding run_id directory is renamed aside to `<run_id>.superseded-<unique>` — never reused, never deleted; teardown scoped strictly under the canonicalized root (temp-parent heuristic + `rm -rf` exhaustion fallback deleted), removes the per-run dir plus its superseded siblings, and surfaces its outcome as `workspace_cleaned` on the workflow result; missing workspace = explicit terminal diagnostic. SCOPE CAVEAT: run_id threading from `workflow.id()` exists in the `examples/stacked-dev-remote` **workflow only** — the `.meridian` bundle's Gleam codecs carry no `clone_url`/`run_id`, so that bundle cannot dispatch remote clones at all (remote placement terminal-fails at provision); its worker-side changes are defensive parity, unreachable until the bundle codecs are regenerated. RETENTION (deliberate, not a leak fix): failed runs keep their per-run directory for salvage and nothing GCs the root — pruning guidance in `examples/stacked-dev-remote/README.md` ("Retention and pruning"). Loss repro + lifecycle (incl. superseded sweep) pinned by `worker/tests/workspace_persistence.rs` in both variants. |
| **#14** | 🐞 | Teardown fires eagerly on worker connect — pending teardown activities from previously-failed workflows execute immediately when a fresh worker connects, destroying failure evidence | session §14. |
| **#6** | 🐞 | Workflow-ID mismatch in server bridge logs | session §6. |
| **#7** | 🐞 | `collect_race` recovery gap | session §7. |
| **#13** | ⏳ | Worker graceful shutdown for in-flight activities | session §13 (operational improvement). |
| **AW-009 R3** | ⏳ NOT wired | TLS not wired — `run.rs` explicitly rejects TLS config | **VERIFIED**: `reject_tls_until_supported` is still called at `crates/aion-server/src/run.rs:162` (defined `:746`). The rejection is live; TLS is unimplemented. |

---

## 4. Runtime loops / partial wiring not tracked anywhere

| Item | State | Design doc | Verification |
|------|-------|-----------|--------------|
| **Worker heartbeat expiry loop not driven** | 🐞 not driven | **NO DOC** | **VERIFIED**: no heartbeat-expiry driving loop in `crates/aion-server/src/run.rs` — the only `heartbeat` reference is a `heartbeat_window` config value inside a test (`run.rs:831`); no reaper/expiry task is spawned at boot. NOTE: this is distinct from `roadmap.json` RM-005 "Activity heartbeats" (status: idea), which is a coarse activity-progress signal — a different concept. The *worker*-heartbeat-expiry-loop gap has no ledger line of its own. |
| **DIST-003 catch-up responder only partially wired** | ⏳ partial | **DESIGN** `STORAGE-SWAP-MATURITY-DESIGN.md` | **VERIFIED**: the doc itself states (~line 150) "history pull/push logic exists (`sync/pull.rs`, `sync/push.rs`) but the catch-up responder is only partially wired"; sync scheduler/topology "DIST-003 deferred". No ledger line. |

---

## 5. RM-021..029 authoring / `aion_kit` line — likely PARKED

`roadmap.json` RM-021..029 (the authoring / DX / `aion_kit` standard-library tail) is
recorded here so it is **not misread as in-flight**:

- **`aion_kit` (RM-022, the worker standard library) does NOT exist as a Gleam package.**
  **VERIFIED**: `gleam/` contains only `aion_client` and `aion_flow` — there is no
  `aion_kit` package anywhere in the tree. ADR-011 defines its intended scope, but no
  crate/package realizes it. This whole authoring line (RM-021 declare-once authoring,
  RM-022 aion_kit, RM-023 thin-workflow reshape, RM-025 `aion dev`, RM-026
  server-as-compiler, RM-027 time-travel debugger, RM-028 agent scaffold, RM-029
  determinism linter) appears **parked**, not actively building.
- Action for a human: mark RM-021..029 as parked/backlog explicitly in the ledger so
  it doesn't read as active work, OR re-prioritize. `ROADMAP.md` Track E carries some of
  the same intent (CLI ergonomics, `aion dev`, Elixir SDK, DSL) but the RM-numbered
  ledger entries have no status transition since 2026-06-21.

---

## Summary for a human slotting these in

- **Has a design doc, no ledger line:** #146, #147, worker-deployment, zero-config
  formation, NOI observability, DIST-003 catch-up (all §1/§4 above).
- **No design doc, no ledger line:** dashboard ADR-020..031 (only ADR entries), the
  `SESSION-2026-06-15` orphan bugs #6/#7/#13/#14 (#8 now RESOLVED via #175, fixed,
  this change), AW-009 R3 TLS, worker heartbeat-expiry loop.
- **Live bugs worth an issue TODAY:** #14 eager teardown (destroys evidence), worker
  heartbeat-expiry loop (failover liveness). (#8 remote-worker volatile-workspace
  data loss: RESOLVED via #175, fixed, this change.)
- **Parked, record-so-not-misread:** RM-021..029 authoring / `aion_kit` (no `aion_kit`
  package exists).
