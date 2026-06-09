# aion-proto

Shared gRPC and serde wire contracts for Aion servers, clients, and workers. This crate mirrors core domain values into transport-safe types, provides conversion helpers, and optionally exposes generated tonic service definitions behind the `generated` feature.

## Install

```toml
[dependencies]
aion-proto = "0.1.0"
```

## Key public types

- `ProtoWorkflowId`, `ProtoRunId`, `ProtoPayload`, and related wrappers encode core identifiers and payloads.
- `ProtoStartWorkflowRequest`, `ProtoSignalRequest`, and response types model workflow RPCs.
- `SubscriptionRequest` and `StreamedEvent` model event streaming.
- `ProtoActivityTask`, `ProtoActivityResult`, and `ProtoRegisterWorker` model worker protocol messages.
- `WireError`, `ProtoWireError`, and conversion functions report invalid wire values.

## Minimal usage

```rust
use aion_core::WorkflowId;
use aion_proto::{decode_core_value, encode_core_value};

let id = WorkflowId::new_v4();
let envelope = encode_core_value("default", Some("request-1".to_owned()), &id)?;
let decoded: WorkflowId = decode_core_value(&envelope)?;
assert_eq!(decoded, id);
# Ok::<(), Box<dyn std::error::Error>>(())
```
