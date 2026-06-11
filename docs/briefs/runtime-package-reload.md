# Design Brief #62 — Runtime Package Load Seam (Live Reload)

**Repo:** `/Users/tom/Developer/ablative/aion`, main @ `afe5a90c` ("Add aion-cli package subcommand and migrate examples to workflow.toml (#61 Wave 2)").
**beamr:** crates.io `0.4.9` (local mirror at `~/Developer/ablative/beamr`, READ-ONLY; this brief requires **zero** beamr changes — verified below).
**Coordination warning:** the working tree is dirty across `crates/aion/src/engine/{api,builder,delegated,startup}.rs` and most of `crates/aion/src/runtime/` (in-flight #60 work; #58 Waves D/E and the #45/#58 combined closeout are still queued). **This brief implements only after the #58/#45 closeout lands.** All file:line references are HEAD state (`git show HEAD:<path>`). No `git stash` — commit verified waves immediately (hard project rule).
**Consumer:** Meridian workflow-packaging pieces P5/P8, checklist C25–C27 (`yggdrasil/docs/design/workflow-packaging/design.json`): deploy a `.aion` to a *running* server; new dispatches use the new version; running instances keep theirs; `meridian workflow rollback <type>` re-points routing at a prior content hash; version retirement when no running or recoverable instance pins a version.

This brief is self-contained: an implementing agent needs no prior conversation context.

---

## 1. Verified current-state map

### 1.1 The routing map (what actually exists)

There is no routing table separate from the loader record. `LoadedWorkflows` (`crates/aion/src/loader/load.rs:46-50`) holds:

- `by_version: HashMap<(String, ContentHash), LoadedWorkflow>` — exact lookups,
- `by_type: BTreeMap<String, Vec<ContentHash>>` — **"latest" = `versions.last()`** (`latest()`, load.rs:120-124), i.e. *insertion order of `load_package` calls*, not an explicit route pointer,
- `registered_modules: HashMap<String, ContentHash>` — the loader-side collision index over deployed names.

**`LoadedWorkflows` is a plain struct that is `clone()`d, not shared.** `EngineBuilder::build()` clones it into at least four independent owners:

| Owner | Site |
|---|---|
| `Engine.loaded_workflows` (plain field, `&` accessor) | `engine/api.rs:48,171` |
| `ChildNifBridge.loaded_workflows` | `engine/builder.rs:524-535` → `runtime/nif_child_engine.rs:31` |
| `EngineScheduleStarter` (`ScheduleRuntimeDeps`) | `engine/api.rs:96-107` |
| `ProcessExitContext` — **a fresh `Arc::new(clone())` per workflow start** | `lifecycle/start.rs:210` |

A runtime load into any one copy would be invisible to the others. **The first structural job of this brief is replacing every clone with one shared, atomically-swappable catalog.** No external crate reaches `Engine::loaded_workflows()` (verified: no callers in aion-server/aion-cli/aion-nif), so the accessor can change shape freely.

### 1.2 Version pinning (the actual mechanism — contradicts the working assumption)

The working assumption was "pinned through the deployed module name in history". **False.** Durable history carries *no* version information of any kind:

- `Event::WorkflowStarted` (`crates/aion-core/src/event.rs:35-47`) has `workflow_type`, `input`, `run_id`, `parent_run_id` — **no content hash, no deployed module name**.
- `Event::ChildWorkflowStarted` (event.rs:206) likewise records type + input + child id only.
- Pinning is **in-memory only**: `WorkflowHandle.loaded_version: ContentHash` (`registry/handle.rs:107,284`), set at start from the resolved `LoadedWorkflow` (`lifecycle/start.rs:173`) and lost on process exit/engine restart.
- Recovery after restart (`durability/recovery.rs:268-294`) therefore resolves by **`single_loaded(workflow_type)`** (`loader/load.rs:136-151`), which *refuses with a typed error when more than one version of a type is loaded* — explicitly documented as a stopgap "when durable history predates explicit package-version metadata".

**Consequence: multi-version live reload is structurally incompatible with crash recovery today.** Load v2 while a v1 instance is active, restart, and that instance fails recovery ("has 2 loaded versions; active recovery requires an exact persisted package version"). Durable version pinning is a hard prerequisite, not an option (Wave 0 below).

### 1.3 Version resolution per dispatch path (verified, inconsistent)

| Path | Resolution today | Site |
|---|---|---|
| `Engine::start_workflow` | `latest()` | `lifecycle/start.rs:99-105` |
| Schedules (`EngineScheduleStarter`) | `latest()` via its build-time clone | `engine/api.rs:1041-1073` |
| **Child spawn** (`start_child_under_recorded_id`) | **`latest()` at spawn time — NOT parent-pinned** | `runtime/nif_child_engine.rs:168-175` (`..Default::default()` → `loaded_version: None`) |
| Child spawn-recovery sweep (#56 crash window) | `latest()` *at sweep time* | `engine/startup.rs:269-272` |
| Continue-as-new, live paths (engine API + exit monitor) | **pinned to predecessor**: `loaded_version: Some(handle.loaded_version())` | `lifecycle/continue_as_new.rs:123`, `lifecycle/completion.rs:196` |
| Continue-as-new startup sweep (crash between CAN record and successor start) | **`latest()`** — *inconsistent with the live CAN path* | `engine/startup.rs:364-370` |
| Startup recovery of active workflows | `single_loaded()` (refuses multi-version) | `durability/recovery.rs:278` |

Two latent defects fall out of this even before live reload exists:

1. **CAN successor version depends on whether the engine crashed** between the `WorkflowContinuedAsNew` record and the successor's `WorkflowStarted` (live path pins, sweep takes latest).
2. **Child spawn-recovery resolves the version at repair time**, so a crash between the parent's `ChildWorkflowStarted` record and the child's start can produce a different version than a crash-free run — version-resolution nondeterminism in exactly the crash paths replay is supposed to make boring.

### 1.4 Replay determinism for children (analysis the decision hinges on)

Record-then-spawn (#56, `startup.rs:201-213` doc comment) means the parent records `ChildWorkflowStarted` durably **before** the child process exists, and "the parent's replayed spawn resolves from the recorded event" — a replayed parent never re-runs version resolution. So **latest-wins does not break parent replay determinism per se**. What breaks is everything that re-resolves *outside* replay: the spawn-recovery sweep (1.3 #2), the CAN sweep (1.3 #1), and post-restart recovery of the child itself (1.2). The structural fix is the same for every policy choice: **resolve the version once, at record time, and record it durably**. Once the resolution is in history, both "pin to parent" and "latest at spawn" are deterministic; the policy choice (Decision D1) is then about *semantics*, not safety.

One package-format fact constrains D1: a `.aion` package manifest declares exactly one `entry_module` → **one workflow type per package** (`aion-package/src/manifest.rs`, `loader/load.rs:296-331`). A child of a *different* type is necessarily from a *different* package, where "the parent's content hash" does not exist in `by_version`. "Pin child to parent's version" is only well-defined for same-type (recursive) children; for the common cross-package case it is not implementable as stated.

### 1.5 beamr module registry semantics (verified in the 0.4.9 mirror, read-only)

- `ModuleRegistry` is a **`DashMap<Atom, ModuleVersions>`** (`beamr/crates/beamr/src/module.rs:253-257`): `insert`/`lookup` are thread-safe and lock-striped. **Registering a module while schedulers run is safe**; running processes hold `Arc<Module>` code pointers (`CodePointer`, module.rs:199-216) and are untouched by inserts.
- The two-deep current/old version limit applies *per module name*. Aion's content-hash namespacing (`module$hash` via `deployed_name`, `aion-package/src/namespace.rs:80`) gives every package version a globally fresh name, so a runtime load never promotes/evicts anything — that is precisely why invariant 5 (CLAUDE.md) exists. Aion additionally refuses to register over retained old code (`runtime/module.rs:102-105`).
- **Unloading:** `ModuleRegistry::delete_module(name)` removes every retained version (module.rs:409-414) — already exposed as `RuntimeHandle::unregister_module` (`runtime/module.rs:120-129`, currently `pub(crate)` and used only for failed-load rollback). It performs **no process-reference check** ("callers are responsible"); a process still executing the module keeps its `Arc` (code is not freed underneath it), but any *future* external call into the deleted name gets `ExecError::Undef`. The safe-purge machinery with process scanning/killing (`scheduler/module_management.rs:85-136`, `check_process_code`) exists but targets old-version purge by name; for aion's unique names, `delete_module` plus aion-side "no instance pins this version" verification is the correct unload primitive (Decision D2).
- The loader's existing staged registration with rollback-on-failure (`loader/load.rs:218-255`: preflight → register each module → unregister all on first failure, aggregated rollback errors) and hash-collision preflight (`preflight`, load.rs:257-269) carry over unchanged.

### 1.6 Event serialization & version record

Events are serde JSON (`#[serde(tag = "type", content = "data")]`, event.rs:31-32) with `ts_rs` bindings for the dashboard. `aion-core` is a leaf crate and **cannot** depend on `aion-package`'s `ContentHash`; `WorkflowVersion` (`aion-package/src/version.rs:16-27`) already documents the contract: *"Stores that cannot depend on aion-package can persist the textual content-hash form."* So the durable pin is the canonical textual hash (a `String` newtype in aion-core), parsed back to `ContentHash` at the engine boundary. `aion-package` is otherwise ready: `ContentHash`, `deployed_name`/`parse_deployed_name`, `Package::version_record() -> WorkflowVersion` all exist.

### 1.7 Concurrency machinery already present

`Engine` has a `ShutdownGate` (`engine/api.rs:729-800`): `begin_start()` refuses after shutdown begins; `close_and_wait()` drains active operations. Runtime loads must participate (a load racing `shutdown()` → `runtime.shutdown()` would otherwise register modules into a dying VM).

---

## 2. Design

### 2.1 `WorkflowCatalog` — one shared, snapshot-swapped routing authority

New `crates/aion/src/loader/catalog.rs`:

```rust
/// Shared, atomically-swappable workflow package catalog.
pub struct WorkflowCatalog {
    /// Immutable snapshot; readers clone the Arc and resolve against a
    /// consistent view. Swapped wholesale under `mutations`.
    snapshot: std::sync::RwLock<Arc<CatalogSnapshot>>,
    /// Serializes load / route / unload. tokio::sync::Mutex: mutation paths
    /// are async (store scans during unload verification).
    mutations: tokio::sync::Mutex<()>,
    /// In-flight start guards per version (see 2.4).
    pinned_starts: …,
}

struct CatalogSnapshot {
    by_version: HashMap<(String, ContentHash), LoadedWorkflow>,
    /// EXPLICIT route pointer — replaces `Vec::last()` insertion-order "latest".
    routed: HashMap<String, ContentHash>,
    registered_modules: HashMap<String, ContentHash>,
}
```

- **Readers** (`resolve_routed(type)`, `resolve_exact(type, hash)`): take the read lock just long enough to clone the `Arc`, then resolve against the immutable snapshot. A reader sees entirely-before or entirely-after any mutation — no torn state, by construction. (Plain `std::sync::RwLock<Arc<_>>` rather than `arc-swap`: no new dependency, the critical section is one `Arc::clone`, and lock poison maps to the existing typed poison errors per house rules.)
- **Writers** hold `mutations` for the whole load/route/unload, build a *new* `CatalogSnapshot`, and commit it with one write-lock pointer swap. The swap **is** the atomic route flip.
- `LoadedWorkflows` as a build-time accumulation type dissolves into the catalog (NO BACKWARDS COMPATIBILITY: replace, don't wrap — `lib.rs:73` re-exports change accordingly). The staged-load/rollback/preflight/idempotency logic in `loader/load.rs` moves into the catalog's load path intact, including its tests.

Every former clone-site takes `Arc<WorkflowCatalog>`: `Engine`, `ChildNifBridge`, `ScheduleRuntimeDeps`, `ProcessExitContext`, `StartWorkflowContext`/`ContinueAsNewContext` (field type changes from `&LoadedWorkflows`), `StartupRecoveryContext`, and the `ActiveWorkflowRecoverySeam` trait signature.

### 2.2 Load protocol (`Engine::load_package`)

```rust
impl Engine {
    /// Load a validated package into the running engine and atomically route
    /// its workflow type's new dispatches to it.
    pub async fn load_package(
        &self,
        source: WorkflowPackageSource,
    ) -> Result<LoadedWorkflow, EngineError>;
}
```

Ordering (each step justified):

1. **Shutdown gate** — `begin_start()`-semantics (refuse with `EngineError::ShuttingDown` once shutdown begins; a load is new-work admission, not a wind-down operation).
2. **Acquire `mutations`** — one load/route/unload at a time. Dispatch is *never* blocked by this lock (readers only touch `snapshot`).
3. **Parse + validate** — `package_from_source` (existing, `engine/builder.rs:620-632`); archive integrity/hash recomputation already happens inside `Package` loading.
4. **Preflight against the current snapshot** — deployed-name collision check (same hash → idempotent fast path; different hash → typed `EngineError::Load`, exactly today's `preflight`).
5. **Register modules into beamr** — `register_module_with_renames` per module with the package rename map (existing path, safe under running schedulers per 1.5). On any failure: unregister everything registered in this call (existing `rollback_registered`), return typed error. **The snapshot has not been touched: routing, existing versions, and in-flight dispatches are bit-for-bit unaffected.** That is the failure-atomicity guarantee — there is no "half-routed" state because routing is a single pointer swap that only happens after every module is registered.
6. **Entry-point verification** — resolve `deployed_entry_module:entry_function` in the registry (`lookup` + export check) before committing the route. A package whose entry module loads but exports nothing routable must fail the load, not the first dispatch.
7. **Commit** — build the new snapshot: add `(type, hash)` to `by_version`, set `routed[type] = hash`, merge `registered_modules`; write-lock swap. From this instant every new `start_workflow`/schedule fire resolves the new version; every dispatch that resolved before the swap completes on the old version (its modules are still registered — loads never remove anything).
8. **Idempotency:** re-loading an already-loaded hash registers nothing (step 4/5 skip via `registered_modules`, today's `already_committed` logic) and returns the existing record — but **still sets `routed[type] = hash`**. Rationale: "deploy archive X" is a routing intent; re-deploying a previously rolled-back version must take effect. (Loading the currently-routed hash is a full no-op.) This is specified behavior, not a decision point — any other reading makes `deploy` after `rollback` silently dead.

Concurrency vs. recovery: startup recovery runs inside `build()` before an `Engine` value exists, so `load_package` cannot race it. Post-restart recovery resolves pinned versions from history (Wave 0) against whatever the builder loaded; a pinned version absent from the catalog fails *that workflow's* recovery with a typed error naming the hash (per-workflow isolation as in `recover()`, `durability/recovery.rs:134-156`) — it must not abort the engine or other workflows' recovery.

### 2.3 Durable version pinning (Wave 0 — prerequisite, independently valuable)

- `aion-core`: new `PackageVersion(String)` newtype (canonical textual content hash; serde + ts_rs like every identifier in `event.rs:14`). Added as a **required** field to `Event::WorkflowStarted` and `Event::ChildWorkflowStarted` (Decision D4 covers strictness). Recorder signatures (`record_workflow_started*`, the child-start record) gain the parameter; `WorkflowStartRecord` carries it.
- Resolution sites populate it from the `LoadedWorkflow` they resolved (start: `lifecycle/start.rs:99-105` resolves *before* recording — already true, the record at :129 just gains the field; child: the record-then-spawn site resolves per the D1 policy *before* recording `ChildWorkflowStarted`).
- Consumers:
  - **Recovery** (`ActiveWorkflowRecoverySeamImpl`): read the run's `WorkflowStarted.package_version`, resolve `resolve_exact(type, hash)`. **Delete `single_loaded`** (no compat: histories predating the field are not supported — see D4).
  - **Child spawn-recovery sweep** (`startup.rs:214-277`): start the child with `loaded_version: Some(recorded)` from the `ChildWorkflowStarted` event — the crash path now resolves identically to the crash-free path.
  - **CAN startup sweep** (`startup.rs:297-375`): per the D1 CAN policy — either pin to the continued run's recorded `package_version` or take the routed version; whichever is adopted, the sweep and the live monitor path (`completion.rs:155-203`) must implement the *same* rule (closing defect 1.3 #1).
- Ripple (sized honestly): `aion-store` conformance fixtures, `aion-store-libsql` round-trip tests, every test constructing `WorkflowStarted`/`ChildWorkflowStarted` (~40 sites), ts_rs regenerated dashboard types, and the event wire encoding in `aion-proto` if events stream over it (verify at implementation; the `/events/stream` surface serializes `Event`).

### 2.4 The unload race (why `pinned_starts` exists)

`unload_version` must guarantee no instance pins the version. Resident instances: registry scan (`WorkflowHandle.loaded_version`). Recoverable instances: `store.list_active()` + per-history `WorkflowStarted.package_version` (possible only after Wave 0). Remaining window: a starter has resolved the version from a snapshot but not yet durably recorded `WorkflowStarted` (the "registration birth window", `engine/api.rs:636-642`). Closing it: `resolve_routed`/`resolve_exact` return an RAII guard that holds the `(type, hash)` in `pinned_starts` until the start path has inserted the registry handle (or failed). Unload's verification, under `mutations`: (1) swap the version out of the snapshot first — no *new* resolution can produce it; (2) wait for/check zero `pinned_starts` entries; (3) registry scan; (4) store scan; (5) `unregister_module` each module of that version. Any check failing → swap the version back in (snapshot restore is the same single-pointer commit) and return a typed error saying exactly what pins it.

---

## 3. Public API (new surface on `Engine`, all serde-ready per D3)

```rust
/// One loaded version of one workflow type.
pub struct WorkflowVersionInfo {
    pub workflow_type: String,
    pub content_hash: ContentHash,        // textual form when serialized
    pub deployed_entry_module: String,
    pub entry_function: String,
    pub manifest_version: ManifestVersion,
    pub loaded_at: DateTime<Utc>,         // engine-local load instant
    pub route_active: bool,               // routed[type] == content_hash
}

impl Engine {
    pub async fn load_package(&self, source: WorkflowPackageSource)
        -> Result<LoadedWorkflow, EngineError>;            // §2.2
    pub fn list_workflow_versions(&self)
        -> Result<Vec<WorkflowVersionInfo>, EngineError>;  // snapshot read; sorted by (type, loaded_at)
    pub async fn route_workflow_version(&self, workflow_type: &str, version: &ContentHash)
        -> Result<(), EngineError>;                        // re-point: rollback / roll-forward
    pub async fn unload_workflow_version(&self, workflow_type: &str, version: &ContentHash)
        -> Result<(), EngineError>;                        // if D2 commissioned
}
```

- `route_workflow_version` fails typed when `(type, version)` is not loaded (`EngineError::Load`, message naming type + hash + the loaded set) — rollback to a never-loaded hash must be impossible, matching Meridian C26 ("re-points routing at the **prior** content hash", i.e. one that is retained and loaded). It is a snapshot swap under `mutations`: atomic, idempotent.
- `EngineBuilder` keeps `load_workflows`/`load_workflow_sources` (startup loads now populate the catalog; last-loaded-wins per type becomes "the route points at the last source loaded for that type" — identical observable behavior for today's single-version reality, and `build()` should now *reject* two sources with the same type+different hash routing ambiguity? No — multiple versions at startup are legitimate (operator restarts an engine that had v1+v2 live); sources load in order, route lands on the last. Stated explicitly so nobody "fixes" it.)

---

## 4. Test plan

Engine e2e (`crates/aion/tests/engine_reload.rs`, fixtures per `crates/aion/tests/fixtures/README.md` — two compiled versions of one workflow type with distinguishable outputs, e.g. `version/0 -> 1 | 2`):

1. **Load into a running engine + latest-wins:** start on v1 (parked in `receive_signal`); `load_package(v2)`; new start returns v2 behavior; the parked v1 instance, when released, completes with v1 behavior; histories carry the respective `package_version`.
2. **Route-flip atomicity under fire:** task A loops `start_workflow`; task B loads v2 mid-stream. Every start succeeds (zero `WorkflowNotFound`/`Undef`/partial-route errors); every result is *entirely* v1 or v2; all starts initiated after `load_package` returns are v2.
3. **Concurrent load-during-dispatch (stress):** N parallel starters + M sequential loads of distinct versions; assert per-start version consistency between recorded `package_version`, registry `loaded_version`, and output.
4. **Idempotent re-load:** same archive twice → equal records, beamr registration count unchanged (probe via `has_registered_module` + a counting seam like `load_with_rollback`'s closure tests), route unchanged; re-load of a *rolled-back* hash re-routes to it (§2.2 step 8).
5. **Replay of an old-version instance after a newer version loads:** start on v1, record progress (activity/timer), load v2, **rebuild the engine over the same store with both archives**; the v1 instance recovers and completes on v1 (assert deployed entry module + output + byte-identical history modulo new events — the #45 determinism-proof pattern); new starts are v2.
6. **Recovery with a missing pinned version:** restart with only v2 loaded while a v1 instance is active → that workflow's recovery fails typed naming the v1 hash; v2 workflows recover; engine builds.
7. **Crash-window sweeps respect recorded versions:** synthesize a parent history with `ChildWorkflowStarted{package_version: v1}` and empty child history, load v2, build → sweep starts the child on **v1**. Same for the CAN sweep per the adopted D1 policy.
8. **Listing / re-pointing:** load v1, v2 → listing shows both, `route_active` on v2 only; `route_workflow_version(v1)` → flag flips, new starts v1; route to unknown hash → typed error.
9. **Unload (if D2 commissioned):** refuse while route-active; refuse while a Resident instance pins it; refuse while a recoverable (active-in-store) instance pins it; succeed when free, after which `has_registered_module` is false and routing to it fails typed; unload racing a start → either the start wins (unload reports the pin) or the unload wins (the start resolves the routed version instead) — never `Undef`.
10. **Shutdown interaction:** `load_package` after `shutdown()` → `ShuttingDown`; `shutdown()` during a slow load waits (gate drain) and the engine closes cleanly.
11. **Concurrency primitives:** loom-style or stress unit tests on the catalog itself (snapshot swap vs. reader clone; RAII start-guard release on both success and failure paths).

Unit: catalog load/rollback/preflight (ported from `loader/load.rs` tests), route pointer semantics, `single_loaded` deletion fallout, recovery resolution by recorded hash. Gates: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, conformance suite green on both stores.

---

## 5. DECISION points for Tom

### D1 — Child-workflow and CAN-successor version resolution

**What the code does TODAY (verified, §1.3):** children = `latest()` at spawn time, *not* parent-pinned; CAN = pinned-to-predecessor on the live paths but `latest()` in the crash-recovery sweep (inconsistent). Nothing is recorded durably.

**What replay determinism actually requires (§1.4):** record-then-spawn means a replayed parent resolves the child from the recorded `ChildWorkflowStarted`, never re-resolving — so latest-wins does **not** violate parent replay. The forced part is narrower: *the resolved version must be recorded durably at record time*, because the crash-repair sweeps and the child's own recovery DO re-resolve today and are nondeterministic the moment two versions coexist. Wave 0 makes recording mandatory under **every** policy; the policy itself is then a semantics choice.

**A structural fact that reshapes the options:** one workflow type per package (§1.4) — a cross-type child is from a different package, where "the parent's content hash" has no meaning. "Pin to parent's version" is only implementable for same-type recursive children.

- **(a) Pin-to-parent/predecessor where defined, latest otherwise** — tree-snapshot consistency for the recursive case only; cross-package children (the common case) fall back to latest anyway. Two rules where one would do; the consistency it buys is mostly illusory.
- **(b) Always-latest-at-record-time, recorded durably (children AND CAN successors)** — one rule for every spawn-shaped transition. Mixed trees are possible (a long-lived v1 parent spawns v2 children), which is real but is exactly the contract `.aion` schemas exist to police (`input_schema`/`output_schema` in the manifest; the parent talks to the child through recorded payloads, not shared code). For CAN it *changes current live behavior* — deliberately: CAN is the only upgrade path a long-lived workflow has. A pinned-forever CAN chain means an eternal cron-style workflow **never** picks up a deploy and its version can **never** be retired, which guts D2 for precisely the workflows that live long enough to matter. It also closes defect 1.3 #1 in the right direction (sweep and live path both take the routed version, recorded in the successor's `WorkflowStarted`).
- **(c) Pin both children and CAN to the predecessor's version** — maximal conservatism; immortal workflows freeze their code forever, retirement of their version is permanently impossible, and cross-package children still need a second rule.

**Recommendation: (b)**, with the durable record (Wave 0) doing the determinism work. If Tom wants tree-snapshot consistency later, a per-start "version stickiness" option can be added to `StartWorkflowOptions` without unwinding (b).

### D2 — Version retirement / unload

**What beamr supports (verified, §1.5):** `delete_module` removes a uniquely-named version atomically; no process-reference check, but running holders keep their `Arc` (no use-after-free); future calls into the name → `Undef`. Safe-purge-with-process-kill exists only for same-name old-code, irrelevant under content-hash naming. So unload is fully buildable aion-side; the entire safety burden is "prove nothing pins it" (§2.4).

- **(a) Manual `unload_workflow_version` with full safety verification** — Meridian P8 explicitly owns retirement policy ("unload a version when no running or recoverable instance pins it… Meridian's reload endpoint calls the engine seam"), so the engine provides the *mechanism with hard safety checks* and the *listing to decide from*, and the embedder decides *when*. No policy defaults inside aion (house rule).
- **(b) Eager auto-GC on last-instance completion** — requires a version→active-instance index maintained on every terminal event plus the same race handling, to save the embedder one call it is better placed to make (it knows about retained archives, C25). Premature.
- **(c) Deferred/periodic sweep** — a policy timer inside aion; violates the no-assumed-defaults rule (what interval?) and duplicates (a) behind a config knob.

**Recommendation: (a)** now (it is ~the §2.4 mechanism plus one public method), (b)/(c) never inside aion — if automation is wanted, Meridian schedules calls to (a).

### D3 — aion-server exposure now vs. embedded-only

- **(a) Embedded-only API now; server endpoint in a follow-up brief** — Meridian embeds the engine directly (its reload endpoint is *Meridian's*, calling this seam), so P8/C27 are fully unblocked with zero wire-contract churn. The wire cost of an endpoint is real: proto messages, `WireErrorCode` rows, CLIENT-CONTRACT, four client SDKs, namespace/authz semantics for a *code-deployment* surface (a categorically more privileged operation than start/signal — it deserves its own authz design, not a bolt-on).
- **(b) Endpoint now** — the #37 live-conformance closeout runs against a live server and would mildly benefit from restartless fixture deploys, and aion-server deployments get parity. But #37's remaining scope (Rust client wave + conformance) does not *require* it, and rushing a deployment-privilege endpoint without the authz story is the wrong kind of ambitious.

**Recommendation: (a)**, with every listing/result type serde-derived from day one (§3) so the later endpoint is mechanical, and the follow-up brief explicitly covering deploy authz. Revisit immediately if the #37 closeout in practice needs restartless deploys.

### D4 — Strictness of the new `package_version` event field

- **(a) Required field, no `Option`, `single_loaded` deleted** — pre-1.0 NO BACKWARDS COMPATIBILITY rule (CLAUDE.md); histories written before Wave 0 fail deserialization and dev/test stores get reset. One code path, no zombie fallback that silently picks an arbitrary version.
- **(b) `Option<PackageVersion>` with `single_loaded` retained as the `None` fallback** — tolerant of existing histories, but institutionalizes the exact ambiguity this brief exists to kill, forever, in every consumer.

**Recommendation: (a).** Aion has shipped nowhere durable; this is the last cheap moment to make the field load-bearing. (Flag: Tom should confirm no store he cares about holds histories that must survive Wave 0.)

---

## 6. Waves & interaction with in-flight work

**Sequencing constraint (hard):** `crates/aion` is mid-#60 with a dirty working tree across `engine/*` and `runtime/*`; #58 Waves D/E and the #45/#58 combined closeout are queued ahead. This brief **implements only after that closeout lands**, then rebases. Files this brief must touch that the in-flight work also touches: `engine/builder.rs`, `engine/api.rs`, `engine/startup.rs`, `runtime/nif_child_engine.rs`, `lifecycle/*` — do not start Wave 0 from a stale base.

- **Wave 0 — durable version pinning (1 agent, blocking everything):** `PackageVersion` in aion-core; `WorkflowStarted`/`ChildWorkflowStarted` fields; recorder/start/child-record plumbing; recovery-by-recorded-hash; both sweeps pin recorded versions; delete `single_loaded`; conformance + fixture + ts_rs fallout. *Exit: workspace green; restart-with-two-loaded-versions recovery e2e passes (test 5).* Independently valuable even if reload never ships (fixes 1.2, 1.3 #1, 1.3 #2).
- **Wave 1 — `WorkflowCatalog` (1 agent):** catalog with explicit route pointer + snapshot swap + start guards; thread `Arc<WorkflowCatalog>` through every former clone-site; `LoadedWorkflows` dissolved; behavior identical for the single-version world. *Exit: full suite green with zero new public surface.*
- **Wave 2 — runtime load + routing API (1 agent):** `Engine::load_package`, `route_workflow_version`, `list_workflow_versions`, shutdown-gate integration, `unload_workflow_version` if D2(a) adopted. *Exit: tests 1–4, 8–10.*
- **Wave 3 — e2e + stress (1 agent, parallel-capable with Wave 2 review):** fixtures (two versions), tests 5–7, 11, determinism proofs.
- **Wave 4 — review:** Fable-level rigorous review per CLAUDE.md (brief + intent + files); patient-records standard; then notify Meridian that P8's seam dependency (D9) is live.

Estimated size: Wave 0 ≈ 1.2k LoC (mostly test fallout); Wave 1 ≈ 1k; Wave 2 ≈ 800; Wave 3 ≈ 900.

---

### Appendix: key file:line index (HEAD `afe5a90c`)

| Concern | Location |
|---|---|
| Routing map + `latest()` + `single_loaded` | `crates/aion/src/loader/load.rs:46-50, 120-124, 136-151` |
| Staged load / rollback / preflight / idempotency | `crates/aion/src/loader/load.rs:218-293` |
| Builder load + clone fan-out | `crates/aion/src/engine/builder.rs:94-110, 486, 524-535, 565` |
| Engine field + schedule-starter clone | `crates/aion/src/engine/api.rs:48, 96-107, 171, 1041-1073` |
| Start resolution + in-memory pin + per-start clone | `crates/aion/src/lifecycle/start.rs:99-105, 173, 210` |
| Child spawn = latest() | `crates/aion/src/runtime/nif_child_engine.rs:145-178` |
| CAN live pin | `crates/aion/src/lifecycle/continue_as_new.rs:123`, `completion.rs:196` |
| Sweeps re-resolving (crash-path nondeterminism) | `crates/aion/src/engine/startup.rs:269-272, 364-370` |
| Recovery via `single_loaded` | `crates/aion/src/durability/recovery.rs:268-294` |
| `WorkflowHandle.loaded_version` | `crates/aion/src/registry/handle.rs:107, 284` |
| Event shapes (no version today) | `crates/aion-core/src/event.rs:31-47, 206` |
| Module register/unregister boundary | `crates/aion/src/runtime/module.rs:53-74, 95-129` |
| ShutdownGate | `crates/aion/src/engine/api.rs:729-800` |
| beamr registry (read-only ref) | `beamr/crates/beamr/src/module.rs:253-431`, `scheduler/module_management.rs:85-136` |
| `WorkflowVersion` / namespacing | `crates/aion-package/src/version.rs:16-27`, `namespace.rs:80-123` |
| Meridian consumer contract | `yggdrasil/docs/design/workflow-packaging/design.json` P5/P8, checklist C24–C27 |
