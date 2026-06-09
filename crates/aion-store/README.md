# aion-store

Persistence contracts and in-memory event stores for Aion durable workflows. The crate defines the async event-store traits that engines use for history, run-chain summaries, timers, and visibility, plus a correct non-durable `InMemoryStore` implementation for tests and local development.

## Install

```toml
[dependencies]
aion-store = "0.1.0"
```

## Key public types

- `ReadableEventStore`, `WritableEventStore`, and `EventStore` define the persistence contract.
- `WriteToken` provides optimistic append fencing for workflow histories.
- `RunSummary` and `TimerEntry` describe persisted run and timer state.
- `VisibilityStore`, `VisibilityRecord`, and `ListWorkflowsFilter` model workflow search.
- `InMemoryStore` is the reference in-memory implementation.

## Minimal usage

```rust
use aion_store::{InMemoryStore, ReadableEventStore, WorkflowId};

let store = InMemoryStore::default();
let workflow_id = WorkflowId::new_v4();
let history = store.read_history(&workflow_id).await?;
assert!(history.is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```
