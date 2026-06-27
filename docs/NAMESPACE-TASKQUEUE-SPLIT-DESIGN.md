# Namespace / task-queue split — making two dimensions out of one (DESIGN)

> Status: **design pass only, not yet briefs.** Read-only analysis + decomposition,
> 2026-06-27, off `main` ab4c3d82. No implementation in this doc — it specifies
> the work and the back-compat risk so it can be built deliberately.
>
> This is ROUTING-MODEL.md **Tier 2** ("make task-queue a real second dimension"),
> currently unimplemented. It is a **standalone doc** (not a section appended to
> ROUTING-MODEL.md) because: (a) it is long and code-dense — it pins an exact
> file:line conflation surface and a migration plan that would swamp the
> conceptual routing-model notes; (b) it must be cross-referenced from
> LIMINAL-SWAP-DESIGN.md §5 (13-3), which is also a standalone decomposition doc;
> (c) ROUTING-MODEL.md is the *target model* (the "what"), this is the *build plan*
> (the "how + the risk"). ROUTING-MODEL.md §4 Tier 2 should gain a one-line
> pointer here.

---

## 0. TL;DR

- **Two dimensions, deliberately kept disjoint.** `namespace` is the
  **workflow-level isolation/correctness boundary** — a workflow never escapes
  its namespace and its activities only ever dispatch to workers in that
  namespace, which is what makes the misrouting failure of ROUTING-MODEL §2
  *structurally* impossible. `task_queue` is the **worker-pool/flavour selector
  inside a namespace** — which flavour of worker (norn / claude / mixed / a
  budget tier) serves a given activity. Hierarchy: **namespace ⊃ task-queue ⊃
  node**.
- **Today they are one dimension wearing two names.** The worker SDK field
  `WorkerConfig.task_queue` (`crates/aion-worker/src/config.rs:116`) is sent **as**
  the wire's `RegisterWorker.namespace` field (`crates/aion-worker/src/protocol/session.rs:387`;
  proto `crates/aion-proto-generated/proto/worker.proto:83`). The server registry
  keys workers by `ActivityKey = (namespace, activity_type)`
  (`crates/aion-server/src/worker/registry.rs:25`). There is **no task_queue
  anywhere on the wire, in the registry, in the outbox row, or in history.**
- **The load-bearing risk is the AW wire field.** The wire field is *named*
  `namespace` but is *fed* the task-queue value. Splitting requires the wire to
  carry **both** a namespace and a task_queue, without breaking the existing
  single-field workers/workflows. The recommendation (§3) is **additive, not a
  rename**: add a new `task_queue` field to `RegisterWorker`; treat an absent
  task_queue as the explicit `default` task queue; never repurpose the existing
  `namespace` field's meaning.
- **Decomposition is spike-first and back-compat-safe** (§5): NSTQ-0..NSTQ-7,
  smallest first, each keeping the default build byte-identical. The proto field
  add (NSTQ-1) is additive and proto3-default-safe; the registry triple-key
  (NSTQ-2) defaults to behaving identically when every task_queue is `default`.
- **This corrects LIMINAL-SWAP §5 (13-3).** 13-3 currently proposes routing by
  `(namespace, activity_type)` as the liminal channel key. That is wrong on two
  counts: (1) `activity_type` is **not** a routing dimension — it is *what to
  run*, selected within a pool, not *which pool*; (2) it omits task_queue
  entirely. Post-split the liminal channel key is **`f(namespace, task_queue)`**
  and the worker matches `activity_type` after delivery, exactly as the gRPC
  registry's `(namespace, activity_type)` lookup already conflates two different
  ideas. See §4.

---

## 1. The two dimensions — precise, disjoint definitions

### 1.1 namespace = the correctness boundary (a property of the workflow)

A **namespace** is the logical isolation group a *workflow* belongs to. It is:

- **Declared at the workflow level** and **durable** — recorded as the
  `aion.namespace` search attribute at `WorkflowStarted`
  (`crates/aion/src/durability/recorder.rs:1212,1279`) and re-derived from history
  on recovery (`crates/aion-server/src/namespace/resolver.rs:222`,
  `HistoryNamespaceSource::workflow_attribution`). This is the durability that the
  2026-06-15 namespace-recovery fix established and that ROUTING-MODEL §3 confirms
  is "real and durable today."
- **A hard boundary on dispatch.** A workflow's activities dispatch *only* to
  workers registered in that namespace. The registry physically partitions
  workers by namespace (`registry.rs:25` `ActivityKey`, `registry.rs:171-177`
  insert keyed on `(namespace, activity_type)`), and worker registration is
  namespace-authorized (`registry.rs:130` `accept_registration` →
  `NamespaceGuard::scope` with `NamespaceOperation::register_worker`,
  `crates/aion-server/src/namespace/guard.rs:209`).
- **The thing that makes misrouting structurally impossible** (ROUTING-MODEL §2):
  a `local`-namespace workflow can only ever reach `local`-namespace workers, so
  a remote workflow cannot land on a local worker (the painful failure of the
  "whichever worker registered first picked up everything" incident).

**Namespace is a correctness/isolation invariant, not a performance knob.**
Crossing it is a bug, never a tuning choice.

### 1.2 task_queue = the pool/flavour selector (within a namespace)

A **task_queue** chooses *which flavour of worker inside a namespace* serves a
given activity. It is:

- **A property of the dispatch (the activity/workflow), not the workflow's
  identity.** Two activities in the same namespace can target different task
  queues. Per ROUTING-MODEL §1, the worker flavours Tom named: a norn worker, a
  claude worker, a mixed worker, or the same agent at different reasoning
  levels/budgets — you stand up one worker pool per flavour and route a step to
  the flavour it needs.
- **A selector, not a boundary.** Picking the wrong task_queue makes an activity
  wait (or pick a non-ideal pool), but it cannot violate isolation — every
  candidate worker is *already* inside the workflow's namespace. So a task_queue
  miss is a liveness/efficiency issue; a namespace miss would be a correctness
  bug. Keeping them separate keeps that distinction honest.

### 1.3 The hierarchy and a concrete example

```
namespace                 (correctness boundary — workflow can't escape)
   └── task_queue         (pool/flavour selector — which workers within the ns)
          └── node        (physical device — Tier 3, not in this design)
```

Concrete (ROUTING-MODEL's own example, made explicit):

- Namespace `remote` — the isolation group for workflows that must run on remote
  infrastructure (`stacked_dev_remote`). Inside it, two task queues:
  - task_queue `gpu` — a worker pool on GPU boxes for model-heavy steps;
  - task_queue `cpu` — a worker pool on cheap CPU boxes for glue steps.
  A `remote` workflow dispatches its model step to `(remote, gpu)` and its
  cleanup step to `(remote, cpu)`. **Neither can ever reach a `local` worker** —
  namespace forbids it. *Within* `remote`, task_queue decides gpu-vs-cpu.
- Namespace `local` — workflows pinned to the local machine (`stacked_dev`).
  Inside it, task queues `norn`, `claude`, `mixed` (the agent-flavour example):
  the same brief content (both backends accept the same structured output,
  ROUTING-MODEL §1) routed to `(local, norn)` or `(local, claude)` by flag only.

The pair `(namespace, task_queue)` is the full *addressing* of a worker pool;
`activity_type` then selects *what the matched worker runs*, and `node` (Tier 3)
would select *which physical instance* of that pool.

---

## 2. Where they are conflated today (exact surface + blast radius)

### 2.1 The conflation surface (verified file:line)

| # | Site | What it does today | The conflation |
|---|---|---|---|
| C1 | `crates/aion-worker/src/config.rs:116` | `WorkerConfig.task_queue: String` — its own doc comment: *"The current AW wire names this field `namespace`; this SDK maps the task queue value to that owned wire shape."* | The SDK has a `task_queue` field that is **defined as** an alias of the wire namespace. |
| C2 | `crates/aion-worker/src/protocol/session.rs:387` | `RegisterWorker { namespace: self.config.task_queue.clone(), activity_types }` | The worker's `task_queue` is sent **as** the wire `namespace`. This is the single conflation hinge. |
| C3 | `crates/aion-proto-generated/proto/worker.proto:83` / `crates/aion-proto/src/worker.rs:43-50` | `message RegisterWorker { string namespace = 1; repeated string activity_types = 2; }` | The wire has **one** scoping field (`namespace`), no `task_queue`. |
| C4 | `crates/aion-server/src/worker/registry.rs:25` | `type ActivityKey = (String, String);` = `(namespace, activity_type)`; insert at `:171-177`, lookup `workers_for` `:218-241`, rotation `:234` | Workers are partitioned and round-robin-rotated on `(namespace, activity_type)` — **no task_queue dimension.** |
| C5 | `crates/aion-server/src/worker/dispatch.rs:18-37,85-150` | `ScheduledActivity { namespace, activity_type, ... }`; `dispatch` routes via `registry.workers_for(&namespace, &activity_type)` (`:103`) | The dispatch addressing key is `(namespace, activity_type)` — task_queue absent. |
| C6 | `crates/aion-store/src/outbox.rs:80-102` | `OutboxRow` has `workflow_id, ordinal, run_id, activity_type, input, …` — **no namespace, no task_queue.** | The durable dispatch record carries neither routing dimension. |
| C7 | `crates/aion-server/src/worker/outbox_dispatcher.rs:118-126,152-163` | `WorkerOutboxDispatch` injects the server's `default_namespace` (`run.rs:348`) because the row carries no namespace; the schema's `namespace` column is "reserved for the later liminal cross-node send" | Outbox dispatch fakes a namespace and has **no** task_queue concept. |
| C8 | `examples/stacked-dev/norn-worker/src/main.rs:58-60` (and `mixed-worker:61`, `stacked-dev-remote/worker:60`) | `--task-queue <value>` CLI flag → `WorkerConfig.task_queue` | The operator-facing flag is `--task-queue`, but it sets the wire namespace (C2). The naming lies. |

**A note on a contradiction the code already contains.** `WorkerConfig` *also*
has a separate `namespace: String` field (`config.rs:109`) used **only** for the
`x-aion-namespaces` gRPC auth metadata header (`session.rs:347-361`,
`apply_auth_metadata`). So the worker today sends namespace **twice, with two
different meanings**: the auth-metadata `namespace` (`config.namespace`,
defaulting to `"default"`) and the registration-scope `namespace`
(`config.task_queue`). ROUTING-MODEL §3 describes "two names for one dimension";
in fact the code has *three* namespace-ish strings (`config.namespace`,
`config.task_queue`→wire `namespace`, and the server `default_namespace`) whose
relationship is undocumented and partly accidental. **This design must
disentangle all three** (see open question OQ-5).

### 2.2 Blast radius of separating them

To make task_queue a real second dimension, the following surfaces change. Each
is a discrete, independently-testable edit (mapped to NSTQ steps in §5):

1. **Wire/proto (C3).** Add `string task_queue = 3;` to `RegisterWorker`
   (additive, proto3-default-safe — an old worker omits it and it decodes as
   `""`). Optionally add `task_queue` to `ActivityTask` if the worker needs to
   echo/observe it; not strictly required since the worker is already pool-scoped
   by its subscription. **(NSTQ-1.)**
2. **Worker config + SDK (C1, C2).** Stop aliasing: `config.task_queue` becomes a
   genuine second value sent in the new field; `config.namespace` (auth metadata)
   and a *new* registration `namespace` must be reconciled (OQ-5). **(NSTQ-3.)**
3. **Registry / dispatch addressing (C4, C5).** `ActivityKey` →
   `(namespace, task_queue, activity_type)`; `workers_for` /`select_worker`
   /`rotation`/`deregister` all re-key; `ScheduledActivity` gains `task_queue`.
   The AW-014 round-robin rotation cursor keyspace changes (ROUTING-MODEL §5
   open question). **(NSTQ-2, NSTQ-4.)**
4. **Outbox row schema (C6, C7).** `OutboxRow` gains `namespace` and `task_queue`
   columns so the durable record carries the routing identity instead of the
   dispatcher inventing `default_namespace`. The libSQL schema gets two additive
   `ALTER TABLE … ADD COLUMN` migrations (mirroring the existing
   `ensure_outbox_run_id_column` pattern, `crates/aion-store-libsql/src/schema.rs:172`).
   **(NSTQ-5.)**
5. **History / recovery re-derivation.** Namespace is already recorded + recovered
   (`recorder.rs:1212`, `resolver.rs:222`). task_queue must be recorded in
   `ActivityScheduled` so reopen/recovery can re-target the **same** queue
   (ROUTING-MODEL §4 Tier 2: "record `task_queue` in history … it is not recorded
   today"). **(NSTQ-6.)**
6. **Activity-level task-queue selection (Gleam SDK).** How a step says "run me
   on the claude queue", threaded through the dispatch config into the engine —
   the same path activity labels already ride (`ActivityTask.labels`,
   `worker.proto:102`). **(NSTQ-7.)**

**Crucially, namespace's existing durability and recovery are untouched** — they
already work and are load-bearing. The split *adds* a parallel, weaker (liveness,
not correctness) dimension; it must not perturb the correctness dimension.

---

## 3. Back-compat / migration (the load-bearing risk)

The risk concentrates on the AW wire field (C2/C3). Three options were weighed:

### Option A — **Additive field, `default` task queue (RECOMMENDED)**

- Add `string task_queue = 3;` to `RegisterWorker` (new tag, never reuse tag 1).
  An old worker that doesn't set it sends proto3 default `""`; the server reads
  `""` as **"the `default` task queue"**.
- The existing `namespace` field (tag 1) **keeps its current meaning unchanged**
  on the wire — it is the registration's namespace scope. (Today the worker feeds
  it `config.task_queue`; under the migration the worker feeds it the genuine
  namespace, and feeds the new tag-3 field the genuine task_queue — see the
  migration sequence below.)
- The server's `ActivityKey` becomes `(namespace, task_queue, activity_type)`;
  when `task_queue == ""` it is normalized to the literal `"default"` so an
  un-upgraded worker and a workflow that names no task_queue both land on the
  same `default` pool, preserving today's single-pool behaviour exactly.
- **Why this is safe:** byte-for-byte, an old worker's `RegisterWorker` decodes
  identically; the only change is the server adding a third key component that is
  constant (`"default"`) until anyone opts in. Round-robin within a namespace's
  `default` queue is identical to today's round-robin within a namespace.

**The one subtlety — the existing wire `namespace` is fed the task-queue value
today.** So a literal "add a field and feed both correctly" flips what tag 1
*means in practice* (it currently carries `--task-queue`). The migration must not
silently change where existing deployments' work lands. Sequence:

1. **NSTQ-1/2 (server-side first):** add the field + triple-key, normalizing
   absent task_queue to `"default"`. Server now *accepts* a task_queue but no
   worker sends one, and no workflow selects one → every dispatch is
   `(ns, "default", activity_type)`; behaviour identical to today because the
   workflow's namespace and the worker's tag-1 value still line up exactly as
   they do now (by the operator convention ROUTING-MODEL §3 describes).
2. **NSTQ-3 (worker-side, opt-in):** introduce the genuine `task_queue` send +
   keep tag 1 carrying the *namespace*. **Gate behind a worker-SDK version /
   builder method** so existing example workers compile and run unchanged until
   their `main.rs` is updated. Until a worker opts in, it behaves as in step 1.
3. Only once both ends speak the field does a real second pool exist.

### Option B — Rename `--task-queue` → `--namespace` then re-add task_queue later

This is ROUTING-MODEL §4 **Tier 1**, explicitly rejected there: it "would delete
task-queue as a concept right when we want it back — rename-now/re-add-later
churn against the do-it-once rule." **Do not do Tier 1 standalone.** Subsumed by
Option A, which renames nothing and adds the real dimension once.

### Option C — Reuse the existing `namespace` field's meaning, infer task_queue elsewhere

Rejected: overloads one field with two meanings (the exact disease being cured)
and gives no clean recovery story.

### Recommendation

**Option A.** Additive proto field (tag 3), absent ⇒ normalized `"default"` task
queue, existing tag-1 `namespace` unchanged on the wire, worker send opt-in
behind an SDK builder method so the default build and every existing example
worker is byte-identical until explicitly migrated. Combined with the
no-arbitrary-defaults house rule, "absent ⇒ `default`" is the *one* explicit,
named fallback (OQ-1 asks Tom to confirm this exception, since ROUTING-MODEL §5
flags it).

---

## 4. Dispatch addressing post-split (and why this corrects 13-3)

### 4.1 The post-split routing key

A dispatch is addressed by **`(namespace, task_queue)`** to select a worker pool,
then matched on `activity_type` to pick a handler within that pool:

```
dispatch(activity):
    ns  = workflow.namespace            # correctness boundary (durable, recovered)
    tq  = activity.task_queue ?? "default"   # pool selector (per §3 fallback)
    pool = registry.workers_for(ns, tq, activity.activity_type)   # triple key
    push activity → a worker in pool                              # round-robin
```

- **namespace scopes (correctness):** `workers_for` can only return workers whose
  registration namespace matches — isolation is enforced by the key, as today.
- **task_queue selects (liveness):** within that namespace, the second key
  component chooses the flavour pool.
- **activity_type matches (dispatch):** the third component ensures the chosen
  worker actually implements the activity.

This is the registry change of §2.2 item 3: `ActivityKey = (namespace,
task_queue, activity_type)`. The round-robin rotation (`registry.rs:234`) now
keys its cursor on the triple, so each `(ns, tq, activity_type)` pool rotates
independently (ROUTING-MODEL §5's AW-014 caveat — verify the closed-channel
fallback in `dispatch.rs:117-130` still holds per triple).

### 4.2 Why LIMINAL-SWAP §5 (13-3) is wrong/premature as written

LIMINAL-SWAP-DESIGN.md §5 increment **13-3** says (verbatim): *"route a dispatch
to the right worker pool by `(namespace, activity_type)` … as the liminal
channel/group"* and adds a `namespace` field to `OutboxRow`. Two problems:

1. **`activity_type` is not a routing/pool dimension.** It is *what to run*, and
   in this model it is matched **within** a pool, not used to *select* the pool.
   Using `(namespace, activity_type)` as the channel key fuses "which pool" with
   "which handler" — the same category error the gRPC registry already commits at
   `registry.rs:25`, and the one this whole split exists to undo. A worker pool
   serving N activity types would need N channels, and you could not stand up a
   "norn pool that runs everything" (Tom's explicit flavour example,
   ROUTING-MODEL §1) addressed as one unit.
2. **It omits task_queue entirely** — the very dimension this design adds. With
   the split, the worker-pool address is `(namespace, task_queue)`; the liminal
   channel/pg-group key must be **`f(namespace, task_queue)`**, not
   `f(namespace, activity_type)`.

**Corrected 13-3:** the liminal channel key (the beamr pg group a worker pool
subscribes to) is `channel_name = f(namespace, task_queue)` — e.g.
`"aion.dispatch.{namespace}.{task_queue}"`. A worker subscribes to its pool's
channel; `activity_type` rides *inside* the `DispatchRequest`
(`crates/aion-server/src/worker/liminal_transport.rs:82-94`, which already carries
`activity_type` in the payload, **not** the channel) and the worker matches it
after delivery. This is consistent with how the gРPC path already pushes
`activity_type` *in* the `ActivityTask` body while selecting the worker by
registry key.

### 4.3 The codebase is ahead of LIMINAL-SWAP-DESIGN.md — flag the drift

LIMINAL-SWAP-DESIGN.md (dated 2026-06-27, "the aion seam is ready; liminal's wire
transport is not") frames 13-0 as not-yet-built. **It is built on this branch:**
`crates/aion-server/src/worker/liminal_transport.rs` exists with
`LiminalOutboxDispatch` + `LiminalCompletionSource`, wired via
`select_outbox_row_dispatch` (`run.rs:338-383`) behind the `liminal-transport`
feature and `outbox.transport=liminal`, with tests
(`crates/aion-server/tests/liminal_outbox_spike.rs`,
`outbox_transport_e2e.rs`). The spike **hard-codes** the channel name
(`liminal_transport.rs:170-192`, `LiminalOutboxDispatch::new(server_address,
channel_name)`; module docs at `:11-19` explicitly defer `(namespace,
activity_type)` derivation to "13-3"). So:

- The **dependency** from §5 below is real and present: 13-3's channel derivation
  is the next liminal increment, and it should consume NSTQ-2/NSTQ-5 (the
  triple-key + the outbox `namespace`+`task_queue` columns), then derive
  `channel_name = f(namespace, task_queue)`.
- The spike's idempotency note (`liminal_transport.rs:148-163`,
  `{dispatch_key}#{attempt}`) is orthogonal to this split and unaffected.

**Recommendation:** when 13-3 is briefed, it must (a) depend on NSTQ-5 for the
row carrying `namespace`+`task_queue`, and (b) derive the channel from
`(namespace, task_queue)` — *not* `(namespace, activity_type)`. LIMINAL-SWAP §5
should be amended accordingly (a one-line correction noting the dependency on
this doc).

---

## 5. Spike-first decomposition (NSTQ-0 … NSTQ-7)

Each increment is independently implementable + verifiable, smallest-first. Every
step keeps a **default build/server byte-identical** to today: absent task_queue
normalizes to `"default"`, the new proto field is additive, and worker-side send
is opt-in. No step changes the correctness (namespace) dimension's existing
durability/recovery.

### NSTQ-0 — characterization spike: pin today's conflation in a test (no behaviour change)
- **Goal:** a test that asserts, on `main`, that the worker's `task_queue` is what
  the registry keys on as `namespace`, and that there is no task_queue dimension.
  This is the regression backstop the rest of the work must not break.
- **Seam:** `registry.rs` `workers_for`; a worker registered with task_queue `X`
  is found under namespace `X`.
- **Verify:** test passes on `main` unchanged; documents the starting contract.
- **Risk:** NONE (test-only). **Depends on:** nothing.

### NSTQ-1 — proto: additive `task_queue` field on `RegisterWorker` (no reader yet)
- **Goal:** add `string task_queue = 3;` to `RegisterWorker`
  (`worker.proto:82`, `ProtoRegisterWorker` `worker.rs:43`). No server logic reads
  it yet; it decodes to `""` for every existing worker.
- **Verify:** round-trip encode/decode test; an old-shape `RegisterWorker`
  (no tag 3) still decodes; `prost` default is `""`.
- **Risk:** LOW (additive proto3 tag). **Depends on:** NSTQ-0.

### NSTQ-2 — registry: triple key `(namespace, task_queue, activity_type)`, `default`-normalized
- **Goal:** `ActivityKey` → 3-tuple; `register`/`workers_for`/`select_worker`/
  `deregister`/`rotation` re-keyed; absent/empty task_queue normalized to
  `"default"`. With every worker sending `""`→`default` and every dispatch using
  `default`, behaviour is identical to today.
- **Seam:** `registry.rs:25,171-241,280-339`.
- **Verify:** NSTQ-0's characterization test still passes (everything funnels to
  `default`); a new test: two workers, task queues `a` and `b`, same namespace +
  activity_type, resolve to disjoint pools; round-robin holds **per triple**
  (ROUTING-MODEL §5 AW-014 check).
- **Risk:** MEDIUM (touches the hot routing path + rotation). **Depends on:** NSTQ-1.

### NSTQ-3 — worker SDK: send a genuine task_queue (opt-in), disentangle the three namespace strings
- **Goal:** `WorkerConfig` cleanly separates `namespace` (registration scope) and
  `task_queue` (pool). The wire `RegisterWorker.namespace` (tag 1) is fed the
  genuine namespace; the new tag 3 is fed `task_queue`. Opt-in via a builder
  method / SDK version so existing example workers compile + behave unchanged
  until migrated. Reconcile the auth-metadata `config.namespace`
  (`config.rs:109`, `session.rs:347`) vs registration namespace (OQ-5).
- **Seam:** `config.rs:107-125`, `session.rs:378-391`.
- **Verify:** an opted-in worker registers under `(real_ns, real_tq)`; a
  not-opted-in worker registers exactly as today (its tag-1 value → namespace,
  tag-3 absent → `default`).
- **Risk:** MEDIUM-HIGH — this is the back-compat hinge (§3). **Depends on:** NSTQ-2.

### NSTQ-4 — dispatch: `ScheduledActivity.task_queue` threaded to the registry lookup
- **Goal:** `ScheduledActivity` (`dispatch.rs:18`) gains `task_queue`; `dispatch`
  (`dispatch.rs:103`) calls `workers_for(ns, tq, activity_type)`. Default
  producers stamp `"default"` until NSTQ-7 lets a workflow choose.
- **Verify:** dispatch routes to the correct pool; existing dispatch tests pass
  with `task_queue="default"`.
- **Risk:** LOW-MEDIUM. **Depends on:** NSTQ-2.

### NSTQ-5 — outbox row: carry `namespace` + `task_queue` (additive columns)
- **Goal:** `OutboxRow` (`outbox.rs:80`) gains `namespace` and `task_queue`;
  two additive libSQL `ADD COLUMN` migrations (pattern:
  `schema.rs:172 ensure_outbox_run_id_column`). `WorkerOutboxDispatch::to_scheduled`
  (`outbox_dispatcher.rs:152`) reads them off the row instead of injecting
  `default_namespace` (`run.rs:348`). Legacy rows (NULL) read as
  `(default_namespace, "default")`.
- **Verify:** a staged row round-trips both fields; a pre-migration row defaults
  correctly; outbox dispatcher routes via the row's real namespace+task_queue.
- **Risk:** MEDIUM (schema migration — but additive, mirrors a landed pattern).
- **Depends on:** NSTQ-4. **This is the first schema change** (LIMINAL-SWAP §6
  promised "everything before 13-3 needs no schema change"; this is that change,
  and it is what 13-3 should consume).

### NSTQ-6 — history: record `task_queue` in `ActivityScheduled` for reopen/recovery
- **Goal:** task_queue recorded durably so reopen/recovery re-targets the same
  queue (ROUTING-MODEL §4 Tier 2). Namespace is already recorded+recovered
  (`recorder.rs:1212`, `resolver.rs:222`) — this adds the parallel for task_queue.
- **Verify:** schedule an activity on task_queue `X`; restart; recovery
  re-dispatches to `(ns, X)`, not `(ns, default)`. Reuse the namespace-recovery
  test shape.
- **Risk:** MEDIUM (history schema/replay determinism — must be a record-only,
  replay-safe addition). **Depends on:** NSTQ-5.

### NSTQ-7 — Gleam SDK: activity-level (or workflow-level) task_queue selection
- **Goal:** a workflow/activity declares its task_queue; threaded through the
  dispatch config into the engine (same path activity labels ride,
  `worker.proto:102`). Granularity is OQ-2 (per-activity vs per-workflow vs
  per-workflow-with-override).
- **Verify:** a workflow routes step A to `(ns, norn)` and step B to
  `(ns, claude)`; end-to-end with two pools.
- **Risk:** MEDIUM (SDK surface + engine threading). **Depends on:** NSTQ-6.

### Dependency on / from LIMINAL-SWAP #13
- **13-3 (liminal channel addressing) depends on NSTQ-2 and NSTQ-5**, and must be
  corrected to derive `channel = f(namespace, task_queue)` (not
  `(namespace, activity_type)`) — §4.2. Until NSTQ-5 lands, the liminal channel
  stays hard-coded (the current spike state, `liminal_transport.rs:11-19`).
- **13-4/13-5/13-6** (RunId end-to-end, crash recovery, cutover demo) are
  orthogonal to this split and do not depend on NSTQ.
- NSTQ-0..NSTQ-2 can land entirely independently of #13 (gRPC path only).

**Ordering:** NSTQ-0 → NSTQ-1 → NSTQ-2 → {NSTQ-3, NSTQ-4 in parallel} → NSTQ-5 →
NSTQ-6 → NSTQ-7. 13-3 slots in after NSTQ-5.

---

## 6. Open questions / decisions for Tom

- **OQ-1 — the `default` task-queue fallback (vs no-default house rule).**
  ROUTING-MODEL §5 leaves this open: when an activity names no task_queue, do we
  (a) require every activity to name one, or (b) define a named `default` queue
  as the explicit fallback? This design's back-compat story (§3 Option A)
  **requires (b)** — absent ⇒ normalized `"default"` is what keeps existing
  workers/workflows byte-identical. **Decision: confirm `default` as the one
  sanctioned named fallback** (an explicit, documented exception to
  no-arbitrary-defaults), or accept a harder migration if you want (a).

- **OQ-2 — task_queue granularity.** Per-activity, per-workflow, or
  per-workflow-with-per-activity-override? The norn/claude/mixed vision points at
  per-activity (a workflow mixing flavours per step), but per-workflow is simpler.
  Affects NSTQ-7's SDK surface. **Your call.**

- **OQ-3 — wire field: additive (recommended) vs rename.** §3 recommends additive
  tag 3 + opt-in send, never renaming tag 1. Confirm you do **not** want the Tier
  1 rename (ROUTING-MODEL §4 already rejects it, but it is the riskiest decision
  so it is restated here).

- **OQ-4 — workflow→namespace binding declaration/enforcement** (ROUTING-MODEL
  §5, still open). Declared in the workflow definition / `.aion` metadata, vs
  supplied at `aion start --namespace` and validated? Goal: a `local` workflow
  *cannot* be started into `remote`. **Out of scope to implement here but it
  interacts** — task_queue selection (NSTQ-7) likely rides the same declaration
  channel, so deciding the binding shape informs NSTQ-7's design.

- **OQ-5 — reconcile the three namespace-ish strings** (§2.1 contradiction). The
  worker has `config.namespace` (auth metadata header), `config.task_queue`
  (→ wire registration namespace), and the server has `default_namespace`. After
  the split: should the auth-metadata `namespace` and the registration namespace
  be the **same value** (they morally should — a worker authorized for namespace
  N registers into namespace N)? Today they can differ silently. **Decision:
  unify them, or document why they differ.** This is the cleanup that makes the
  split honest rather than adding a fourth string.

- **OQ-6 — interaction with Tier 3 node-affinity.** The hierarchy reserves `node`
  below task_queue. Confirm the addressing tuple is intended to grow to
  `(namespace, task_queue, node)` (Tier 3) — i.e. the registry/outbox/history
  field additions in this design should be shaped to admit a later `node` field
  without re-migration (e.g. nullable column now, populated later), as L3 reopen
  already carries "the whole recorded routing identity" forward (ROUTING-MODEL §6).

---

## 7. Cross-reference corrections (where code/docs disagree)

- **ROUTING-MODEL.md §3** says namespace and task-queue are "two names for one
  dimension." Precise correction: there are effectively **three** namespace-ish
  strings on the worker side (`config.namespace` auth metadata `config.rs:109`,
  `config.task_queue`→wire `namespace` `session.rs:387`, server
  `default_namespace`), only two of which are the conflation §3 describes. See
  §2.1 + OQ-5.
- **LIMINAL-SWAP-DESIGN.md §5 (13-3)** proposes `(namespace, activity_type)` as
  the routing/channel key and is **premature/incorrect** given this split: the
  pool address is `(namespace, task_queue)`, with `activity_type` matched inside
  the pool. See §4.2. Also: LIMINAL-SWAP frames 13-0 as unbuilt; it is built on
  this branch (`liminal_transport.rs`), with channel addressing explicitly
  deferred to 13-3 (§4.3) — so the dependency is live, not hypothetical.
- **ROUTING-MODEL.md §4 Tier 2** correctly anticipates every mechanical change
  here (proto field, triple ActivityKey, history recording, AW-014 rotation
  caveat). This doc adds the **migration/back-compat plan and the disjointness
  framing** Tier 2 left open.
