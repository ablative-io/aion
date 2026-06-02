# Aion-Core — Checklist

## Domain Identifiers

- [ ] **C1** — WorkflowId is a newtype over Uuid deriving Serialize, Deserialize, Clone, Debug, Eq, Hash, and Display.
- [ ] **C2** — ActivityId is a newtype whose value derives from the activity's scheduling sequence position, enabling deterministic replay matching.
- [ ] **C3** — TimerId is a newtype supporting both author-assigned (named) and engine-assigned (anonymous) construction.
- [ ] **C4** — RunId is a newtype distinguishing successive runs of the same logical workflow (reset / continue-as-new).
- [ ] **C5** — No public signature in aion-core accepts a bare Uuid or String where a domain identifier is meant.

## Event Model

- [ ] **C6** — Event enum has workflow-lifecycle variants: WorkflowStarted, WorkflowCompleted, WorkflowFailed, WorkflowCancelled, WorkflowTimedOut.
- [ ] **C7** — Event enum has activity variants: ActivityScheduled, ActivityStarted, ActivityCompleted, ActivityFailed (with attempt), ActivityCancelled.
- [ ] **C8** — Event enum has timer variants: TimerStarted, TimerFired, TimerCancelled.
- [ ] **C9** — Event enum has a SignalReceived variant carrying signal name and payload.
- [ ] **C10** — Event enum has child-workflow variants: ChildWorkflowStarted, ChildWorkflowCompleted, ChildWorkflowFailed, ChildWorkflowCancelled.
- [ ] **C11** — Every event carries an envelope with a monotonic per-workflow sequence number, a recorded timestamp, and the owning WorkflowId.
- [ ] **C12** — Event derives Serialize and Deserialize and round-trips losslessly through serde_json.
- [ ] **C13** — Payload is a newtype over bytes with a content-type tag.
- [ ] **C14** — Payload constructs from and converts to serde_json::Value and round-trips losslessly.
- [ ] **C15** — Activity inputs/results, signal payloads, and workflow inputs are stored as Payload; Event carries no generic type parameter.

## Status, Filters, Summaries

- [ ] **C16** — WorkflowStatus enum has variants Running, Completed, Failed, Cancelled, TimedOut.
- [ ] **C17** — WorkflowStatus is computed by projecting over event history (the last lifecycle event determines the status).
- [ ] **C18** — No code path sets WorkflowStatus independently of the events that justify it.
- [ ] **C19** — WorkflowFilter supports filtering by workflow type, status, time range, and parent workflow.
- [ ] **C20** — WorkflowSummary carries workflow id, type, status, start time, and end time.

## Error Taxonomy

- [ ] **C21** — ActivityError carries an explicit retryable-vs-terminal classification, not a string or bool.
- [ ] **C22** — WorkflowError is defined as the terminal failure type of a workflow.
- [ ] **C23** — StoreError is a closed enum with SequenceConflict, NotFound, Backend, and Serialization variants.

## EventStore Trait

- [ ] **C24** — EventStore is an async trait that is Send + Sync + 'static.
- [ ] **C25** — append accepts an expected sequence number, applies all events atomically, and returns SequenceConflict if the stored head does not match.
- [ ] **C26** — read_history returns the complete event history for a workflow in sequence order.
- [ ] **C27** — list_active returns the WorkflowIds of all non-terminal workflows.
- [ ] **C28** — query returns WorkflowSummary values matching a WorkflowFilter.
- [ ] **C29** — schedule_timer persists a durable timer and expired_timers returns timers due to fire as of a given instant.

## In-Memory Reference Store

- [ ] **C30** — InMemoryStore implements EventStore in full, including timer scheduling and retrieval.
- [ ] **C31** — InMemoryStore append enforces the expected-sequence guard and applies events atomically (all or none).
- [ ] **C32** — A reusable behavioural test suite exercises append/read/list/query/timers plus conflict and atomicity, runnable against any EventStore, and passes against InMemoryStore.
