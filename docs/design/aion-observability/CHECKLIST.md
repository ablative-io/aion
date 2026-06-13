# Aion-Observability — Checklist

## Engine Exit Logging

- [ ] **C1** — handle_process_exit_async in crates/aion/src/lifecycle/completion.rs emits an error-level tracing call on the abnormal-exit branch (WorkflowProcessOutcome::Failed) carrying the workflow id, run id, workflow type, and the WorkflowError.message text, before the existing notify/reconcile steps.
- [ ] **C2** — handle_process_exit_async emits an error-level tracing call on the monitor-failure branch (the Err(error) arm that builds the 'workflow process monitor failed' WorkflowError) carrying the same fields.
- [ ] **C3** — handle_process_exit_async emits an info-level tracing call on the clean-completion branch (WorkflowProcessOutcome::Completed) carrying the workflow id, run id, and workflow type.
- [ ] **C4** — The idempotent-replay branch (a terminal already present in history) emits no failure log: a recovered run whose terminal was already recorded and already logged does not re-emit the error line.
- [ ] **C5** — Both monitor wiring sites — install_completion_monitor in lifecycle/start.rs and install_recovered_completion_monitor in engine/startup.rs — route their callback through handle_process_exit, so the single funnel covers freshly-started and recovered runs alike (verified by inspection; no new log site is added at either wiring site).
- [ ] **C6** — The added tracing calls are pure side effects: no recorded event, completion notification, registry reconciliation, or return value of handle_process_exit_async changes, and the existing completion.rs tests still pass unchanged in their assertions on recorded history and notified outcome.

## Server Log Surface

- [ ] **C7** — InstrumentedEventStore::record_events in crates/aion-server/src/observability/instrumented_store.rs emits an error-level tracing call on Event::WorkflowFailed carrying the namespace, workflow id, and the recorded WorkflowError.message, alongside the existing metrics counter.
- [ ] **C8** — record_events emits an error-level tracing call on Event::WorkflowTimedOut carrying the namespace and workflow id (a timeout is a terminal failure an operator must see), alongside the existing metrics counter.
- [ ] **C9** — record_events emits an error-level tracing call on Event::ChildWorkflowFailed carrying the namespace, parent workflow id, child workflow id, and the recorded error message.
- [ ] **C10** — The server log line uses the codebase tracing-field conventions (workflow_id rendered via Display, error message via Display) consistent with crates/aion-server/src/observability/tracing.rs, and the metrics counters that record_events already increments are unchanged.

## Worker Surface Confirmation

- [ ] **C11** — crates/aion-worker activity error logging is confirmed to already cover the failures the worker is the first to observe (activity input-decode failure and handler panic at error level with structured fields), and OBS-001 documents that the worker hosts no workflow process and therefore adds no workflow-process-exit log it cannot truthfully emit.

## Child Failure Propagation

- [ ] **C12** — record_parent_child_terminal in crates/aion/src/runtime/nif_child_watch.rs emits an error-level tracing call when it records a ChildWorkflowFailed terminal, carrying the parent workflow id, parent run id, child workflow id, and the child's WorkflowError.message — matching the existing warn/error lines the watcher emits for transient and invariant record failures.
- [ ] **C13** — The added child-failure log is emitted on the TerminalOutcome::Failed arm only (and the Cancelled/TimedOut arms that map to ChildWorkflowFailed), not on the Completed arm, and does not change the RecordDisposition returned or the events recorded.
- [ ] **C14** — A failed child's terminal surfaces on the parent run's describe timeline: the recorded ChildWorkflowFailed carries the child workflow id and the child's reason, and is encoded into the describe history by describe.rs and payload.rs::encode_history (verified end to end against a parent history containing a ChildWorkflowFailed).
- [ ] **C15** — A watcher subscribed to the event stream receives the parent-side ChildWorkflowFailed event when a child fails: the existing subscription/push surface forwards it because it is a normal appended event, with no new push path added.

## Invariants and Gate

- [ ] **C16** — No new Event variant, no new Payload type, and no mutable status field are introduced anywhere in the cluster; status remains derived solely by status_from_events (invariant 4), and no brief calls EventStore::append directly (invariant 3).
- [ ] **C17** — No #[allow], #[expect], unwrap, or expect appears in the non-test library code added by either brief; every touched source file stays under 500 LOC excluding tests.
- [ ] **C18** — cargo fmt --check, cargo clippy --workspace --all-targets -- -D warnings, and the full workspace test suite pass clean at the cluster's landing commit.
