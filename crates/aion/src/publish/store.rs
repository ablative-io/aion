//! [`PublishingEventStore`]: event-store wrapper that publishes every
//! committed event into a broadcast channel.

use std::num::NonZeroUsize;
use std::sync::Arc;

use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_store::{
    EventStore, PackageRecord, PackageRouteRecord, PackageStore, ReadableEventStore, RunSummary,
    StoreError, TimerEntry, WritableEventStore, WriteToken,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use super::publisher::BroadcastEventPublisher;

/// Maximum broadcast capacity accepted by tokio's broadcast channel.
const MAX_BROADCAST_CAPACITY: usize = usize::MAX / 2;

/// Live event publication setup failure.
#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishError {
    /// The requested broadcast capacity exceeds the channel maximum.
    #[error(
        "event streaming capacity {capacity} exceeds the broadcast channel maximum {MAX_BROADCAST_CAPACITY}"
    )]
    CapacityTooLarge {
        /// Capacity requested by the caller.
        capacity: usize,
    },
}

/// Event-store wrapper that publishes appended events after they commit.
///
/// `append` delegates to the wrapped store and, only when that append
/// succeeds, sends each appended event into a broadcast channel in slice
/// order. Because exactly one `Recorder` writes a given workflow's history
/// and publication strictly follows the commit, the broadcast order per
/// workflow equals its sequence order. Reads and timer operations delegate
/// untouched.
///
/// A send with no live subscribers is not a failure: live subscriptions are a
/// tail, and events committed before a subscriber attaches are observed
/// through history reads, not the broadcast. Once any subscriber has
/// existed, up to `capacity` already-delivered events stay resident in the
/// channel slots until overwritten by later sends.
pub struct PublishingEventStore {
    inner: Arc<dyn EventStore>,
    events: broadcast::Sender<Event>,
}

impl PublishingEventStore {
    /// Wrap `inner` with a broadcast channel holding up to `capacity` events.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::CapacityTooLarge`] when `capacity` exceeds the
    /// broadcast channel maximum of `usize::MAX / 2`.
    pub fn new(inner: Arc<dyn EventStore>, capacity: NonZeroUsize) -> Result<Self, PublishError> {
        if capacity.get() > MAX_BROADCAST_CAPACITY {
            return Err(PublishError::CapacityTooLarge {
                capacity: capacity.get(),
            });
        }
        let (events, initial_receiver) = broadcast::channel(capacity.get());
        drop(initial_receiver);
        Ok(Self { inner, events })
    }

    /// Build the publisher seam wired to this store's broadcast channel.
    #[must_use]
    pub fn publisher(&self) -> BroadcastEventPublisher {
        BroadcastEventPublisher::new(self.events.clone())
    }

    /// Broadcast each committed event into the live-tail channel, in slice order.
    ///
    /// Shared by [`Self::append`] and [`Self::append_with_outbox`] so both the
    /// event-only and durable-outbox commit paths publish identically. A send
    /// with no live subscribers is a non-event (the events are observed through
    /// history reads), matching the `append` contract.
    fn publish_committed(&self, events: &[Event]) {
        for event in events {
            if self.events.receiver_count() == 0 {
                continue;
            }
            let delivery = self.events.send(event.clone());
            drop(delivery);
        }
    }
}

#[async_trait]
impl WritableEventStore for PublishingEventStore {
    /// Append through the wrapped store, then broadcast the committed events.
    ///
    /// Not cancellation-safe: dropping this future between the inner store's
    /// durable commit and the broadcast sends would leave events committed
    /// but never published — a silent gap no lag error reports. No engine
    /// append site wraps this future in a timeout or `select!`; any new
    /// caller must preserve that, or the subscribe-then-snapshot splice
    /// proof (committed ⇒ published after attach) no longer holds.
    async fn append(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        self.inner
            .append(token, workflow_id, events, expected_seq)
            .await?;
        // A subscriber may attach mid-batch; `publish_committed` re-checks the
        // receiver count per event. A broadcast send only errs when no
        // subscriber is attached — a non-event for a live tail, not a swallowed
        // failure.
        self.publish_committed(events);
        Ok(())
    }

    /// Append the atomic durable-outbox batch through the wrapped store, then
    /// broadcast the committed events exactly as [`Self::append`] does.
    ///
    /// Without this override the refusing default [`WritableEventStore::append_with_outbox`]
    /// would reject every fan-out batch routed through the streaming wrapper, so
    /// an `outbox.enabled` engine with event streaming on (the production server
    /// build) could never stage a fan-out member. The same cancellation caveat
    /// as [`Self::append`] applies: dropping this future between the inner
    /// commit and the broadcast would leave events committed but unpublished.
    async fn append_with_outbox(
        &self,
        token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
        outbox_rows: &[aion_store::OutboxRow],
    ) -> Result<(), StoreError> {
        self.inner
            .append_with_outbox(token, workflow_id, events, expected_seq, outbox_rows)
            .await?;
        self.publish_committed(events);
        Ok(())
    }

    /// Forward the crash-recovery outbox re-arm to the wrapped store.
    ///
    /// Re-arm writes no history events, so there is nothing to publish; the
    /// override exists only so the refusing default does not strand a recovered
    /// fan-out member routed through the streaming wrapper.
    async fn rearm_outbox_pending(&self, rows: &[aion_store::OutboxRow]) -> Result<(), StoreError> {
        self.inner.rearm_outbox_pending(rows).await
    }

    /// Forward the fan-out cancellation settle to the wrapped store.
    ///
    /// MUST be forwarded: the trait default is a SILENT `Ok(())` no-op, so
    /// without this override a cancelled fan-out ordinal's outbox row is never
    /// settled on an `outbox.enabled` server — it stays claimable and the
    /// dispatcher re-dispatches the cancelled activity. Same silent-default
    /// forwarding hazard as the per-shard failover seam (#157). Settle writes no
    /// history events, so there is nothing to publish.
    async fn settle_outbox_row_cancelled(&self, dispatch_key: &str) -> Result<(), StoreError> {
        self.inner.settle_outbox_row_cancelled(dispatch_key).await
    }

    /// Forward the workflow-terminal outbox settle (#253) to the wrapped store.
    ///
    /// MUST be forwarded: the trait default is a silent empty-`Ok` no-op, so
    /// without this override a terminal workflow's live outbox rows are never
    /// settled on an `outbox.enabled` server with event streaming on — they stay
    /// claimable and the dispatcher redelivers a dead workflow's activities.
    /// Settle writes no history events, so there is nothing to publish.
    async fn settle_workflow_outbox_rows_cancelled(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<String>, StoreError> {
        self.inner
            .settle_workflow_outbox_rows_cancelled(workflow_id)
            .await
    }
}

#[async_trait]
impl ReadableEventStore for PublishingEventStore {
    /// Forward owned-shard scoping to the inner store; this decorator only
    /// publishes appends and never owns shard policy.
    fn set_owned_shards(&self, shards: Option<&[usize]>) {
        self.inner.set_owned_shards(shards);
    }

    /// Forward the SS-2 shard election to the inner store; this decorator only
    /// publishes appends and never owns shard policy, so the inner backend runs
    /// the election (or no-ops in single-node mode).
    fn acquire_owned_shards(&self, shards: &[usize]) -> Result<(), StoreError> {
        self.inner.acquire_owned_shards(shards)
    }

    /// Forward the per-shard (ADR-021 clean-partial) election to the inner store.
    /// MUST be forwarded: the adoption fence (`Engine::adopt_shards`) drives the
    /// SINGULAR per-shard seam, and the trait default is a silent no-op that would
    /// let a survivor "adopt" a shard WITHOUT winning the election — its in-memory
    /// live epoch is then never seeded, so every recovery write is fenced by the
    /// surviving quorum and cross-node failover stalls (#157).
    fn acquire_owned_shard(&self, shard: usize) -> Result<(), StoreError> {
        self.inner.acquire_owned_shard(shard)
    }

    /// Forward the SS-5 failover scope-widening to the inner store; this
    /// decorator only publishes appends and never owns shard policy.
    fn extend_owned_shards(&self, shards: &[usize]) {
        self.inner.extend_owned_shards(shards);
    }

    /// Forward the residual-window ownership re-assertion (ADR-021). MUST be
    /// forwarded: the trait default returns `true`, which would make the adoption
    /// planner treat a shard it never actually won as a survivor (#157).
    fn is_current_owner(&self, shard: usize) -> bool {
        self.inner.is_current_owner(shard)
    }

    /// Forward the SS-3 shard-owner directory publish (fenced by the election just
    /// won). MUST be forwarded: the trait default is a silent no-op, so a request
    /// reaching a different survivor would mis-resolve to the dead declared owner
    /// instead of this adopter (#157).
    fn publish_shard_owner(&self, shard: usize) -> Result<(), StoreError> {
        self.inner.publish_shard_owner(shard)
    }

    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history(workflow_id).await
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        self.inner.read_history_from(workflow_id, from_seq).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.inner.read_run_chain(workflow_id).await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_workflow_ids().await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_active().await
    }

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.inner.list_paused().await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        self.inner.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.inner.expired_timers(as_of).await
    }
}

#[async_trait]
impl PackageStore for PublishingEventStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        self.inner.put_package(record).await
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        self.inner.list_packages().await
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner.delete_package(workflow_type, content_hash).await
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        self.inner
            .put_package_route(workflow_type, content_hash)
            .await
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        self.inner.list_package_routes().await
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use aion_store::{InMemoryStore, StoreError, WriteToken};
    use futures::StreamExt;
    use serde_json::json;

    use crate::engine::delegated::EventFilter;
    use crate::engine::delegated::EventPublisher;

    use super::*;

    fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
        NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
    }

    fn payload(label: &str) -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    fn started(seq: u64, workflow_id: &WorkflowId) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(seq, workflow_id),
            workflow_type: "checkout".to_owned(),
            input: payload("input")?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn signal(seq: u64, workflow_id: &WorkflowId) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::SignalReceived {
            envelope: envelope(seq, workflow_id),
            name: "approved".to_owned(),
            payload: payload("signal")?,
        })
    }

    fn publishing_store(cap: usize) -> Result<PublishingEventStore, Box<dyn std::error::Error>> {
        let inner: Arc<dyn aion_store::EventStore> = Arc::new(InMemoryStore::default());
        Ok(PublishingEventStore::new(inner, capacity(cap)?)?)
    }

    async fn next_item(
        stream: &mut futures::stream::BoxStream<
            'static,
            Result<Event, crate::engine::delegated::EventStreamLagged>,
        >,
    ) -> Result<
        Result<Event, crate::engine::delegated::EventStreamLagged>,
        Box<dyn std::error::Error>,
    > {
        tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await?
            .ok_or_else(|| "subscription stream ended unexpectedly".into())
    }

    #[tokio::test]
    async fn append_publishes_committed_events_in_seq_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = publishing_store(8)?;
        let workflow_id = WorkflowId::new_v4();
        let mut subscription = store.publisher().subscribe(EventFilter::default());

        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[started(1, &workflow_id)?, signal(2, &workflow_id)?],
                0,
            )
            .await?;
        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[signal(3, &workflow_id)?],
                2,
            )
            .await?;

        for expected_seq in 1..=3 {
            let event = next_item(&mut subscription).await??;
            assert_eq!(event.seq(), expected_seq);
        }
        Ok(())
    }

    #[tokio::test]
    async fn failed_append_publishes_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let store = publishing_store(8)?;
        let workflow_id = WorkflowId::new_v4();
        let mut subscription = store.publisher().subscribe(EventFilter::default());

        let conflict = store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[started(6, &workflow_id)?],
                5,
            )
            .await;
        assert!(matches!(conflict, Err(StoreError::SequenceConflict { .. })));

        // The first delivered event must come from the later successful
        // append, proving the failed batch published nothing.
        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[started(1, &workflow_id)?],
                0,
            )
            .await?;
        let event = next_item(&mut subscription).await??;
        assert_eq!(event.seq(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn reads_delegate_to_inner_store() -> Result<(), Box<dyn std::error::Error>> {
        let inner = Arc::new(InMemoryStore::default());
        let store = PublishingEventStore::new(
            Arc::clone(&inner) as Arc<dyn aion_store::EventStore>,
            capacity(8)?,
        )?;
        let workflow_id = WorkflowId::new_v4();

        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[started(1, &workflow_id)?],
                0,
            )
            .await?;

        let wrapped_history = store.read_history(&workflow_id).await?;
        let inner_history = inner.read_history(&workflow_id).await?;
        assert_eq!(wrapped_history, inner_history);
        assert_eq!(wrapped_history.len(), 1);
        assert_eq!(store.list_active().await?, vec![workflow_id]);
        Ok(())
    }

    #[tokio::test]
    async fn forwards_per_shard_failover_seam_to_inner() -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::testing::ShardSeamSpy;

        let spy = Arc::new(ShardSeamSpy::new());
        let store = PublishingEventStore::new(
            Arc::clone(&spy) as Arc<dyn aion_store::EventStore>,
            capacity(8)?,
        )?;

        // The spy returns sentinels distinct from the trait defaults, so a
        // decorator that silently inherited the no-op defaults would observe
        // `Ok(())` / `true` here instead of the spy's `Err` / `false`.
        assert!(
            store.acquire_owned_shard(0).is_err(),
            "acquire_owned_shard must forward to the spy's NotOwner sentinel, not the Ok(()) default"
        );
        assert!(
            !store.is_current_owner(1),
            "is_current_owner must forward to the spy's false, not the `true` default"
        );
        assert!(
            store.publish_shard_owner(2).is_err(),
            "publish_shard_owner must forward to the spy's NotOwner sentinel, not the Ok(()) default"
        );

        let calls = spy.calls();
        assert!(
            calls.contains(&"acquire_owned_shard:0".to_owned()),
            "spy did not record acquire_owned_shard:0 — call was not forwarded; saw {calls:?}"
        );
        assert!(
            calls.contains(&"is_current_owner:1".to_owned()),
            "spy did not record is_current_owner:1 — call was not forwarded; saw {calls:?}"
        );
        assert!(
            calls.contains(&"publish_shard_owner:2".to_owned()),
            "spy did not record publish_shard_owner:2 — call was not forwarded; saw {calls:?}"
        );

        // The three PLURAL owned-shard seams have no value sentinel (their
        // defaults — and the spy's inner InMemoryStore — are no-op/`Ok(())`), so
        // forwarding is proved by the recorded call alone. These were unguarded
        // before: a dropped forward (e.g. `extend_owned_shards`, which gates which
        // adopted shards recovery enumerates) would regress silently like #157.
        store.set_owned_shards(Some(&[3]));
        assert!(
            store.acquire_owned_shards(&[4]).is_ok(),
            "acquire_owned_shards must forward to the spy's inner Ok(()), not error"
        );
        store.extend_owned_shards(&[5]);

        let calls = spy.calls();
        for expected in [
            "set_owned_shards:Some([3])",
            "acquire_owned_shards:[4]",
            "extend_owned_shards:[5]",
        ] {
            assert!(
                calls.contains(&expected.to_owned()),
                "spy did not record {expected} — call was not forwarded; saw {calls:?}"
            );
        }
        Ok(())
    }

    /// Regression guard (#157 family): the decorator must FORWARD
    /// `settle_outbox_row_cancelled` to its inner store. The trait default is a
    /// silent `Ok(())` no-op, so a dropped forward strands a cancelled fan-out
    /// ordinal's outbox row — it stays claimable and the dispatcher re-dispatches
    /// the cancelled activity. The spy records the call and returns an `Err`
    /// sentinel distinct from that default, so the swallow is caught either way.
    #[tokio::test]
    async fn forwards_outbox_cancel_settle_to_inner() -> Result<(), Box<dyn std::error::Error>> {
        use aion_store::testing::ShardSeamSpy;

        let spy = Arc::new(ShardSeamSpy::new());
        let store = PublishingEventStore::new(
            Arc::clone(&spy) as Arc<dyn aion_store::EventStore>,
            capacity(8)?,
        )?;

        assert!(
            store.settle_outbox_row_cancelled("wf-7").await.is_err(),
            "settle must forward to the spy's Err sentinel, not the silent Ok(()) no-op default"
        );
        let calls = spy.calls();
        assert!(
            calls.contains(&"settle_outbox_row_cancelled:wf-7".to_owned()),
            "spy did not record settle_outbox_row_cancelled — the decorator swallowed it; saw {calls:?}"
        );

        // Same hazard for the workflow-terminal settle (#253): the default is a
        // silent empty-Ok no-op, so a dropped forward would leave a terminal
        // workflow's rows claimable.
        let workflow_id = aion_core::WorkflowId::new_v4();
        assert!(
            store
                .settle_workflow_outbox_rows_cancelled(&workflow_id)
                .await
                .is_err(),
            "workflow settle must forward to the spy's Err sentinel, not the empty-Ok default"
        );
        let calls = spy.calls();
        assert!(
            calls.contains(&format!(
                "settle_workflow_outbox_rows_cancelled:{workflow_id}"
            )),
            "spy did not record settle_workflow_outbox_rows_cancelled — the decorator swallowed \
             it; saw {calls:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn capacity_above_broadcast_maximum_is_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        let inner: Arc<dyn aion_store::EventStore> = Arc::new(InMemoryStore::default());
        let error = PublishingEventStore::new(inner, capacity(usize::MAX)?).err();

        assert_eq!(
            error,
            Some(PublishError::CapacityTooLarge {
                capacity: usize::MAX
            })
        );
        Ok(())
    }
}
