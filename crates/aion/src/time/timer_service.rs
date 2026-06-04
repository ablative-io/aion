//! Durable timer service: schedule, wheel arm, and `TimerFired` delivery.

use std::sync::Arc;

use aion_core::{Event, EventEnvelope, TimerId, WorkflowId};
use aion_store::{EventStore, StoreError};
use chrono::{DateTime, Utc};
use dashmap::DashSet;

use crate::engine_seam::{
    EngineHandle, EngineSeamError, TimerWheelEntry, WorkflowMailboxMessage, WorkflowResidency,
};

/// Durable timer scheduling and wheel-fire handling.
///
/// The service owns the AT live path for timers, while all history writes still go through the
/// engine's recorder seam. `EventStore::schedule_timer` is used only for the durable timer row that
/// recovery later polls; workflow-history observations are never appended directly here.
pub struct TimerService {
    engine: Arc<dyn EngineHandle>,
    store: Arc<dyn EventStore>,
    recorded_at: fn() -> DateTime<Utc>,
    terminal_updates: DashSet<(WorkflowId, TimerId)>,
}

struct TerminalUpdateSlot<'a> {
    terminal_updates: &'a DashSet<(WorkflowId, TimerId)>,
    key: (WorkflowId, TimerId),
}

impl Drop for TerminalUpdateSlot<'_> {
    fn drop(&mut self) {
        self.terminal_updates.remove(&self.key);
    }
}

/// Errors returned by [`TimerService`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum TimerServiceError {
    /// Durable timer storage or history inspection failed.
    #[error("timer store operation failed: {0}")]
    Store(#[from] StoreError),

    /// Engine seam operation failed.
    #[error("timer engine operation failed: {0}")]
    Engine(#[from] EngineSeamError),

    /// Anonymous sleep timers are cancelled by cancelling the owning workflow.
    #[error("anonymous sleep timers cannot be cancelled separately: {timer_id}")]
    AnonymousTimerNotCancellable {
        /// Anonymous timer that was passed to the named-timer cancellation API.
        timer_id: TimerId,
    },
}

impl TimerService {
    /// Creates a durable timer service from the engine seam and timer store.
    #[must_use]
    pub fn new(engine: Arc<dyn EngineHandle>, store: Arc<dyn EventStore>) -> Self {
        Self::with_recorded_at(engine, store, Utc::now)
    }

    /// Creates a durable timer service with an injected history timestamp source.
    #[must_use]
    pub fn with_recorded_at(
        engine: Arc<dyn EngineHandle>,
        store: Arc<dyn EventStore>,
        recorded_at: fn() -> DateTime<Utc>,
    ) -> Self {
        Self {
            engine,
            store,
            recorded_at,
            terminal_updates: DashSet::new(),
        }
    }

    /// Schedules a durable timer and arms the live wheel when the workflow is resident.
    ///
    /// The operation is considered successful only after the durable timer row, `TimerStarted`
    /// recorder event, and any required live wheel arm have succeeded. If the recorder fails after
    /// the timer row is written, the row is left for operator/recovery visibility and the error is
    /// returned; this service does not pretend a cross-store transaction exists.
    ///
    /// # Errors
    ///
    /// Returns [`TimerServiceError`] when durable storage, recording, residency resolution, or wheel
    /// arming fails.
    pub async fn schedule(
        &self,
        workflow_id: WorkflowId,
        timer_id: TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), TimerServiceError> {
        self.store
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;

        let event = Event::TimerStarted {
            envelope: self.next_envelope(&workflow_id).await?,
            timer_id: timer_id.clone(),
            fire_at,
        };
        self.engine.record_workflow_event(&workflow_id, event)?;

        if let WorkflowResidency::Resident(process) = self.engine.resolve_workflow(&workflow_id)? {
            self.engine.arm_timer(TimerWheelEntry {
                process,
                timer_id,
                fire_at,
            })?;
        }

        Ok(())
    }

    /// Cancels a durable named timer that has not already reached a terminal timer state.
    ///
    /// Already-fired and already-cancelled timers are treated as idempotent no-ops. For active
    /// resident timers the live wheel is disarmed through the engine seam before `TimerCancelled` is
    /// recorded through the workflow recorder seam. Non-resident timers still record the cancellation
    /// so recovery/replay can suppress a later fire.
    ///
    /// # Errors
    ///
    /// Returns [`TimerServiceError`] when history inspection, residency resolution, wheel disarming,
    /// or event recording fails.
    pub async fn cancel(
        &self,
        workflow_id: WorkflowId,
        timer_id: TimerId,
    ) -> Result<(), TimerServiceError> {
        if timer_id.name().is_none() {
            return Err(TimerServiceError::AnonymousTimerNotCancellable { timer_id });
        }

        let key = (workflow_id.clone(), timer_id.clone());
        let terminal_update_slot = self.wait_for_terminal_update_slot(key).await;

        let result = self.cancel_guarded(workflow_id, timer_id).await;
        drop(terminal_update_slot);
        result
    }

    async fn cancel_guarded(
        &self,
        workflow_id: WorkflowId,
        timer_id: TimerId,
    ) -> Result<(), TimerServiceError> {
        if self.timer_is_terminal(&workflow_id, &timer_id).await? {
            return Ok(());
        }

        if let WorkflowResidency::Resident(process) = self.engine.resolve_workflow(&workflow_id)? {
            self.engine.disarm_timer(process, &timer_id)?;
        }

        let event = Event::TimerCancelled {
            envelope: self.next_envelope(&workflow_id).await?,
            timer_id,
        };
        self.engine.record_workflow_event(&workflow_id, event)?;

        Ok(())
    }

    /// Handles a live timer-wheel fire.
    ///
    /// `TimerFired` is recorded before any mailbox delivery. If the workflow is no longer resident,
    /// the recorded event remains the durable observation that replay/recovery can surface later.
    ///
    /// # Errors
    ///
    /// Returns [`TimerServiceError`] when history inspection, recording, residency resolution, or
    /// live mailbox delivery fails.
    pub async fn fire_timer(
        &self,
        workflow_id: WorkflowId,
        timer_id: TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), TimerServiceError> {
        let key = (workflow_id.clone(), timer_id.clone());
        let terminal_update_slot = self.wait_for_terminal_update_slot(key).await;

        let result = self
            .fire_timer_guarded(workflow_id, timer_id, fire_at)
            .await;
        drop(terminal_update_slot);
        result
    }

    async fn wait_for_terminal_update_slot(
        &self,
        key: (WorkflowId, TimerId),
    ) -> TerminalUpdateSlot<'_> {
        loop {
            if self.terminal_updates.insert(key.clone()) {
                return TerminalUpdateSlot {
                    terminal_updates: &self.terminal_updates,
                    key,
                };
            }
            tokio::task::yield_now().await;
        }
    }

    async fn fire_timer_guarded(
        &self,
        workflow_id: WorkflowId,
        timer_id: TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), TimerServiceError> {
        if self.timer_is_terminal(&workflow_id, &timer_id).await? {
            return Ok(());
        }

        let event = Event::TimerFired {
            envelope: self.next_envelope(&workflow_id).await?,
            timer_id: timer_id.clone(),
        };
        self.engine.record_workflow_event(&workflow_id, event)?;

        if let WorkflowResidency::Resident(process) = self.engine.resolve_workflow(&workflow_id)? {
            self.engine.deliver_workflow_message(
                process,
                WorkflowMailboxMessage::TimerFired { timer_id, fire_at },
            )?;
        }

        Ok(())
    }

    async fn timer_is_terminal(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
    ) -> Result<bool, StoreError> {
        let history = self.store.read_history(workflow_id).await?;
        Ok(history.iter().any(|event| match event {
            Event::TimerFired {
                timer_id: recorded, ..
            }
            | Event::TimerCancelled {
                timer_id: recorded, ..
            } => recorded == timer_id,
            _ => false,
        }))
    }

    async fn next_envelope(&self, workflow_id: &WorkflowId) -> Result<EventEnvelope, StoreError> {
        let history = self.store.read_history(workflow_id).await?;
        let seq = history.iter().map(Event::seq).max().unwrap_or_default() + 1;
        Ok(EventEnvelope {
            seq,
            recorded_at: (self.recorded_at)(),
            workflow_id: workflow_id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, EventEnvelope, TimerId, WorkflowId};
    use aion_store::{EventStore, InMemoryStore, StoreError};
    use chrono::{DateTime, Utc};

    use super::{TimerService, TimerServiceError};
    use crate::engine_seam::test_support::{
        DeliveredWorkflowMessage, FakeEngineHandle, FakeEngineOperation,
    };
    use crate::engine_seam::{
        EngineHandle, TimerWheelEntry, WorkflowProcessHandle, WorkflowResidency,
    };

    fn instant(offset_seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new_v4()
    }

    fn timer_id() -> TimerId {
        TimerId::anonymous(7)
    }

    fn service() -> (Arc<InMemoryStore>, Arc<FakeEngineHandle>, TimerService) {
        let concrete_store = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = concrete_store.clone();
        let engine = Arc::new(FakeEngineHandle::recording_to(store.clone()));
        let service = TimerService::with_recorded_at(engine.clone(), store, recorded_at);
        (concrete_store, engine, service)
    }

    fn recorded_at() -> DateTime<Utc> {
        instant(1)
    }

    async fn history(
        store: &InMemoryStore,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Event>, StoreError> {
        store.read_history(workflow_id).await
    }

    fn count_timer_fired(events: &[Event], timer_id: &TimerId) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(event, Event::TimerFired { timer_id: recorded, .. } if recorded == timer_id)
            })
            .count()
    }

    #[tokio::test]
    async fn schedule_records_timer_row_and_timer_started_event() -> Result<(), TimerServiceError> {
        let (store, _engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(10);

        service
            .schedule(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;

        let expired = store.expired_timers(fire_at).await?;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].workflow_id, workflow_id);
        assert_eq!(expired[0].timer_id, timer_id);
        assert_eq!(expired[0].fire_at, fire_at);

        let history = history(&store, &workflow_id).await?;
        assert!(matches!(
            history.as_slice(),
            [Event::TimerStarted {
                envelope,
                timer_id: recorded,
                fire_at: recorded_fire_at,
            }] if envelope.recorded_at == recorded_at()
                && recorded == &timer_id
                && recorded_fire_at == &fire_at
        ));
        Ok(())
    }

    #[tokio::test]
    async fn schedule_arms_wheel_for_resident_workflow() -> Result<(), TimerServiceError> {
        let process = WorkflowProcessHandle::new(42);
        let (_store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(20);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        service
            .schedule(workflow_id, timer_id.clone(), fire_at)
            .await?;

        assert_eq!(
            engine.armed_timers()?,
            vec![TimerWheelEntry {
                process,
                timer_id,
                fire_at
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn schedule_for_nonresident_records_without_arming() -> Result<(), TimerServiceError> {
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(30);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::NonResident)?;

        service
            .schedule(workflow_id.clone(), timer_id, fire_at)
            .await?;

        assert!(engine.armed_timers()?.is_empty());
        assert_eq!(history(&store, &workflow_id).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn fire_records_timer_fired_then_delivers_mailbox_message()
    -> Result<(), TimerServiceError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(40);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            1
        );
        assert_eq!(
            engine.delivered_messages()?,
            vec![(
                process,
                DeliveredWorkflowMessage::TimerFired {
                    timer_id: timer_id.clone(),
                    fire_at
                }
            )]
        );
        assert!(matches!(
            engine.operations()?.as_slice(),
            [
                FakeEngineOperation::EventRecorded {
                    workflow_id: recorded_workflow_id,
                    event: Event::TimerFired { timer_id: recorded_timer_id, .. },
                },
                FakeEngineOperation::Delivered {
                    process: delivered_process,
                    message: DeliveredWorkflowMessage::TimerFired { timer_id: delivered_timer_id, .. },
                }
            ] if recorded_workflow_id == &workflow_id
                && recorded_timer_id == &timer_id
                && delivered_process == &process
                && delivered_timer_id == &timer_id
        ));
        Ok(())
    }

    #[tokio::test]
    async fn fire_records_without_delivery_when_workflow_becomes_nonresident()
    -> Result<(), TimerServiceError> {
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(50);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::NonResident)?;

        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            1
        );
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn firing_same_timer_twice_records_and_delivers_once() -> Result<(), TimerServiceError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(60);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;
        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            1
        );
        assert_eq!(engine.delivered_messages()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn firing_cancelled_timer_is_noop() -> Result<(), TimerServiceError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(70);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        let cancelled = Event::TimerCancelled {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: instant(69),
                workflow_id: workflow_id.clone(),
            },
            timer_id: timer_id.clone(),
        };
        engine.record_workflow_event(&workflow_id, cancelled)?;

        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;

        let history = history(&store, &workflow_id).await?;
        assert_eq!(count_timer_fired(&history, &timer_id), 0);
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn fire_resolves_residency_at_fire_time() -> Result<(), TimerServiceError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        let fire_at = instant(80);

        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        engine.set_residency(workflow_id.clone(), WorkflowResidency::NonResident)?;
        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), fire_at)
            .await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            1
        );
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }
}
