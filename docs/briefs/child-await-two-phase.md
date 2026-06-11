# Implementation Brief #58 — Two-Phase Suspending Child Awaits (`await_child` + `collect_all`/`collect_race`/`collect_map`)

**Repo:** `/Users/tom/Developer/ablative/aion`, main @ `0f51a0a8` ("docs: query brief decisions signed off — strict resolution, all nine").
**beamr:** crates.io `0.4.9` (`Cargo.toml:63`; local mirror `~/Developer/ablative/beamr/crates/beamr`, READ-ONLY). **This design requires ZERO beamr changes** — verified mechanism-by-mechanism below; anything that would need one is explicitly rejected.
**Coordination warning:** other agents are editing `crates/aion-worker` and `sdks/` (worker-reconnect #46) and brief #45 (query execution) is the sibling workstream in the *same* runtime module set. All file:line references are HEAD state (`git show HEAD:<path>` if the working tree looks mid-edit). No `git stash`; commit verified waves immediately.
**Mandate (Tom, signed off in #45 Q7):** converting child awaits from dirty blocking NIFs to two-phase suspending natives ships **in this delivery**, and "query a parent parked in `await_child`" is a passing e2e before the delivery is complete. This brief also disposes of task **#56** (child-initiation crash window — folded in, see §2.5) and the task **#55** LOWs in the same files (folded in, §2.7).

This brief is self-contained: an implementing agent needs no prior conversation context.

---

## DECISIONS — ADOPTED (orchestrator under Tom's standing "no halfway house" directive, 2026-06-11; Tom may override)

- **D-1 → abort-the-await, leave-the-child-running.** Scope expiry returns the typed scope error; the child runs on and remains awaitable. Child-cancellation-on-timeout is a real product surface (Temporal cancellation types) and is NOT commissioned here — it gets its own brief if wanted. This is current-shipped semantics made suspension-safe, not an accommodation.
- **D-2 → no.** Fire-and-forget child terminals are not recorded into parent history (matches HEAD). An always-watch spawn flag changes history shape for every detached spawn; not commissioned.
- **D-3 → activity-collect is the v1 contract, and the dormant child-fan-out `concurrency/` module is REMOVED in Wave C.** CLAUDE.md bans zombie code; a module nothing ships is zombie code. The new watcher machinery is the natural substrate if child-collect is ever commissioned — rebuilding from a live seam beats preserving a dead one.
- **D-4 → keep Failed-mapping with message prefixes.** Distinct ChildWorkflowCancelled taxonomy touches event taxonomy + 4 SDK error surfaces; separate brief if wanted.

Sequencing note: Wave A (durability) is dispatched after #45 Wave 1 lands — it is logically independent, but same-crate concurrent agents were ruled out after the 2026-06-11 churn incidents.

---

## 0. Scope ruling: what "child awaits" actually are at HEAD

The task framing says "child awaits: `await_child` and `collect_*`". Verified reality at HEAD:

- `await_child` **is** a child-workflow await (`runtime/nif_child.rs:98-134`).
- `collect_all`/`collect_race`/`collect_map` are **activity fan-out**, not child fan-out: the Gleam surface takes `List(Activity(i, o))` (`gleam/aion_flow/src/aion/workflow/concurrency.gleam:22-75`), the NIFs decode `ActivitySpec { name, input, config }` and record `ActivityScheduled/Completed/Failed/Cancelled` (`runtime/nif_concurrency.rs:25-30, 125-160`). The AT-cluster Rust module `crates/aion/src/concurrency/{all,race,map,correlation}.rs` *was designed* for child-workflow fan-out ("Spawn a fixed set of child workflows…", `concurrency/mod.rs:3-10`) but is only referenced from the NIFs via `std::hint::black_box(type_name::<…>())` coverage shims (`nif_concurrency.rs:247, 279-281, 446`).

**Ruling for this brief:** convert what ships. `await_child` gets the child-terminal wake machinery (§2.1); `collect_*` get two-phase **parallel activity** execution reusing the proven `dispatch_activity` async machinery (§2.3). The child-terminal wake machinery is deliberately shaped so a future child-collect surface reuses it without rework. Whether a child-collect surface should ever ship is recorded as an open decision (§6, D-3).

All five problem dimensions from the task (notification, race, partial progress, recovery, exactly-once initiation, continue-as-new) are answered in §2 against the surfaces that actually exist.

---

## 1. Verified current-state map

### 1.1 The blocking awaits (what we are removing)

- **NIF registrations** — `runtime/engine_nifs.rs:156-166`: `spawn_child/3`, `await_child/1`, `collect_all/2`, `collect_race/2`, `collect_map/2` are all `NifEntry::dirty`. The registration test at `engine_nifs.rs:224-265` pins the dirty/normal partition and must be updated when entries flip.
- **`await_child` blocking point** — not `run_until_exit` anymore (that survives only in the activity path, `runtime/handle/delivery.rs:33`, and the per-workflow monitor, `runtime/outcome.rs:38`). The block is `CompletionMailbox::new` (`runtime/nif_child_engine.rs:96-123`): `tokio_handle.block_on` over a `watch::Receiver<Option<TerminalOutcome>>` loop — **a dirty thread is held for the child's entire lifetime**. beamr's dirty IO pool defaults to **10 threads** (`beamr/src/scheduler/dirty.rs:24`, `DEFAULT_DIRTY_IO_THREADS`); ten parents parked on long-lived children exhaust it engine-wide. aion exposes no dirty-pool size knob (`runtime/config.rs`, `runtime/handle.rs:103-107` uses `SchedulerConfig::default()` for dirty fields).
- **`collect_all`/`collect_map` blocking point** — `nif_concurrency.rs:241-272`: activities run **sequentially**, each via the synchronous `dispatcher.dispatch_from_process` (`activity/bridge.rs:40-49`) on the dirty thread. No parallelism exists today despite the combinator name.
- **`collect_race` is not a race** — `nif_concurrency.rs:274-356`: the *first* spec in input order is dispatched synchronously and declared winner (`:316-341`); the rest are recorded `ActivityCancelled` without ever running (`:342`). First-settle semantics promised by the SDK (`concurrency.gleam:40-43`) are not delivered.
- **None of these are yield points** for the #45 query pump: a parent inside them cannot service queries (#45 brief, "Dirty blocking awaits", and DECISIONS Q7).
- `with_timeout` interaction: `receive_signal`, `sleep`, `await_activity_result` all check `expired_scope_message` (`nif_timeout.rs:52`) on re-entry; **the child/collect awaits never do** — `with_timeout` around `child.await` cannot fire today.

### 1.2 The established two-phase pattern (what we are converting *to*)

Three natives already ship it; the conversion must be mechanically parallel to them:

| Concern | Where |
|---|---|
| Park, never block | `ctx.request_suspend(None)` then return NIL — `nif_signal.rs:359-362`, `nif_timer.rs:189-196`, `nif_activity_dispatch.rs:239-240`. beamr re-invokes the native **from the top on any mailbox wake** (`beamr/src/interpreter/opcodes/trampoline.rs:206-240`, `handle_suspend` stores the call position and `Waiting` state; verified at 0.4.9). |
| Await identity pinned | `EngineNifState::pending_awaits: DashMap<pid, PendingAwait>` (`nif_state.rs:63, 74-87`) — pinned at first live arrival so re-entries resolve the same logical operation; ordinal sequences advance on allocation, so a re-entry must never re-allocate. Cleared on terminal resolution and by `cleanup_process` (`nif_state.rs:112-116`) from the monitor thread (`runtime/monitor.rs:61`). |
| Wake markers are pure wakes | one atom per arrival, durable state in history/runtime maps, any await may consume any marker — `nif_wake.rs:1-60` (marker array `:28-34`: `activity_complete`, `activity_failed`, `aion_activity_result`, `aion_signal_received`, `aion_timer_fired`). One marker consumed per invocation or the suspend busy-spins (`:14-26`). |
| Record-before-wake | the arrival service records durably through the target's single Recorder *first*, then enqueues the marker: `signal/router.rs:36-94` (terminal guard + record under recorder lock `:42-61`, deliver `:63-84`). The await native then resolves purely from recorded history (`nif_signal.rs:183-189`). |
| Delivery retry | `enqueue_signal_marker_with_retry` (`delivery.rs:267-300`) covers beamr's just-spawned/executing windows; `deliver_signal_received` (`:64-69`) and `wake_workflow` (`:151-158`) are the precedents. `Scheduler::enqueue_atom_message` is `beamr scheduler/mod.rs:979`. |
| Replay resolution | per-NIF-call `NifContext` builds a fresh run-segment resolver (`nif_context.rs:134-171`, run-segment slice `:159`), `resolve_command` fast-forwards past already-consumed commands (`:398-413`). |

### 1.3 The #41 child-correlation machinery (landed; we build on it)

- Run-scoped child ordinals on the handle: `registry/handle.rs:138, 177-190` (`allocate_child_ordinals`), surfaced as `NifContext::next_child_ordinal` (`nif_context.rs:211-222`), used by `next_child_key` (`nif_child.rs:163-172`).
- Positional `CorrelationKey::Child(n)` — n-th spawn ↔ n-th recorded `ChildWorkflowStarted` in the run segment (`durability/correlation.rs:15-25, 75-88, 114-118`).
- `AwaitChild` is keyed by **child workflow id**, not position: `Command::AwaitChild` has no key (`durability/command.rs:48-52, 71-80`); resolution via `fast_forward_to_child_terminal` + `resolve_child_terminal` → `ChildTerminalResolveResult` (`durability/cursor.rs:188-227`, `resolver.rs:95-113`).
- E2E coverage: `crates/aion/tests/child_workflows_e2e.rs` — live round trip, restart-after-terminal no-respawn, restart mid-child, continue-as-new run scoping, interleaved-signal ordinal stability. **All five must stay green.**
- Pid-watermark/exit-tombstone machinery: `runtime/handle.rs:84-88, 365, 410-418` — a pid ≤ watermark stays observable through scheduler exit tombstones after death; `monitor_process` accepts already-exited pids (`runtime/monitor.rs:38-44`).

### 1.4 Child lifecycle and terminal observation today

- Every workflow (parent or child) gets a dedicated OS monitor thread (`runtime/monitor.rs:45-68`) that waits for process exit, then `handle_process_exit` (`lifecycle/completion.rs:52-140`): records the run's terminal into **its own** history under the recorder lock with an atomic already-terminal check (`:72-104`), reconciles registry + flips residency to `Suspended` (`:142-153`), and publishes `TerminalOutcome` on the handle's `CompletionNotifier` watch channel (`:124, :138`; channel semantics `registry/handle.rs:47-94` — `send_replace`, late subscribers observe the stored value).
- The **parent-side** `ChildWorkflowCompleted`/`ChildWorkflowFailed` event is recorded only inside the parent's blocked `await_child` NIF after the watch channel yields (`nif_child.rs:121-133` → `child/spawn.rs:316-356` → `record_child_event` `nif_child_engine.rs:337-396`), using a hand-computed envelope seq (`nif_child.rs:174-190`) — fragile and racy with async-arrival appends; it dies in this conversion.
- `TerminalOutcome::Cancelled/TimedOut` are mapped to `ChildWorkflowFailed` with `cancelled:`/`timed_out:` message prefixes (`nif_child_engine.rs:138-168`). `Event::ChildWorkflowCancelled` exists but `resolution_from_matched` rejects it (`resolver.rs:267-274`) — keep the Failed mapping (taxonomy change is out of scope, §6 D-4).
- **No process link exists in production**: `child/spawn.rs` requests `ChildWorkflowSpawnMode::Linked` (`:208-223`) and its module doc claims AE links, but `NifChildEngine::spawn_child_workflow` (`nif_child_engine.rs:229-275`) ignores `request.mode` *and* `request.run_id` and just calls `start_workflow_with_options`. Parent death does not propagate to children. (This is load-bearing *good news* for recovery: children keep running while a parent is down.)
- `NifChildEngine::deliver_workflow_message` supports only `SignalReceived` (`nif_child_engine.rs:210-227`) — there is no child-terminal mailbox delivery anywhere.

### 1.5 Continue-as-new in the child (problem #6 input facts)

- `continue_as_new` NIF records `WorkflowContinuedAsNew` then cancels its own pid (`runtime/nif_continue_as_new.rs:38-93`).
- The exiting run's monitor sees the recorded CAN terminal, starts the replacement run (same `workflow_id`, fresh `run_id`, `parent_run_id` chain) and notifies `TerminalOutcome::ContinuedAsNew` (`lifecycle/completion.rs:106-126, 155-204`). The replacement gets a **new registry entry and a new CompletionNotifier** (`lifecycle/start.rs:167-199`).
- `CompletionMailbox` hard-fails on CAN: `outcome_to_message` returns `child_continued_as_new_without_terminal_result` (`nif_child_engine.rs:169-172`) — a parent awaiting a child that CANs gets an error today (**#56's second half**).
- Registry lookups by bare `workflow_id` use `registry.list().find(…)` (`nif_child_engine.rs:101-107, 192-200`) — after CAN there are *two* handles for the workflow id and `.find` picks an arbitrary one. Run-chain truth lives in history: `terminal_outcome_from_history` scans the **latest** run (`engine/api.rs:890-943`) vs the run-scoped variant (`lifecycle/completion.rs:206-273`); `read_run_chain` exists on the store (`aion-store` `run_chain_from_history`, used at `durability/recovery.rs:378-384`).

### 1.6 Recovery (problem #4/#5 input facts)

- Startup recovery enumerates `list_active`, re-spawns each via `ActiveWorkflowRecoverySeamImpl` (`durability/recovery.rs:255-294`) **while iterating** (`engine/startup.rs:97-189`): a recovered parent's process can be executing NIFs before sibling children are registered — a real registration-ordering window the new design must tolerate (today's `CompletionMailbox::new` would `unknown_child_workflow` in it).
- Terminal workflows are *not* re-registered (`startup.rs:111-118` skips them): a child that completed before the crash has **no handle** after restart — its terminal is recoverable only from its own durable history.
- Replay is organic: re-executed workflow code resolves per-NIF against the run segment (`nif_context.rs:398-413`); the strict `Replay::drive` driver is not on the production startup path.
- **#56 first half, verified:** spawn-before-record. `spawn_with_mode` calls `engine.spawn_child_workflow` (`child/spawn.rs:298` → `start_workflow_with_options`, which durably records the *child's* `WorkflowStarted` **and spawns its process** before returning, `lifecycle/start.rs:128-158`) and only then records the parent's `ChildWorkflowStarted` (`child/spawn.rs:306-312`). Crash in between ⇒ a live, durable child the parent has no record of; parent replay resolves `SpawnChild` as `ResumeLive` and starts a **second** child. Duplicate execution + orphan.
- `start_workflow_with_options` accepts a caller-supplied `workflow_id` and resumes the recorder head from existing history (`start.rs:107-121`) — the hook that makes record-then-spawn possible (§2.5).

### 1.7 The query pump contract this work must adopt (#45, signed off)

From `docs/briefs/query-execution.md` §3.1–3.2 + DECISIONS: queries arrive as a per-pid `PendingQuery` queue + `aion_query` wake marker; **every suspending native, on every invocation (fresh entry and wake re-entry), calls `take_pending_query_sentinel(state, pid)` queries-first** — before its own resolution, before any recorded-result fast path — and returns the sentinel `{error, <<"aion_query:{json}">>}` when one is queued, leaving its `pending_awaits` pin untouched; the SDK pump (`aion/internal/pump.gleam` + `aion_flow_query_pump.erl`) services the handler and re-enters the await. The helper lives in the new `runtime/nif_query_pump.rs` (#45 Wave 1). The `aion_query` marker is added to the `consume_wake_marker` array. **Hard prerequisite: #45 Wave 1 must land before (or be co-developed with) Wave B below.** The converted natives adopt the identical entry-check; the new `aion_child_terminal` marker (§2.1) must be added to the same consumable-marker array so a query wake consumed by a child await (or vice versa) stays harmless (`nif_wake.rs:22-26` property).

### 1.8 Defects discovered during verification (all in-scope; each gets a test)

- **D1 (#56a)** — spawn-before-record exactly-once gap (§1.6). Fixed by record-then-spawn + recovery sweep (§2.5).
- **D2 (#56b)** — `CompletionMailbox` fails on child CAN and is ambiguous across run chains (§1.5). Fixed by the run-chain-following watcher (§2.1, §2.6).
- **D3 — strict activity walk vs interleaved arrivals.** `HistoryCursor::resolve_activity` (`cursor.rs:296-338`) consumes `Scheduled→terminal` and **mismatches on any foreign event inside the range** (`:333`, `_ => mismatch_at_index`). Any async arrival recorded between an activity's `Scheduled` and its terminal — a `SignalReceived` from the router, a watcher-recorded child terminal, or a *parallel* activity's events — turns replay of that activity into a `NonDeterminism` failure. This is latent at HEAD (no e2e replays a history with an arrival inside an activity range; `tests/` has no such fixture) and becomes routine the moment `collect_*` records N parallel activities. Fixed in Wave A: the terminal walk skips (does not consume, does not mismatch on) events that are not for the matched activity id, preserving the key-equality determinism check at the `Scheduled` anchor. Same id-keyed-scan philosophy as `fast_forward_to_child_terminal`.
- **D4 — SDK/engine result-envelope mismatch.** `aion/child.gleam:103-115` decodes `"ok:"`/`"error:"`-prefixed payloads inside the `Ok` branch (the pure-Gleam harness honors this, `test/aion_flow_ffi.erl:277-285`), but the production NIF returns the bare payload as `{ok, Payload}` and child failure as `{error, Details}` (`nif_child.rs:112-117, 126-129`). Production `child.await` through the typed SDK would misdecode. Fix direction: engine adopts the SDK contract (`{ok, <<"ok:", Payload>>}` / `{ok, <<"error:", Details>>}` for child-failure-as-data; `{error, …}` reserved for engine faults), since the SDK + harness already agree. Wave B, with a Gleam-side test.
- **D5 — runtime completion-map leak.** `RuntimeHandle::activity_results/activity_errors` entries are removed only by `take_*` (`delivery.rs:213-231`); a completion delivered after the workflow stopped awaiting (race losers, post-exit deliveries) leaks. Wave C: monitor cleanup drains both maps for the exiting pid alongside `cleanup_process`.
- **D6 (#55 LOWs, same files)** — (i) misleading comment `correlation.rs:81-84` (on 64-bit the `usize→u64` conversion is infallible and a `None` key does *not* reliably surface "loudly"; rewrite to state the actual contract); (ii) dead arms `cursor.rs:267-281` — unreachable because `resolve_next` requires `found.key == expected_key` and terminal child events carry `key: None` (`correlation.rs:123-131` keys only `ChildWorkflowStarted`), so `resolve_child` can only ever be entered positioned at a `ChildWorkflowStarted`; delete the arms, simplify; (iii) O(n²) fast-forward — `next_matchable_index` rescans from index 0 via `.enumerate().skip(position)` per step (`cursor.rs:371-377`) and both fast-forwards advance one matchable at a time (`:178-206`); make the scan resume from the current position. All three are Wave A.

---

## 2. The hard problems — chosen mechanisms

### 2.1 Problem 1 — child-terminal notification as a wake marker (the core mechanism)

**Today** the watcher *is* the parked dirty thread. **Chosen mechanism: an engine-side child-terminal watcher task per armed await, applying the signal router's record-before-wake discipline to the parent's history.**

Candidates considered:

| Candidate | No beamr change | Survives CAN | Survives recovery | Marker-only mailbox invariant | Verdict |
|---|---|---|---|---|---|
| **A — per-await tokio watcher task: store-truth loop + CompletionNotifier doorbell; records parent-side terminal under the parent Recorder, then enqueues `aion_child_terminal` marker with standard retry** | ✓ | ✓ (follows run chain) | ✓ (await re-arms on replay, watcher is rebuilt) | ✓ | **RECOMMENDED** |
| B — central ChildTerminalRouter with a durable parent→child routing table hooked into `handle_process_exit` | ✓ | ✓ | needs table reconstruction at recovery — which is exactly "the await re-arms", i.e. A with extra moving parts | ✓ | rejected (strictly more machinery for the same recovery story) |
| C — beamr process link/monitor from parent to child process | ✓ (links exist) | ✗ (CAN replaces the pid; monitor messages name pids) | ✗ | ✗ (a `{'DOWN',…}` tuple in the workflow mailbox violates the atoms-only contract `nif_wake.rs:50-59` and would busy-spin every suspending await) | rejected |
| D — poll child history from the woken native on timer wakes | ✓ | ✓ | ✓ | ✓ | rejected: latency/cost; and something must *generate* the wake — that something is candidate A |

**Watcher specification** (new `runtime/nif_child_watch.rs`, owned by `ChildNifBridge`):

- Keyed dedup map `DashMap<(parent_pid, child_workflow_id), AbortHandle>` on the bridge; arming is idempotent (second arm is a no-op). Aborted by `cleanup_process`-adjacent teardown when the parent process exits (wire through the same monitor callback path that calls `nif_state.cleanup_process`, `monitor.rs:61`), and self-terminating on parent-terminal observation.
- Loop (store is truth; notifier is a doorbell):
  1. `store.read_history(child_id)`; compute the **latest run's** outcome (`engine/api.rs:890-943` semantics).
  2. Real terminal (`Completed|Failed|Cancelled|TimedOut`) → go to step 4.
  3. `ContinuedAsNew` or still-running → resolve the child's *current* run handle from the registry (latest non-terminal run for that workflow id — fix the bare-`workflow_id` `.find` ambiguity here, §1.5); if found, `await receiver.changed()` on its `CompletionNotifier` then loop; if not found (recovery registration window, CAN replacement window) → bounded async backoff (reuse `SignalDeliveryConfig`-style policy from the builder; **no hardcoded values** per CLAUDE.md — thread the policy through `ChildNifBridgeParts`) and loop. The store re-read on each iteration makes every missed-edge race converge.
  4. **Record parent-side terminal, idempotently, under the parent recorder lock** (single-writer invariant 3): re-read parent history; skip if the parent's current run segment already holds a `ChildWorkflowCompleted/Failed` for this child id; skip-and-stop if the parent run is terminal (mirror of the signal router's atomic terminal guard, `signal/router.rs:42-61`). Otherwise append `ChildWorkflowCompleted(result)` or `ChildWorkflowFailed(error)` via the recorder's `record_child_workflow_*` (the `record_child_event` body, `nif_child_engine.rs:337-396`, moves here; the hand-rolled `ChildWorkflowRecordingContext` seq computation in the NIF path dies). Cancelled/TimedOut map to Failed exactly as `outcome_to_message` does today (`:138-168`).
  5. Enqueue the new **`aion_child_terminal`** atom marker to the parent pid via a new `RuntimeHandle::deliver_child_terminal(pid)` (mirror of `deliver_signal_received`, `delivery.rs:64-69`, using `enqueue_signal_marker_with_retry`). Delivery failure after a durable record is non-fatal: trace and exit (the parent is terminal/crashed; recovery resolves from the recorded event). Remove the dedup entry.

This is precisely the signal pattern with "signal router" → "child watcher" and `SignalReceived` → child-terminal events: recorded history is the only state the await native reads.

### 2.2 Problem 2 — race semantics under suspension (`collect_race`, activity fan-out)

- **First arrival:** pin `PendingAwait::Collect { base_ordinal, count, kind: Race }`; record `ActivityScheduled`+`ActivityStarted` for all N ordinals (contiguous at this point — no interleaving yet), dispatch **all N** asynchronously via the existing `spawn_completion_task` machinery (`nif_activity_dispatch.rs:177-211`; completions come back through `deliver_activity_completion_message`/`deliver_activity_failure_message` keyed `(pid, ordinal)` + `activity_complete`/`activity_failed` markers, `delivery.rs:82-116`), then suspend. This makes `race` a *real* first-settle race for the first time.
- **Winner determination (live):** on each wake re-entry, scan ordinals `base..base+N` in order: a recorded terminal (from a previous re-entry) or a takeable runtime-map completion (`take_activity_result/take_activity_error`, `delivery.rs:213-231`) settles the race. The **first terminal recorded into history wins**; when several completions are sitting in the maps on one wake, take-and-record in ordinal order — the lowest ordinal of that batch becomes the recorded winner. Record the winner's `ActivityCompleted`/`ActivityFailed`, then record `ActivityCancelled` for every ordinal without a terminal, drop their runtime-map entries (D5 hygiene), clear the pin, return first-settle result (success or failure — `concurrency.gleam:40-43` contract).
- **Losers' notifications:** in-flight dispatcher futures are not killable (they are dispatcher-owned futures, not processes); their late deliveries insert map entries and enqueue markers. Markers are pure wakes consumed harmlessly by whatever await is parked (`nif_wake.rs:22-26`); map entries are removed eagerly at race return when present, and any straggler is drained by the D5 monitor cleanup. No recorded event ever results from a loser's late completion — recording only happens inside the awaits.
- **Replay determinism:** the recorded history for a settled race contains exactly one non-cancelled terminal among the N ordinals (winner) and N−1 `ActivityCancelled`. Replay resolution does **not** re-race and does not consult runtime maps: it derives the winner as *the ordinal whose recorded terminal is Completed/Failed* (equivalently: earliest-seq terminal) via a direct run-segment scan helper (`activity_terminal_recorded(context, ordinal) -> Option<Completed|Failed|Cancelled>`, the activity analogue of `timer_terminal_recorded`, `nif_timer.rs:253-271`). Byte-identical across replays because it reads only recorded events. The `resolve_command` path is *not* used for per-ordinal resolution here because `resolution_from_matched` rejects `ActivityCancelled` (`resolver.rs:267-274`) — the direct scan sidesteps that without taxonomy changes.

### 2.3 Problem 3 — `collect_all`/`collect_map` partial progress ("k of N")

**Where the "k of N" state lives: recorded history, re-derived per invocation — never in memory.**

- **First arrival:** allocate base ordinal once (`allocate_activity_ordinals(N)`, `nif_context.rs:196-199`) and pin `PendingAwait::Collect { base_ordinal, count, kind: All }` **before** any side effect — re-entries reuse the pinned base; the ordinal counter advances on allocation (`registry/handle.rs:164-175`), so an unpinned re-entry would corrupt correlation. Record `Scheduled`+`Started` for all N, dispatch all N async, suspend.
- **Each wake re-entry** (queries-first, consume one marker, then): for each ordinal, in order — (a) recorded terminal in the run segment → have it; (b) takeable runtime-map completion → **record it now** (`record_completed`/`record_failed`, same calls as `await_activity_result`, `nif_activity_dispatch.rs:277-294`) → have it; (c) neither → missing. If after the sweep any recorded failure exists: record `ActivityCancelled` for every ordinal still without a terminal, drop their map entries, clear pin, return the **lowest-ordinal recorded failure** (fail-fast). Else if all N have completions: clear pin, return the input-ordered result list. Else: suspend again.
- **Why replay reproduces it byte-identically:** the per-ordinal terminals are durable and ordered; replay's sweep finds every ordinal already in state (a) and applies the same total function — *lowest-ordinal failure if any failure recorded; cancelled-set ⇒ the same failure; else ordered completions*. The incremental live recording order (arrival order) is captured in history seq, but the **returned value** is a function of the per-ordinal terminal *set*, which is identical at replay. The fail-fast rule "lowest ordinal among recorded failures" is chosen exactly because it is set-derivable: live can only return once it has recorded the failure plus cancellations for everything unresolved, so the recorded set replay sees is the set live decided on.
- `collect_map` remains an alias of `collect_all` (`nif_concurrency.rs:432-448`, `concurrency.gleam:68-75`). Empty list never reaches the NIF (`concurrency.gleam:26-27`); the NIF still handles it defensively (empty → immediate `Ok([])`, no pin).
- **`with_timeout` scope expiry** (new for these awaits): on re-entry with `expired_scope_message(state, pid)` `Some` (`nif_timeout.rs:52`), record `ActivityCancelled` for all unresolved ordinals, drop map entries, clear pin, return the scope's timeout error — the `with_timeout` scope records its own `WithTimeoutCompleted` outcome as it does for the other awaits, and the cancelled set makes the collect's replay deterministic (cancelled-without-failure ⇒ aborted-by-scope; return the canonical scope-expiry error string so the recorded scope outcome and the await's return stay aligned). Mirrors `await_activity_result`'s expired-scope arm (`nif_activity_dispatch.rs:232-237`) generalized to N.

**Candidate note:** a pure-SDK decomposition (collect_all ≡ `list.map(dispatch_activity)` then await each) was considered and rejected: `collect_race` is inexpressible over await-one-correlation, batch ordinal allocation and the fail-fast/cancel recording rules live engine-side, and splitting all/race across layers leaves two divergent determinism models. The natives stay; they just stop blocking.

### 2.4 `await_child` native — the converted flow

New `run_await_child` (normal NIF, no longer dirty), invoked fresh on every wake:

1. Decode child id; recover `state`/`bridge`/pid.
2. **Queries first:** `take_pending_query_sentinel(state, pid)` → return sentinel if `Some` (§1.7).
3. `consume_wake_marker` (array now includes `aion_child_terminal` and `aion_query`).
4. Pin check: `PendingAwait::Child { child_workflow_id }` must match the argument (a different pinned await kind is a hard typed error, as `nif_signal.rs:204-210` / `nif_timer.rs:31-39` do).
5. Build `NifContext`; `resolve_command(AwaitChild)` → `Recorded(ChildCompleted/ChildFailed)` ⇒ clear pin, return with the **D4-corrected envelope** (`{ok, "ok:"++payload}` / `{ok, "error:"++details}`).
6. `ResumeLive` ⇒ check `expired_scope_message`: expired ⇒ clear pin, return scope error (child untouched — see §6 D-1 for the product call); else:
7. Arm the watcher (idempotent, §2.1), pin `Child { child_workflow_id }`, `request_suspend(None)`, return NIL.

Re-entrancy/idempotence: every step is a pure read or an idempotent arm; the only history write on this path happens in the *watcher* (signal-pattern), so re-invocation on any surplus marker is safe. The early-arrival case (child finished before the await is reached, terminal already recorded by a previously-armed watcher — or by the lazy first-arrival read inside the watcher's first loop iteration before the parent ever suspends) resolves at step 5/7 within one wake.

**Spawn stays dirty** (`spawn_child` does recorder appends + a child `start_workflow` round trip — short, bounded work; no suspension needed).

### 2.5 Problems 4+5 — recovery and exactly-once initiation (#56 fold-in)

**Record-then-spawn.** Invert `spawn_with_mode` (`child/spawn.rs:282-314`):

1. Parent NIF (live path) pre-allocates `child_workflow_id = WorkflowId::new_v4()` — nondeterministic but **recorded before any observable use**, so replay returns the recorded id (same principle as every recorded nondeterminism in the engine).
2. Record `ChildWorkflowStarted { child_workflow_id, workflow_type, input }` through the parent Recorder (already the append path — single-writer invariant intact; nothing about the invariant ever forced spawn-first, the old order existed only because the id came out of `start_workflow`).
3. Start the child with `StartWorkflowOptions { workflow_id: Some(child_id), .. }` (`lifecycle/start.rs:107-121` already supports supplied ids and resumes head from existing history — a fresh child reads head 0).
4. `ChildWorkflowSpawnRequest` gains the pre-allocated `child_workflow_id`; `NifChildEngine::spawn_child_workflow` passes it through (and the dead `mode`/`run_id` fields get honest treatment: either wired or removed — no zombie fields, CLAUDE.md).

**Crash-window disposition:**
- *Crash after record, before child start* → "recorded-but-never-spawned child". Repaired by a **recovery sweep**: after `repopulate_active_workflows` registers a recovered parent (`engine/startup.rs:121-185`), scan its current run segment for each `ChildWorkflowStarted` lacking a parent-side terminal; for each, `read_history(child_id)` — empty ⇒ start the child now with the recorded id/type/input (idempotent: non-empty ⇒ child exists; `list_active` recovery handles its process). The sweep also covers fire-and-forget children (which no await would ever lazily repair). Replay of the parent resolves `SpawnChild` as `Recorded(ChildStarted(id))` (`resolver.rs:253-266`) and never re-runs the live spawn — exactly-once initiation holds.
- *Parent crashes mid-await, children still running* (the headline recovery case): on rebuild, actives are re-spawned; the parent replays to `await_child`; `AwaitChild` resolves `ResumeLive` (no recorded terminal, `cursor.rs:199-227`); step 7 re-arms the watcher fresh (`pending_awaits`/watcher state are engine-epoch-local and correctly empty). The watcher's store-truth loop covers all three child states: still-running (subscribe to the recovered handle — tolerating the registration-ordering window via the bounded retry, §1.6), already-terminal-with-no-handle (terminal read directly from the child's durable history — the durable analogue of the pid tombstone), and mid-CAN (run-chain following, §2.6).
- *Parent process crashes while engine lives*: the parent's monitor records the parent's own `WorkflowFailed` terminal (`completion.rs:86-101`); the watcher's terminal guard (§2.1 step 4) observes it and exits without recording. Children, unlinked (§1.4), continue; their disposition on parent failure is unchanged from HEAD.

### 2.6 Problem 6 — continue-as-new in the child

Ruling (mine, defended): **child CAN is transparent to the awaiting parent** — the await is satisfied by the first non-CAN terminal of the child's **run chain**, Temporal-consistent and the only option that doesn't surface an engine-internal mechanism (run rotation) as a workflow-visible error (which is today's broken behavior, D2). Mechanics are already fully specified by the watcher loop (§2.1 step 3): on `ContinuedAsNew`, re-resolve the *current* run's handle and keep waiting; the recorded parent event carries the stable `child_workflow_id`, so the `AwaitChild` replay identity is untouched across any number of rotations. The recorded result payload is the final run's terminal payload. `CompletionMailbox` and its CAN error path are deleted outright (no compat shim — CLAUDE.md).

### 2.7 #55 LOW fold-in

Wave A executes D6 exactly as specified in §1.8 (comment rewrite, dead-arm deletion with the `resolve_child` simplification, incremental-index fast-forward). They are in the same functions the conversion touches; leaving them would mean re-reviewing the same lines twice.

---

## 3. Implementation plan by file

### 3.1 Wave A — durability layer (`crates/aion/src/durability/`)

**`cursor.rs`**
- `resolve_activity` (`:296-338`): id-keyed terminal walk — events not belonging to the matched `activity_id` are *skipped in place* (left for their own commands), not mismatched; consumed range becomes only the matched activity's own events. Key-equality determinism stays enforced at the `Scheduled` anchor via `resolve_next`'s family/key check (`:241-247`). Document why (D3).
- Delete dead arms in `resolve_child` (`:267-281`); collapse to the `ChildWorkflowStarted` arm + mismatch.
- `next_matchable_index`/fast-forwards: incremental scanning (no O(n²)).
- `take_range` semantics with skipped interior events: consumed events list = matched-id events only; position advances past the terminal. Verify `ResolvedCommand::recorded_at` (last consumed event) is unaffected for the determinism-time bookkeeping (`resolver.rs:209-216`).

**`correlation.rs`** — comment rewrite `:81-84` (D6-i).

**Unit tests** (same modules): interleaved `SignalReceived` inside an activity range replays clean; two overlapping activities' ranges replay clean; child-terminal event inside an activity range; strict-replay mismatch still fires for genuinely reordered keys; fast-forward complexity regression (large-history smoke).

### 3.2 Wave B — `await_child` two-phase + #56 (`crates/aion/src/runtime/`, `child/`, `engine/`)

**`runtime/nif_state.rs`** — `PendingAwait::Child { child_workflow_id: WorkflowId }` (+ Wave C adds `Collect`); `cleanup_process` unchanged (pin removal already covered); doc update.

**`runtime/nif_child_watch.rs` (new)** — the watcher of §2.1: `arm_child_terminal_watch(bridge, parent_handle, child_workflow_id)`; dedup map + abort handles on `ChildNifBridge`; backoff policy threaded from the builder (no hardcoded values); terminal-guarded idempotent parent-side record (absorbs `record_child_event` from `nif_child_engine.rs:337-396`); run-chain following; marker delivery.

**`runtime/nif_child_engine.rs`** — delete `CompletionMailbox` (`:92-173`) and `outcome_to_message`; `ChildNifBridge` gains the watch map + backoff policy + abort wiring; `spawn_child_workflow` honors the pre-allocated `child_workflow_id` and loses the ignored-field lies (§2.5.4).

**`runtime/nif_child.rs`** — `run_await_child` per §2.4 (entry checks, pin, suspend; D4 envelope); `run_spawn_child` keeps `next_child_key` ordinal correlation but live path becomes record-then-spawn (`Command::SpawnChild` resolution unchanged); the `recording_context`/`next_sequence` seq plumbing (`:174-190`) dies with `CompletionMailbox`.

**`child/spawn.rs`** — `spawn_with_mode` inverted (§2.5); `ChildWorkflowSpawnRequest` carries `child_workflow_id`; `await_child`/`wait_for_child_result`/`ChildWorkflowMailbox` trait and `VecChildWorkflowMailbox`: the NIF no longer uses them — keep only what the AT seam tests still exercise, delete the rest (no zombie code); module doc corrected re: linking (§1.4).

**`runtime/handle/delivery.rs`** — `deliver_child_terminal(pid)` + `child_terminal_atom()` (mirror `deliver_signal_received`).

**`runtime/nif_wake.rs`** — add `child_terminal_atom` (and #45's `aion_query`) to the marker array.

**`runtime/engine_nifs.rs`** — `await_child` → `NifEntry::new` (normal scheduler); registration test (`:224-265`) updated to assert the new partition (`await_child` joins the non-dirty list; Wave C moves `collect_*` too).

**`engine/startup.rs`** — recovered-parent child sweep (§2.5), placed after each parent's registration in `repopulate_active_workflows`; needs `LoadedWorkflows` + start context (all already in `StartupRecoveryContext`).

**Integration with #45:** entry-check call sites use `nif_query_pump::take_pending_query_sentinel`; if Wave B lands first in a shared branch, the natives compile against the #45 Wave 1 module — coordinate the merge order explicitly (prerequisite, §1.7).

### 3.3 Wave C — `collect_*` two-phase (`runtime/nif_concurrency.rs` rewrite)

- `PendingAwait::Collect { base_ordinal: u64, count: u64, kind: CollectKind }` in `nif_state.rs`.
- Rewrite per §2.2/§2.3 over `spawn_completion_task` + `take_activity_result/take_activity_error` + `record_*` helpers; new `activity_terminal_recorded` scan helper (place beside `timer_terminal_recorded` or in `nif_concurrency.rs`).
- Drop the `black_box` coverage shims; reconcile the `crates/aion/src/concurrency/` AT module (its child-fan-out machinery is unreferenced production code — see §6 D-3 for its disposition; default: leave the module intact as the AT-cluster seam with its own tests, remove only the shim references).
- `engine_nifs.rs`: `collect_*` → `NifEntry::new`; test updated.
- D5: monitor exit path (`runtime/monitor.rs` callback or `handle.rs` helper) drains `activity_results`/`activity_errors` for the pid.

### 3.4 Wave D — SDK + fixtures (`gleam/aion_flow`, fixtures)

- Wrap the new yield points in the #45 pump: `child.gleam:59` (`ffi.await_child`) and `concurrency.gleam:31, 56` (`ffi.collect_all/collect_race`) call through `pump.run` exactly like `signal.gleam:42`/`run.gleam:48`/`timer.gleam:31` (#45 §3.4). Depends on #45 Wave 2a (`internal/pump.gleam`).
- D4 envelope: engine emits `ok:`/`error:`-prefixed payloads (Wave B); `child.gleam` decode unchanged; add a Gleam test pinning the contract; pure-Gleam harness (`test/aion_flow_ffi.erl`) already conforms.
- Engine test fixtures (`crates/aion/tests/fixtures/`, README conventions: commit `.erl` + `.beam`, `erlc -Werror`): extend `aion_parent_fixture.erl` (or add `aion_parent_query_fixture.erl`) with: a query-registering parent that parks in `await_child` running a hand-rolled pump loop (`{error, <<"aion_query:", Rest/binary>>} → service → re-await`, #45 §3.6 style); a collect-using parent; a CAN child fixture (child that `continue_as_new`s once then completes).

---

## 4. Test plan

**Unit (Wave A)** — listed in §3.1.

**Unit (Waves B/C)**
1. Watcher: idempotent arm; terminal-guarded record (parent already terminal ⇒ no append); duplicate-record suppression; CAN chain follow over a synthetic registry; registry-miss backoff path; marker-failure-after-record is non-fatal.
2. `await_child` native: pin lifecycle; sentinel-before-resolution ordering; recorded resolution clears pin; expired-scope abort releases pin without touching the child.
3. Collect: pinned base ordinal reused across re-entries (counter not advanced twice); fail-fast lowest-ordinal rule; race winner = first-recorded; cancellation set completeness; empty-list guard.

**Engine e2e (`crates/aion/tests/`)** — extend `child_workflows_e2e.rs` + new `concurrency_e2e.rs` / additions to query e2e:
4. **Query a parent parked in `await_child`** (the commissioning test, #45 Q7): parent registers a handler, spawns a slow child (gated on a signal to the child), parent parks in `await_child`; `engine.query` answers via the pump; then release the child; parent completes; history byte-identical to a never-queried control run.
5. **Query a parent parked in `collect_all`** (same shape over slow activities).
6. **Crash-recovery determinism proofs:** (a) crash mid-`await_child` with the child still running → recover → child completes → parent completes; compare final parent history with an uncrashed control (identical event kinds/payloads; envelope seq/timestamps per existing determinism-test conventions); (b) same with the parent *queried* before the crash and after recovery — queried/non-queried and crashed/uncrashed histories all agree; (c) crash after child terminal recorded in the *child's* history but before the parent-side record (no handle after restart) → watcher resolves from the store.
7. **#56 windows:** (a) recorded-but-never-spawned child (synthesize parent history with `ChildWorkflowStarted` and no child history, as `restart_mid_child` synthesizes its window at `child_workflows_e2e.rs:224-281`) → recovery sweep starts exactly one child with the recorded id; (b) assert no duplicate `ChildWorkflowStarted` across the whole matrix (existing assertions stay green).
8. **Continue-as-new child:** parent awaits a child that CANs once → await resolves with the final run's result; recorded `ChildWorkflowCompleted` carries the stable child id; restart-replay returns the same result with zero new spawns.
9. **Race & collect matrix:** all-success ordering; one-fails fail-fast + cancellations recorded; race first-settle wins with success and with failure; race losers' late completions produce no events and no leaked map entries (assert maps empty after completion — D5); collect under `with_timeout` expiry; replay each of these histories after restart and compare byte-identically.
10. **Thread-pinning regression proof:** engine with `scheduler_threads(1)`, default dirty pool: park **> 10** parents simultaneously in `await_child` on gated children (today this wedges the `DEFAULT_DIRTY_IO_THREADS = 10` pool — the test must fail at HEAD, pass after), then release all and assert all complete; plus the `engine_nifs.rs` registration test pinning `await_child`/`collect_*` as non-dirty entries.
11. **Existing suites green:** all of `child_workflows_e2e.rs`, `recovery_*`, `replay_behaviour.rs`, `continue_as_new.rs`, `determinism_violation.rs`.

**Gleam:** pump-wrapped child/collect awaits against the harness; D4 envelope decode test.

**Gates:** `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `gleam test`; no `#[allow]`/`#[ignore]`/`_var` bypasses (CLAUDE.md).

---

## 5. Wave structure for implementation agents

Prereq: #45 Wave 1 (query pump engine core: `nif_query_pump.rs`, `aion_query` marker, pending-query state) landed or co-branched.

- **Wave A — durability (1 agent, `durability/` only):** D3 cursor leniency + D6 trio + unit tests. *Exit: `cargo test -p aion` green; no behavior change for existing histories (full suite).* Independent of #45.
- **Wave B — child awaits (1 agent):** watcher + `await_child` conversion + record-then-spawn + recovery sweep + marker plumbing + D4 envelope + unit tests + e2e 4, 6, 7, 8. Depends on A and on #45 Wave 1.
- **Wave C — collect (1 agent, parallel with B after A):** `nif_concurrency.rs` rewrite + `PendingAwait::Collect` + D5 + e2e 5, 9 + registration flips. Shares `nif_state.rs`/`engine_nifs.rs`/`nif_wake.rs` with B — sequence the shared-file merges B-then-C or co-branch.
- **Wave D — SDK + fixtures (1 agent):** pump wrapping, fixtures, Gleam tests, e2e 10 + full-matrix run. Depends on B, C, and #45 Wave 2a.
- **Wave E — review:** rigorous sub-agent review per CLAUDE.md (brief + intent + files), determinism proofs re-run, full workspace gates, update `docs/briefs/README.md` table row for this brief.

Estimated size: A ≈ 400 LoC; B ≈ 1.5k; C ≈ 1k; D ≈ 500 (excl. fixtures' generated `.beam`).

---

## 6. Open decisions for Tom (product-level only)

- **D-1 — `with_timeout` over `await_child`: what happens to the child?** This brief implements *abort-the-await, leave-the-child-running* (typed scope error; the await consumed nothing; the child can still be awaited later or runs to completion detached). The alternative — cancel the child on scope expiry — is a real product semantics choice (Temporal's child-workflow cancellation types live here). Recommend shipping abort-only now; child-cancellation policy is a separate surface.
- **D-2 — fire-and-forget children and parent history.** The watcher only exists for *awaited* children, so fire-and-forget terminals are never recorded into the parent (matches HEAD). If you want parent-side observability for detached children, that's a spawn-time always-watch flag — cheap with this machinery, but it changes history shape for every detached spawn. Recommend: not now.
- **D-3 — the dormant child-fan-out concurrency module.** `crates/aion/src/concurrency/` implements child-workflow `all`/`race` that nothing ships. The SDK `collect_*` contract is activities. Confirm: (a) activity-collect is the v1 contract (this brief's assumption), and (b) whether the AT module stays as the seam for a future child-collect surface or gets a removal brief. The new watcher machinery is the natural substrate for (b) whenever it's commissioned.
- **D-4 — child Cancelled/TimedOut taxonomy.** Today (and after this brief) both map to `ChildWorkflowFailed` with message prefixes (`nif_child_engine.rs:152-168` semantics preserved). `Event::ChildWorkflowCancelled` exists but is rejected by resolution (`resolver.rs:267-274`). Distinct recorded child-cancelled semantics would touch the event taxonomy + SDK error types — separate brief if wanted.

Everything else in this design — watcher mechanism, record-then-spawn, run-chain transparency for CAN, collect determinism rules, D3–D6 fixes — is justified from the verified constraints above and is decided here.

---

### Appendix: key file:line index

| Concern | Location |
|---|---|
| Dirty registrations + partition test | `crates/aion/src/runtime/engine_nifs.rs:156-166, 224-265` |
| `await_child` NIF / blocking mailbox | `runtime/nif_child.rs:98-134`; `runtime/nif_child_engine.rs:96-123` (block_on watch loop) |
| Child spawn path (spawn-before-record, #56) | `crates/aion/src/child/spawn.rs:282-314`; `runtime/nif_child.rs:56-96` |
| Parent-side terminal record (old path, dies) | `child/spawn.rs:316-356`; `nif_child_engine.rs:337-396`; seq plumbing `nif_child.rs:174-190` |
| CAN mailbox failure (D2) | `nif_child_engine.rs:169-172`; registry `.find` ambiguity `:101-107, 192-200` |
| collect_* (sequential/fake-race) | `runtime/nif_concurrency.rs:241-272, 274-356` |
| Two-phase precedents | `nif_signal.rs:190-251, 314-365`; `nif_timer.rs:26-108, 162-199`; `nif_activity_dispatch.rs:126-241` |
| Async activity completion machinery (reused) | `nif_activity_dispatch.rs:177-211`; `runtime/handle/delivery.rs:82-116, 213-231, 267-300` |
| Wake markers | `runtime/nif_wake.rs:27-60`; atoms `delivery.rs:233-251` |
| Pending awaits / cleanup | `runtime/nif_state.rs:63, 74-87, 112-116`; monitor `runtime/monitor.rs:45-68` |
| #41 correlation/resolution | `durability/correlation.rs:9-38, 75-88`; `cursor.rs:188-227, 262-294`; `resolver.rs:95-113`; `command.rs:48-80`; `nif_context.rs:398-413` |
| D3 strict walk | `durability/cursor.rs:296-338` (`:333`) |
| D6 LOWs | `correlation.rs:81-84`; `cursor.rs:267-281`; `cursor.rs:371-377` + `:178-206` |
| Child lifecycle / CompletionNotifier | `lifecycle/completion.rs:52-273`; `registry/handle.rs:24-94`; `lifecycle/start.rs:93-223` |
| CAN | `runtime/nif_continue_as_new.rs:38-93`; `lifecycle/completion.rs:106-126, 155-204`; latest-run outcome `engine/api.rs:890-943` |
| Recovery | `engine/startup.rs:97-189`; `durability/recovery.rs:255-294`; e2e `crates/aion/tests/child_workflows_e2e.rs` |
| Record-before-wake precedent | `crates/aion/src/signal/router.rs:36-94` |
| Query pump contract | `docs/briefs/query-execution.md` §3.1-3.2, DECISIONS Q6/Q7 |
| Gleam SDK | `gleam/aion_flow/src/aion/child.gleam:56-115`; `workflow/concurrency.gleam:22-75`; harness `test/aion_flow_ffi.erl:152-185, 277-285` |
| beamr (read-only refs, 0.4.9) | `trampoline.rs:206-240` (handle_suspend); `scheduler/mod.rs:979` (enqueue_atom_message); `native/context/mod.rs:1361-1368` (request/take_suspend); `scheduler/dirty.rs:21-32` (pool defaults) |
