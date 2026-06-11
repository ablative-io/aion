# Design: Namespace Ownership for SCHEDULE Resources

Produced by a Plan agent during the 2026-06 remediation. Implemented and committed as `9f554017`; preserved here as the design record.

## 1. Findings: where schedule state durably lives

**Schedules are fully event-sourced and durable.** All schedule resource state lives as events (`ScheduleCreated`, `ScheduleUpdated`, `SchedulePaused`, `ScheduleResumed`, `ScheduleDeleted`, `ScheduleTriggered` — defined in `aion-core`'s `Event` enum) appended to a single well-known **schedule coordinator workflow history**:

- Coordinator id: fixed UUID `0x..a10a..0004`, returned by `Engine::schedule_coordinator_workflow_id()` (public, `crates/aion/src/engine/api.rs:201`); writes go through a dedicated `schedule_recorder: Arc<AsyncMutex<Recorder>>` — the single-writer invariant is respected.
- The in-memory `ScheduleEvaluator` map (`upsert_state` / `state` / `states`, `crates/aion/src/schedule/evaluator.rs:174-189`) is only a serving **projection**, rebuilt on startup via `recover_on_startup` + `StoreScheduleEventSource` from the coordinator history (`api.rs:466-480`). It is not a source of truth.
- `ScheduleConfig.search_attributes` (added in `crates/aion-core/src/schedule.rs:108-109`) is serialized inside every `ScheduleCreated`/`ScheduleUpdated` event payload, and the server force-stamps `aion.namespace` into it in `stamp_schedule_namespace` (`crates/aion-server/src/api/handlers.rs:100-109`) on **both** create and update paths, before the engine call.

**Conclusion: the stamped `aion.namespace` in the `ScheduleCreated` event's config is the correct projection-consistent ownership fact.** It is durable (event history), atomic with creation (one append), and force-stamped server-side so it cannot be spoofed via the wire envelope.

**Ownership must derive from `ScheduleCreated`, not the latest config.** `ScheduleUpdated` replaces the whole config. Because update will be ownership-gated and re-stamped with the verified owner namespace, the latest config can never legitimately disagree with creation — but deriving from the *first* `ScheduleCreated` for a schedule id makes ownership immutable by construction (a bug in the update path can never migrate a schedule between tenants). This mirrors `project_schedule_state`'s first-create-wins behavior (`crates/aion/src/schedule/state.rs:146-172`).

**No-backwards-compat stance (per CLAUDE.md):** no migration code for schedules created before stamping existed, and no default namespace. A `ScheduleCreated` whose config lacks `aion.namespace` resolves to owner `None` → it is **invisible through every namespaced server API** (NotFound on target ops, excluded from list). This also covers schedules created through the embedded `Engine` API directly: server-mode tenants must create schedules through the server. A non-string `aion.namespace` value is a hard `ServerError::Config` (data corruption), mirroring `HistoryNamespaceSource` exactly (`resolver.rs:132-140`).

**Durability flag (the one weak spot):** schedule ownership verification requires reading the *entire* coordinator history, which grows unboundedly with `ScheduleTriggered` events (no compaction exists; cf. design doc open question 2). This is correct but O(global schedule history) per schedule op, unlike workflow verification which is O(one workflow's history). Acceptable now; note for a future per-schedule visibility index. Nothing about schedule state is insufficiently durable — only this read-amplification deserves a code comment.

## 2. Implementation plan

### 2.1 New file: `crates/aion-server/src/namespace/schedule_source.rs`

Mirrors the workflow source shape in `resolver.rs`:

```rust
#[async_trait]
pub trait ScheduleNamespaceSource: Send + Sync {
    /// Namespace recorded at schedule creation, or None when the schedule is
    /// unknown or its creation config recorded no namespace attribute.
    async fn schedule_namespace(&self, schedule_id: &ScheduleId)
        -> Result<Option<String>, ServerError>;
}
```

- `pub(crate) struct HistoryScheduleNamespaceSource { engine: Arc<Engine> }` — reads `engine.store().read_history(engine.schedule_coordinator_workflow_id())`, scans for the **first** `Event::ScheduleCreated { schedule_id, config, .. }` matching the id, returns `config.search_attributes.get(NAMESPACE_ATTRIBUTE)`:
  - `Some(SearchAttributeValue::String(ns))` → `Ok(Some(ns))`
  - `Some(other)` → `Err(ServerError::Config { message: "schedule {id} recorded a non-string aion.namespace search attribute: {other:?}" })`
  - missing attribute or no matching `ScheduleCreated` → `Ok(None)`
  - `ScheduleDeleted` does **not** erase ownership — a foreign probe of a deleted schedule must still be the guard's NotFound, and the owner's probe falls through to the engine's `ScheduleNotFound`.
- `pub struct StaticScheduleNamespaces` — `Arc<RwLock<HashMap<ScheduleId, String>>>` test fixture with `record(schedule_id, namespace)`, mirroring `StaticWorkflowNamespaces` (`resolver.rs:147-179`) including explicit lock-poison mapping.

### 2.2 `crates/aion-server/src/namespace/resolver.rs`

- Add field `schedule_ownership: Arc<dyn ScheduleNamespaceSource>` to `NamespaceResolver`.
- `from_config(config, engine)`: also construct `HistoryScheduleNamespaceSource` (signature unchanged).
- **Signature changes (no compat shims — change and fix all call sites):**
  - `from_parts(mode, engine, ownership, schedule_ownership)`
  - `authorization_only(mode, ownership, schedule_ownership)`
- Add, directly mirroring `verify_workflow_ownership` (`resolver.rs:303-316`) including its anti-leak doc comment:

```rust
pub async fn verify_schedule_ownership(&self, namespace: &str, schedule_id: &ScheduleId)
    -> Result<(), ServerError> {
    match self.schedule_ownership.schedule_namespace(schedule_id).await? {
        Some(owner) if owner == namespace => Ok(()),
        Some(_) | None => Err(ServerError::Wire {
            wire: WireError::not_found(format!("schedule not found in namespace {namespace}")),
        }),
    }
}
```

Foreign-owned and nonexistent schedules produce this byte-identical `not_found`; a caller with no grant never reaches it (`resolve` runs first in `guard.scope`), preserving "NamespaceDenied = no grant for the requested namespace, nothing else."

### 2.3 `crates/aion-server/src/namespace/guard.rs`

- Add `ScheduleTarget<'a>` mirroring `WorkflowTarget` (lines 249-294):

```rust
#[derive(Clone, Copy)]
pub struct ScheduleTarget<'a> { schedule_id: &'a ScheduleId }
impl<'a> ScheduleTarget<'a> {
    pub const fn schedule(schedule_id: &'a ScheduleId) -> Self { ... }
    pub const fn schedule_id(&self) -> &ScheduleId { ... }
    async fn verify(&self, resolver: &NamespaceResolver, namespace: &str) -> Result<(), ServerError> {
        resolver.verify_schedule_ownership(namespace, self.schedule_id).await
    }
}
```

- Change operation variants and constructors to carry the target (decoded by the handler before scoping, exactly like workflow ops):
  - `UpdateSchedule(&'a ProtoUpdateScheduleRequest, ScheduleTarget<'a>)`
  - `PauseSchedule / ResumeSchedule / DeleteSchedule / DescribeSchedule(&'a ProtoScheduleIdRequest, ScheduleTarget<'a>)`
  - `CreateSchedule` and `ListSchedules` unchanged (no target).
- `verify()` (lines 219-244) per operation:
  - **Create** → `Ok(())` (grant check only; handler stamps; schedule id is server-generated `ScheduleId::new_v4`, so create can never collide with or probe another tenant's resource).
  - **Update / Pause / Resume / Delete / Describe** → `target.verify(...)` (NotFound on miss per anti-leak rule).
  - **ListSchedules** → `Ok(())` (grant check; result filtering is handler-level, like workflow list).
- If non-test guard.rs approaches the 500-LOC limit, extract `WorkflowTarget` + `ScheduleTarget` into `namespace/target.rs` with re-exports from `namespace/mod.rs`.

### 2.4 `crates/aion-server/src/namespace/mod.rs`

Add `pub mod schedule_source;` and re-export `ScheduleNamespaceSource`, `StaticScheduleNamespaces`, `ScheduleTarget`.

### 2.5 `crates/aion-server/src/api/handlers.rs` (schedule handlers, lines 359-585)

Per-handler changes:

- `create_schedule`: unchanged flow (scope → stamp → engine). Already correct.
- `update_schedule`: `required_schedule_id` is already decoded before scoping (line 403) — pass `ScheduleTarget::schedule(&schedule_id)` into `NamespaceOperation::update_schedule(&request, target)`. Keep `stamp_schedule_namespace` after scoping (defense in depth; the verified namespace == owner, so the stamp re-asserts the owner and `ScheduleUpdated` configs can never carry a foreign namespace). Keep config decode **after** scoping (matches the existing "denied ops don't decode payloads" tests).
- `pause_schedule` / `resume_schedule` / `delete_schedule` / `describe_schedule`: decode id, build target, pass to the operation constructor. No other flow change — once verified, the engine's `ScheduleNotFound` (for owner-deleted/raced schedules) already maps to wire `not_found` with `error_type: "ScheduleNotFound"` via `crates/aion-server/src/error.rs:297-298`; that path is intra-tenant only, so no cross-tenant leak.
- `list_schedules`: filter the engine result by stamped owner before encoding:

```rust
fn schedule_in_namespace(state: &ScheduleState, namespace: &str) -> bool {
    matches!(
        state.config.search_attributes.get(crate::namespace::NAMESPACE_ATTRIBUTE),
        Some(aion_core::SearchAttributeValue::String(owner)) if owner == namespace
    )
}
```

  Unstamped or non-string-stamped schedules match no namespace (invisible everywhere — the no-back-compat stance made explicit in a comment). Filtering on the engine's projected `ScheduleState.config` is sound here: it is a fold of the same durable events (single writer, rebuilt on startup), and it is the only list source the engine offers; the *targeted* verification stays on the store-read source per the "never an in-memory map" rule.

**File-size note:** non-test handlers.rs is already ~725 raw lines. Adding target plumbing pushes it toward the 500-LOC (excluding tests/comments) ceiling. The implementer should split the seven schedule handlers plus `stamp_schedule_namespace`, `schedule_in_namespace`, `required_schedule_id`, `required_schedule_config` into a new `crates/aion-server/src/api/schedule_handlers.rs`, with `api/mod.rs` declaring it and `grpc.rs`/`http.rs` updating call paths (or re-export through `handlers` so call sites keep `handlers::create_schedule`). Move the schedule handler tests with them.

### 2.6 Call-site updates (constructor signature changes)

`authorization_only` / `from_parts` callers to update with a `StaticScheduleNamespaces` argument:
- `namespace/resolver.rs` tests, `namespace/guard.rs` tests (`guard_with_ownership`, `denied_guard`)
- `api/handlers.rs` tests (`context_from_engine`, `denied_guard`)
- `stream/subscribe.rs` tests, `worker/registry.rs` tests
- `crates/aion-server/src/state.rs` (uses `from_config` — verify, likely unchanged)

### 2.7 Wire/proto implications

**None.** `ProtoUpdateScheduleRequest`, `ProtoScheduleIdRequest`, `ProtoListSchedulesRequest` (`crates/aion-proto/src/schedule.rs`) already carry `namespace` + `schedule_id`; the guard already reads them (`guard.rs:207-213`). No `.proto` or prost changes. No engine (`crates/aion`) changes — the engine stays namespace-agnostic; `NAMESPACE_ATTRIBUTE` remains an aion-server concept.

## 3. Test plan

**Unit — `schedule_source.rs` / `resolver.rs`:**
1. `HistoryScheduleNamespaceSource` returns the namespace from `ScheduleCreated` config (build an `InMemoryStore`, append a `ScheduleCreated` to the coordinator id via `WriteToken::recorder()` as in existing handler tests).
2. Unknown schedule id → `Ok(None)`; created-without-attribute → `Ok(None)`; non-string attribute → `ServerError::Config`.
3. Ownership is creation-pinned: `ScheduleCreated(tenant-a)` then `ScheduleUpdated` with config stamped `tenant-b` → still `tenant-a`.
4. `verify_schedule_ownership`: owner match → Ok; foreign-owned and nonexistent → **byte-identical** `WireError` (`code == NotFound`, `message == "schedule not found in namespace tenant-b"`, compare full structs), mirroring `ownership_misses_are_indistinguishable_not_found`.

**Guard-level (`guard.rs` tests, `StaticScheduleNamespaces`):**
5. Update/Pause/Resume/Delete/Describe against a `tenant-b`-owned schedule, caller granted `tenant-a` → `NotFound`, engine never touched (extend `denied_targeted_operations_do_not_call_engine`).
6. Caller with no grant for the requested namespace → `NamespaceDenied` for all seven schedule ops (including create and list).
7. Create and List with a granted namespace → `Ok` scoped engine.

**Handler-level (real engine + `InMemoryStore`, resolver via `from_config`-style wiring with the real history sources):**
8. `create_schedule` with a config pre-stamped `aion.namespace = tenant-b` by the caller → stored/returned state shows `tenant-a` (spoof overwritten).
9. Owner round-trip: create → describe/pause/resume/update/list/delete all succeed in `tenant-a`.
10. Cross-namespace denial: caller granted `tenant-b` targets the `tenant-a` schedule id → `NotFound` for update/pause/resume/delete/describe, byte-identical to targeting a random nonexistent `ScheduleId` from `tenant-b`.
11. `list_schedules`: schedules created in `tenant-a` and `tenant-b`; each caller lists only their own; an unstamped schedule (appended directly to the coordinator history) appears in neither.
12. Update cannot migrate ownership: owner updates with config stamped `tenant-b` → describe still shows `tenant-a`; `tenant-b` caller still gets NotFound; list unchanged.
13. Deleted schedule: owner re-delete/describe → `NotFound` (`error_type: "ScheduleNotFound"`); foreign caller → guard `NotFound` (no leak that it ever existed).
14. Denied update does not decode the config envelope before the namespace check (extend the existing "denied … does not decode" pattern).

## 4. Sequencing

1. `schedule_source.rs` (trait + history impl + static fixture + unit tests)
2. `resolver.rs` (field, `verify_schedule_ownership`, constructor signatures)
3. `guard.rs` (`ScheduleTarget`, operation variants, verify arms) + all constructor call-site fixes
4. Handler changes + split into `api/schedule_handlers.rs` + `mod.rs`/transport import updates
5. Handler-level and cross-namespace tests
6. `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

### Critical Files for Implementation
- /Users/tom/Developer/ablative/aion/crates/aion-server/src/namespace/resolver.rs
- /Users/tom/Developer/ablative/aion/crates/aion-server/src/namespace/guard.rs
- /Users/tom/Developer/ablative/aion/crates/aion-server/src/api/handlers.rs
- /Users/tom/Developer/ablative/aion/crates/aion/src/engine/api.rs (read-only reference: coordinator id, schedule engine API)
- /Users/tom/Developer/ablative/aion/crates/aion-server/src/namespace/mod.rs (plus new file namespace/schedule_source.rs)