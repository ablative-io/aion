# Routing model — namespace, task-queue, node

> ✅ SUPERSEDED / IMPLEMENTED (reconciled 2026-07-02). All three tiers now SHIP.
> The "Nothing in Tier 2/3 is implemented" line below is STALE. Tier 2 (namespace ×
> task_queue split, NSTQ) landed — `crates/aion-proto-generated/proto/worker.proto`
> (`namespaces`/`task_queue`/`node` fields) + `aion-server/src/worker/registry.rs`
> (`(namespace, task_queue, node)` pool key); NSTQ merge `6c0276fc`. Tier 3 (node
> affinity) landed — see NODE-AFFINITY-DESIGN.md (NODE-1..5). This doc is retained as
> the original conceptual record; for current design see NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md
> and NODE-AFFINITY-DESIGN.md.
>
> Status (original): design notes, not yet briefs. Captured 2026-06-16 during the L3
> (workflow reopen) design discussion. Records the agreed direction for how
> work is routed to workers so we can build it deliberately rather than letting
> it stay as the current single-dimension accident.
>
> Decision so far: **workflows are keyed to a namespace, set at the workflow
> level.** L3 (reopen) builds on the namespace dimension as it exists today; the
> namespace/task-queue split (Tier 2 below) is the next brief after L3; node
> affinity (Tier 3) is a later follow-on. Nothing in Tier 2/3 is implemented.

## 1. The three dimensions (target model)

Three **independent** routing dimensions. Today only one exists (see §3); the
other two are names without teeth.

- **namespace** — the logical group a workflow belongs to. It is a property of
  the **workflow**, declared at the workflow level, and it is durable. It
  expresses *where a workflow is allowed to run*. Concretely: the
  `stacked_dev_remote` workflow is designed to operate on remote systems and
  must only run on remote infrastructure → it is keyed to the `remote`
  namespace. The plain `stacked_dev` (local) workflow operates on the local
  machine and cannot run anywhere else → it is keyed to a `local` namespace. A
  workflow never escapes its namespace; its activities only ever dispatch to
  workers registered in that namespace.

- **task-queue** — *which flavour of worker* inside a namespace handles a given
  activity. Different workers in the same namespace can subscribe to different
  task queues, so you can stand up specialised worker pools and route work to
  them. Examples Tom gave: a worker that runs everything with **norn**, a worker
  that runs everything with **Claude**, a worker that runs a **mix**; or the
  same agent at different **reasoning levels / budgets**. You spin up one worker
  per flavour (a norn queue, a claude queue, a mixed queue) and the workflow (or
  the activity) chooses which queue a step runs on. Note: both norn and Claude
  accept the same structured output, so the brief/handler content is unchanged —
  only the routing flags around it differ.

- **node** — the specific device a worker runs on. The unit of *physical*
  affinity: when a workflow's external state lives on one machine (a git
  worktree, a norn session, cloned files), a reopened or continued run has to
  land back on **that** device. Today there is no node identity in routing.

**Worker backends and resumability (decided 2026-06-16).** Worker flavours can
run different agent backends — norn or Claude Code — behind the same structured
output, so the brief/handler logic is unchanged; only the agent invocation
differs. The two backends differ in *session reopen*, which matters because L3
reopen relies on a step reconnecting to its prior session (see
`docs/WORKFLOW-REOPEN-DESIGN.md` §13):
- **norn**: `--session-id <branch>` + `--resume-if-exists` — a re-dispatched
  step reconnects to the same branch-keyed session automatically. Fully
  reopenable.
- **Claude Code**: the session id must be a valid UUID; you can instead set a
  *name* and reopen by that name, but there is **no `--resume-if-exists`
  equivalent**. So Claude workers run **locally** and are treated as best-effort
  / effectively non-reopenable for now — be careful with them. Making a
  Claude-backed step auto-reopen would need worker-side run-then-retry-with-
  reopen logic (try; if the named session already exists, re-invoke with the
  reopen flag) or a workflow change. Deferred — more complexity than needed now.

Hierarchy: **namespace ⊃ task-queue ⊃ node**. A workflow picks a namespace;
task queues partition workers within it; a node is a specific worker location.

## 2. The problem this fixes (why it matters)

A couple of sessions back we hit a real, painful failure: workers were not
keyed to anything durable, so **whichever worker registered first picked up
everything**. The `remote` workflow got deployed to a local worker and the
`local` workflow got deployed to a remote worker. Result: every local workflow
**failed** when it ran on a remote worker (its files/tools weren't there), and
the remote workflows ran on local workers consuming resources doing work that
made no sense for their environment. (This is the same class of bug as the
namespace-recovery fix from 2026-06-15, where recovered remote workflows routed
to a local worker after restart because the namespace wasn't being re-derived
from history.)

Keying a workflow to a namespace at the workflow level — and only dispatching
its activities to workers registered in that namespace — makes that
misrouting **structurally impossible**. A `local`-namespace workflow can only
ever reach `local`-namespace workers. This is the load-bearing reason the model
matters; it is not cosmetic.

## 3. Current state (verified in code, 2026-06-16)

Today namespace and task-queue are **two names for one dimension**:

- The worker is started with `--task-queue <value>`. The worker SDK config
  field is literally called `task_queue`
  (`crates/aion-worker/src/config.rs`), and its doc comment admits: *"The
  current AW wire names this field `namespace`; this SDK maps the task queue
  value to that owned wire shape."*
- On registration the worker sends its task-queue value **as** the wire's
  namespace field: `RegisterWorker { namespace: self.config.task_queue.clone(),
  activity_types }` (`crates/aion-worker/src/protocol/session.rs`).
- The dispatch registry keys workers by
  `ActivityKey = (namespace, activity_type)`
  (`crates/aion-server/src/worker/registry.rs`) — no task-queue, no node.
- A workflow is started with `aion start <wf> --namespace <ns>`; its activities
  dispatch to whichever workers are registered under that namespace, round-robin
  across the pool (the AW-014 rotation), with **no stickiness** to a worker
  instance or device.
- The workflow's namespace **is** recorded durably (as a search attribute), and
  restart recovery already re-derives it and routes correctly. So the namespace
  dimension is real and durable today; task-queue and node are not.

So in the current setup `stacked_dev_remote` works only because, by convention,
the remote worker is started with `--task-queue remote` (→ namespace `remote`)
and the workflow with `--namespace remote`, and there happens to be one worker
per namespace. The model below makes that convention explicit and enforced.

## 4. Work to get there — three tiers

### Tier 1 — naming consistency (NOT planned as a standalone)
Rename `--task-queue` → `--namespace` on the workers so both worker and
workflow speak `namespace`. Small/mechanical (SDK config field + builder + the
example workers' flag + doc comment + tests; no engine/proto/history change
because the wire already says namespace). **Deliberately not doing this on its
own** — it would delete task-queue as a concept right when we want it back,
which is rename-now/re-add-later churn against the do-it-once rule. Subsumed by
Tier 2.

### Tier 2 — make task-queue a real second dimension (next brief after L3)
- proto: `RegisterWorker` gains a real `task_queue` field, independent of
  `namespace`.
- registry: `ActivityKey` goes from `(namespace, activity_type)` to
  `(namespace, task_queue, activity_type)` — touches the AW-014 round-robin
  code (`registry.rs`, `bridge.rs`).
- workflow-level namespace binding: a workflow **declares** its namespace (so
  `stacked_dev_remote` is intrinsically `remote`, not reliant on the operator
  passing `--namespace remote` correctly each time). Enforced + durable.
- activity-level task-queue selection in the Gleam SDK (how a step says "run me
  on the claude queue"), threaded through the dispatch config into the engine —
  same path the activity labels already ride.
- record `task_queue` in history (in `ActivityScheduled`) so reopen/recovery can
  re-target the same queue (it is **not** recorded today).
- tests across all of it.
Medium effort; needs the design pass for the open questions in §5.

### Tier 3 — node affinity (later follow-on)
Worker node identity + sticky pinning so a workflow reopens/continues on the
**same physical device** that holds its external state. Biggest of the three;
separate design. Needed when a namespace+task-queue ever has more than one
worker on different machines.

## 5. Open design questions (decide when building Tier 2)

- **No default, per the no-arbitrary-defaults rule:** when an activity doesn't
  name a task-queue, do we (a) require every activity to name one, or (b) define
  a named `default` queue as the explicit fallback? Tom's call.
- **Granularity:** is task-queue chosen per-activity, per-workflow, or
  per-workflow-with-per-activity-override? The norn/claude/mixed vision points
  at per-activity (or per-workflow picking a flavour). To be specified.
- **How is the workflow→namespace binding declared and enforced?** Options:
  declared in the workflow definition / `.aion` package metadata vs supplied at
  start and validated. Goal: a `local` workflow cannot be started into `remote`.
- **Interaction with the AW-014 rotation:** the triple key changes the rotation
  cursor's keyspace; verify the round-robin + closed-channel fallback still hold
  per `(namespace, task_queue, activity_type)`.

## 6. Relationship to L3 (workflow reopen)

L3 and this routing split are **decoupled**. L3 keys reopen affinity on
**namespace**, which is durable today, so it already returns a reopened
`remote` workflow to a `remote` worker (the requirement that a remote reopen
land on the device holding the files is met by preserving the namespace). L3 is
designed to preserve the workflow's **whole recorded routing identity**, so when
task-queue becomes a durable dimension (Tier 2) and node affinity arrives
(Tier 3), reopen picks them up with no rework — the routing identity just gains
fields that reopen already carries forward wholesale.

See `docs/WORKFLOW-RESILIENCE.md` for L3 itself.
