//! The durable observability (`O`) keyspace contract — NOI-5's durability spine.
//!
//! This module defines the persistence contract for the agent-observability
//! transcript: an **append-only, per-`(workflow, activity, attempt)`** stream of
//! [`aion_core::ActivityEvent`] records that survives kill-9 and failover, is
//! replayable by `store_seq`, and is **never** part of the workflow replay log.
//!
//! # The `O` keyspace is NOT the `E`-stream (LOCKED)
//!
//! Workflow replay authority lives exclusively on the `E`-stream (the
//! [`crate::WritableEventStore`] append path). An [`ActivityRecord`] is an
//! observability record: the replay decoder never scans this keyspace and could
//! not decode one of these records as an `Event` even if it did (different region
//! tag, different schema). The byte-level disjointness is what makes "durable but
//! non-replay-authoritative" a *guarantee*, not a hope — see
//! `aion-store-haematite`'s `observability` module for the `O` (0x4F) region tag
//! and the disjointness test.
//!
//! # Single-writer, server-allocated `store_seq`
//!
//! `store_seq` is **not** allocated by the store: it is a caller-supplied
//! `expected_seq` under optimistic concurrency, exactly like the workflow-history
//! append path. [`ObservabilityStore::append_activity_event`] returns the
//! `SequenceConflict` the server's sequencer re-reads-head-and-retries on. The
//! server is the *single writer* to this keyspace, so monotonicity is enforced by
//! the server's read-head -> append(expected_seq) -> retry loop, not by any magic
//! in the store. This mirrors [`StoreError::SequenceConflict`] on the workflow
//! path and is why the store deliberately does not auto-allocate an id.

use async_trait::async_trait;

use aion_core::{ActivityEvent, ActivityId, WorkflowId};

use crate::StoreError;

/// The durable key of one observability stream: a `(workflow, activity, attempt)`
/// triple. Every [`ActivityRecord`] for one running agent attempt shares this key
/// and is ordered by `store_seq` within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActivityStreamKey {
    /// The workflow the activity belongs to.
    pub workflow_id: WorkflowId,
    /// The activity within the workflow.
    pub activity_id: ActivityId,
    /// The attempt number — the third key axis (NOI-0). Two attempts of one
    /// activity are DISTINCT streams; a within-attempt failover shares one stream
    /// (so a dying + adopting worker's events dedupe), while a retry is a new
    /// attempt and therefore a new stream.
    pub attempt: u32,
}

impl ActivityStreamKey {
    /// Build a stream key from its three components.
    #[must_use]
    pub const fn new(workflow_id: WorkflowId, activity_id: ActivityId, attempt: u32) -> Self {
        Self {
            workflow_id,
            activity_id,
            attempt,
        }
    }

    /// The stream key an [`ActivityEvent`] belongs to.
    #[must_use]
    pub fn of(event: &ActivityEvent) -> Self {
        Self {
            workflow_id: event.workflow_id.clone(),
            activity_id: event.activity_id.clone(),
            attempt: event.attempt,
        }
    }
}

/// A durably persisted observability event: an [`ActivityEvent`] with its
/// server-stamped `store_seq` guaranteed present.
///
/// The wire envelope carries `store_seq: Option<u64>` (`None` until persisted);
/// once read back from the `O` keyspace the sequence is always present, so this
/// record exposes it as a non-optional field alongside the event.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityRecord {
    /// The monotonic, server-allocated sequence assigned at durable commit.
    pub store_seq: u64,
    /// The persisted event. Its `store_seq` field is populated to match
    /// [`Self::store_seq`] so a record read back is self-describing.
    pub event: ActivityEvent,
}

/// One retained transcript stream of a workflow: its key and its head
/// (the number of durably retained records / the next `store_seq`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityStreamSummary {
    /// The stream's `(workflow, activity, attempt)` key.
    pub key: ActivityStreamKey,
    /// Next `store_seq` to be written == count of retained records.
    pub head: u64,
}

/// Durable, append-only observability keyspace contract.
///
/// Implemented by the haematite backend for production and by
/// [`InMemoryObservabilityStore`] for tests + conformance. The server is the
/// single writer; every method keys on the `(workflow, activity, attempt)`
/// triple, never on the workflow alone.
#[async_trait]
pub trait ObservabilityStore: Send + Sync + 'static {
    /// Append `event` to its `(workflow, activity, attempt)` stream at
    /// `expected_seq` (the current head the caller believes it holds).
    ///
    /// On success returns the newly assigned `store_seq` (which equals
    /// `expected_seq`) — the caller advances its head to `store_seq + 1`. On a
    /// stale expectation returns [`StoreError::SequenceConflict`] with the actual
    /// head, leaving the stream unchanged, so the server's sequencer can re-read
    /// the advanced head and retry. **Ephemeral events must never be passed
    /// here** — they are WS-forward-only and are filtered out before this call.
    ///
    /// # Errors
    /// [`StoreError::SequenceConflict`] on a stale `expected_seq`; otherwise a
    /// backend or serialization error.
    async fn append_activity_event(
        &self,
        expected_seq: u64,
        event: &ActivityEvent,
    ) -> Result<u64, StoreError>;

    /// Read the current head (next `store_seq` to be written) for `key`.
    ///
    /// An unwritten stream reads head `0`. The server's sequencer seeds its
    /// retry loop from this value.
    ///
    /// # Errors
    /// A backend or serialization error.
    async fn activity_head(&self, key: &ActivityStreamKey) -> Result<u64, StoreError>;

    /// Read every record for `key` with `store_seq >= from_seq`, in order.
    ///
    /// This is the resume-by-`store_seq` primitive: a reconnecting transcript
    /// client replays from its last-seen cursor without paying for the whole
    /// stream. An unwritten stream (or a `from_seq` beyond the head) reads empty.
    ///
    /// # Errors
    /// A backend or serialization error.
    async fn read_activity_events_from(
        &self,
        key: &ActivityStreamKey,
        from_seq: u64,
    ) -> Result<Vec<ActivityRecord>, StoreError>;

    /// Enumerate every retained transcript stream of `workflow_id`, ordered by
    /// `(activity_id, attempt)` ascending. A workflow with no retained
    /// transcript reads empty (old runs simply have none).
    ///
    /// # Errors
    /// A backend or serialization error.
    async fn list_activity_streams(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<ActivityStreamSummary>, StoreError>;
}

/// An in-memory [`ObservabilityStore`] reference implementation for tests.
///
/// Enforces the SAME optimistic-concurrency contract the haematite backend does:
/// an append with a stale `expected_seq` returns [`StoreError::SequenceConflict`]
/// and writes nothing, so the server's retry loop can be exercised without a real
/// database. A `std::sync::Mutex` serializes the read-compare-write so two racing
/// appends on one stream cannot both win — the same single-shard-actor guarantee
/// the haematite backend gives.
#[derive(Debug, Default)]
pub struct InMemoryObservabilityStore {
    streams:
        std::sync::Mutex<std::collections::HashMap<ActivityStreamKeyBytes, Vec<ActivityRecord>>>,
}

/// A hashable, owned encoding of [`ActivityStreamKey`] for the in-memory map.
type ActivityStreamKeyBytes = (uuid::Uuid, u64, u32);

fn key_bytes(key: &ActivityStreamKey) -> ActivityStreamKeyBytes {
    (
        key.workflow_id.as_uuid(),
        key.activity_id.sequence_position(),
        key.attempt,
    )
}

/// The next `store_seq` for an in-memory stream = its record count.
///
/// A `Vec` length that does not fit in `u64` is unrepresentable on any supported
/// target (a 64-bit `usize` maxes at `u64::MAX`), so the saturating conversion is
/// exact in practice; it is written as a fallible convert to satisfy the
/// deny-level pedantic cast lints without an `as` cast.
fn stream_head(stream: &[ActivityRecord]) -> u64 {
    u64::try_from(stream.len()).unwrap_or(u64::MAX)
}

#[async_trait]
impl ObservabilityStore for InMemoryObservabilityStore {
    async fn append_activity_event(
        &self,
        expected_seq: u64,
        event: &ActivityEvent,
    ) -> Result<u64, StoreError> {
        let key = ActivityStreamKey::of(event);
        let mut streams = self.streams.lock().map_err(|error| {
            StoreError::Backend(format!("observability mutex poisoned: {error}"))
        })?;
        let stream = streams.entry(key_bytes(&key)).or_default();
        let head = stream_head(stream);
        if head != expected_seq {
            return Err(StoreError::SequenceConflict {
                expected: expected_seq,
                found: head,
            });
        }
        let mut event = event.clone();
        event.store_seq = Some(head);
        stream.push(ActivityRecord {
            store_seq: head,
            event,
        });
        Ok(head)
    }

    async fn activity_head(&self, key: &ActivityStreamKey) -> Result<u64, StoreError> {
        let streams = self.streams.lock().map_err(|error| {
            StoreError::Backend(format!("observability mutex poisoned: {error}"))
        })?;
        Ok(streams
            .get(&key_bytes(key))
            .map_or(0, |stream| stream_head(stream)))
    }

    async fn read_activity_events_from(
        &self,
        key: &ActivityStreamKey,
        from_seq: u64,
    ) -> Result<Vec<ActivityRecord>, StoreError> {
        let streams = self.streams.lock().map_err(|error| {
            StoreError::Backend(format!("observability mutex poisoned: {error}"))
        })?;
        Ok(streams
            .get(&key_bytes(key))
            .map_or_else(Vec::new, |stream| {
                stream
                    .iter()
                    .filter(|record| record.store_seq >= from_seq)
                    .cloned()
                    .collect()
            }))
    }

    async fn list_activity_streams(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<ActivityStreamSummary>, StoreError> {
        let streams = self.streams.lock().map_err(|error| {
            StoreError::Backend(format!("observability mutex poisoned: {error}"))
        })?;
        let mut summaries: Vec<ActivityStreamSummary> = streams
            .iter()
            .filter(|((workflow, _activity, _attempt), _records)| {
                *workflow == workflow_id.as_uuid()
            })
            .map(
                |(&(workflow, activity_seq, attempt), records)| ActivityStreamSummary {
                    key: ActivityStreamKey::new(
                        WorkflowId::new(workflow),
                        ActivityId::from_sequence_position(activity_seq),
                        attempt,
                    ),
                    head: stream_head(records),
                },
            )
            .collect();
        summaries.sort_by_key(|summary| {
            (
                summary.key.activity_id.sequence_position(),
                summary.key.attempt,
            )
        });
        Ok(summaries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_core::{ActivityEventKind, MessageRole};
    use chrono::Utc;
    use uuid::Uuid;

    fn event(attempt: u32, worker_seq: u64, text: &str) -> ActivityEvent {
        ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::from_u128(1)),
            activity_id: ActivityId::from_sequence_position(3),
            attempt,
            agent_id: Uuid::from_u128(9),
            agent_role: "orchestrator".to_owned(),
            emitted_at: Utc::now(),
            worker_seq,
            store_seq: None,
            ephemeral: false,
            kind: ActivityEventKind::Message {
                role: MessageRole::Assistant,
                text: text.to_owned(),
            },
        }
    }

    #[tokio::test]
    async fn append_assigns_contiguous_store_seq_from_zero() -> Result<(), StoreError> {
        let store = InMemoryObservabilityStore::default();
        let key = ActivityStreamKey::new(
            WorkflowId::new(Uuid::from_u128(1)),
            ActivityId::from_sequence_position(3),
            0,
        );
        assert_eq!(store.activity_head(&key).await?, 0);
        assert_eq!(store.append_activity_event(0, &event(0, 1, "a")).await?, 0);
        assert_eq!(store.append_activity_event(1, &event(0, 2, "b")).await?, 1);
        assert_eq!(store.activity_head(&key).await?, 2);
        let records = store.read_activity_events_from(&key, 0).await?;
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].store_seq, 0);
        assert_eq!(records[0].event.store_seq, Some(0));
        assert_eq!(records[1].store_seq, 1);
        Ok(())
    }

    #[tokio::test]
    async fn stale_expected_seq_conflicts_and_writes_nothing() -> Result<(), StoreError> {
        let store = InMemoryObservabilityStore::default();
        store.append_activity_event(0, &event(0, 1, "a")).await?;
        // Re-appending at the already-consumed seq 0 conflicts against head 1.
        let conflict = store.append_activity_event(0, &event(0, 2, "dup")).await;
        assert_eq!(
            conflict,
            Err(StoreError::SequenceConflict {
                expected: 0,
                found: 1
            })
        );
        let key = ActivityStreamKey::of(&event(0, 0, ""));
        // Nothing partial was written: still exactly one record.
        assert_eq!(store.read_activity_events_from(&key, 0).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn attempts_are_disjoint_streams() -> Result<(), StoreError> {
        let store = InMemoryObservabilityStore::default();
        store
            .append_activity_event(0, &event(0, 1, "attempt-0"))
            .await?;
        // A different attempt is a fresh stream with its own head at 0.
        store
            .append_activity_event(0, &event(1, 1, "attempt-1"))
            .await?;
        let key0 = ActivityStreamKey::new(
            WorkflowId::new(Uuid::from_u128(1)),
            ActivityId::from_sequence_position(3),
            0,
        );
        let key1 = ActivityStreamKey::new(
            WorkflowId::new(Uuid::from_u128(1)),
            ActivityId::from_sequence_position(3),
            1,
        );
        assert_eq!(store.activity_head(&key0).await?, 1);
        assert_eq!(store.activity_head(&key1).await?, 1);
        Ok(())
    }

    /// Two activities x two attempts of wf-1 plus one stream of wf-2: listing
    /// wf-1 yields exactly its three streams, ordered by `(activity, attempt)`
    /// ascending, each with the correct head.
    #[tokio::test]
    async fn list_activity_streams_orders_by_activity_then_attempt() -> Result<(), StoreError> {
        let store = InMemoryObservabilityStore::default();
        let event_for = |activity_seq: u64, attempt: u32, workflow: u128| {
            let mut event = event(attempt, 1, "x");
            event.workflow_id = WorkflowId::new(Uuid::from_u128(workflow));
            event.activity_id = ActivityId::from_sequence_position(activity_seq);
            event
        };
        // wf-1: activity 3 attempt 0 (two records), activity 3 attempt 1 (one),
        // activity 5 attempt 0 (one). Inserted deliberately out of order.
        store.append_activity_event(0, &event_for(5, 0, 1)).await?;
        store.append_activity_event(0, &event_for(3, 1, 1)).await?;
        store.append_activity_event(0, &event_for(3, 0, 1)).await?;
        store.append_activity_event(1, &event_for(3, 0, 1)).await?;
        // wf-2: one stream that must not leak into wf-1's enumeration.
        store.append_activity_event(0, &event_for(3, 0, 2)).await?;

        let summaries = store
            .list_activity_streams(&WorkflowId::new(Uuid::from_u128(1)))
            .await?;
        let listed: Vec<(u64, u32, u64)> = summaries
            .iter()
            .map(|summary| {
                (
                    summary.key.activity_id.sequence_position(),
                    summary.key.attempt,
                    summary.head,
                )
            })
            .collect();
        assert_eq!(listed, vec![(3, 0, 2), (3, 1, 1), (5, 0, 1)]);
        Ok(())
    }

    #[tokio::test]
    async fn list_activity_streams_is_empty_for_unknown_workflow() -> Result<(), StoreError> {
        let store = InMemoryObservabilityStore::default();
        store.append_activity_event(0, &event(0, 1, "a")).await?;
        let summaries = store
            .list_activity_streams(&WorkflowId::new(Uuid::from_u128(99)))
            .await?;
        assert!(summaries.is_empty(), "an unwritten workflow lists empty");
        Ok(())
    }

    #[tokio::test]
    async fn read_from_resumes_by_store_seq() -> Result<(), StoreError> {
        let store = InMemoryObservabilityStore::default();
        for seq in 0..5u64 {
            store
                .append_activity_event(seq, &event(0, seq, "x"))
                .await?;
        }
        let key = ActivityStreamKey::of(&event(0, 0, ""));
        let tail = store.read_activity_events_from(&key, 3).await?;
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].store_seq, 3);
        assert_eq!(tail[1].store_seq, 4);
        Ok(())
    }
}
