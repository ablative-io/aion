//! Durable timer service: schedule, wheel arm, and `TimerFired` delivery.

use std::sync::Arc;

use aion_core::{Event, EventEnvelope, TimerId, WorkflowId};
use aion_store::{ReadableEventStore, StoreError};
use chrono::{DateTime, Utc};
use dashmap::DashSet;

use crate::engine_seam::{
    EngineHandle, EngineSeamError, TimerWheelEntry, WorkflowMailboxMessage, WorkflowResidency,
};

/// Durable timer scheduling and wheel-fire handling.
///
/// The service owns the AT live path for timers. Workflow-issued `TimerStarted` events are recorded
/// by AD's resume-live handoff before this service is called; this service persists only the durable
/// timer row and later asynchronous arrival/cancellation history through the engine recorder seam.
pub struct TimerService {
    engine: Arc<dyn EngineHandle>,
    store: Arc<dyn ReadableEventStore>,
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
}

impl TimerService {
    /// Creates a durable timer service from the engine seam and timer store.
    #[must_use]
    pub fn new(engine: Arc<dyn EngineHandle>, store: Arc<dyn ReadableEventStore>) -> Self {
        Self::with_recorded_at(engine, store, Utc::now)
    }

    /// Creates a durable timer service with an injected history timestamp source.
    #[must_use]
    pub fn with_recorded_at(
        engine: Arc<dyn EngineHandle>,
        store: Arc<dyn ReadableEventStore>,
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
    /// The operation persists the durable timer row and arms the wheel when needed. The
    /// command-issued `TimerStarted` recorder event is appended by AD's resume-live handoff before
    /// AE/AT reaches this service, so this method deliberately does not record it again.
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

        if let WorkflowResidency::Resident(process) = self.engine.resolve_workflow(&workflow_id)? {
            self.engine.arm_timer(TimerWheelEntry {
                process,
                timer_id,
                fire_at,
            })?;
        }

        Ok(())
    }

    /// Cancels a durable timer that has not already reached a terminal timer state.
    ///
    /// Already-fired and already-cancelled timers are treated as idempotent no-ops. For active
    /// resident timers the live wheel is disarmed through the engine seam before `TimerCancelled` is
    /// recorded through the workflow recorder seam. Non-resident timers still record the cancellation
    /// so recovery/replay can suppress a later fire.
    ///
    /// Anonymous timers are accepted: authors can never address one (the SDK's `cancel_timer`
    /// takes a `TimerRef` minted by `start_timer`, which is always named), but the engine settles
    /// `with_timeout` scope deadlines — anonymous by construction — through this first-recorded-wins
    /// race against [`Self::fire_timer`].
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
        if !self.timer_is_live(&workflow_id, &timer_id).await? {
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
        if !self.timer_is_live(&workflow_id, &timer_id).await? {
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

    /// Whether the timer is started and not yet terminal in the workflow's
    /// active run segment.
    ///
    /// A timer belongs to the run that recorded its `TimerStarted`, and
    /// anonymous timer identities are run-scoped ordinals that replacement
    /// runs (continue-as-new) re-allocate from zero. Scoping the check to
    /// the latest run segment keeps a stale fire from a finished run from
    /// recording into — or suppressing — the replacement run's identically
    /// named timer.
    async fn timer_is_live(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
    ) -> Result<bool, StoreError> {
        let history = self.store.read_history(workflow_id).await?;
        let segment_start = history
            .iter()
            .rposition(|event| matches!(event, Event::WorkflowStarted { .. }))
            .unwrap_or(0);
        let segment = &history[segment_start..];
        let started = segment.iter().any(|event| {
            matches!(
                event,
                Event::TimerStarted { timer_id: recorded, .. } if recorded == timer_id
            )
        });
        let terminal = segment.iter().any(|event| match event {
            Event::TimerFired {
                timer_id: recorded, ..
            }
            | Event::TimerCancelled {
                timer_id: recorded, ..
            } => recorded == timer_id,
            _ => false,
        });
        Ok(started && !terminal)
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
    use aion_store::{InMemoryStore, ReadableEventStore, StoreError, WritableEventStore};
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
        let recorder_store: Arc<dyn WritableEventStore> = concrete_store.clone();
        let readable_store: Arc<dyn ReadableEventStore> = concrete_store.clone();
        let engine = Arc::new(FakeEngineHandle::recording_to(recorder_store));
        let service = TimerService::with_recorded_at(engine.clone(), readable_store, recorded_at);
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

    fn timer_started_event(workflow_id: &WorkflowId, timer_id: &TimerId, seq: u64) -> Event {
        Event::TimerStarted {
            envelope: EventEnvelope {
                seq,
                recorded_at: instant(0),
                workflow_id: workflow_id.clone(),
            },
            timer_id: timer_id.clone(),
            fire_at: instant(5),
        }
    }

    fn workflow_started_event(workflow_id: &WorkflowId, seq: u64) -> Event {
        Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq,
                recorded_at: instant(0),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: "fixture".to_owned(),
            input: aion_core::Payload::new(aion_core::ContentType::Json, b"null".to_vec()),
            run_id: aion_core::RunId::new_v4(),
            parent_run_id: None,
        }
    }

    #[tokio::test]
    async fn schedule_records_timer_row_without_timer_started_event()
    -> Result<(), TimerServiceError> {
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

        assert!(history(&store, &workflow_id).await?.is_empty());
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
        assert!(history(&store, &workflow_id).await?.is_empty());
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
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;

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
                    event: Event::TimerStarted { .. },
                    ..
                },
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
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;

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
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;

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
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
        let cancelled = Event::TimerCancelled {
            envelope: EventEnvelope {
                seq: 2,
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
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
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
    async fn firing_unstarted_timer_records_nothing() -> Result<(), TimerServiceError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;

        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), instant(90))
            .await?;

        assert!(history(&store, &workflow_id).await?.is_empty());
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn firing_prior_run_timer_after_continue_as_new_is_noop() -> Result<(), TimerServiceError>
    {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, service) = service();
        let workflow_id = workflow_id();
        let timer_id = timer_id();
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        // Run 1 started the timer; run 2's WorkflowStarted closes that segment.
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
        engine.record_workflow_event(&workflow_id, workflow_started_event(&workflow_id, 2))?;

        service
            .fire_timer(workflow_id.clone(), timer_id.clone(), instant(100))
            .await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            0
        );
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }
}
