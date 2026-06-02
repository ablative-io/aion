---
type: design
cluster: aion-durability
title: Aion Durability — Event Sourcing, Replay, and Determinism
---

# Aion Durability — Event Sourcing, Replay, and Determinism

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map. This cluster
> lives **inside the `aion` crate** as a durability module set
> (`aion::durability`). It consumes the domain types from `aion-core`
> (`Event`, `Payload`, `WorkflowStatus`, `ActivityError`) and the
> `EventStore` trait + `InMemoryStore` from `aion-store`. It defines neither.

## Intention

This is the cluster that makes Aion *durable*. Everything else in the engine
— spawning workflow processes, routing signals, firing timers, dispatching
activities — is mechanics that a non-durable orchestrator could also have.
The durability layer is what lets a workflow crash on step nine of ten and
come back exactly where it was, what lets a workflow sleep for three months
and resume in the same logical state, what makes a workflow that ran once
run *as if it ran once* even after five replays.

It does three things. First, it **records**: every externally-observable
action the engine takes becomes an `Event` appended to the `EventStore`
under a strict single-writer-per-workflow discipline with an
expected-sequence guard. Second, it **replays**: given a workflow's complete
history, it re-runs the workflow function from the start, and for every
point where the workflow reaches out to the world — an activity call, a
signal wait, a timer, a child workflow — it consults the recorded history
and *returns the recorded outcome instead of acting again*. Third, it
**enforces determinism**: it supplies the recorded timestamp for
`workflow.now`, a workflow-seeded value for `workflow.random`, and it
detects the moment a replay diverges from recorded history — the symptom of
non-deterministic workflow code — and fails loudly rather than corrupting
state silently.

When this cluster is done, the engine (cluster AE) can spawn a workflow
process and hand it a *resolver* that transparently answers every
world-touching call from history during replay and falls through to live
execution at the resume point. Recovery on startup becomes a loop:
`list_active`, replay each, resume. The workflow author writes a plain
deterministic Gleam function and never knows whether any given call
executed for real or returned a cached result.

## Problem

Durable execution rests on one mechanism: a workflow function that can be
re-executed from the beginning and *take the same path every time* because
every non-deterministic input is fed back from a recorded log. Get any part
of this wrong and the failure is silent and catastrophic — a workflow that
charges a card twice, sends two emails, or reconstructs a state that never
actually existed.

Several distinct hazards must be handled, none of them optional:

**Recording must be correct and exactly-once-per-action.** When the engine
schedules an activity, it must append exactly one `ActivityScheduled`; when
that activity completes, exactly one `ActivityCompleted` carrying the
result. The append must use the right `expected_seq` or a concurrent or
duplicate writer corrupts the log. There must be a *single writer per
workflow* — the workflow's own execution context — so sequence numbers stay
monotonic and gap-free. A double-append, a wrong sequence, or a second
writer all break replay.

**Replay must match recorded results, not re-execute side effects.** This is
the heart of it. During replay, a `workflow.run(activity)` call must NOT run
the activity — it must look up the recorded outcome. If `ActivityCompleted`
exists, return the recorded result. If `ActivityFailed` with retries
exhausted exists, return the recorded terminal error. If neither exists, the
replay has caught up to reality and this call is where live execution
resumes. The matching must be deterministic: the engine must know *which*
recorded event corresponds to *this* call, with no ambiguity, even when a
workflow issues many activity calls. Signals, timers, and child workflows
have the same shape — return recorded or, at the resume point, act live.

**Determinism inputs must come from history, never the environment.**
`workflow.now` must return the recorded timestamp of the event being
processed, not `SystemTime::now()` — otherwise two replays see two clocks
and diverge. `workflow.random` must be seeded deterministically from the
workflow identity so it yields the same sequence on every replay. These are
the engine-side behaviours that the Gleam SDK functions (cluster AF) bind
to.

**Divergence must be caught, not absorbed.** If workflow code is
accidentally non-deterministic (it read the wall clock directly, iterated a
hash map, branched on un-recorded state), replay will reach a call that does
not match the recorded history — it will ask for the result of activity
"send-email" when history at that position records activity "charge-card".
This is a *non-determinism violation*. It must be detected at the point of
mismatch and surfaced as a hard, typed error that fails the workflow
deterministically, never silently papered over by guessing.

**Recovery must be complete.** On engine startup, every non-terminal
workflow must be replayed from its full history and resumed at its resume
point. Missing one means a workflow silently stalls forever.

If this layer is built loosely — its own ad-hoc idea of how to match a call
to history, a best-effort clock, a swallowed mismatch — durability is a
lie. It must instead be a precise, fully-tested transcription of the
event-sourcing model, with the determinism-violation path as rigorously
covered as the happy path.

## Solution

A durability module set inside the `aion` crate, `aion::durability`,
depending on `aion-core` and `aion-store`. It is organised around four
collaborating pieces:

- **The Recorder** — the single-writer append path. Holds the current
  sequence head for a workflow and appends events with the correct
  `expected_seq`. Every world-touching action the engine takes flows through
  one `Recorder` instance per workflow; nothing else appends to that
  workflow's history. This is where the single-writer discipline is
  structurally enforced.
- **The History Cursor** — an ordered, position-aware view over a
  workflow's recorded `Event` list, built once at the start of replay. It
  answers "what is the next recorded outcome for a call of this kind, with
  this correlation key?" and tracks whether the cursor has been exhausted
  (i.e. replay has caught up to live).
- **The Resolver** — the decision engine the workflow process consults for
  every world-touching call. Given a *command* (run-activity, await-signal,
  start-timer, spawn-child) it either returns a recorded `Resolution`
  (Completed / Failed / Fired / Signalled) drawn from the cursor, signals
  *resume-live* when the cursor is exhausted, or raises a
  `NonDeterminismError` when the next recorded event does not match the
  command. The Resolver is the seam the engine drives.
- **The Determinism Context** — per-execution state: the recorded timestamp
  of the event currently being applied (the source for `workflow.now`) and a
  seeded deterministic RNG (the source for `workflow.random`), seeded from
  `WorkflowId` + `RunId`.

Above these sit two orchestration entry points the engine calls:

- **Replay** — drive a freshly-created workflow execution through its
  recorded history using a Resolver built from `read_history`, until the
  cursor is exhausted, then hand control to live execution.
- **Recovery** — on startup, `list_active`, then replay-and-resume each
  active workflow.

### The boundary with AE (the rest of `aion`)

`aion::durability` and the engine cluster (**AE**) co-own the `aion` crate.
The split is by responsibility, stated here so the seam is unambiguous:

- **AE owns** workflow *lifecycle* and *process management*: creating the
  BEAM workflow process, the supervision tree, module loading from `.aion`
  packages, the `Engine`/`EngineBuilder` public API, and the act of
  *invoking the workflow function* and *actually executing* a live activity
  / setting a live timer / delivering a live signal.
- **AD (this cluster) owns** *durability*: recording events, building and
  driving replay, the Resolver that decides recorded-vs-live, the
  determinism context, and recovery-on-startup orchestration.

The contract between them is a small set of traits AD defines and AE
implements (or AD calls): AE provides a `LiveExecutor` (run an activity for
real, start a real timer, etc.) that AD invokes only when the Resolver
reports resume-live; AD provides the `Recorder`, `Resolver`, and `replay` /
`recover` entry points that AE wires into each workflow process. AD never
spawns a process, loads a module, or touches the supervision tree. AE never
appends an event directly or invents a sequence number — it goes through the
`Recorder`.

### The boundary with AT (timers, signals, queries, children)

Cluster **AT** owns the *live* execution of durable timers, the signal
router, the query service, child-workflow spawning, and the in-workflow
concurrency primitives — i.e. how these things *happen* when replay is
exhausted and execution is live. **AD owns how their events replay.** For
each of these, the seam is:

- AT, when live, performs the action and tells AD's `Recorder` to append the
  corresponding event(s) (`TimerStarted`/`TimerFired`,
  `SignalReceived`, `ChildWorkflowStarted`/`...Completed`/`...Failed`).
- AD, during replay, consults the cursor and returns the recorded outcome
  for that timer / signal / child *without* asking AT to do anything live —
  a fired timer is skipped instantly, a received signal is delivered
  immediately, a completed child returns its recorded result.
- The resume point is the single handoff: the first command whose recorded
  event is absent is where AD reports resume-live and AT takes over.

AD defines the `Resolution` shapes and the matching rules for these event
families; AT defines their live behaviour. AD's tests use recorded histories
directly and never need AT.

### The boundary with AF (the Gleam SDK)

The Gleam functions `workflow.now` and `workflow.random` (cluster **AF**)
are thin `@external` bindings. AD defines the *engine-side behaviour* they
resolve to: `now` returns the Determinism Context's current recorded
timestamp; `random` draws from the seeded RNG. AF owns the Gleam type
signatures and the binding; AD owns what the binding does.

### How replay matches a recorded result to a call

This is the load-bearing mechanism, so it is specified precisely. Each
world-touching command carries a **correlation key** that is deterministic
across runs:

- **Activities** correlate by `ActivityId`, which (per AC, decision D4) is
  derived from the activity's *scheduling sequence position* — the Nth
  activity scheduled in a run is always activity N. Because workflow code is
  deterministic, the Nth `workflow.run` in one replay is the Nth in every
  replay, so the cursor matches recorded `ActivityScheduled`/`Completed`/
  `Failed` events to the call by that ordinal identity.
- **Timers** correlate by `TimerId` (author-named or engine-assigned by
  ordinal, per AC D4).
- **Signals** correlate by signal *name* in recorded order: the kth recorded
  `SignalReceived` for a given name satisfies the kth `receive` of that name.
- **Child workflows** correlate by child `WorkflowId`, assigned at spawn by
  ordinal position (same scheme as activities).

The cursor walks history in sequence order. For each command the workflow
issues, the Resolver asks the cursor for the next recorded event *of the
matching family and key*. Three outcomes:

1. **Match found** → return the recorded `Resolution` (the recorded result,
   error, fired-ness, or signal payload). The Determinism Context's current
   timestamp advances to that event's recorded timestamp.
2. **Cursor exhausted** (no more recorded events) → return `ResumeLive`. From
   here on, the engine executes for real and the Recorder appends new events.
3. **Mismatch** (the next recorded event exists but is a different family or
   key than the command expects) → raise `NonDeterminismError` describing
   the expected-vs-found, failing the workflow.

### How determinism violations are detected

A violation is precisely "the workflow, on replay, issued a command that
does not line up with the recorded history at the cursor's current
position." The Resolver is the single chokepoint where this is checked,
because every world-touching call goes through it. Detection is *structural*,
not heuristic: the cursor knows exactly which recorded event is next; the
command states exactly what it expects; if family or correlation key differ,
that is a violation. It is raised as a typed `NonDeterminismError` carrying
the workflow id, the sequence position, the expected command shape, and the
found event shape — enough for an operator to locate the offending workflow
code. The workflow fails deterministically (a `WorkflowFailed` is recorded
once, by the Recorder, with this classification); it is never absorbed,
retried blindly, or allowed to corrupt state. This path has dedicated test
coverage (a workflow whose recorded history deliberately disagrees with the
replayed command stream).

### Activity-result caching on replay

During replay, a matched `ActivityCompleted` returns its recorded result
`Payload` directly — the activity is never dispatched. A matched
`ActivityFailed` whose attempts are exhausted returns the recorded terminal
`ActivityError`. A recorded `ActivityFailed` that does *not* exhaust the
retry policy is part of the recorded retry sequence: replay walks past the
recorded failed attempts and only resumes-live (or returns the eventual
recorded completion) according to what history records. The cache is the
history itself; there is no separate cache store — the History Cursor *is*
the cache.

## Structure

```
crates/aion/src/durability/mod.rs            [AD-001] pub mod + re-exports only
crates/aion/src/durability/recorder.rs       [AD-002] Recorder: single-writer append path
crates/aion/src/durability/seq.rs            [AD-002] sequence-head tracking for a workflow
crates/aion/src/durability/cursor.rs         [AD-003] HistoryCursor over recorded events
crates/aion/src/durability/correlation.rs    [AD-003] correlation keys + matching rules
crates/aion/src/durability/command.rs        [AD-004] Command + Resolution types (the AD/AE+AT seam)
crates/aion/src/durability/resolver.rs       [AD-004] Resolver: recorded-vs-live decision + violation detection
crates/aion/src/durability/error.rs          [AD-005] NonDeterminismError + DurabilityError taxonomy
crates/aion/src/durability/determinism.rs    [AD-006] DeterminismContext: recorded-now + seeded RNG
crates/aion/src/durability/executor.rs        [AD-007] LiveExecutor trait (implemented by AE) + handoff glue
crates/aion/src/durability/replay.rs          [AD-008] replay: drive an execution through history to the resume point
crates/aion/src/durability/recovery.rs        [AD-009] recover: list_active + replay-and-resume on startup
```

(`aion`'s own `lib.rs` and the rest of the crate are AE's; this cluster adds
the `durability` module subtree and AE declares `pub mod durability;` from
`lib.rs`. The seam module `executor.rs` defines the `LiveExecutor` trait AE
implements.)

## Constraints

- **CO1** — `unsafe_code = "deny"`. No unsafe anywhere in the module set.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses. Tests
  needing a store use `InMemoryStore` (in-process), never an env-gated skip.
- **CO3** — `mod.rs` holds only `pub mod` declarations and re-exports; all
  logic lives in named files.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace). The
  Resolver and replay driver are the likeliest to approach it; split before
  they exceed it.
- **CO5** — `aion::durability` depends only on `aion-core` and `aion-store`
  (plus an RNG crate and `chrono`). It does **not** depend on AT, AF, or any
  networking; it does not reach into beamr directly — process/runtime
  concerns are AE's, reached only through the `LiveExecutor` trait.
- **CO6** — The single-writer discipline is structural: only a `Recorder`
  appends to a workflow's history, and there is exactly one `Recorder` per
  active workflow. No other code path calls `EventStore::append` for a
  workflow that has a live `Recorder`.
- **CO7** — Every append states an `expected_seq` derived from the
  `Recorder`'s tracked head; the head is never guessed and never read from
  outside the `Recorder`. A `SequenceConflict` from the store is a bug
  (double-writer) and is surfaced as a hard error, not retried by re-reading.
- **CO8** — `workflow.now` derives only from the recorded event timestamp in
  the `DeterminismContext`; no code path in this cluster calls the wall clock
  (`SystemTime::now` / `Utc::now`) to produce a value visible to workflow
  code. The recovery timestamp used for `expired_timers` is engine wall time
  and is explicitly *not* a workflow-visible value.
- **CO9** — `workflow.random` derives only from the seed
  (`WorkflowId` + `RunId`); no entropy source is consulted. Two executions
  of the same run produce identical random sequences.
- **CO10** — A non-determinism violation is always raised as a typed
  `NonDeterminismError` at the point of mismatch; it is never swallowed,
  never guessed past, and never allowed to produce a `Resolution`.
- **CO11** — Replay never invokes a live side effect: while the cursor can
  satisfy a command, the `LiveExecutor` is not called. The `LiveExecutor` is
  called only after `ResumeLive`.
- **CO12** — Determinism-violation detection has dedicated test coverage; the
  test suite drives a recorded history that deliberately diverges from the
  replayed command stream and asserts the typed error.

## Non-Goals

- **No live execution of timers, signals, queries, or child workflows** —
  that is cluster AT. AD defines only how their *recorded* forms replay.
- **No workflow lifecycle, process spawning, supervision, or module
  loading** — that is cluster AE. AD is invoked by AE through the
  `Recorder`/`Resolver`/`replay`/`recover` surface and the `LiveExecutor`
  seam.
- **No Gleam SDK surface** — the `workflow.now`/`workflow.random` Gleam
  functions are AF; AD defines only the engine-side behaviour they bind to.
- **No storage backend** — AD uses the `EventStore` trait and tests against
  `InMemoryStore` (AC). libSQL is AS, PostgreSQL is AX.
- **No event compaction / snapshotting** — long-history truncation is a
  later, separately-measured concern; replay here always consumes full
  history.
- **No retry-policy *definition*** — the retry policy lives with the activity
  contract (AC `ActivityError` retryable/terminal split; policy application
  is AE/AT). AD only *replays* the recorded outcome of whatever retries
  already happened.
