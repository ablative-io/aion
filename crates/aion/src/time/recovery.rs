//! Expired timer polling on startup and periodic recovery tick.

use aion_core::{Event, TimerId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aion_store::{ReadableEventStore, StoreError};
use chrono::{DateTime, Utc};

use crate::engine_seam::EngineSeamError;
use crate::time::{TimerService, TimerServiceError};

/// Recovery service for durable timers that elapsed outside the live wheel path.
pub struct TimerRecovery {
    store: Arc<dyn ReadableEventStore>,
    timer_service: Arc<TimerService>,
    recovery_interval: Duration,
    now: fn() -> DateTime<Utc>,
}

/// Errors returned by [`TimerRecovery`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum TimerRecoveryError {
    /// Durable timer polling failed.
    #[error("timer recovery store operation failed: {0}")]
    Store(#[from] StoreError),

    /// Recovered timer firing failed.
    #[error("timer recovery fire operation failed: {0}")]
    Timer(#[from] TimerServiceError),
}

impl TimerRecovery {
    /// Creates a timer recovery service with an engine-configured recovery cadence.
    #[must_use]
    pub fn new(
        store: Arc<dyn ReadableEventStore>,
        timer_service: Arc<TimerService>,
        recovery_interval: Duration,
    ) -> Self {
        Self::with_clock(store, timer_service, recovery_interval, Utc::now)
    }

    /// Creates a timer recovery service with an injected clock for deterministic ticking.
    #[must_use]
    pub fn with_clock(
        store: Arc<dyn ReadableEventStore>,
        timer_service: Arc<TimerService>,
        recovery_interval: Duration,
        now: fn() -> DateTime<Utc>,
    ) -> Self {
        Self {
            store,
            timer_service,
            recovery_interval,
            now,
        }
    }

    /// Runs the engine-startup recovery sweep for timers due as of `now`.
    ///
    /// Each due timer is delegated to [`TimerService::fire_timer`], which owns terminal filtering,
    /// the in-flight fire guard, recording `TimerFired`, and mailbox delivery.
    ///
    /// # Errors
    ///
    /// Returns [`TimerRecoveryError`] when polling expired timers or firing a due timer fails.
    pub async fn recover_on_startup(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, TimerRecoveryError> {
        let fired = self.recover_due(now).await?;
        self.rearm_future_from_active_histories(now).await?;
        Ok(fired)
    }

    /// Runs one recovery tick using the injected clock.
    ///
    /// AE owns driving this method at [`Self::recovery_interval`]; this service intentionally does
    /// not spawn or own the production runtime task.
    ///
    /// # Errors
    ///
    /// Returns [`TimerRecoveryError`] when polling expired timers or firing a due timer fails.
    pub async fn tick(&self) -> Result<usize, TimerRecoveryError> {
        self.recover_due((self.now)()).await
    }

    /// Returns the engine-configured recovery cadence.
    #[must_use]
    pub const fn recovery_interval(&self) -> Duration {
        self.recovery_interval
    }

    async fn recover_due(&self, now: DateTime<Utc>) -> Result<usize, TimerRecoveryError> {
        let due_timers = self.store.expired_timers(now).await?;
        let mut fired = 0;
        for entry in due_timers {
            match self
                .timer_service
                .fire_timer(
                    entry.workflow_id.clone(),
                    entry.timer_id.clone(),
                    entry.fire_at,
                )
                .await
            {
                Ok(()) => fired += 1,
                // An orphaned timer whose workflow no longer exists — e.g. the
                // workflow was cancelled and purged from the engine's known set —
                // must never abort recovery or block engine startup. The workflow
                // is gone, so the timer is moot: log it and skip. (A terminal but
                // still-known workflow's timer is already filtered inside
                // `fire_timer`'s liveness check, which returns `Ok` without firing.)
                Err(TimerServiceError::Engine(EngineSeamError::UnknownWorkflow {
                    workflow_id,
                })) => {
                    tracing::warn!(
                        %workflow_id,
                        timer_id = %entry.timer_id,
                        "skipping recovered timer for unknown workflow (orphaned timer); \
                         the workflow no longer exists"
                    );
                }
                Err(other) => return Err(other.into()),
            }
        }
        Ok(fired)
    }

    async fn rearm_future_from_active_histories(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, TimerRecoveryError> {
        let mut rearmed = 0;
        for workflow_id in self.store.list_active().await? {
            let history = self.store.read_history(&workflow_id).await?;
            for (timer_id, fire_at) in outstanding_future_timers(&history, now) {
                self.timer_service
                    .schedule(workflow_id.clone(), timer_id, fire_at)
                    .await?;
                rearmed += 1;
            }
        }
        Ok(rearmed)
    }
}

fn outstanding_future_timers(
    history: &[Event],
    now: DateTime<Utc>,
) -> Vec<(TimerId, DateTime<Utc>)> {
    let mut outstanding: HashMap<TimerId, DateTime<Utc>> = HashMap::new();
    for event in history {
        match event {
            Event::TimerStarted {
                timer_id, fire_at, ..
            } => {
                outstanding.insert(timer_id.clone(), *fire_at);
            }
            Event::TimerFired { timer_id, .. } | Event::TimerCancelled { timer_id, .. } => {
                outstanding.remove(timer_id);
            }
            _ => {}
        }
    }
    outstanding
        .into_iter()
        .filter(|(_, fire_at)| *fire_at > now)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use aion_core::{Event, EventEnvelope, RunId, TimerCancelCause, TimerId, WorkflowId};
    use aion_store::{InMemoryStore, ReadableEventStore, StoreError, WritableEventStore};
    use chrono::{DateTime, Utc};

    use super::{TimerRecovery, TimerRecoveryError, outstanding_future_timers};
    use crate::engine_seam::test_support::{DeliveredWorkflowMessage, FakeEngineHandle};
    use crate::engine_seam::{
        EngineHandle, EngineSeamError, WorkflowProcessHandle, WorkflowResidency,
    };
    use crate::time::TimerService;
    use crate::time::deadline_timer_id;

    const RECOVERY_INTERVAL: Duration = Duration::from_millis(10);

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Recovery(#[from] TimerRecoveryError),

        #[error(transparent)]
        Store(#[from] StoreError),

        #[error(transparent)]
        Engine(#[from] EngineSeamError),
    }

    fn instant(offset_seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    fn recorded_at() -> DateTime<Utc> {
        instant(1)
    }

    fn tick_now() -> DateTime<Utc> {
        instant(30)
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new_v4()
    }

    fn timer_id(sequence: u64) -> TimerId {
        TimerId::anonymous(sequence)
    }

    fn recovery() -> (Arc<InMemoryStore>, Arc<FakeEngineHandle>, TimerRecovery) {
        let concrete_store = Arc::new(InMemoryStore::default());
        let writable: Arc<dyn WritableEventStore> = concrete_store.clone();
        let readable: Arc<dyn ReadableEventStore> = concrete_store.clone();
        let engine = Arc::new(FakeEngineHandle::recording_to(writable));
        let timer_service = Arc::new(TimerService::with_recorded_at(
            engine.clone(),
            readable.clone(),
            recorded_at,
        ));
        let recovery =
            TimerRecovery::with_clock(readable, timer_service, RECOVERY_INTERVAL, tick_now);
        (concrete_store, engine, recovery)
    }

    async fn history(
        store: &InMemoryStore,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Event>, StoreError> {
        store.read_history(workflow_id).await
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

    fn count_timer_fired(events: &[Event], timer_id: &TimerId) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(event, Event::TimerFired { timer_id: recorded, .. } if recorded == timer_id)
            })
            .count()
    }

    #[tokio::test]
    async fn startup_sweep_fires_past_timer_and_delivers() -> Result<(), TestError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, recovery) = recovery();
        let workflow_id = workflow_id();
        let timer_id = timer_id(1);
        let fire_at = instant(10);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
        store
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;

        let recovered = recovery.recover_on_startup(instant(20)).await?;

        assert_eq!(recovered, 1);
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
        Ok(())
    }

    #[tokio::test]
    async fn startup_sweep_does_not_fire_future_timer() -> Result<(), TestError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, recovery) = recovery();
        let workflow_id = workflow_id();
        let timer_id = timer_id(2);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        store
            .schedule_timer(&workflow_id, &timer_id, instant(30))
            .await?;

        let recovered = recovery.recover_on_startup(instant(20)).await?;

        assert_eq!(recovered, 0);
        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            0
        );
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn tick_uses_injected_clock_and_fires_newly_due_timer_once() -> Result<(), TestError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, recovery) = recovery();
        let workflow_id = workflow_id();
        let timer_id = timer_id(3);
        let fire_at = instant(25);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
        store
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;

        assert_eq!(recovery.recovery_interval(), RECOVERY_INTERVAL);
        assert_eq!(recovery.tick().await?, 1);
        assert_eq!(recovery.tick().await?, 1);

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            1
        );
        assert_eq!(engine.delivered_messages()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn running_startup_sweep_twice_fires_due_timer_once_total() -> Result<(), TestError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, recovery) = recovery();
        let workflow_id = workflow_id();
        let timer_id = timer_id(4);
        let fire_at = instant(10);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
        store
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;

        recovery.recover_on_startup(instant(20)).await?;
        recovery.recover_on_startup(instant(20)).await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            1
        );
        assert_eq!(engine.delivered_messages()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_timer_is_never_fired_by_recovery() -> Result<(), TestError> {
        let process = WorkflowProcessHandle::new(42);
        let (store, engine, recovery) = recovery();
        let workflow_id = workflow_id();
        let timer_id = timer_id(5);
        let fire_at = instant(10);
        engine.set_residency(workflow_id.clone(), WorkflowResidency::Resident(process))?;
        store
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;
        engine.record_workflow_event(
            &workflow_id,
            Event::TimerCancelled {
                cause: aion_core::TimerCancelCause::WorkflowIntent,
                envelope: EventEnvelope {
                    seq: 1,
                    recorded_at: instant(9),
                    workflow_id: workflow_id.clone(),
                },
                timer_id: timer_id.clone(),
            },
        )?;

        recovery.recover_on_startup(instant(20)).await?;

        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            0
        );
        assert!(engine.delivered_messages()?.is_empty());
        Ok(())
    }

    /// D5 resurrection hazard: `outstanding_future_timers` is whole-history
    /// scoped, so a continue-as-new predecessor's still-outstanding
    /// `deadline:{run}` WOULD be re-armed after failover — firing a timeout
    /// against a run that already continued. The `WorkflowIntent` cancel recorded
    /// at the CAN terminal closes exactly that hole. This proves both halves at
    /// the precise mechanism the scout flagged.
    #[test]
    fn cancelled_predecessor_deadline_is_not_rearmed_after_continue_as_new() {
        let workflow_id = workflow_id();
        let predecessor_run = RunId::new_v4();
        let deadline = deadline_timer_id(&predecessor_run).unwrap_or_else(|_| timer_id(0));
        let now = instant(0);
        let fire_at = instant(120); // future: eligible for re-arm

        let started = |seq: u64, run: &RunId| Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq,
                recorded_at: instant(0),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: "sleeper".to_owned(),
            input: aion_core::Payload::new(aion_core::ContentType::Json, b"null".to_vec()),
            run_id: run.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };
        let deadline_started = Event::TimerStarted {
            envelope: EventEnvelope {
                seq: 2,
                recorded_at: instant(0),
                workflow_id: workflow_id.clone(),
            },
            timer_id: deadline.clone(),
            fire_at,
        };
        let continued = Event::WorkflowContinuedAsNew {
            envelope: EventEnvelope {
                seq: 3,
                recorded_at: instant(1),
                workflow_id: workflow_id.clone(),
            },
            input: aion_core::Payload::new(aion_core::ContentType::Json, b"null".to_vec()),
            workflow_type: None,
            parent_run_id: predecessor_run.clone(),
        };

        // Before the cancel: the hazard is real — the predecessor's future
        // deadline is outstanding across the whole history even after CAN.
        let uncancelled = vec![
            started(1, &predecessor_run),
            deadline_started.clone(),
            continued.clone(),
        ];
        assert!(
            outstanding_future_timers(&uncancelled, now)
                .into_iter()
                .any(|(timer_id, _)| timer_id == deadline),
            "an uncancelled predecessor deadline WOULD be re-armed after failover"
        );

        // With the WorkflowIntent cancel recorded at the CAN terminal: closed.
        let cancelled = vec![
            started(1, &predecessor_run),
            deadline_started,
            continued,
            Event::TimerCancelled {
                envelope: EventEnvelope {
                    seq: 4,
                    recorded_at: instant(1),
                    workflow_id: workflow_id.clone(),
                },
                timer_id: deadline.clone(),
                cause: TimerCancelCause::WorkflowIntent,
            },
        ];
        assert!(
            !outstanding_future_timers(&cancelled, now)
                .into_iter()
                .any(|(timer_id, _)| timer_id == deadline),
            "the WorkflowIntent cancel closes the whole-history re-arm hole"
        );
    }

    #[tokio::test]
    async fn orphaned_timer_for_unknown_workflow_is_skipped_not_fatal() -> Result<(), TestError> {
        // Regression: a durable timer whose workflow was cancelled and purged
        // from the engine's known set must NOT abort startup recovery. Before the
        // fix, `fire_timer`'s `UnknownWorkflow` error propagated and bricked engine
        // startup — observed in production after a restart:
        //   "timer recovery fire operation failed: ... workflow <id> is unknown".
        let (store, engine, recovery) = recovery();
        let workflow_id = workflow_id();
        let timer_id = timer_id(6);
        let fire_at = instant(10);

        // The timer is live in history (started, never fired/cancelled) ...
        store
            .schedule_timer(&workflow_id, &timer_id, fire_at)
            .await?;
        engine.record_workflow_event(
            &workflow_id,
            timer_started_event(&workflow_id, &timer_id, 1),
        )?;
        // ... but the workflow itself is gone: the engine rejects the recovered
        // fire with `UnknownWorkflow` (exactly what the real engine does when a
        // cancelled workflow's record has been purged).
        engine.push_record_response(Err(EngineSeamError::UnknownWorkflow {
            workflow_id: workflow_id.clone(),
        }))?;

        // Recovery must SUCCEED by skipping the orphan, not error out.
        let recovered = recovery.recover_on_startup(instant(20)).await?;

        assert_eq!(recovered, 0, "the orphaned timer is skipped, not fired");
        assert_eq!(
            count_timer_fired(&history(&store, &workflow_id).await?, &timer_id),
            0,
            "no TimerFired is recorded for an unknown workflow"
        );
        assert!(
            engine.delivered_messages()?.is_empty(),
            "nothing is delivered for an unknown workflow"
        );
        Ok(())
    }
}
