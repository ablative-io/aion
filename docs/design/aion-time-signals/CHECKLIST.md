# Aion-Time-Signals — Checklist

## Engine Seam

- [ ] **C1** — An engine-facing handle/trait (consumed, not owned, by this cluster) exposes: resolve a WorkflowId to a live process handle, deliver a message to a workflow process mailbox, request AE to spawn a child workflow, and arm/disarm a beamr timer-wheel entry.
- [ ] **C2** — The engine seam is an explicit trait (or handle type) so this cluster never manages workflow process lifecycle, supervision, or module loading directly.
- [ ] **C3** — The time/signal/query/child/concurrency modules live under the aion crate with mod.rs files containing only pub mod declarations and pub use re-exports.

## Durable Timer Service

- [ ] **C4** — Scheduling a durable timer records it via EventStore::schedule_timer with the timer's TimerId and fire_at, and records a TimerStarted event in the workflow history.
- [ ] **C5** — Scheduling a durable timer arms a beamr timer-wheel entry (via the engine seam) so the timer fires live during normal execution without a store read.
- [ ] **C6** — When a wheel entry fires, the service records a TimerFired event and delivers TimerFired to the owning workflow process's mailbox.
- [ ] **C7** — A timer is fired exactly once: a timer already recorded as TimerFired (by the wheel) is not fired again by recovery, and vice versa.

## Timer Recovery

- [ ] **C8** — On engine startup the timer service polls EventStore::expired_timers(now) and, for each entry whose TimerFired has not been recorded, records TimerFired and delivers it to the owning workflow process.
- [ ] **C9** — A periodic recovery tick (interval engine-configured, not hardcoded) re-polls expired_timers as a safety net for timers whose owning process was not resident on the wheel.
- [ ] **C10** — Recovery is idempotent: re-running it does not double-fire a timer already recorded as fired or cancelled.

## Named Timers and Sleeps

- [ ] **C11** — start_timer takes an author-assigned (named) TimerId and schedules a durable, cancellable timer.
- [ ] **C12** — cancel_timer disarms the wheel entry and records a TimerCancelled event; cancelling an already-fired timer is a no-op, not an error.
- [ ] **C13** — sleep schedules an anonymous timer with an engine-assigned TimerId derived from sequence position; an anonymous sleep is not separately cancellable.

## Signal Router

- [ ] **C14** — Delivering a signal records a SignalReceived event (signal name + Payload) before/atomically with delivery, so a crash after delivery but before consumption is recoverable.
- [ ] **C15** — The signal is delivered to the target workflow process's mailbox so the workflow's selective receive picks it up without polling.
- [ ] **C16** — Signal delivery resolves a WorkflowId to a live process via the engine seam; the router does not manage process lifecycle.

## Non-Resident Signal Delivery

- [ ] **C17** — Signalling a workflow whose process is not currently resident still records SignalReceived; the signal is delivered to the mailbox once AE makes the process resident.
- [ ] **C18** — Signalling a terminal or unknown workflow returns a typed error and records nothing.

## Query Service

- [ ] **C19** — A query is dispatched to the workflow process as a distinct message kind and answered from a registered query handler, replying on a one-shot reply channel.
- [ ] **C20** — No Event is ever appended for a query, and the query never mutates workflow state or blocks the workflow's forward progress.
- [ ] **C21** — An unknown query name returns a typed QueryError; the workflow does not panic.
- [ ] **C22** — Query dispatch carries an engine-configured timeout and returns QueryError::Timeout if no reply arrives; querying a terminal/unknown workflow returns a typed NotRunning/Unknown error and never replays the workflow solely to answer.

## Child Workflow Spawning

- [ ] **C23** — Spawning a child requests AE to start a new workflow execution (distinct WorkflowId/RunId, own history) as a process linked to the parent, and records ChildWorkflowStarted in the parent history.
- [ ] **C24** — spawn_and_wait blocks the parent on its mailbox until the child result arrives, recording ChildWorkflowCompleted on success and ChildWorkflowFailed on child failure.
- [ ] **C25** — Fire-and-forget spawn records ChildWorkflowStarted and detaches (monitor rather than blocking await).
- [ ] **C26** — Cancelling the parent propagates over the link to the child process.

## Concurrency Correlation and Cancellation

- [ ] **C27** — Each spawned child in an all/race/map carries a per-spawn correlation token derived deterministically from the spawning sequence position, carried in its result message.
- [ ] **C28** — Selective receive matches an arriving result to its spawn position by correlation token, correctly even when many children of the same type run concurrently.
- [ ] **C29** — Cancellation terminates losing/remaining linked children via their links (exit signal) and records the cancellation so replay reconstructs started-vs-cancelled children; the workflow process traps exits so child exits arrive as messages.

## all / race / map

- [ ] **C30** — all spawns N linked children, blocks until all N results arrive, and returns them in input order; any child failure fails the whole all and cancels the remaining children.
- [ ] **C31** — race spawns N linked children, returns the first result to arrive, then cancels the remaining children and records their cancellation.
- [ ] **C32** — map applies a function to each element of a runtime list to produce child specs (dynamic fan-out), then collects like all (ordered, fail-fast).
