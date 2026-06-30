//! WS3 cluster topology/ownership broadcast publisher.
//!
//! [`ClusterEventPublisher`] is the cluster-channel analog of the workflow
//! [`aion::BroadcastEventPublisher`]: it owns a deployment-global
//! `broadcast::Sender<ClusterEvent>` plus a single monotonic `cluster_seq`
//! stamper (an [`AtomicU64`]). Every cluster state-change site (the supervisor
//! `tick`, the worker registry register/deregister) calls [`Self::emit`] with a
//! *constructor* that receives the freshly-allocated [`ClusterEventMeta`] and
//! returns the fully-formed [`ClusterEvent`]; the publisher fans it out to every
//! live subscriber.
//!
//! # Why a constructor closure, not a pre-built event
//!
//! The `cluster_seq` and `observed_at` must be stamped atomically with the
//! broadcast so two concurrent emitters cannot interleave a higher seq ahead of
//! a lower one on the wire. Taking a `FnOnce(ClusterEventMeta) -> ClusterEvent`
//! lets the publisher allocate the meta under its own monotonic counter and hand
//! it to the caller, who fills in the variant-specific payload. There is no path
//! by which a caller can fabricate a seq.
//!
//! # No timer anywhere
//!
//! This type contains no `tokio::time::interval` and no polling loop. Events are
//! emitted *only* when a real subsystem mutation occurs (edge-triggered). This is
//! the structural guarantee against the polling-as-push regression WS3 exists to
//! remove.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aion_core::{ClusterEvent, ClusterEventMeta};
use futures::stream::{self, BoxStream};
use tokio::sync::broadcast;

/// A lag item on the cluster broadcast: `skipped` deltas were dropped because
/// the subscriber fell behind the bounded buffer. Surfaced to the client as the
/// typed `ClusterLagged` terminal frame, never a silent skip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClusterStreamLagged {
    /// Number of cluster deltas dropped.
    pub skipped: u64,
}

/// Deployment-global cluster-event broadcaster with a monotonic seq stamper.
#[derive(Clone, Debug)]
pub struct ClusterEventPublisher {
    events: broadcast::Sender<ClusterEvent>,
    next_seq: Arc<AtomicU64>,
}

impl ClusterEventPublisher {
    /// Build a publisher over a fresh bounded broadcast channel of `capacity`.
    ///
    /// `capacity` is the operator-configured `websocket.cluster_broadcast_capacity`
    /// (validated non-zero at startup), so this never receives a zero.
    #[must_use]
    pub fn new(capacity: std::num::NonZeroUsize) -> Self {
        let (events, _receiver) = broadcast::channel(capacity.get());
        Self {
            events,
            next_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Stamp and broadcast one cluster event.
    ///
    /// `build` receives the publisher-allocated [`ClusterEventMeta`] (carrying the
    /// next monotonic `cluster_seq` and the observation instant) and returns the
    /// fully-formed event. The return value is the broadcast event (for tests);
    /// a send with no live subscribers is not an error (the calm single-node
    /// case has no dashboard attached).
    pub fn emit<F>(&self, build: F) -> ClusterEvent
    where
        F: FnOnce(ClusterEventMeta) -> ClusterEvent,
    {
        let meta = ClusterEventMeta {
            cluster_seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
            observed_at: chrono::Utc::now(),
        };
        let event = build(meta);
        // A closed channel (no subscribers) is the expected calm-state case, not
        // a failure: the seq still advanced so a later reconnect's gap math is
        // consistent.
        let send_result = self.events.send(event.clone());
        drop(send_result);
        event
    }

    /// Subscribe to the live cluster delta stream, suppressing any delivered
    /// delta with `cluster_seq <= after_seq`.
    ///
    /// `after_seq` dedups the splice seam: the cluster subscription attaches this
    /// receiver BEFORE reading the priming snapshot (gap-free splice), so the
    /// live stream may carry a delta the snapshot already reflects (one with
    /// `cluster_seq <= snapshot.as_of_seq`). Passing `after_seq = as_of_seq`
    /// suppresses exactly those already-applied deltas. Like every tokio
    /// `broadcast` receiver, this sees only events sent AFTER it attaches — there
    /// is no replay of pre-subscription history, which is why a lagged reconnect
    /// re-requests a full snapshot rather than resuming.
    ///
    /// A receiver that falls behind the bounded buffer yields one
    /// `Err(`[`ClusterStreamLagged`]`)` with the skipped count and then closes —
    /// the same lag contract as the workflow path, surfaced typed, never silent.
    #[must_use]
    pub fn subscribe(
        &self,
        after_seq: u64,
    ) -> BoxStream<'static, Result<ClusterEvent, ClusterStreamLagged>> {
        let receiver = self.events.subscribe();
        Box::pin(stream::unfold(
            (receiver, after_seq),
            |(mut receiver, after_seq)| async move {
                loop {
                    match receiver.recv().await {
                        Ok(event) => {
                            if event_seq(&event) > after_seq {
                                return Some((Ok(event), (receiver, after_seq)));
                            }
                            // Buffered backlog already applied by the client:
                            // suppress without surfacing it.
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            return Some((
                                Err(ClusterStreamLagged { skipped }),
                                (receiver, after_seq),
                            ));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        ))
    }

    /// The next `cluster_seq` that will be assigned (i.e. one past the last
    /// stamped). Used by the snapshot path to stamp `as_of_seq` consistently with
    /// the live stream the subscriber spliced onto.
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        self.next_seq.load(Ordering::SeqCst).saturating_sub(1)
    }
}

/// The `cluster_seq` carried by any cluster event's meta.
fn event_seq(event: &ClusterEvent) -> u64 {
    cluster_event_meta(event).cluster_seq
}

/// Borrow the shared meta off any cluster event variant.
#[must_use]
pub fn cluster_event_meta(event: &ClusterEvent) -> &ClusterEventMeta {
    match event {
        ClusterEvent::PeerAdded { meta, .. }
        | ClusterEvent::PeerConnected { meta, .. }
        | ClusterEvent::PeerDisconnected { meta, .. }
        | ClusterEvent::ShardAdopted { meta, .. }
        | ClusterEvent::ShardAdoptionFailed { meta, .. }
        | ClusterEvent::ShardAdoptionSkipped { meta, .. }
        | ClusterEvent::WorkerConnected { meta, .. }
        | ClusterEvent::WorkerDisconnected { meta, .. }
        | ClusterEvent::SupervisorStarted { meta, .. }
        | ClusterEvent::SupervisorStopped { meta, .. }
        | ClusterEvent::NamespaceCreated { meta, .. } => meta,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use aion_core::ClusterEvent;
    use futures::StreamExt;

    use super::*;

    fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
        NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
    }

    fn supervisor_started(meta: ClusterEventMeta) -> ClusterEvent {
        ClusterEvent::SupervisorStarted {
            meta,
            node: "node-1@127.0.0.1".to_owned(),
        }
    }

    #[tokio::test]
    async fn emit_stamps_monotonic_increasing_seq() -> Result<(), Box<dyn std::error::Error>> {
        let publisher = ClusterEventPublisher::new(capacity(8)?);
        let mut subscription = publisher.subscribe(0);

        let first = publisher.emit(supervisor_started);
        let second = publisher.emit(supervisor_started);

        assert_eq!(cluster_event_meta(&first).cluster_seq, 1);
        assert_eq!(cluster_event_meta(&second).cluster_seq, 2);

        let received_first = subscription
            .next()
            .await
            .ok_or("missing first")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        let received_second = subscription
            .next()
            .await
            .ok_or("missing second")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        assert_eq!(cluster_event_meta(&received_first).cluster_seq, 1);
        assert_eq!(cluster_event_meta(&received_second).cluster_seq, 2);
        Ok(())
    }

    #[tokio::test]
    async fn after_seq_suppresses_already_applied_splice_deltas()
    -> Result<(), Box<dyn std::error::Error>> {
        // Models the attach-before-snapshot splice: the receiver is attached
        // first (a broadcast receiver only ever sees events sent AFTER it
        // attaches), then deltas spanning the cursor arrive on the live stream.
        // With after_seq=2 the already-applied seqs 1..=2 are suppressed and the
        // first surfaced delta is seq 3.
        let publisher = ClusterEventPublisher::new(capacity(8)?);
        let mut subscription = publisher.subscribe(2);

        for _ in 0..3 {
            publisher.emit(supervisor_started);
        }

        let survivor = subscription
            .next()
            .await
            .ok_or("missing survivor")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        assert_eq!(
            cluster_event_meta(&survivor).cluster_seq,
            3,
            "deltas at or below after_seq must be suppressed at the splice seam"
        );
        Ok(())
    }

    #[tokio::test]
    async fn lagged_subscriber_yields_typed_skip_count() -> Result<(), Box<dyn std::error::Error>> {
        let publisher = ClusterEventPublisher::new(capacity(2)?);
        let mut subscription = publisher.subscribe(0);

        // Overflow the capacity-2 channel without consuming.
        for _ in 0..5 {
            publisher.emit(supervisor_started);
        }

        let lagged = subscription.next().await.ok_or("missing lag item")?;
        assert_eq!(lagged, Err(ClusterStreamLagged { skipped: 3 }));
        Ok(())
    }

    #[tokio::test]
    async fn emit_with_no_subscribers_is_not_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let publisher = ClusterEventPublisher::new(capacity(2)?);
        // No subscribers: emit must still advance the seq and not panic.
        let event = publisher.emit(supervisor_started);
        assert_eq!(cluster_event_meta(&event).cluster_seq, 1);
        assert_eq!(publisher.current_seq(), 1);
        Ok(())
    }
}
