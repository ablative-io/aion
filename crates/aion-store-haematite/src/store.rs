//! `HaematiteStore`: a single-node Aion [`EventStore`] over [`haematite`].
//!
//! # Design
//!
//! * **Events** are the source of truth. Each workflow's history lives in one
//!   haematite event stream ([`keyspace::event_stream_key`]); appends route
//!   through [`haematite::EventStore::append_batch`] under haematite's own
//!   optimistic-concurrency guard, and reads come back through `read`/`read_from`.
//!   Aion's 1-based event sequence and haematite's 0-based stream `expected_seq`
//!   (the stream's current event count) coincide: both equal the number of
//!   already-stored events, so Aion's `expected_seq` is passed straight through.
//! * **Projections** (status, summaries, run chains) reuse the exact same
//!   `aion-store`/`aion-core` helpers the in-memory and libSQL stores use, so a
//!   workflow projects identically regardless of backend.
//! * **Timers, packages, routes, and the outbox** are keyed KV records in
//!   haematite's general keyspace (see [`keyspace`]). Each mutation is followed
//!   by a [`haematite::Database::commit`] so it is durable before the call
//!   returns. Range scans over a region prefix enumerate the region.
//!
//! # Single-node shard invariant
//!
//! The store creates haematite with `shard_count == 1`. haematite's `range` is
//! shard-local (routed from the lower bound), so a one-shard database is what
//! makes a prefix range scan globally complete — every timer / package / outbox
//! row is in the same shard as the scan's lower bound. Multi-shard support is a
//! later (cluster) increment and is intentionally out of scope for B1.
//!
//! # Outbox = Design B (events are the single source of truth)
//!
//! Outbox rows are their own keyed KV entries. [`append_with_outbox`] writes the
//! events first (the authoritative durable write) and then the outbox rows;
//! [`rearm_outbox_pending`] upserts rows back to `Pending` with a read-modify-write
//! that preserves `attempt`. Because Aion recovery rebuilds the pending-dispatch
//! set from history, a single-node `append_with_outbox` does not require
//! cross-key single-transaction atomicity: a crash between the event commit and
//! the outbox commit is recovered by the re-arm path, never a lost dispatch.
//!
//! [`append_with_outbox`]: HaematiteStore::append_with_outbox
//! [`rearm_outbox_pending`]: HaematiteStore::rearm_outbox_pending

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aion_core::{
    Event, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary, status_from_events,
};
use aion_store::{
    OutboxRow, OutboxStatus, OutboxStore, PackageRecord, PackageRouteRecord, PackageStore,
    ReadableEventStore, RunSummary, StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use haematite::sync::membership::WriteMembership;
use haematite::{Database, DatabaseConfig, DatabaseError};
use serde::{Deserialize, Serialize};

use crate::error::{api_error, database_error, join_error, serde_error};
use crate::keyspace;

/// Quorum-replication routing for a distributed [`HaematiteStore`].
///
/// When present, event appends route through [`Database::replicate_append`] to
/// this `membership` quorum, INSTEAD of the local single-node `append_batch`
/// path. Only event history is replicated — workflows are enumerated from the
/// replicated event streams themselves (see
/// [`keyspace::workflow_id_from_event_stream_key`]), so there is no separate
/// workflow-id index to replicate. The byte-level event/stream-key encoding is
/// identical to the single-node path (haematite's `replicate_append` lays events
/// out exactly as the local `append_batch` does), so `read_history` decodes a
/// replicated stream the same way it decodes a locally-appended one.
#[derive(Clone)]
struct DistributedRouting {
    /// The quorum membership (denominator + reachable send targets) for writes.
    membership: WriteMembership,
    /// Per-operation quorum timeout passed to `replicate_append`.
    timeout: Duration,
}

/// A durable Aion event store backed by haematite.
///
/// Runs in one of two modes:
///
/// * **Single-node** ([`create`](HaematiteStore::create) /
///   [`open`](HaematiteStore::open)): every write is a local haematite commit.
/// * **Distributed** ([`with_distribution`](HaematiteStore::with_distribution)):
///   event appends are quorum-REPLICATED to a cluster membership, so a workflow's
///   durable history survives the owner node's death and is readable (and
///   enumerable) on the survivor after it becomes the shard owner. The outbox
///   stays Design-B local (rebuilt from replicated history on the survivor).
#[derive(Clone)]
pub struct HaematiteStore {
    inner: Arc<haematite::EventStore>,
    /// `Some` in distributed mode; `None` in single-node mode (B1, unchanged).
    distribution: Option<DistributedRouting>,
}

impl std::fmt::Debug for HaematiteStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HaematiteStore").finish_non_exhaustive()
    }
}

impl HaematiteStore {
    /// Create a fresh single-node store rooted at `data_dir`.
    ///
    /// The directory is created if absent. The store runs haematite with a single
    /// shard (see the module docs) and no TTL sweeper or distribution.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when haematite cannot create the database.
    pub fn create(data_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::create_with_shard_count(data_dir, 1)
    }

    /// Create a fresh single-node store with `shard_count` haematite shards.
    ///
    /// `shard_count == 1` is the default [`create`](HaematiteStore::create)
    /// behavior; `> 1` exercises the cross-shard fan-out scan path
    /// ([`scan_prefix`]). Cluster/distribution is unaffected.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when haematite cannot create the database.
    pub fn create_with_shard_count(
        data_dir: impl Into<PathBuf>,
        shard_count: usize,
    ) -> Result<Self, StoreError> {
        let database = Database::create(DatabaseConfig {
            data_dir: data_dir.into(),
            shard_count,
            sweep_interval: None,
            distributed: None,
        })
        .map_err(|error| database_error(&error))?;
        Ok(Self::from_database(database))
    }

    /// Open an existing single-node store rooted at `data_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] when haematite cannot open the database.
    pub fn open(data_dir: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let database = Database::open(data_dir).map_err(|error| database_error(&error))?;
        Ok(Self::from_database(database))
    }

    /// Build a DISTRIBUTED store over an already-distribution-attached `database`.
    ///
    /// The caller is responsible for opening `database` with
    /// [`Database::with_distribution`] and for making this node the live shard
    /// owner (via [`Database::acquire_shard_and_serve`]) before issuing appends —
    /// `replicate_append` draws its commit stamp from the live owner state. Event
    /// appends ([`WritableEventStore::append`] / `append_with_outbox`) route
    /// through `replicate_append` to `membership`'s quorum; reads, timers,
    /// packages, routes, and the outbox stay local (Design B: the survivor
    /// rebuilds the outbox from replicated history, and enumerates workflows from
    /// the replicated event streams).
    ///
    /// `timeout` bounds each quorum write.
    #[must_use]
    pub fn with_distribution(
        database: Database,
        membership: WriteMembership,
        timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(haematite::EventStore::new(database)),
            distribution: Some(DistributedRouting {
                membership,
                timeout,
            }),
        }
    }

    /// Borrow the shared [`haematite::EventStore`] this store routes through.
    ///
    /// Exposes the underlying handle so a distributed deployment can drive the
    /// cluster lifecycle on the SAME `Database` the store writes to —
    /// `event_store().database().acquire_shard_and_serve(..)` to take ownership,
    /// or `Arc::clone(event_store())` for a background inbound-write responder. The
    /// store never elects or serves on its own; that is the deployment's job.
    #[must_use]
    pub fn event_store(&self) -> &Arc<haematite::EventStore> {
        &self.inner
    }

    fn from_database(database: Database) -> Self {
        Self {
            inner: Arc::new(haematite::EventStore::new(database)),
            distribution: None,
        }
    }

    /// Run a blocking haematite closure on the blocking pool, sharing the
    /// `Arc<EventStore>`.
    async fn blocking<F, T>(&self, function: F) -> Result<T, StoreError>
    where
        F: FnOnce(&haematite::EventStore) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || function(&inner))
            .await
            .map_err(|error| join_error(&error))?
    }
}

// --- value encodings for KV-region records ---------------------------------

/// Serialized form of a stored timer (the full [`TimerEntry`]).
fn encode_timer(entry: &TimerEntry) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(entry).map_err(|error| serde_error(&error))
}

fn decode_timer(bytes: &[u8]) -> Result<TimerEntry, StoreError> {
    serde_json::from_slice(bytes).map_err(|error| serde_error(&error))
}

/// On-disk form of a deployed package: deploy order is recovered from
/// `deployed_at`, so it is stored alongside the archive and content fields.
#[derive(Serialize, Deserialize)]
struct StoredPackage {
    workflow_type: String,
    content_hash: String,
    archive: Vec<u8>,
    deployed_at: String,
}

fn encode_package(record: &PackageRecord) -> Result<Vec<u8>, StoreError> {
    let stored = StoredPackage {
        workflow_type: record.workflow_type.clone(),
        content_hash: record.content_hash.clone(),
        archive: record.archive.clone(),
        deployed_at: encode_instant(record.deployed_at),
    };
    serde_json::to_vec(&stored).map_err(|error| serde_error(&error))
}

fn decode_package(bytes: &[u8]) -> Result<PackageRecord, StoreError> {
    let stored: StoredPackage = serde_json::from_slice(bytes).map_err(|error| serde_error(&error))?;
    Ok(PackageRecord {
        workflow_type: stored.workflow_type,
        content_hash: stored.content_hash,
        archive: stored.archive,
        deployed_at: decode_instant(&stored.deployed_at)?,
    })
}

/// On-disk form of one outbox row. Mirrors [`OutboxRow`] with the status token
/// and instant rendered as text, matching the libSQL encoding semantics.
#[derive(Serialize, Deserialize)]
struct StoredOutboxRow {
    dispatch_key: String,
    workflow_id: WorkflowId,
    ordinal: u64,
    activity_type: String,
    input: aion_core::Payload,
    status: String,
    attempt: u32,
    visible_after: String,
}

fn encode_outbox(row: &OutboxRow) -> Result<Vec<u8>, StoreError> {
    let stored = StoredOutboxRow {
        dispatch_key: row.dispatch_key.clone(),
        workflow_id: row.workflow_id.clone(),
        ordinal: row.ordinal,
        activity_type: row.activity_type.clone(),
        input: row.input.clone(),
        status: row.status.as_str().to_owned(),
        attempt: row.attempt,
        visible_after: encode_instant(row.visible_after),
    };
    serde_json::to_vec(&stored).map_err(|error| serde_error(&error))
}

fn decode_outbox(bytes: &[u8]) -> Result<OutboxRow, StoreError> {
    let stored: StoredOutboxRow =
        serde_json::from_slice(bytes).map_err(|error| serde_error(&error))?;
    Ok(OutboxRow {
        dispatch_key: stored.dispatch_key,
        workflow_id: stored.workflow_id,
        ordinal: stored.ordinal,
        activity_type: stored.activity_type,
        input: stored.input,
        status: OutboxStatus::parse_token(&stored.status)?,
        attempt: stored.attempt,
        visible_after: decode_instant(&stored.visible_after)?,
    })
}

fn encode_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_instant(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

fn timer_id_token(timer_id: &TimerId) -> Result<String, StoreError> {
    serde_json::to_string(timer_id).map_err(|error| serde_error(&error))
}

// --- blocking KV helpers (run inside `spawn_blocking`) ----------------------

/// Scan every value under `prefix` and decode it with `decode`.
fn scan_prefix<T, D>(
    store: &haematite::EventStore,
    prefix: &[u8],
    mut decode: D,
) -> Result<Vec<T>, StoreError>
where
    D: FnMut(&[u8], &[u8]) -> Result<T, StoreError>,
{
    let database = store.database();
    // Fan the prefix scan out across EVERY shard and concatenate the per-shard
    // results. `Database::range` is shard-local (routed from the lower bound), so
    // it only returns the keys that happen to live in the lower bound's shard;
    // for a globally-complete enumeration we must visit each shard directly via
    // `range_per_shard`. At `shard_count == 1` this is exactly one
    // `range_per_shard(0, ..)` call, byte-for-byte equivalent to the old
    // `range(..)`. The cross-shard concatenation order is arbitrary, but every
    // caller of `scan_prefix` re-sorts its results, so this is correct.
    let Some(upper) = keyspace::prefix_upper_bound(prefix) else {
        return Ok(Vec::new());
    };
    let mut decoded = Vec::new();
    for shard in 0..database.shard_count() {
        let entries = database
            .range_per_shard(shard, prefix, &upper)
            .map_err(|error| database_error(&error))?;
        for (key, value) in &entries {
            decoded.push(decode(key, value)?);
        }
    }
    Ok(decoded)
}

/// Read and decode all event payloads for `workflow_id`, in sequence order.
fn read_events(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
) -> Result<Vec<Event>, StoreError> {
    read_events_from(store, workflow_id, 0)
}

/// Read and decode event payloads for `workflow_id` with Aion seq `>= from_seq`.
fn read_events_from(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
    from_seq: u64,
) -> Result<Vec<Event>, StoreError> {
    let stream_key = keyspace::event_stream_key(workflow_id);
    // Aion seq is 1-based; haematite read_from takes a 0-based stream offset.
    // Aion seq `s` is stored at haematite stream offset `s - 1`, so the lower
    // bound is `from_seq.saturating_sub(1)`. `from_seq <= 1` reads everything.
    let offset = from_seq.saturating_sub(1);
    let raw = store
        .read_from(&stream_key, offset)
        .map_err(|error| api_error(&error))?;
    let mut events = Vec::with_capacity(raw.len());
    for event in raw {
        let decoded: Event =
            serde_json::from_slice(&event.payload).map_err(|error| serde_error(&error))?;
        events.push(decoded);
    }
    Ok(events)
}

/// Append `events` for `workflow_id` and, when `outbox_rows` is `Some`, the
/// outbox rows. Shared by `append` and `append_with_outbox`.
///
/// When `routing` is `Some` the event batch is quorum-REPLICATED through
/// haematite's `replicate_append` to the configured membership; the outbox rows
/// ALWAYS stay local (Design B). When `routing` is `None` this is the unchanged
/// single-node B1 path. Per-workflow KV records (timers, outbox) co-locate on the
/// workflow's shard by routing on its event-stream key.
fn append_blocking(
    store: &haematite::EventStore,
    workflow_id: &WorkflowId,
    events: &[Event],
    expected_seq: u64,
    outbox_rows: Option<&[OutboxRow]>,
    routing: Option<&DistributedRouting>,
) -> Result<(), StoreError> {
    let has_outbox = outbox_rows.is_some_and(|rows| !rows.is_empty());

    // Enforce the expected-head guard FIRST, before contiguity: a stale append
    // (e.g. expected_seq=0 against a head of 1) must surface as SequenceConflict,
    // matching the libSQL/in-memory stores, even if the supplied events are not
    // contiguous with the caller's stale expectation.
    let head = stream_head(store, workflow_id)?;
    if head != expected_seq {
        return Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: head,
        });
    }

    // Validate Aion's contiguity contract before any write (matches libSQL/in-memory).
    let mut next_seq = expected_seq + 1;
    for event in events {
        if event.seq() != next_seq {
            return Err(StoreError::Backend(format!(
                "event sequence must be contiguous: expected {next_seq}, got {}",
                event.seq()
            )));
        }
        next_seq += 1;
    }

    if events.is_empty() && !has_outbox {
        return Ok(());
    }

    if !events.is_empty() {
        let stream_key = keyspace::event_stream_key(workflow_id);
        let payloads: Vec<Vec<u8>> = events
            .iter()
            .map(|event| serde_json::to_vec(event).map_err(|error| serde_error(&error)))
            .collect::<Result<_, _>>()?;

        if let Some(routing) = routing {
            // DISTRIBUTED: replicate the whole batch to a quorum. The event/stream
            // key + value encoding is byte-identical to the local `append_batch`
            // path (haematite shares the encoding between `append` and
            // `replicate_append`), so `read_history` decodes a replicated stream
            // exactly as it decodes a locally-appended one. Workflows are
            // enumerated from these replicated streams, so there is no separate
            // workflow-id index to replicate.
            replicate_events(store, &stream_key, payloads, expected_seq, routing)?;
        } else {
            // SINGLE-NODE (B1, unchanged): local optimistic-concurrency append.
            // `append_batch` self-commits, and workflows are enumerated from the
            // event streams themselves, so no separate index write/commit is
            // needed here.
            let payload_refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
            match store.append_batch(&stream_key, &payload_refs, expected_seq) {
                Ok(_) => {}
                Err(haematite::ApiError::SequenceConflict(conflict)) => {
                    return Err(StoreError::SequenceConflict {
                        expected: expected_seq,
                        found: conflict.actual,
                    });
                }
                Err(error) => return Err(api_error(&error)),
            }
        }
    }

    if let Some(rows) = outbox_rows {
        insert_outbox_rows(store, rows)?;
    }

    Ok(())
}

/// Run `work` on a FRESH OS thread that has NO entered tokio runtime, returning its
/// result.
///
/// haematite's distribution coordinator (`replicate_append`, `replicate_write`,
/// their quorum waits and catch-up rounds) BLOCKS and explicitly refuses to run
/// from a thread with an entered tokio runtime (`Handle::try_current().is_ok()` →
/// `TransportBlockingFromAsync`). The adapter executes its haematite work inside
/// `tokio::task::spawn_blocking`, whose blocking-pool threads STILL carry the
/// runtime context, so a direct call would be rejected. A brand-new `std::thread`
/// (here via `std::thread::scope`, so it can borrow) carries no runtime context, so
/// the coordinator runs there; it drives the endpoint's OWN internal runtime
/// internally, which is unaffected.
fn run_off_runtime<F, T>(work: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    std::thread::scope(|scope| {
        match scope.spawn(work).join() {
            Ok(value) => value,
            // Propagate a panic from the replication thread unchanged rather than
            // swallowing it into a fabricated value.
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// Replicate one event batch to the routing quorum via `replicate_append`,
/// mapping a quorum sequence-conflict back to Aion's [`StoreError::SequenceConflict`].
fn replicate_events(
    store: &haematite::EventStore,
    stream_key: &[u8],
    payloads: Vec<Vec<u8>>,
    expected_seq: u64,
    routing: &DistributedRouting,
) -> Result<(), StoreError> {
    let database = store.database();
    let result = run_off_runtime(|| {
        database.replicate_append(
            stream_key.to_vec(),
            payloads,
            expected_seq,
            &routing.membership,
            routing.timeout,
        )
    });
    match result {
        Ok(_) => Ok(()),
        Err(DatabaseError::SequenceConflict { actual, .. }) => Err(StoreError::SequenceConflict {
            expected: expected_seq,
            found: actual,
        }),
        Err(error) => Err(database_error(&error)),
    }
}

/// The current stored head (event count) for `workflow_id`.
fn stream_head(store: &haematite::EventStore, workflow_id: &WorkflowId) -> Result<u64, StoreError> {
    let events = read_events(store, workflow_id)?;
    Ok(events.iter().map(Event::seq).max().unwrap_or(0))
}

/// Insert `rows` into the outbox, ignoring any whose `dispatch_key` already
/// exists (at-most-once dispatch). Commits before returning.
fn insert_outbox_rows(
    store: &haematite::EventStore,
    rows: &[OutboxRow],
) -> Result<(), StoreError> {
    let database = store.database();
    for row in rows {
        // Co-locate the outbox row on the workflow's shard by routing on the
        // workflow's event-stream key.
        let route_key = keyspace::event_stream_key(&row.workflow_id);
        let key = keyspace::outbox_key(&row.dispatch_key);
        if database
            .get_routed(&route_key, &key)
            .map_err(|error| database_error(&error))?
            .is_none()
        {
            database
                .put_routed(&route_key, key, encode_outbox(row)?)
                .map_err(|error| database_error(&error))?;
        }
    }
    database.commit().map_err(|error| database_error(&error))?;
    Ok(())
}

#[async_trait]
impl WritableEventStore for HaematiteStore {
    async fn append(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        let workflow_id = workflow_id.clone();
        let events = events.to_vec();
        let routing = self.distribution.clone();
        self.blocking(move |store| {
            append_blocking(store, &workflow_id, &events, expected_seq, None, routing.as_ref())
        })
        .await
    }

    async fn append_with_outbox(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[OutboxRow],
    ) -> Result<(), StoreError> {
        let workflow_id = workflow_id.clone();
        let events = events.to_vec();
        let outbox_rows = outbox_rows.to_vec();
        let routing = self.distribution.clone();
        self.blocking(move |store| {
            append_blocking(
                store,
                &workflow_id,
                &events,
                expected_seq,
                Some(&outbox_rows),
                routing.as_ref(),
            )
        })
        .await
    }

    async fn rearm_outbox_pending(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let rows = rows.to_vec();
        self.blocking(move |store| {
            let database = store.database();
            for row in &rows {
                // Co-locate on the workflow's shard by routing on its event-stream
                // key, matching the original insert.
                let route_key = keyspace::event_stream_key(&row.workflow_id);
                let key = keyspace::outbox_key(&row.dispatch_key);
                // Read-modify-write: preserve the existing `attempt` budget when a
                // row already exists; insert a fresh Pending row otherwise.
                let merged = match database
                    .get_routed(&route_key, &key)
                    .map_err(|error| database_error(&error))?
                {
                    Some(existing) => {
                        let prior = decode_outbox(&existing)?;
                        OutboxRow {
                            status: OutboxStatus::Pending,
                            attempt: prior.attempt,
                            visible_after: row.visible_after,
                            ..prior
                        }
                    }
                    None => OutboxRow {
                        status: OutboxStatus::Pending,
                        ..row.clone()
                    },
                };
                database
                    .put_routed(&route_key, key, encode_outbox(&merged)?)
                    .map_err(|error| database_error(&error))?;
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl OutboxStore for HaematiteStore {
    async fn append_outbox_batch(&self, rows: &[OutboxRow]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let rows = rows.to_vec();
        self.blocking(move |store| insert_outbox_rows(store, &rows)).await
    }

    async fn claim_outbox_rows(&self, limit: u32) -> Result<Vec<OutboxRow>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.blocking(move |store| {
            let now = Utc::now();
            let mut claimable: Vec<OutboxRow> = scan_prefix(
                store,
                keyspace::OUTBOX_PREFIX,
                |_, value| decode_outbox(value),
            )?
            .into_iter()
            .filter(|row| row.status == OutboxStatus::Pending && row.visible_after <= now)
            .collect();
            // Match the libSQL claim order: visible_after ASC, dispatch_key ASC.
            claimable.sort_by(|left, right| {
                left.visible_after
                    .cmp(&right.visible_after)
                    .then_with(|| left.dispatch_key.cmp(&right.dispatch_key))
            });
            let take = usize::try_from(limit).unwrap_or(usize::MAX);
            claimable.truncate(take);

            let database = store.database();
            let mut claimed = Vec::with_capacity(claimable.len());
            for row in claimable {
                let updated = OutboxRow {
                    status: OutboxStatus::Claimed,
                    ..row
                };
                // Rewrite in place on the row's own shard (co-located by the
                // workflow's event-stream key).
                let route_key = keyspace::event_stream_key(&updated.workflow_id);
                database
                    .put_routed(
                        &route_key,
                        keyspace::outbox_key(&updated.dispatch_key),
                        encode_outbox(&updated)?,
                    )
                    .map_err(|error| database_error(&error))?;
                claimed.push(updated);
            }
            database.commit().map_err(|error| database_error(&error))?;
            Ok(claimed)
        })
        .await
    }

    async fn complete_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, |row| OutboxRow {
            status: OutboxStatus::Done,
            ..row
        })
        .await
    }

    async fn retry_outbox_row(
        &self,
        dispatch_key: &str,
        next_attempt: u32,
        visible_after: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, move |row| OutboxRow {
            status: OutboxStatus::Pending,
            attempt: next_attempt,
            visible_after,
            ..row
        })
        .await
    }

    async fn fail_outbox_row(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.transition_outbox(dispatch_key, |row| OutboxRow {
            status: OutboxStatus::Failed,
            ..row
        })
        .await
    }
}

impl HaematiteStore {
    /// Apply `transition` to the row at `dispatch_key`; an absent key is a no-op.
    async fn transition_outbox<F>(
        &self,
        dispatch_key: &str,
        transition: F,
    ) -> Result<(), StoreError>
    where
        F: FnOnce(OutboxRow) -> OutboxRow + Send + 'static,
    {
        let dispatch_key = dispatch_key.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            // The outbox row is co-located on its workflow's shard. The
            // dispatch_key is canonically "{workflow_id}:{ordinal}" and UUIDs
            // contain no ':', so derive the workflow id (and thus the route key)
            // by splitting off the trailing ordinal. A dispatch_key that has no
            // ':' or whose prefix is not a workflow id is a hard error — silently
            // falling back to a non-routed key would split the record from its
            // workflow's shard.
            let (workflow_text, _ordinal) = dispatch_key.rsplit_once(':').ok_or_else(|| {
                StoreError::Backend(format!(
                    "outbox dispatch_key missing ':' separator: {dispatch_key}"
                ))
            })?;
            let workflow_id = parse_workflow_id(workflow_text)?;
            let route_key = keyspace::event_stream_key(&workflow_id);
            let key = keyspace::outbox_key(&dispatch_key);
            let Some(existing) = database
                .get_routed(&route_key, &key)
                .map_err(|error| database_error(&error))?
            else {
                return Ok(());
            };
            let updated = transition(decode_outbox(&existing)?);
            database
                .put_routed(&route_key, key, encode_outbox(&updated)?)
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl ReadableEventStore for HaematiteStore {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| read_events(store, &workflow_id)).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| read_events_from(store, &workflow_id, from_seq)).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        let workflow_id = workflow_id.clone();
        self.blocking(move |store| {
            let history = read_events(store, &workflow_id)?;
            if history.is_empty() {
                return Ok(Vec::new());
            }
            aion_store::run_chain::run_chain_from_history(&history)
        })
        .await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.blocking(|store| {
            let mut ids = workflow_stream_ids(store)?;
            ids.sort_by_key(ToString::to_string);
            Ok(ids)
        })
        .await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.blocking(|store| {
            let workflow_ids = workflow_stream_ids(store)?;
            let mut active = Vec::new();
            for workflow_id in workflow_ids {
                let history = read_events(store, &workflow_id)?;
                if matches!(status_from_events(&history), WorkflowStatus::Running) {
                    active.push(workflow_id);
                }
            }
            active.sort_by_key(ToString::to_string);
            Ok(active)
        })
        .await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        let filter = filter.clone();
        self.blocking(move |store| {
            let workflow_ids = workflow_stream_ids(store)?;
            let mut summaries = Vec::new();
            for workflow_id in workflow_ids {
                let history = read_events(store, &workflow_id)?;
                if let Some(summary) = WorkflowSummary::from_history(&history) {
                    if filter.matches(&summary) {
                        summaries.push(summary);
                    }
                }
            }
            summaries.sort_by(|left, right| {
                left.started_at.cmp(&right.started_at).then_with(|| {
                    left.workflow_id
                        .to_string()
                        .cmp(&right.workflow_id.to_string())
                })
            });
            Ok(summaries)
        })
        .await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let workflow_id = workflow_id.clone();
        let timer_id = timer_id.clone();
        self.blocking(move |store| {
            let token = timer_id_token(&timer_id)?;
            let entry = TimerEntry {
                workflow_id: workflow_id.clone(),
                timer_id,
                fire_at,
            };
            let database = store.database();
            // Co-locate the timer on the workflow's shard by routing on its
            // event-stream key (the same key the event stream routes by).
            let route_key = keyspace::event_stream_key(&workflow_id);
            database
                .put_routed(
                    &route_key,
                    keyspace::timer_key(&workflow_id, &token),
                    encode_timer(&entry)?,
                )
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.blocking(move |store| {
            let mut timers: Vec<TimerEntry> =
                scan_prefix(store, keyspace::TIMER_PREFIX, |_, value| decode_timer(value))?
                    .into_iter()
                    .filter(|entry| entry.fire_at <= as_of)
                    .collect();
            timers.sort_by(|left, right| {
                left.fire_at
                    .cmp(&right.fire_at)
                    .then_with(|| {
                        left.workflow_id
                            .to_string()
                            .cmp(&right.workflow_id.to_string())
                    })
                    .then_with(|| left.timer_id.to_string().cmp(&right.timer_id.to_string()))
            });
            Ok(timers)
        })
        .await
    }
}

#[async_trait]
impl PackageStore for HaematiteStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.blocking(move |store| {
            let database = store.database();
            database
                .put(
                    keyspace::package_key(&record.workflow_type, &record.content_hash),
                    encode_package(&record)?,
                )
                .map_err(|error| database_error(&error))?;
            // put_package re-points the type's route at this version.
            database
                .put(
                    keyspace::route_key(&record.workflow_type),
                    record.content_hash.clone().into_bytes(),
                )
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.blocking(|store| {
            let mut records: Vec<PackageRecord> =
                scan_prefix(store, keyspace::PACKAGE_PREFIX, |_, value| decode_package(value))?;
            records.sort_by(|left, right| {
                left.deployed_at
                    .cmp(&right.deployed_at)
                    .then_with(|| left.workflow_type.cmp(&right.workflow_type))
                    .then_with(|| left.content_hash.cmp(&right.content_hash))
            });
            Ok(records)
        })
        .await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let workflow_type = workflow_type.to_owned();
        let content_hash = content_hash.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            database
                .delete(keyspace::package_key(&workflow_type, &content_hash))
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let workflow_type = workflow_type.to_owned();
        let content_hash = content_hash.to_owned();
        self.blocking(move |store| {
            let database = store.database();
            database
                .put(keyspace::route_key(&workflow_type), content_hash.into_bytes())
                .map_err(|error| database_error(&error))?;
            database.commit().map_err(|error| database_error(&error))?;
            Ok(())
        })
        .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.blocking(|store| {
            let mut routes: Vec<PackageRouteRecord> =
                scan_prefix(store, keyspace::ROUTE_PREFIX, |key, value| {
                    let workflow_type = keyspace::workflow_type_from_route_key(key)
                        .ok_or_else(|| {
                            StoreError::Backend(String::from("malformed package-route key"))
                        })?;
                    let content_hash = String::from_utf8(value.to_vec()).map_err(|error| {
                        StoreError::Serialization(format!("invalid route content hash: {error}"))
                    })?;
                    Ok(PackageRouteRecord {
                        workflow_type,
                        content_hash,
                    })
                })?;
            routes.sort_by(|left, right| left.workflow_type.cmp(&right.workflow_type));
            Ok(routes)
        })
        .await
    }
}

/// Enumerate every workflow id by reading the co-located event streams.
///
/// Each workflow's history lives in one haematite event stream keyed by
/// [`keyspace::event_stream_key`]; `scan_sequence_keys` returns the
/// `(stream_key, next_seq)` pair for every stream across all shards. Mapping each
/// stream key back through [`keyspace::workflow_id_from_event_stream_key`] yields
/// the workflow ids — no separate workflow-id index is kept. Non-`E` keys are
/// skipped defensively (the scan only ever returns event-stream sequence keys, so
/// this is belt-and-braces).
fn workflow_stream_ids(store: &haematite::EventStore) -> Result<Vec<WorkflowId>, StoreError> {
    let streams = store
        .database()
        .scan_sequence_keys()
        .map_err(|error| database_error(&error))?;
    Ok(streams
        .into_iter()
        .filter_map(|(stream_key, _next_seq)| {
            keyspace::workflow_id_from_event_stream_key(&stream_key)
        })
        .collect())
}

fn parse_workflow_id(text: &str) -> Result<WorkflowId, StoreError> {
    uuid::Uuid::parse_str(text)
        .map(WorkflowId::new)
        .map_err(|error| StoreError::Serialization(format!("invalid workflow id index: {error}")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_core::{ContentType, Payload, WorkflowId};
    use aion_store::{
        OutboxRow, OutboxStatus, OutboxStore, ReadableEventStore, WritableEventStore, WriteToken,
    };
    use chrono::{Duration, Utc};

    use super::HaematiteStore;
    use crate::keyspace;

    fn unique_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "aion-store-haematite-unit-{name}-{}-{nanos}-{counter}",
            std::process::id()
        ))
    }

    fn store(name: &str) -> HaematiteStore {
        HaematiteStore::create(unique_dir(name)).expect("create store")
    }

    fn pending_row(
        workflow_id: &WorkflowId,
        ordinal: u64,
        activity_type: &str,
        visible_after: chrono::DateTime<Utc>,
    ) -> OutboxRow {
        OutboxRow::pending(
            workflow_id.clone(),
            ordinal,
            String::from(activity_type),
            Payload::new(ContentType::Json, b"{}".to_vec()),
            visible_after,
        )
    }

    async fn status_of(store: &HaematiteStore, dispatch_key: &str) -> Option<OutboxStatus> {
        // A claim returns the row only when Pending+due, so probe lifecycle by
        // reading the keyed value directly through the public claim/scan surface.
        let key = dispatch_key.to_owned();
        store
            .blocking(move |inner| {
                let bytes = inner
                    .database()
                    .get(&keyspace::outbox_key(&key))
                    .expect("get outbox row");
                Ok(bytes.map(|value| super::decode_outbox(&value).expect("decode").status))
            })
            .await
            .expect("status probe")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn outbox_claim_complete_retry_fail_round_trip() {
        let store = store("outbox-round-trip");
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let row_a = pending_row(&workflow_id, 0, "a", past);
        let row_b = pending_row(&workflow_id, 1, "b", past);

        store
            .append_outbox_batch(&[row_a.clone(), row_b.clone()])
            .await
            .expect("append batch");

        let claimed = store.claim_outbox_rows(10).await.expect("claim");
        assert_eq!(claimed.len(), 2);
        assert!(claimed.iter().all(|row| row.status == OutboxStatus::Claimed));
        // Claim order is visible_after ASC then dispatch_key ASC.
        assert_eq!(claimed[0].ordinal, 0);
        assert_eq!(claimed[1].ordinal, 1);

        // Claimed rows are no longer claimable.
        assert!(store.claim_outbox_rows(10).await.expect("reclaim").is_empty());

        store
            .complete_outbox_row(&row_a.dispatch_key)
            .await
            .expect("complete");
        assert_eq!(
            status_of(&store, &row_a.dispatch_key).await,
            Some(OutboxStatus::Done)
        );

        // Retry into the future: pending but not yet claimable.
        let future = Utc::now() + Duration::hours(1);
        store
            .retry_outbox_row(&row_b.dispatch_key, 1, future)
            .await
            .expect("retry future");
        assert_eq!(
            status_of(&store, &row_b.dispatch_key).await,
            Some(OutboxStatus::Pending)
        );
        assert!(store.claim_outbox_rows(10).await.expect("claim none").is_empty());

        // Retry into the past: claimable again with the bumped attempt.
        store
            .retry_outbox_row(&row_b.dispatch_key, 2, past)
            .await
            .expect("retry past");
        let reclaimed = store.claim_outbox_rows(10).await.expect("reclaim past");
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].dispatch_key, row_b.dispatch_key);
        assert_eq!(reclaimed[0].attempt, 2);

        store
            .fail_outbox_row(&row_b.dispatch_key)
            .await
            .expect("fail");
        assert_eq!(
            status_of(&store, &row_b.dispatch_key).await,
            Some(OutboxStatus::Failed)
        );
        assert!(store.claim_outbox_rows(10).await.expect("no claim").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_outbox_batch_ignores_duplicate_dispatch_key() {
        let store = store("outbox-dup");
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);
        let first = pending_row(&workflow_id, 0, "charge", past);
        let duplicate = pending_row(&workflow_id, 0, "different-activity", past);

        store
            .append_outbox_batch(std::slice::from_ref(&first))
            .await
            .expect("first");
        store
            .append_outbox_batch(&[duplicate])
            .await
            .expect("duplicate ignored");

        let claimed = store.claim_outbox_rows(10).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].activity_type, "charge");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rearm_preserves_attempt_and_inserts_fresh_rows() {
        let store = store("outbox-rearm");
        let workflow_id = WorkflowId::new_v4();
        let past = Utc::now() - Duration::hours(1);

        // Stage one row, drive it claimed then retried to attempt=3 (its budget so far).
        let original = pending_row(&workflow_id, 0, "charge", past);
        store
            .append_outbox_batch(std::slice::from_ref(&original))
            .await
            .expect("append");
        let _ = store.claim_outbox_rows(10).await.expect("claim");
        store
            .retry_outbox_row(&original.dispatch_key, 3, past)
            .await
            .expect("retry to attempt 3");

        // Re-arm the SAME dispatch_key with a FRESH OutboxRow whose attempt is 0:
        // the re-arm must NOT reset the budget — it preserves the stored attempt=3.
        let revived = pending_row(&workflow_id, 0, "charge", Utc::now());
        assert_eq!(revived.attempt, 0);
        let fresh = pending_row(&workflow_id, 1, "settle", Utc::now());
        WritableEventStore::rearm_outbox_pending(&store, &[revived.clone(), fresh.clone()])
            .await
            .expect("rearm");

        let mut reclaimed = store.claim_outbox_rows(10).await.expect("reclaim");
        reclaimed.sort_by_key(|row| row.ordinal);
        assert_eq!(reclaimed.len(), 2);
        // Existing row's attempt budget was preserved across re-arm.
        assert_eq!(reclaimed[0].dispatch_key, revived.dispatch_key);
        assert_eq!(reclaimed[0].attempt, 3);
        // Brand-new dispatch_key inserted as Pending with its own attempt (0).
        assert_eq!(reclaimed[1].dispatch_key, fresh.dispatch_key);
        assert_eq!(reclaimed[1].attempt, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_with_outbox_persists_events_and_rows() {
        let store = store("outbox-atomic");
        let workflow_id = WorkflowId::new_v4();
        let event = aion_core::Event::WorkflowStarted {
            envelope: aion_core::EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("checkout"),
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            run_id: aion_core::RunId::new_v4(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };
        let row = pending_row(&workflow_id, 0, "charge", Utc::now() - Duration::hours(1));

        store
            .append_with_outbox(
                WriteToken::recorder(),
                &workflow_id,
                std::slice::from_ref(&event),
                0,
                std::slice::from_ref(&row),
            )
            .await
            .expect("append_with_outbox");

        assert_eq!(store.read_history(&workflow_id).await.expect("history").len(), 1);
        let claimed = store.claim_outbox_rows(10).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].dispatch_key, row.dispatch_key);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn durable_state_survives_close_and_reopen() {
        use aion_store::{PackageRecord, PackageStore, TimerId};

        let dir = unique_dir("reopen");
        let workflow_id = WorkflowId::new_v4();
        let timer_id = TimerId::anonymous(7);
        let fire_at = Utc::now();

        {
            let store = HaematiteStore::create(&dir).expect("create");
            let event = aion_core::Event::WorkflowStarted {
                envelope: aion_core::EventEnvelope {
                    seq: 1,
                    recorded_at: Utc::now(),
                    workflow_id: workflow_id.clone(),
                },
                workflow_type: String::from("checkout"),
                input: Payload::new(ContentType::Json, b"{}".to_vec()),
                run_id: aion_core::RunId::new_v4(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            };
            store
                .append(WriteToken::recorder(), &workflow_id, &[event], 0)
                .await
                .expect("append");
            store
                .schedule_timer(&workflow_id, &timer_id, fire_at)
                .await
                .expect("timer");
            store
                .put_package(PackageRecord {
                    workflow_type: String::from("checkout"),
                    content_hash: "b".repeat(64),
                    archive: b"archive".to_vec(),
                    deployed_at: Utc::now(),
                })
                .await
                .expect("package");
            // Drop closes the store; haematite committed each write durably.
        }

        let reopened = HaematiteStore::open(&dir).expect("reopen");
        assert_eq!(
            reopened.read_history(&workflow_id).await.expect("history").len(),
            1,
            "event history survives reopen"
        );
        assert_eq!(
            reopened.list_workflow_ids().await.expect("ids"),
            vec![workflow_id.clone()],
            "workflow index survives reopen"
        );
        assert_eq!(
            reopened.list_active().await.expect("active"),
            vec![workflow_id],
            "projected status survives reopen"
        );
        assert_eq!(
            reopened.expired_timers(fire_at).await.expect("timers").len(),
            1,
            "durable timer survives reopen"
        );
        assert_eq!(
            reopened.list_packages().await.expect("packages").len(),
            1,
            "deployed package survives reopen"
        );
        assert_eq!(
            reopened.list_package_routes().await.expect("routes").len(),
            1,
            "package route survives reopen"
        );
    }
}
