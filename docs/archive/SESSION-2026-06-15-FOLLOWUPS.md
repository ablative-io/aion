# Session follow-ups — 2026-06-15

## Bugs fixed this session

| # | Issue | Commit | Summary |
|---|-------|--------|---------|
| 1 | collect_all recovery | c297d1b8 | `dispatch_unscheduled` in `nif_collect.rs` now re-dispatches scheduled-but-incomplete activities on `first_arrival`. Without this, `workflow.all` fan-outs hang forever after server restart. |
| 2 | Namespace recovery | 149129e0 | `startup.rs` was hardcoding `namespace: "default"` for every recovered workflow. Now reads `aion.namespace` from `SearchAttributesUpdated` event in history. |
| 3 | Session ID doubling | c297d1b8 | Removed `resolve_session_id` from both workers. With `--resume-if-exists`, norn handles resume directly. |
| 4 | Test FFI missing workflow_id | c297d1b8 | Added `workflow_id/0` to `gleam/aion_flow/test/aion_flow_ffi.erl`. Required after the `workflow.id()` NIF was added. |
| 5 | Stale test assertion | c297d1b8 | `dev_instructions_defer_the_gate_to_the_workflow_test` expected old text. Updated to match current `dev_instructions` constant. |

## Bugs / issues still open

### 6. Workflow ID mismatch in server bridge logs

The server bridge logs workflow IDs that don't match the events table. Seen with IDs `9a962a30`, `b249f2f0`, `4cb80232`, `bdaaa562` — all absent from the DB when queried via sqlite3. Makes operational debugging extremely difficult because you can't correlate server logs with DB state.

**Where to look:** `crates/aion-server/src/worker/bridge.rs` — the `workflow_id` field on `ActivityTask` sent to workers.

### 7. collect_race recovery gap

The `first_arrival` re-dispatch fix in `dispatch_unscheduled` covers both `collect_all` and `collect_race` (they share the function). However, `settle_race` may have its own recovery gap — needs verification that a race whose winner hasn't arrived yet properly re-dispatches after restart.

### 8. Remote worker temp directory cleanup

`provision_clone` creates workspaces in OS temp dirs (`/var/folders/...` on macOS). macOS cleans these up between restarts. Remote workflows lose their workspace permanently.

**Fix:** Use a stable directory like `~/.aion/clones/<branch>` or a configurable path instead of `mktemp`.

### 9. Remote push 403 on omarchy

`annabelgray89` (the Git user on omarchy) lacks write access to `ablative-io` repos. Auth/credential issue — not a code bug.

### 10. cargo fmt diffs

Two files need formatting: `crates/aion/src/runtime/nif_child_engine.rs` and `crates/aion-server/src/worker/bridge.rs`. Blocks BD-007 R5 landing gate.

### 11. Remote worker test failures

`clone_url` field missing from test fixtures in `examples/stacked-dev-remote/worker/tests/wire_compat.rs` and `handlers_shims.rs`. Pre-existing.

## Operational improvements needed

### 12. DB recovery CLI command

We've done manual SQL DELETE of poison terminal events 3 times this session. The pattern is always the same: delete `ActivityFailed`, `WorkflowFailed`, `ChildWorkflowFailed`, and any teardown `ActivityScheduled`/`ActivityStarted`/`ActivityCompleted` events that were recorded because of a restart-induced failure.

**Proposal:** `aion recover <workflow-id>` — identifies in-flight activities at the time of failure, shows the events it would delete, asks for confirmation, deletes them.

### 13. Worker graceful shutdown for in-flight activities

Killing the worker kills norn processes immediately. The worker shows "server drain received; finishing in-flight work" on server disconnect but doesn't do the same on its own SIGTERM. Should: finish the current norn step, checkpoint the session, then exit.

### 14. Teardown firing eagerly on worker connect

Pending teardown activities from previously-failed workflows execute immediately when a fresh worker connects. This can destroy evidence before anyone inspects the failure. Consider: teardown should require explicit dispatch or respect a cooldown.

## Improvements shipped this session

| # | Change | Commit | Summary |
|---|--------|--------|---------|
| 15 | File-length guidance | chiron 6153d66, yg 847739a4 | "Split by responsibility, not by line count" + do_not banning part1/part2 naming |
| 16 | Diagnostics rendering | c297d1b8, c77f3664 | `resume_feedback` parses JSON → readable text with de-duplicated do_not rules |
| 17 | Dispatch waits for workers | 69ebe976 (prior session) | Tested and validated during this session's restart scenarios |

## Design cluster audit results

35 briefs across 6 clusters audited:

| Cluster | Briefs | Done | Partial | Not started |
|---------|--------|------|---------|-------------|
| brief-dev | 8 | 6 | 0 | 2 (BD-007, BD-008) |
| aion-server | 14 | 11 | 3 (AW-006, AW-009, AW-014) | 0 |
| aion-observability | 4 | 0 | 0 | 4 |
| parent-close | 3 | 0 | 0 | 3 |
| aion-kit | 3 | 0 | 0 | 3 |
| workflow-toolkit | 3 | 0 | 0 | 3 (blocked on aion-kit) |
| **Total** | **35** | **17** | **3** | **15** |

### Partial implementations

- **AW-006 R2:** 7 operational values have hardcoded defaults violating CO11 (listen addresses, scheduler threads, drain timeout, JWKS refresh, heartbeat window, outbound buffer bound)
- **AW-009 R3:** TLS not wired — `run.rs` explicitly rejects TLS config with `reject_tls_until_supported`
- **AW-014 R1-R3:** `select_worker` doesn't use rotation index (always lowest-ID worker), bridge dispatch path bypasses round-robin entirely, no distribution tests
