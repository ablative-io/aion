# aion

Transport-agnostic Aion workflow engine with durability, replay, timers, and supervision. The crate embeds the BEAM runtime, loads `.aion` workflow packages, wires event stores and activity dispatchers, and exposes engine APIs that servers and tests can use without depending on a particular transport.

## Install

```toml
[dependencies]
aion = "0.4.0"
```

## Key public types

- `EngineBuilder` and `Engine` construct and operate the workflow engine.
- `RuntimeConfig`, `RuntimeHandle`, and `Pid` configure and identify runtime processes.
- `load_package`, `LoadedWorkflow`, and `LoadedWorkflows` register package contents.
- `Registry`, `WorkflowHandle`, and supervision types track workflow residency and lifecycle.
- `ActivityDispatcher`, `EventPublisher`, `SignalRouter`, and `QueryService` are engine seams.

## Minimal usage

```rust
use aion::{EngineBuilder, RuntimeConfig};
use aion_store::InMemoryStore;

let engine = EngineBuilder::new()
    .with_store(InMemoryStore::default())
    .with_runtime_config(RuntimeConfig::default())
    .build()
    .await?;
# Ok::<(), Box<dyn std::error::Error>>(())
```
