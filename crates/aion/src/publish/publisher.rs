//! [`BroadcastEventPublisher`]: the broadcast-backed [`EventPublisher`].

use aion_core::Event;
use futures::stream::{self, BoxStream};
use tokio::sync::broadcast;

use crate::engine::delegated::{EventFilter, EventPublisher, EventStreamLagged};

/// Live event publisher over the publishing store's broadcast channel.
///
/// Each subscription is a broadcast receiver attached at subscribe time and
/// filtered by [`EventFilter::matches`]. A receiver that falls behind the
/// channel capacity yields one `Err(`[`EventStreamLagged`]`)` item with the
/// skipped count and then continues with the events still buffered — lag is
/// always surfaced, never a silent skip or a silent stream end.
#[derive(Clone, Debug)]
pub struct BroadcastEventPublisher {
    events: broadcast::Sender<Event>,
}

impl BroadcastEventPublisher {
    /// Wrap the broadcast sender shared with [`super::PublishingEventStore`].
    pub(crate) const fn new(events: broadcast::Sender<Event>) -> Self {
        Self { events }
    }
}

impl EventPublisher for BroadcastEventPublisher {
    fn subscribe(
        &self,
        filter: EventFilter,
    ) -> BoxStream<'static, Result<Event, EventStreamLagged>> {
        let receiver = self.events.subscribe();
        Box::pin(stream::unfold(
            (receiver, filter),
            |(mut receiver, filter)| async move {
                loop {
                    match receiver.recv().await {
                        Ok(event) => {
                            if filter.matches(&event) {
                                return Some((Ok(event), (receiver, filter)));
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            return Some((Err(EventStreamLagged { skipped }), (receiver, filter)));
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
    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use aion_store::{InMemoryStore, WritableEventStore, WriteToken};
    use futures::StreamExt;
    use serde_json::json;

    use crate::engine::delegated::{EventFamily, EventFilter, EventStreamLagged};
    use crate::publish::PublishingEventStore;

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
        stream: &mut futures::stream::BoxStream<'static, Result<Event, EventStreamLagged>>,
    ) -> Result<Result<Event, EventStreamLagged>, Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await?
            .ok_or_else(|| "subscription stream ended unexpectedly".into())
    }

    #[tokio::test]
    async fn subscription_applies_workflow_and_family_filters()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = publishing_store(16)?;
        let target = WorkflowId::new_v4();
        let other = WorkflowId::new_v4();
        let mut subscription = store.publisher().subscribe(EventFilter {
            workflow_id: Some(target.clone()),
            run: None,
            family: Some(EventFamily::Signal),
        });

        store
            .append(
                WriteToken::recorder(),
                &other,
                &[started(1, &other)?, signal(2, &other)?],
                0,
            )
            .await?;
        store
            .append(
                WriteToken::recorder(),
                &target,
                &[started(1, &target)?, signal(2, &target)?],
                0,
            )
            .await?;

        let event = next_item(&mut subscription).await??;
        assert_eq!(event.workflow_id(), &target);
        assert_eq!(event.seq(), 2);
        assert!(matches!(event, Event::SignalReceived { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn overflow_yields_lagged_error_then_resumes_with_subsequent_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = publishing_store(2)?;
        let workflow_id = WorkflowId::new_v4();
        let mut subscription = store.publisher().subscribe(EventFilter::default());

        // Five events through a capacity-2 channel without consuming: the
        // receiver lags by three and retains the last two.
        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[
                    started(1, &workflow_id)?,
                    signal(2, &workflow_id)?,
                    signal(3, &workflow_id)?,
                    signal(4, &workflow_id)?,
                    signal(5, &workflow_id)?,
                ],
                0,
            )
            .await?;

        let lagged = next_item(&mut subscription).await?;
        assert_eq!(lagged, Err(EventStreamLagged { skipped: 3 }));

        let resumed = next_item(&mut subscription).await??;
        assert_eq!(resumed.seq(), 4);
        let resumed = next_item(&mut subscription).await??;
        assert_eq!(resumed.seq(), 5);

        // The stream continues past the lag with newly appended events.
        store
            .append(
                WriteToken::recorder(),
                &workflow_id,
                &[signal(6, &workflow_id)?],
                5,
            )
            .await?;
        let event = next_item(&mut subscription).await??;
        assert_eq!(event.seq(), 6);
        Ok(())
    }
}
