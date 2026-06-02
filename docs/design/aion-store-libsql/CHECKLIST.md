# Aion-Store-Libsql — Checklist

## Crate, Config, and Error Mapping

- [ ] **C1** — crates/aion-store-libsql exists, is a workspace member, and depends only on aion-store among Aion crates (plus external crates: libsql, serde, thiserror, chrono, async-trait or native async).
- [ ] **C2** — src/lib.rs contains only module declarations and re-exports, no logic.
- [ ] **C3** — LibSqlConfig is a Deserialize-only type whose mode enum selects Embedded { path } or EmbeddedReplica { path, primary_url, auth_token }.
- [ ] **C4** — LibSqlConfig exposes libSQL durability/WAL settings (e.g. sync interval for replica mode, journal/WAL options) with no hardcoded default values baked into the crate.
- [ ] **C5** — A boundary error-mapping module maps every libsql driver error and serde (de)serialisation failure into StoreError (Backend, Serialization), so no foreign error type leaks across the EventStore surface.

## Connection and Schema

- [ ] **C6** — Opening in Embedded mode produces a working connection to a local file database.
- [ ] **C7** — Opening in EmbeddedReplica mode produces a local replica connection configured to sync with the remote primary URL using the auth token.
- [ ] **C8** — Opening a store runs idempotent DDL (CREATE TABLE IF NOT EXISTS) so a fresh file and an existing file both arrive at the same schema with no external migration tool.
- [ ] **C9** — The events table has columns workflow_id, seq, event (serialised blob), recorded_at, with primary key (workflow_id, seq) and a secondary index supporting query/list_active projections.
- [ ] **C10** — The timers table has columns workflow_id, timer_id, fire_at, with primary key (workflow_id, timer_id) and an index on fire_at.

## Store Wiring

- [ ] **C11** — LibSqlStore is a Send + Sync + 'static type holding the libSQL connection, constructed from a LibSqlConfig.
- [ ] **C12** — LibSqlStore implements the aion-store EventStore trait and is usable as Arc<dyn EventStore>.
- [ ] **C13** — An open/constructor associated function (e.g. LibSqlStore::open and/or LibSqlStore::connect) builds the store from a config and ensures the schema exists before returning.

## Atomic Append

- [ ] **C14** — append reads the current head seq for the workflow and compares the expected next sequence inside one libSQL transaction.
- [ ] **C15** — On a matching expected sequence, all events in the batch are inserted with contiguous seq values and the transaction commits.
- [ ] **C16** — On a non-matching expected sequence, the transaction rolls back, no events are persisted, and StoreError::SequenceConflict { expected, found } is returned.
- [ ] **C17** — Two appends racing on the same expected sequence resolve to exactly one commit and one SequenceConflict, with no partial batch written.

## Reads, Listing, and Query

- [ ] **C18** — read_history returns a workflow's complete event history in ascending seq order, deserialised from the stored blobs.
- [ ] **C19** — list_active returns the WorkflowIds whose projected status (via aion-core's projection) is non-terminal (Running).
- [ ] **C20** — query translates a WorkflowFilter (workflow type, status, time range, parent) into a SQL WHERE clause and returns matching WorkflowSummary values.
- [ ] **C21** — WorkflowStatus and WorkflowSummary fields are derived from lifecycle event rows via aion-core's projection; there is no mutable stored status column.

## Durable Timers

- [ ] **C22** — schedule_timer persists a TimerEntry into the timers table keyed by (workflow_id, timer_id); re-scheduling the same timer id replaces the prior row (idempotent under replay).
- [ ] **C23** — expired_timers(as_of) returns all timers with fire_at <= as_of and excludes timers not yet due.

## Conformance and Durability Verification

- [ ] **C24** — The shared AC-007 conformance suite (run_event_store_suite) runs against LibSqlStore over a temporary local file as a real, non-gated runtime test and passes.
- [ ] **C25** — Persistence-across-reopen tests append history, drop the store, reopen the same file, and assert read_history, list_active, and timers are intact.
- [ ] **C26** — Embedded-replica sync is exercised by a test runtime-gated on AION_LIBSQL_TEST_URL: when the env var is unset the test emits a tracing skip line and returns Ok; it is never marked #[ignore].
