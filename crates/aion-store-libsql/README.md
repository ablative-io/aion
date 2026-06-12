# aion-store-libsql

Durable libSQL-backed event store implementation for Aion workflows. This crate opens embedded or embedded-replica libSQL databases, applies the Aion event-store schema, and implements the `aion-store` persistence traits for histories, timers, and visibility records.

## Install

```toml
[dependencies]
aion-store-libsql = "0.4.0"
```

## Key public types

- `LibSqlStore` is the durable `aion_store::EventStore` implementation.
- `LibSqlConfig` captures operator-provided connection and tuning settings.
- `LibSqlMode` selects embedded local-file or embedded-replica operation.

## Minimal usage

```rust
use aion_store::EventStore;
use aion_store_libsql::LibSqlStore;

let store = LibSqlStore::open("aion.db").await?;
store.validate_event_compatibility().await?;
# Ok::<(), Box<dyn std::error::Error>>(())
```
