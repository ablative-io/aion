//! Recorder: single-writer append path over `EventStore`.

use std::sync::Arc;

use aion_core::{
    ActivityError, ActivityId, Event, EventEnvelope, Payload, TimerId, WorkflowError, WorkflowId,
};
use aion_store::EventStore;
use chrono::{DateTime, Utc};

use crate::durability::{DurabilityError, seq::SequenceHead};

/// Single append authority for one workflow history.
///
/// A recorder owns the workflow's tracked sequence head and computes every `expected_seq` from that
/// tracker. It never reads the store head after construction; a sequence conflict is surfaced as a
/// hard durability error because it indicates a second writer for the same workflow.
pub struct Recorder {
    workflow_id: WorkflowId,
    store: Arc<dyn EventStore>,
    sequence: SequenceHead,
}

impl Recorder {
    /// Creates a recorder for a fresh workflow history starting at sequence head `0`.
    #[must_use]
    pub fn new(workflow_id: WorkflowId, store: Arc<dyn EventStore>) -> Self {
        Self::resume_at(workflow_id, store, 0)
    }

    /// Creates a recorder for a workflow whose existing history head was already derived.
    #[must_use]
    pub fn resume_at(workflow_id: WorkflowId, store: Arc<dyn EventStore>, head: u64) -> Self {
        Self {
            workflow_id,
            store,
            sequence: SequenceHead::from_head(head),
        }
    }

    /// Returns the workflow this recorder appends to.
    #[must_use]
    pub const fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    /// Returns the current tracked sequence head.
    #[must_use]
    pub const fn current_head(&self) -> u64 {
        self.sequence.current()
    }

    /// Records workflow start.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_workflow_started(
        &mut self,
        recorded_at: DateTime<Utc>,
        workflow_type: String,
        input: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::WorkflowStarted {
            envelope,
            workflow_type,
            input,
        })
        .await
    }

    /// Records workflow completion.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_workflow_completed(
        &mut self,
        recorded_at: DateTime<Utc>,
        result: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::WorkflowCompleted { envelope, result })
            .await
    }

    /// Records terminal workflow failure.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_workflow_failed(
        &mut self,
        recorded_at: DateTime<Utc>,
        error: WorkflowError,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::WorkflowFailed { envelope, error })
            .await
    }

    /// Records workflow cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_workflow_cancelled(
        &mut self,
        recorded_at: DateTime<Utc>,
        reason: String,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::WorkflowCancelled { envelope, reason })
            .await
    }

    /// Records activity scheduling.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_activity_scheduled(
        &mut self,
        recorded_at: DateTime<Utc>,
        activity_id: ActivityId,
        activity_type: String,
        input: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ActivityScheduled {
            envelope,
            activity_id,
            activity_type,
            input,
        })
        .await
    }

    /// Records activity start.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_activity_started(
        &mut self,
        recorded_at: DateTime<Utc>,
        activity_id: ActivityId,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ActivityStarted {
            envelope,
            activity_id,
        })
        .await
    }

    /// Records successful activity completion.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_activity_completed(
        &mut self,
        recorded_at: DateTime<Utc>,
        activity_id: ActivityId,
        result: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ActivityCompleted {
            envelope,
            activity_id,
            result,
        })
        .await
    }

    /// Records failed activity attempt.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_activity_failed(
        &mut self,
        recorded_at: DateTime<Utc>,
        activity_id: ActivityId,
        error: ActivityError,
        attempt: u32,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ActivityFailed {
            envelope,
            activity_id,
            error,
            attempt,
        })
        .await
    }

    /// Records timer scheduling.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_timer_started(
        &mut self,
        recorded_at: DateTime<Utc>,
        timer_id: TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::TimerStarted {
            envelope,
            timer_id,
            fire_at,
        })
        .await
    }

    /// Records timer firing.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_timer_fired(
        &mut self,
        recorded_at: DateTime<Utc>,
        timer_id: TimerId,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::TimerFired { envelope, timer_id })
            .await
    }

    /// Records timer cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_timer_cancelled(
        &mut self,
        recorded_at: DateTime<Utc>,
        timer_id: TimerId,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::TimerCancelled { envelope, timer_id })
            .await
    }

    /// Records signal delivery.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_signal_received(
        &mut self,
        recorded_at: DateTime<Utc>,
        name: String,
        payload: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::SignalReceived {
            envelope,
            name,
            payload,
        })
        .await
    }

    /// Records child workflow start.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_child_workflow_started(
        &mut self,
        recorded_at: DateTime<Utc>,
        child_workflow_id: WorkflowId,
        workflow_type: String,
        input: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ChildWorkflowStarted {
            envelope,
            child_workflow_id,
            workflow_type,
            input,
        })
        .await
    }

    /// Records child workflow completion.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_child_workflow_completed(
        &mut self,
        recorded_at: DateTime<Utc>,
        child_workflow_id: WorkflowId,
        result: Payload,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ChildWorkflowCompleted {
            envelope,
            child_workflow_id,
            result,
        })
        .await
    }

    /// Records child workflow failure.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the append or the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_child_workflow_failed(
        &mut self,
        recorded_at: DateTime<Utc>,
        child_workflow_id: WorkflowId,
        error: WorkflowError,
    ) -> Result<(), DurabilityError> {
        let envelope = self.next_envelope(recorded_at);
        self.append_one(Event::ChildWorkflowFailed {
            envelope,
            child_workflow_id,
            error,
        })
        .await
    }

    fn next_envelope(&self, recorded_at: DateTime<Utc>) -> EventEnvelope {
        EventEnvelope {
            seq: self.sequence.next_seq(),
            recorded_at,
            workflow_id: self.workflow_id.clone(),
        }
    }

    async fn append_one(&mut self, event: Event) -> Result<(), DurabilityError> {
        let expected_seq = self.sequence.current();
        self.store
            .append(
                &self.workflow_id,
                std::slice::from_ref(&event),
                expected_seq,
            )
            .await?;
        self.sequence.mark_append_success(1)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, Payload, TimerId};
    use aion_store::{EventStore, InMemoryStore, StoreError};
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::Recorder;
    use crate::durability::DurabilityError;

    fn workflow_id(value: u128) -> aion_core::WorkflowId {
        aion_core::WorkflowId::new(uuid::Uuid::from_u128(value))
    }

    fn recorded_at(offset_seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    fn payload(label: &str) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&json!({ "label": label }))?)
    }

    fn workflow_started(
        seq: u64,
        workflow_id: &aion_core::WorkflowId,
    ) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::WorkflowStarted {
            envelope: aion_core::EventEnvelope {
                seq,
                recorded_at: recorded_at(i64::try_from(seq)?),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: String::from("checkout"),
            input: payload("workflow-input")?,
        })
    }

    #[tokio::test]
    async fn recorder_advances_expected_sequence_between_appends()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = workflow_id(1);
        let store = Arc::new(InMemoryStore::default());
        let mut recorder = Recorder::new(workflow_id.clone(), store.clone());

        recorder
            .record_workflow_started(recorded_at(1), String::from("checkout"), payload("input")?)
            .await?;
        recorder
            .record_workflow_completed(recorded_at(2), payload("result")?)
            .await?;

        let history = store.read_history(&workflow_id).await?;
        assert_eq!(history[0].seq(), 1);
        assert_eq!(history[1].seq(), 2);
        assert_eq!(recorder.current_head(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn records_activity_events_in_sequence_order() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = workflow_id(2);
        let store = Arc::new(InMemoryStore::default());
        let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
        let activity_id = aion_core::ActivityId::from_sequence_position(1);

        recorder
            .record_activity_scheduled(
                recorded_at(1),
                activity_id.clone(),
                String::from("charge-card"),
                payload("input")?,
            )
            .await?;
        recorder
            .record_activity_completed(recorded_at(2), activity_id.clone(), payload("result")?)
            .await?;

        let history = store.read_history(&workflow_id).await?;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].seq(), 1);
        assert_eq!(history[1].seq(), 2);
        match &history[0] {
            Event::ActivityScheduled {
                activity_id: recorded_activity_id,
                ..
            } => assert_eq!(recorded_activity_id, &activity_id),
            other => return Err(format!("expected ActivityScheduled, got {other:?}").into()),
        }
        match &history[1] {
            Event::ActivityCompleted {
                activity_id: recorded_activity_id,
                ..
            } => assert_eq!(recorded_activity_id, &activity_id),
            other => return Err(format!("expected ActivityCompleted, got {other:?}").into()),
        }
        Ok(())
    }

    #[tokio::test]
    async fn resume_at_continues_from_existing_history_head()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = workflow_id(3);
        let store = Arc::new(InMemoryStore::default());
        let seeded = [
            workflow_started(1, &workflow_id)?,
            workflow_started(2, &workflow_id)?,
        ];
        store.append(&workflow_id, &seeded, 0).await?;

        let mut recorder = Recorder::resume_at(workflow_id.clone(), store.clone(), 2);
        recorder
            .record_signal_received(recorded_at(3), String::from("approve"), payload("signal")?)
            .await?;

        let history = store.read_history(&workflow_id).await?;
        assert_eq!(history.len(), 3);
        assert_eq!(history[2].seq(), 3);
        assert_eq!(recorder.current_head(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn sequence_conflict_surfaces_without_advancing_or_retrying()
    -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = workflow_id(4);
        let store = Arc::new(InMemoryStore::default());
        let mut recorder = Recorder::new(workflow_id.clone(), store.clone());
        let rogue_event = workflow_started(1, &workflow_id)?;
        store.append(&workflow_id, &[rogue_event], 0).await?;

        let error = recorder
            .record_timer_fired(recorded_at(2), TimerId::anonymous(2))
            .await;

        match error {
            Err(DurabilityError::Store(StoreError::SequenceConflict { expected, found })) => {
                assert_eq!(expected, 0);
                assert_eq!(found, 1);
            }
            Err(other) => return Err(format!("expected sequence conflict, got {other:?}").into()),
            Ok(()) => return Err("expected sequence conflict".into()),
        }
        assert_eq!(recorder.current_head(), 0);
        let history = store.read_history(&workflow_id).await?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].seq(), 1);
        Ok(())
    }
}
