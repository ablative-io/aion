//! `LibSqlStore` struct and `EventStore` implementation wiring.

use std::path::PathBuf;

use aion_store::{
    Event, OutboxRow, OutboxStore, PackageRecord, PackageRouteRecord, PackageStore,
    ReadableEventStore, RunSummary, StoreError, TimerEntry, TimerId, WorkflowFilter, WorkflowId,
    WorkflowSummary, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::config::{LibSqlConfig, LibSqlMode};
use crate::outbox::OutboxRowState;

/// Durable `EventStore` backed by a shared libSQL connection.
#[derive(Clone)]
pub struct LibSqlStore {
    conn: libsql::Connection,
    db: std::sync::Arc<libsql::Database>,
}

impl LibSqlStore {
    /// Open a store from operator-provided libSQL configuration.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Backend` when the connection cannot be opened or when the idempotent
    /// schema DDL cannot be applied.
    pub async fn connect(config: LibSqlConfig) -> Result<Self, StoreError> {
        let opened = crate::connection::open_connection(&config).await?;
        let conn = opened.connection;
        crate::schema::ensure_schema(&conn).await?;

        Ok(Self {
            conn,
            db: std::sync::Arc::new(opened.database),
        })
    }

    /// Open an embedded local-file store at `path`.
    ///
    /// Operator tunables remain unset; this convenience constructor only selects embedded mode.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Backend` when the connection cannot be opened or when the idempotent
    /// schema DDL cannot be applied.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::connect(LibSqlConfig {
            mode: LibSqlMode::Embedded { path: path.into() },
            journal_mode: None,
            synchronous: None,
            sync_interval_seconds: None,
        })
        .await
    }

    /// Validate stored event blobs against the current Aion event schema.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Serialization` when any stored event cannot be decoded by the current
    /// event schema, or `StoreError::Backend` for libSQL scan failures.
    pub async fn validate_event_compatibility(&self) -> Result<(), StoreError> {
        crate::read::validate_all_events(self.connection()).await
    }

    /// Trigger and await a libSQL replica synchronization cycle.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Backend` when the current libSQL database mode does not support sync or
    /// when the replica sync operation fails.
    pub async fn sync(&self) -> Result<(), StoreError> {
        self.db
            .sync()
            .await
            .map(|_| ())
            .map_err(|error| crate::error::libsql_error(&error))
    }

    /// Atomically append `events` and, when `outbox_rows` is `Some`, the outbox rows in the same
    /// `IMMEDIATE` transaction, under the expected-head sequence guard.
    ///
    /// This is the durable fan-out write: the scheduling events and their outbox rows commit
    /// together or not at all. Passing `None` is equivalent to
    /// [`WritableEventStore::append`](aion_store::WritableEventStore::append).
    ///
    /// # Errors
    ///
    /// Returns `StoreError::SequenceConflict` when the stored head differs from `expected_seq`,
    /// `StoreError::Serialization` when an event or outbox payload cannot be serialized, and
    /// `StoreError::Backend` for libSQL boundary failures.
    pub async fn append_with_outbox(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: Option<&[OutboxRow]>,
    ) -> Result<(), StoreError> {
        crate::append::append_with_outbox(
            self.connection(),
            workflow_id,
            events,
            expected_seq,
            outbox_rows,
        )
        .await
    }

    /// Read the persisted lifecycle state of one outbox row by its dispatch key.
    ///
    /// Returns the `(status, attempt, visible_after)` triple, or `None` when no
    /// row carries `dispatch_key` (the dedup guard may have ignored it). This is
    /// an out-of-band inspection helper — the [`OutboxStore`] dispatch contract
    /// itself keys terminal transitions off `dispatch_key` and never needs to
    /// read a row back — used by the dispatcher's tests and by operators auditing
    /// dead-lettered (`failed`) rows.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Backend` for libSQL boundary failures and
    /// `StoreError::Serialization` when the stored status token or timestamp
    /// cannot be decoded.
    pub async fn outbox_row_state(
        &self,
        dispatch_key: &str,
    ) -> Result<Option<OutboxRowState>, StoreError> {
        crate::outbox::outbox_row_state(self.connection(), dispatch_key).await
    }

    /// Borrow the shared libSQL connection used by append, read, and timer modules.
    pub(crate) fn connection(&self) -> &libsql::Connection {
        &self.conn
    }
}

#[async_trait]
impl PackageStore for LibSqlStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        crate::package::put_package(self.connection(), record).await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        crate::package::list_packages(self.connection()).await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        crate::package::delete_package(self.connection(), workflow_type, content_hash).await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        crate::package::put_package_route(self.connection(), workflow_type, content_hash).await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        crate::package::list_package_routes(self.connection()).await
    }
}

#[async_trait]
impl WritableEventStore for LibSqlStore {
    async fn append(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        crate::append::append(self.connection(), workflow_id, events, expected_seq).await
    }

    async fn append_with_outbox(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        crate::append::append_with_outbox(
            self.connection(),
            workflow_id,
            events,
            expected_seq,
            Some(outbox_rows),
        )
        .await
    }
}

#[async_trait]
impl OutboxStore for LibSqlStore {
    async fn append_outbox_batch(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        crate::outbox::append_outbox_batch(self.connection(), rows).await
    }

    async fn claim_outbox_rows(&self, limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
        crate::outbox::claim_outbox_rows(self.connection(), limit).await
    }

    async fn complete_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError> {
        crate::outbox::complete_outbox_row(self.connection(), dispatch_key).await
    }

    async fn retry_outbox_row(
        &self,
        dispatch_key: &str,
        next_attempt: u32,
        visible_after: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        crate::outbox::retry_outbox_row(
            self.connection(),
            dispatch_key,
            next_attempt,
            visible_after,
        )
        .await
    }

    async fn fail_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError> {
        crate::outbox::fail_outbox_row(self.connection(), dispatch_key).await
    }
}

#[async_trait]
impl ReadableEventStore for LibSqlStore {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        crate::read::read_history(self.connection(), workflow_id).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        crate::read::read_history_from(self.connection(), workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        crate::read::read_run_chain(self.connection(), workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        crate::read::list_workflow_ids(self.connection()).await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        crate::read::list_active(self.connection()).await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        crate::read::query(self.connection(), filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        crate::timer::schedule_timer(self.connection(), workflow_id, timer_id, fire_at).await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        crate::timer::expired_timers(self.connection(), as_of).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::{EventStore, StoreError};

    use super::LibSqlStore;

    #[test]
    fn libsql_store_is_send_sync_static() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}

        assert_send_sync_static::<LibSqlStore>();
    }

    #[tokio::test]
    async fn open_creates_schema() -> Result<(), StoreError> {
        let store = LibSqlStore::open(unique_temp_path("open-schema")).await?;

        assert_schema_object(store.connection(), "table", "events").await?;
        assert_schema_object(store.connection(), "table", "timers").await?;
        assert_schema_object(store.connection(), "table", "visibility").await?;

        Ok(())
    }

    #[tokio::test]
    async fn store_can_be_used_as_event_store_trait_object() -> Result<(), StoreError> {
        let store = LibSqlStore::open(unique_temp_path("trait-object")).await?;
        let store: Arc<dyn EventStore> = Arc::new(store);

        assert_eq!(Arc::strong_count(&store), 1);
        Ok(())
    }

    #[tokio::test]
    async fn connection_accessor_reuses_same_database_handle() -> Result<(), StoreError> {
        let store = LibSqlStore::open(unique_temp_path("shared-handle")).await?;

        store
            .connection()
            .execute(
                "INSERT INTO timers (workflow_id, timer_id, fire_at) VALUES (?1, ?2, ?3)",
                ("workflow-a", "timer-a", "2026-06-03T00:00:00Z"),
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;

        let count = timer_count(store.connection()).await?;
        if count == 1 {
            Ok(())
        } else {
            Err(StoreError::Backend(format!(
                "expected one timer through shared connection, found {count}"
            )))
        }
    }

    async fn assert_schema_object(
        conn: &libsql::Connection,
        object_type: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = ?1 AND name = ?2",
                (object_type, name),
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let found = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .is_some();

        if found {
            Ok(())
        } else {
            Err(StoreError::Backend(format!(
                "schema object {object_type} {name} was not created"
            )))
        }
    }

    async fn timer_count(conn: &libsql::Connection) -> Result<i64, StoreError> {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM timers", ())
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .ok_or_else(|| {
                StoreError::Backend(String::from("timer count query returned no row"))
            })?;

        row.get(0)
            .map_err(|error| crate::error::libsql_error(&error))
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-store-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
