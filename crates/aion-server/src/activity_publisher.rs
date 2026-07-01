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

use aion_core::ActivityEvent;
use aion_store::{ActivityRecord, ActivityStreamKey, ObservabilityStore, StoreError};
use futures::stream::{self, BoxStream};
use tokio::sync::broadcast;

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
/// gives up. Under real single-deployment contention only a handful of writers
/// ever race one stream, so the bound is generous; exceeding it signals a
/// pathological hot loop rather than normal contention and is surfaced as an
/// error rather than spun on forever.
const MAX_SEQUENCE_CONFLICT_RETRIES: usize = 1024;

impl ActivityEventPublisher {
    /// Build a publisher over `store` with a live broadcast of `capacity`.
    ///
    /// `capacity` is the bounded live-tail buffer; a subscriber that lags beyond
    /// it receives one typed [`TranscriptStreamLagged`] then re-resumes from the
    /// durable tail. It must be non-zero (validated by the caller's config).
    #[must_use]
    pub fn new(store: Arc<dyn ObservabilityStore>, capacity: std::num::NonZeroUsize) -> Self {
        let (live, _receiver) = broadcast::channel(capacity.get());
        Self { store, live }
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

        let key = ActivityStreamKey::of(event);
        // Seed the optimistic-concurrency loop from the durable head. On a
        // SequenceConflict a concurrent writer advanced the head between our read
        // and our append, so we re-read the (now advanced) head and retry — this
        // is what keeps store_seq strictly monotonic across racing writers.
        let mut expected_seq = self.store.activity_head(&key).await?;
        for _attempt in 0..MAX_SEQUENCE_CONFLICT_RETRIES {
            match self.store.append_activity_event(expected_seq, event).await {
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
mod tests {
    use std::num::NonZeroUsize;

    use aion_core::{ActivityEventKind, ActivityId, MessageRole, WorkflowId};
    use aion_store::InMemoryObservabilityStore;
    use chrono::Utc;
    use futures::StreamExt;
    use uuid::Uuid;

    use super::*;

    fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
        NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
    }

    fn publisher(cap: usize) -> Result<ActivityEventPublisher, Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryObservabilityStore::default());
        Ok(ActivityEventPublisher::new(store, capacity(cap)?))
    }

    fn event(attempt: u32, worker_seq: u64, ephemeral: bool, text: &str) -> ActivityEvent {
        ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::from_u128(1)),
            activity_id: ActivityId::from_sequence_position(3),
            attempt,
            agent_id: Uuid::from_u128(9),
            agent_role: "orchestrator".to_owned(),
            emitted_at: Utc::now(),
            worker_seq,
            store_seq: None,
            ephemeral,
            kind: if ephemeral {
                ActivityEventKind::Delta {
                    message_id: "m1".to_owned(),
                    text_fragment: text.to_owned(),
                }
            } else {
                ActivityEventKind::Message {
                    role: MessageRole::Assistant,
                    text: text.to_owned(),
                }
            },
        }
    }

    fn key(attempt: u32) -> ActivityStreamKey {
        ActivityStreamKey::new(
            WorkflowId::new(Uuid::from_u128(1)),
            ActivityId::from_sequence_position(3),
            attempt,
        )
    }

    #[tokio::test]
    async fn publish_assigns_commit_allocated_monotonic_store_seq()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = publisher(16)?;
        assert_eq!(publisher.publish(&event(0, 1, false, "a")).await?, Some(0));
        assert_eq!(publisher.publish(&event(0, 2, false, "b")).await?, Some(1));
        assert_eq!(publisher.publish(&event(0, 3, false, "c")).await?, Some(2));
        let tail = publisher.replay_from(&key(0), 0).await?;
        assert_eq!(
            tail.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        Ok(())
    }

    #[tokio::test]
    async fn ephemeral_events_are_never_persisted() -> Result<(), Box<dyn std::error::Error>> {
        let publisher = publisher(16)?;
        // A live subscriber (fresh, no resume cursor) must still SEE the ephemeral
        // delta AND the persisted message at store_seq 0.
        let mut live = publisher.subscribe(key(0), None);
        assert_eq!(publisher.publish(&event(0, 1, true, "wor")).await?, None);
        assert_eq!(
            publisher.publish(&event(0, 2, false, "word")).await?,
            Some(0)
        );
        // Durable tail has ONLY the non-ephemeral message.
        let tail = publisher.replay_from(&key(0), 0).await?;
        assert_eq!(tail.len(), 1);
        assert!(matches!(
            tail[0].event.kind,
            ActivityEventKind::Message { .. }
        ));

        // Live stream delivered the ephemeral delta first (store_seq None), then
        // the persisted message (store_seq Some(0)).
        let first = live.next().await.ok_or("missing ephemeral")??;
        assert!(first.ephemeral);
        assert_eq!(first.store_seq, None);
        let second = live.next().await.ok_or("missing message")??;
        assert!(!second.ephemeral);
        assert_eq!(second.store_seq, Some(0));
        Ok(())
    }

    /// MANDATORY NEGATIVE CONTROL (a): concurrent writers on ONE
    /// `(wf,act,attempt)` stream. Many `publish` calls race the same head; the
    /// read-head -> append -> on-conflict-retry loop must serialize them so the
    /// final durable sequence is strictly monotonic 0..N-1 with NO gap and NO
    /// duplicate — proving the retry loop (not the store) enforces monotonicity.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_writers_produce_gapless_monotonic_store_seq()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = publisher(256)?;
        let writers = 32u64;
        let mut handles = Vec::new();
        for worker_seq in 0..writers {
            let publisher = publisher.clone();
            handles.push(tokio::spawn(async move {
                publisher
                    .publish(&event(0, worker_seq, false, "concurrent"))
                    .await
            }));
        }
        let mut assigned = Vec::new();
        for handle in handles {
            if let Some(store_seq) = handle.await?? {
                assigned.push(store_seq);
            }
        }
        assigned.sort_unstable();
        // Every writer won exactly one distinct, contiguous store_seq.
        assert_eq!(
            assigned,
            (0..writers).collect::<Vec<_>>(),
            "concurrent writers must produce a gapless, duplicate-free monotonic sequence"
        );
        // The durable tail agrees: exactly `writers` records, seqs 0..N-1 in order.
        let tail = publisher.replay_from(&key(0), 0).await?;
        assert_eq!(
            tail.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
            (0..writers).collect::<Vec<_>>()
        );
        Ok(())
    }

    /// MANDATORY NEGATIVE CONTROL (b): failover dedup. A dying worker and an
    /// adopting worker BOTH emit for one `(wf,act,attempt)` (the same session
    /// resumes across failover). Modeled as two publishers sharing ONE store
    /// (the server owns durability, §5.3) racing appends: the commit-allocated
    /// `store_seq` collapses both emitters into one monotonic stream with no
    /// gap/dup — the transcript is not duplicated or reordered by the double-emit.
    #[tokio::test(flavor = "multi_thread")]
    async fn failover_double_emit_dedupes_to_one_monotonic_stream()
    -> Result<(), Box<dyn std::error::Error>> {
        // ONE durable store (the server), TWO publisher clones = the dying +
        // adopting worker's events both flowing through the single server writer.
        let store = Arc::new(InMemoryObservabilityStore::default());
        let dying = ActivityEventPublisher::new(store.clone(), capacity(256)?);
        let adopting = ActivityEventPublisher::new(store, capacity(256)?);

        let mut handles = Vec::new();
        for worker_seq in 0..16u64 {
            let dying = dying.clone();
            let adopting = adopting.clone();
            handles.push(tokio::spawn(async move {
                // Both survivors emit the SAME logical event for this attempt.
                let a = dying.publish(&event(0, worker_seq, false, "dying")).await;
                let b = adopting
                    .publish(&event(0, worker_seq, false, "adopting"))
                    .await;
                (a, b)
            }));
        }
        let mut assigned = Vec::new();
        for handle in handles {
            let (a, b) = handle.await?;
            if let Some(seq) = a? {
                assigned.push(seq);
            }
            if let Some(seq) = b? {
                assigned.push(seq);
            }
        }
        assigned.sort_unstable();
        // 16 events x 2 emitters = 32 durable records, each with a DISTINCT,
        // contiguous store_seq — the double-emit does not corrupt monotonicity.
        assert_eq!(
            assigned,
            (0..32).collect::<Vec<_>>(),
            "failover double-emit must land a gapless monotonic store_seq stream"
        );
        let tail = dying.replay_from(&key(0), 0).await?;
        let seqs: Vec<u64> = tail.iter().map(|r| r.store_seq).collect();
        assert_eq!(seqs, (0..32).collect::<Vec<_>>());
        Ok(())
    }

    /// MANDATORY NEGATIVE CONTROL: the WRONG-allocator case. A process-local
    /// `AtomicU64` (the `ClusterEventPublisher` pattern the design forbids for
    /// `store_seq`) is shown to produce COLLIDING sequences when two "survivors"
    /// each start their own counter after a failover — proving the counter
    /// belongs in the commit, not the process. The commit-allocated publisher
    /// (above) does NOT exhibit this; this test pins the failure mode we avoid.
    #[tokio::test]
    async fn process_local_atomic_counter_collides_across_survivors()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Two survivors, each with its OWN fresh process-local counter (exactly
        // what a per-process AtomicU64 does after restart/failover).
        let dying = AtomicU64::new(0);
        let adopting = AtomicU64::new(0);
        let dying_seq = dying.fetch_add(1, Ordering::SeqCst);
        let adopting_seq = adopting.fetch_add(1, Ordering::SeqCst);
        // Both minted the SAME sequence — a collision. This is the bug the
        // commit-allocated design exists to prevent.
        assert_eq!(
            dying_seq, adopting_seq,
            "process-local counters collide across survivors (the forbidden pattern)"
        );

        // By contrast the commit-allocated publisher over a shared store never
        // collides: the two survivors get DISTINCT sequences.
        let store = Arc::new(InMemoryObservabilityStore::default());
        let a = ActivityEventPublisher::new(store.clone(), capacity(8)?);
        let b = ActivityEventPublisher::new(store, capacity(8)?);
        let sa = a.publish(&event(0, 1, false, "a")).await?;
        let sb = b.publish(&event(0, 2, false, "b")).await?;
        assert_ne!(
            sa, sb,
            "commit-allocated store_seq must be distinct across survivors"
        );
        assert_eq!((sa, sb), (Some(0), Some(1)));
        Ok(())
    }

    /// The live-stream + resume-by-`store_seq` gate: a subscriber tails live
    /// events, and a reconnecting client resumes from the durable tail by
    /// `store_seq` then splices onto the live broadcast with NO gap and NO
    /// duplicate at the seam.
    #[tokio::test]
    async fn live_stream_then_resume_by_store_seq_has_no_gap()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = publisher(64)?;
        // Persist a few events (as if the client was connected then dropped).
        for seq in 0..3u64 {
            publisher.publish(&event(0, seq, false, "early")).await?;
        }
        // Client reconnects having last seen store_seq 1. Splice: attach the live
        // stream BEFORE reading the durable tail (gap-free splice), with the
        // resume cursor as after_seq so live re-delivery of <=cursor is suppressed.
        let mut live = publisher.subscribe(key(0), Some(1));
        let replay = publisher.replay_from(&key(0), 2).await?;
        // The durable tail from the cursor is exactly seq 2 (the one it missed).
        assert_eq!(
            replay.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
            vec![2]
        );
        // Now new live events arrive.
        publisher.publish(&event(0, 10, false, "live-3")).await?;
        publisher.publish(&event(0, 11, false, "live-4")).await?;
        // The live splice yields seq 3 then 4 — no gap, no re-delivery of <=1.
        let first = live.next().await.ok_or("missing live-3")??;
        assert_eq!(first.store_seq, Some(3));
        let second = live.next().await.ok_or("missing live-4")??;
        assert_eq!(second.store_seq, Some(4));
        Ok(())
    }

    #[tokio::test]
    async fn subscribe_filters_out_other_attempt_streams() -> Result<(), Box<dyn std::error::Error>>
    {
        let publisher = publisher(64)?;
        let mut live = publisher.subscribe(key(0), None);
        // An event for a DIFFERENT attempt must not reach this subscriber.
        publisher
            .publish(&event(1, 1, false, "other-attempt"))
            .await?;
        publisher.publish(&event(0, 1, false, "mine")).await?;
        let received = live.next().await.ok_or("missing my event")??;
        assert_eq!(received.attempt, 0);
        assert!(matches!(
            received.kind,
            ActivityEventKind::Message { text, .. } if text == "mine"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn lagged_subscriber_yields_typed_skip_count() -> Result<(), Box<dyn std::error::Error>> {
        let publisher = publisher(2)?;
        let mut live = publisher.subscribe(key(0), None);
        // Overflow the capacity-2 live buffer without consuming.
        for seq in 0..5u64 {
            publisher.publish(&event(0, seq, false, "flood")).await?;
        }
        let lagged = live.next().await.ok_or("missing lag item")?;
        assert!(matches!(lagged, Err(TranscriptStreamLagged { .. })));
        Ok(())
    }
}
