# aion-core

Pure domain model and shared vocabulary for Aion durable workflows. This leaf crate contains the identifiers, payload carrier, event vocabulary, workflow filters, schedules, search attributes, statuses, and error types used by servers, stores, SDKs, and workers.

## Install

```toml
[dependencies]
aion-core = "0.1.0"
```

## Key public types

- `WorkflowId`, `RunId`, `ActivityId`, and `TimerId` identify workflow executions and side effects.
- `Payload` and `ContentType` carry serialized user data through events and APIs.
- `Event` and `EventEnvelope` describe durable history records.
- `WorkflowFilter`, `WorkflowSummary`, and `WorkflowStatus` support visibility APIs.
- `ScheduleConfig`, `TriggerSpec`, and schedule policies model recurring starts.

## Minimal usage

```rust
use aion_core::{Payload, WorkflowId};
use serde_json::json;

let workflow_id = WorkflowId::new_v4();
let payload = Payload::from_json(&json!({ "workflow_id": workflow_id.to_string() }))?;
let decoded = payload.to_json()?;
assert_eq!(decoded["workflow_id"], workflow_id.to_string());
# Ok::<(), Box<dyn std::error::Error>>(())
```
