//! NOI-5 transcript sequencer + fan-out — the durability-critical server bridge.
//!
//! [`ActivityEventPublisher`] is the aion-server's SEQUENCER for the agent
//! observability transcript. It is the counterpart of [`crate::cluster_publisher`]'s
//! `ClusterEventPublisher` for the workflow-agnostic cluster channel, but with one
//! decisive difference the design calls out explicitly (§5.3): the transcript's
//! `store_seq` is **NOT a process-local `AtomicU64`**. A per-process counter resets
//! on restart/failover, so two survivors would mint colliding, non-monotonic
//! sequences. Instead `store_seq` is **commit-allocated**: the server reads the
//! durable `O`-keyspace head, appends at that `expected_seq`, and on a
//! [`StoreError::SequenceConflict`] **re-reads the advanced head and retries**.
//!
//! This read-head -> append(expected_seq) -> on-conflict-retry loop
//! ([`Self::publish`]) is the ONLY thing that keeps `store_seq` monotonic when two
//! writers race for one `(workflow, activity, attempt)` stream (a dying worker +
//! an adopting worker after failover, or two concurrent publish calls). It is
//! correctness-critical code, not an implementation detail, and is covered by the
//! two mandatory NOI-5 negative controls: concurrent-writer monotonicity and
//! failover dedup.
//!
//! # What this does and does not persist
//!
//! - **Non-ephemeral events** are durably appended to the `O` keyspace and then
//!   fanned out to the live transcript broadcast (with the assigned `store_seq`).
//! - **Ephemeral events** (token deltas) are **WS-forward-only**: fanned out live,
//!   **never** persisted. They carry `store_seq: None` on the wire, forever.
//!
//! # Live fan-out + resume
//!
//! Persisted events are also broadcast on a bounded `broadcast::Sender<ActivityEvent>`
//! so a connected transcript socket tails them live. A reconnecting client resumes
//! by `store_seq`: [`Self::replay_from`] reads the durable `O` tail from the store,
//! and [`Self::subscribe`] attaches the live broadcast suppressing any event at or
//! below the resume cursor (the gap-free splice contract the cluster channel uses).

use std::sync::Arc;

use aion_core::{ActivityEvent, ActivityEventKind, ProgressDetail};
use aion_store::{ActivityRecord, ActivityStreamKey, ObservabilityStore, StoreError};
use futures::stream::{self, BoxStream};
use tokio::sync::broadcast;

use crate::activity_bounds::{TranscriptBounds, bound_event};

/// A lag item on the transcript broadcast: `skipped` events were dropped because
/// the subscriber fell behind the bounded buffer. Surfaced typed to the client
/// (which then re-resumes from the durable `O` tail), never a silent skip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptStreamLagged {
    /// Number of transcript events dropped.
    pub skipped: u64,
}

impl std::fmt::Display for TranscriptStreamLagged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "transcript stream lagged: {} events dropped",
            self.skipped
        )
    }
}

impl std::error::Error for TranscriptStreamLagged {}

/// The durable transcript sequencer + live fan-out for one deployment.
///
/// Cloneable: the broadcast sender and the store handle are shared, so every
/// clone sequences into the same `O` keyspace and fans out to the same live
/// subscribers. The store is the single source of `store_seq` monotonicity; the
/// broadcast is best-effort live tail only.
#[derive(Clone)]
pub struct ActivityEventPublisher {
    store: Arc<dyn ObservabilityStore>,
    live: broadcast::Sender<ActivityEvent>,
    bounds: TranscriptBounds,
}

impl std::fmt::Debug for ActivityEventPublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActivityEventPublisher")
            .field("live_receivers", &self.live.receiver_count())
            .finish_non_exhaustive()
    }
}

/// The maximum number of `SequenceConflict` retries before a single `publish`
/// gives up. In-process publishers serialize per tap (the liminal drain
/// queue), so conflicts only come from genuine cross-process races — failover
/// adoption, a dying worker racing its adopter — where a handful of writers
/// contend. Exceeding this signals a pathological hot loop and must FAIL
/// CHEAP: the 2026-07-23 flood burned a core spinning 1024-retry loops (each
/// failed backend append also leaving orphaned store nodes behind), so the
/// budget is sized to real contention, not to hope.
const MAX_SEQUENCE_CONFLICT_RETRIES: usize = 16;

impl ActivityEventPublisher {
    /// Build a publisher over `store` with a live broadcast of `capacity`.
    ///
    /// `capacity` is the bounded live-tail buffer; a subscriber that lags beyond
    /// it receives one typed [`TranscriptStreamLagged`] then re-resumes from the
    /// durable tail. It must be non-zero (validated by the caller's config).
    #[must_use]
    pub fn new(store: Arc<dyn ObservabilityStore>, capacity: std::num::NonZeroUsize) -> Self {
        let (live, _receiver) = broadcast::channel(capacity.get());
        Self {
            store,
            live,
            bounds: TranscriptBounds::default(),
        }
    }

    /// Replace the default retention bounds with operator-configured ones
    /// (`[observability]` config). Bounds apply to the durable append path
    /// only; ephemeral fan-out is untouched.
    #[must_use]
    pub(crate) fn with_bounds(mut self, bounds: TranscriptBounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Sequence + persist + fan out one event.
    ///
    /// Ephemeral events are fanned out live with `store_seq: None` and are NEVER
    /// persisted. Non-ephemeral events are appended to the `O` keyspace under the
    /// commit-allocated `store_seq` (via the read-head -> `append(expected_seq)` ->
    /// on-conflict-re-read-head-and-retry loop), then fanned out carrying that
    /// `store_seq`. Returns the assigned `store_seq` for a persisted event, or
    /// `None` for an ephemeral one.
    ///
    /// A send with no live subscribers is not an error (the calm no-dashboard
    /// case); the durable append is the primary artifact.
    ///
    /// # Errors
    /// A [`StoreError`] from the durable append (after exhausting the retry
    /// budget on pathological contention, or any non-conflict backend error).
    pub async fn publish(&self, event: &ActivityEvent) -> Result<Option<u64>, StoreError> {
        if event.ephemeral {
            // WS-forward-only: fan out live with no store_seq, never persist.
            let mut ephemeral = event.clone();
            ephemeral.store_seq = None;
            let send_result = self.live.send(ephemeral);
            drop(send_result);
            return Ok(None);
        }

        // Bound the event FIRST so the persisted record, the live fan-out, and
        // every later replay all carry the same bounded shape.
        let event = bound_event(event, self.bounds.max_event_bytes)?;
        let key = ActivityStreamKey::of(&event);
        // Seed the optimistic-concurrency loop from the durable head. On a
        // SequenceConflict a concurrent writer advanced the head between our read
        // and our append, so we re-read the (now advanced) head and retry — this
        // is what keeps store_seq strictly monotonic across racing writers.
        let mut expected_seq = self.store.activity_head(&key).await?;
        for _attempt in 0..MAX_SEQUENCE_CONFLICT_RETRIES {
            // The per-stream retention cap is re-evaluated every iteration: a
            // conflict advances `expected_seq`, which can cross the cap.
            if expected_seq > self.bounds.max_stream_events {
                // Past the cap (the marker at the cap seq is already durable):
                // live streaming continues, persistence stops.
                self.fan_out_live_only(&event);
                return Ok(None);
            }
            if expected_seq == self.bounds.max_stream_events {
                match self.append_cap_marker(&event, expected_seq).await {
                    Ok(()) => {
                        // The marker is durable; the triggering event itself is
                        // live-only, like everything after it.
                        self.fan_out_live_only(&event);
                        return Ok(None);
                    }
                    Err(StoreError::SequenceConflict { found, .. }) => {
                        // A concurrent writer won the cap seq: adopt the head
                        // and re-loop (the cap re-check then routes to drop).
                        expected_seq = found;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            match self.store.append_activity_event(expected_seq, &event).await {
                Ok(store_seq) => {
                    let mut persisted = event.clone();
                    persisted.store_seq = Some(store_seq);
                    let send_result = self.live.send(persisted);
                    drop(send_result);
                    return Ok(Some(store_seq));
                }
                Err(StoreError::SequenceConflict { found, .. }) => {
                    // The durable head advanced past our expectation: adopt the
                    // observed head and retry. `found` is the current head, so we
                    // append there next.
                    expected_seq = found;
                }
                Err(error) => return Err(error),
            }
        }
        Err(StoreError::Backend(format!(
            "observability append exceeded {MAX_SEQUENCE_CONFLICT_RETRIES} sequence-conflict retries for {key:?}"
        )))
    }

    /// Fan one non-ephemeral event out live WITHOUT a `store_seq` (past-cap
    /// delivery: the event is real transcript, just not retained).
    fn fan_out_live_only(&self, event: &ActivityEvent) {
        let mut live_only = event.clone();
        live_only.store_seq = None;
        let send_result = self.live.send(live_only);
        drop(send_result);
    }

    /// Durably append the one retention-cap marker record at `cap_seq` (the
    /// stream's `max_stream_events` position) and fan it out with its
    /// `store_seq`. The marker carries the SAME identity fields as the event
    /// that crossed the cap, so it lands in the same stream and attributes to
    /// the same agent.
    async fn append_cap_marker(
        &self,
        event: &ActivityEvent,
        cap_seq: u64,
    ) -> Result<(), StoreError> {
        let cap = self.bounds.max_stream_events;
        let mut marker = event.clone();
        marker.kind = ActivityEventKind::Progress {
            detail: ProgressDetail::Note {
                text: format!(
                    "transcript retention cap reached ({cap} events); further events are live-only and not persisted"
                ),
            },
        };
        let store_seq = self.store.append_activity_event(cap_seq, &marker).await?;
        marker.store_seq = Some(store_seq);
        let send_result = self.live.send(marker);
        drop(send_result);
        Ok(())
    }

    /// Read the durable `O` tail for `key` with `store_seq >= from_seq`.
    ///
    /// The priming read a resuming transcript client replays before splicing onto
    /// the live stream. `from_seq = 0` replays the whole persisted transcript.
    ///
    /// # Errors
    /// A [`StoreError`] from the durable read.
    pub async fn replay_from(
        &self,
        key: &ActivityStreamKey,
        from_seq: u64,
    ) -> Result<Vec<ActivityRecord>, StoreError> {
        self.store.read_activity_events_from(key, from_seq).await
    }

    /// Enumerate the retained transcript streams of `workflow_id` from the
    /// durable `O` keyspace (empty for a workflow with none — old runs simply
    /// have no retained transcript).
    ///
    /// # Errors
    /// A [`StoreError`] from the durable enumeration.
    pub async fn list_streams(
        &self,
        workflow_id: &aion_core::WorkflowId,
    ) -> Result<Vec<aion_store::ActivityStreamSummary>, StoreError> {
        self.store.list_activity_streams(workflow_id).await
    }

    /// Subscribe to the live transcript tail for `key`, suppressing every event
    /// for a DIFFERENT stream and every persisted event already covered by the
    /// resume cursor.
    ///
    /// The broadcast is deployment-wide (one channel), so this filters to `key`'s
    /// `(workflow, activity, attempt)` stream. `after_seq` dedups the splice seam
    /// exactly like the cluster channel: attach this receiver BEFORE reading the
    /// priming [`Self::replay_from`] tail, so an event that races the priming read
    /// is retained by the receiver and applied after it (deduped on `store_seq`).
    ///
    /// The cursor is an `Option` because `store_seq` is **0-based** (the first
    /// event is `store_seq == 0`): `after_seq = None` is a FRESH subscriber that
    /// has applied nothing and must see every event including `store_seq == 0`;
    /// `after_seq = Some(n)` has already applied through `store_seq == n`, so
    /// events with `store_seq <= n` are suppressed at the seam. Ephemeral events
    /// (which carry `store_seq: None`) for `key` are ALWAYS forwarded live — they
    /// have no sequence to dedup and are never replayed.
    #[must_use]
    pub fn subscribe(
        &self,
        key: ActivityStreamKey,
        after_seq: Option<u64>,
    ) -> BoxStream<'static, Result<ActivityEvent, TranscriptStreamLagged>> {
        let receiver = self.live.subscribe();
        Box::pin(stream::unfold(
            (receiver, key, after_seq),
            |(mut receiver, key, after_seq)| async move {
                loop {
                    match receiver.recv().await {
                        Ok(event) => {
                            if ActivityStreamKey::of(&event) != key {
                                // A different attempt's event on the shared
                                // broadcast: not for this subscriber.
                                continue;
                            }
                            match (event.store_seq, after_seq) {
                                // Already-applied persisted event at the splice
                                // seam: suppress it (fall through to re-loop).
                                (Some(seq), Some(cursor)) if seq <= cursor => {}
                                // A live persisted event past the cursor, a fresh
                                // subscriber (no cursor), or an ephemeral (None)
                                // event: forward it.
                                _ => return Some((Ok(event), (receiver, key, after_seq))),
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            return Some((
                                Err(TranscriptStreamLagged { skipped }),
                                (receiver, key, after_seq),
                            ));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        ))
    }
}

#[cfg(test)]
#[path = "activity_publisher_tests.rs"]
mod tests;
