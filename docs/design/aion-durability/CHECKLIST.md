# Aion-Durability — Checklist

## Module Scaffold and Error Taxonomy

- [ ] **C1** — crates/aion/src/durability/mod.rs declares the durability submodules and re-exports the public surface, containing no logic.
- [ ] **C2** — DurabilityError is a thiserror enum covering at least sequence/append failure (wrapping StoreError), non-determinism violation, and history-shape errors, with no panics or unwrap in its construction paths.
- [ ] **C3** — NonDeterminismError is a distinct typed error carrying the WorkflowId, the sequence position of the mismatch, the expected command shape, and the found recorded-event shape.
- [ ] **C4** — DurabilityError implements std::error::Error and Display via thiserror, and no library code path constructs an error by panicking.

## Recorder and Single-Writer Append

- [ ] **C5** — A sequence-head tracker holds the current per-workflow head sequence and advances it only when an append succeeds.
- [ ] **C6** — Recorder is constructed for a single WorkflowId and is the only type that calls EventStore::append for that workflow.
- [ ] **C7** — Recorder computes expected_seq from its tracked head for every append; the head is never read from outside the Recorder nor guessed.
- [ ] **C8** — Recorder exposes recording methods for each event family: workflow lifecycle, activity (scheduled/started/completed/failed), timer (started/fired/cancelled), signal received, and child workflow (started/completed/failed).
- [ ] **C9** — When EventStore::append returns SequenceConflict, the Recorder surfaces a hard DurabilityError (double-writer bug) and does not retry by re-reading the head.
- [ ] **C10** — A Recorder can be initialised with a starting head read from existing history (so a recovered workflow continues appending at the correct sequence).

## History Cursor and Correlation

- [ ] **C11** — A correlation key type identifies a world-touching call: activity/child by scheduling ordinal (matching ActivityId/child WorkflowId derivation), timer by TimerId, signal by name-in-recorded-order.
- [ ] **C12** — HistoryCursor is built from an ordered Vec<Event> (read_history output) and walks it in sequence order.
- [ ] **C13** — HistoryCursor resolves the next recorded event matching a given event family and correlation key, advancing its internal position.
- [ ] **C14** — HistoryCursor reports exhaustion (no further recorded events) distinctly from a family/key mismatch.
- [ ] **C15** — Recorded non-exhausting ActivityFailed attempts are walked past as part of the recorded retry sequence so the cursor lands on the eventual recorded outcome.

## Command, Resolution, and Resolver

- [ ] **C16** — A Command enum models the world-touching calls the workflow can issue: RunActivity, AwaitSignal, StartTimer, SpawnChild, plus workflow-completion intent, each carrying its correlation key and any input Payload.
- [ ] **C17** — A Resolution enum models recorded outcomes: ActivityCompleted(result), ActivityFailedTerminal(error), TimerFired, SignalDelivered(payload), ChildCompleted(result), ChildFailed(error).
- [ ] **C18** — The Resolver, given a Command, returns exactly one of: a recorded Resolution from the cursor, ResumeLive when the cursor is exhausted, or a NonDeterminismError on family/key mismatch.
- [ ] **C19** — All world-touching calls funnel through the single Resolver entry point; recorded-vs-live and violation are decided in exactly one place.

## Determinism Violation Detection

- [ ] **C20** — When the next recorded event differs in family or correlation key from the issued Command, the Resolver raises NonDeterminismError and never returns a Resolution for that command.
- [ ] **C21** — A non-determinism violation causes the workflow to fail deterministically: exactly one WorkflowFailed is recorded (via the Recorder) carrying the violation classification.
- [ ] **C22** — A dedicated test drives a recorded history that deliberately diverges from the replayed command stream (wrong activity family, wrong correlation key) and asserts the typed NonDeterminismError with correct expected/found shapes.

## Determinism Context

- [ ] **C23** — DeterminismContext exposes the recorded timestamp of the event currently being applied as the sole source for workflow.now; it advances as the Resolver consumes recorded events.
- [ ] **C24** — No code path in this cluster calls the wall clock (SystemTime::now / Utc::now) to produce a workflow-visible value.
- [ ] **C25** — DeterminismContext provides a deterministic RNG seeded solely from WorkflowId + RunId as the sole source for workflow.random.
- [ ] **C26** — Two DeterminismContexts constructed for the same WorkflowId + RunId produce identical random sequences (asserted by test).

## Live Executor Seam and Handoff

- [ ] **C27** — A LiveExecutor trait defines the engine-implemented operations AD invokes only on resume-live: run an activity, start a timer, await a signal, spawn a child workflow.
- [ ] **C28** — While the cursor can satisfy a command, the LiveExecutor is never invoked (replay performs no live side effect).
- [ ] **C29** — After ResumeLive, the handoff glue invokes the LiveExecutor and records the resulting event(s) through the Recorder.

## Replay Driver and Activity-Result Caching

- [ ] **C30** — replay builds a Resolver from read_history and a DeterminismContext, and drives a freshly-created execution forward, satisfying each command from the cursor until exhaustion.
- [ ] **C31** — On replay a matched ActivityCompleted returns its recorded result Payload without dispatching the activity; a matched exhausted-retry ActivityFailed returns the recorded terminal ActivityError.
- [ ] **C32** — replay returns a resume point (the first command whose recorded event is absent) so the engine can continue live execution from there.
- [ ] **C33** — Behavioural tests over InMemoryStore assert: a fully-recorded history replays to its recorded terminal state with zero live calls; a partial history replays to the correct resume point; activity results are returned from cache.

## Recovery on Startup

- [ ] **C34** — recover calls EventStore::list_active and, for each returned WorkflowId, reads its history and replays it to its resume point.
- [ ] **C35** — recover initialises each workflow's Recorder at the head implied by its existing history so post-recovery appends continue at the correct sequence.
- [ ] **C36** — recover surfaces a per-workflow failure (e.g. a NonDeterminismError during replay) without silently dropping the workflow or aborting recovery of the others.
- [ ] **C37** — A test seeds InMemoryStore with multiple active and terminal workflows and asserts recover replays exactly the active ones to their correct resume points.

## End-to-End Round-Trip

- [ ] **C38** — An integration test records a workflow's events via the Recorder, then replays from the same store and asserts the replayed Resolutions match the recorded outcomes in order and the resume point is correct.
- [ ] **C39** — An integration test asserts workflow.now values during replay equal the recorded event timestamps in order, and that random draws match across two independent replays of the same run.
