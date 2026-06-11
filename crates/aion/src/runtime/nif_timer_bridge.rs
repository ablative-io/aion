//! Engine-seam bridge backing the timer NIFs.
//!
//! The bridge adapts the engine's registry, event store, and tokio runtime to
//! the [`EngineHandle`] seam consumed by [`TimerService`], and owns the live
//! timer wheel (armed tokio sleep tasks keyed per process and timer id).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_store::{EventStore, ReadableEventStore, RunSummary, StoreError, TimerEntry};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::registry::Registry;
use crate::runtime::nif_state::EngineNifState;
use crate::runtime::nif_timer::NifTimerError;
use crate::time::TimerService;

pub(super) struct TimerNifBridge {
    pub(super) registry: Arc<Registry>,
    store: Arc<dyn ReadableEventStore>,
    pub(super) tokio_handle: Handle,
    pending_timers: DashMap<(WorkflowProcessHandle, TimerId), PendingTimerTask>,
    next_timer_generation: AtomicU64,
    // Weak: the engine state owns this bridge through its timer slot.
    nif_state: Weak<EngineNifState>,
}

struct PendingTimerTask {
    generation: u64,
    handle: JoinHandle<()>,
}

struct ReadableEventStoreAdapter {
    store: Arc<dyn EventStore>,
}

#[async_trait::async_trait]
impl ReadableEventStore for ReadableEventStoreAdapter {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        self.store.read_history(workflow_id).await
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.store.read_run_chain(workflow_id).await
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.store.list_active().await
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        self.store.list_workflow_ids().await
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        self.store.query(filter).await
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.store
            .schedule_timer(workflow_id, timer_id, fire_at)
            .await
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        self.store.expired_timers(as_of).await
    }
}

impl TimerNifBridge {
    pub(super) fn service(self: &Arc<Self>) -> TimerService {
        let engine: Arc<dyn EngineHandle> = self.clone();
        let store: Arc<dyn ReadableEventStore> = self.store.clone();
        TimerService::new(engine, store)
    }

    fn workflow_id_for_process(
        &self,
        process: WorkflowProcessHandle,
    ) -> Result<WorkflowId, EngineSeamError> {
        self.registry
            .list()
            .map_err(|error| EngineSeamError::TimerWheel {
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|handle| handle.pid() == process.pid())
            .map(|handle| handle.workflow_id().clone())
            .ok_or_else(|| EngineSeamError::TimerWheel {
                reason: format!("unknown workflow process {}", process.pid()),
            })
    }
}

enum TimerOutcome {
    Fired(TimerId),
    Cancelled(TimerId),
}

impl EngineHandle for TimerNifBridge {
    fn resolve_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowResidency, EngineSeamError> {
        let handle = self
            .registry
            .list()
            .map_err(|error| EngineSeamError::Delivery {
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id);
        Ok(match handle {
            Some(handle) if handle.residency() == crate::HandleResidency::Resident => {
                WorkflowResidency::Resident(WorkflowProcessHandle::new(handle.pid()))
            }
            Some(_) => WorkflowResidency::NonResident,
            None => WorkflowResidency::Unknown,
        })
    }

    fn deliver_workflow_message(
        &self,
        process: WorkflowProcessHandle,
        message: WorkflowMailboxMessage,
    ) -> Result<(), EngineSeamError> {
        match message {
            WorkflowMailboxMessage::TimerFired { .. } => {
                // The fired terminal is already durably recorded
                // (record-before-deliver in `TimerService::fire_timer`), so
                // delivery is a pure wake: the suspended await re-runs its
                // two-phase resolution and reads the outcome from history.
                let nif_state =
                    self.nif_state
                        .upgrade()
                        .ok_or_else(|| EngineSeamError::Delivery {
                            reason: "engine NIF state has been dropped".to_owned(),
                        })?;
                let runtime =
                    super::nif_activity::runtime_context(&nif_state).map_err(|error| {
                        EngineSeamError::Delivery {
                            reason: error.to_string(),
                        }
                    })?;
                runtime
                    .runtime
                    .wake_workflow(process.pid())
                    .map_err(|error| EngineSeamError::Delivery {
                        reason: error.to_string(),
                    })
            }
            other => Err(EngineSeamError::Delivery {
                reason: format!("unsupported timer NIF bridge mailbox message: {other:?}"),
            }),
        }
    }

    fn spawn_child_workflow(
        &self,
        request: ChildWorkflowSpawnRequest,
    ) -> Result<ChildWorkflowSpawnResult, EngineSeamError> {
        let _ = request;
        Err(EngineSeamError::ChildSpawn {
            reason: "timer NIF bridge does not spawn child workflows".to_owned(),
        })
    }

    fn terminate_linked_child_workflow(
        &self,
        parent_workflow_id: &WorkflowId,
        child_process: WorkflowProcessHandle,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        let _ = (parent_workflow_id, child_process, correlation);
        Err(EngineSeamError::ChildTermination {
            reason: "timer NIF bridge does not terminate child workflows".to_owned(),
        })
    }

    fn terminate_linked_activity(
        &self,
        parent_workflow_id: &WorkflowId,
        activity_process: crate::Pid,
        correlation: u64,
    ) -> Result<(), EngineSeamError> {
        let _ = (parent_workflow_id, activity_process, correlation);
        Err(EngineSeamError::ChildTermination {
            reason: "timer NIF bridge does not terminate activities".to_owned(),
        })
    }

    fn arm_timer(&self, entry: TimerWheelEntry) -> Result<(), EngineSeamError> {
        let workflow_id = self.workflow_id_for_process(entry.process)?;
        let key = (entry.process, entry.timer_id.clone());
        if let Some((_, previous)) = self.pending_timers.remove(&key) {
            previous.handle.abort();
        }

        let fire_at = entry.fire_at;
        let timer_id = entry.timer_id.clone();
        let task_key = key.clone();
        let generation = self.next_timer_generation.fetch_add(1, Ordering::Relaxed);
        let delay = match (fire_at - Utc::now()).to_std() {
            Ok(delay) => delay,
            Err(_) => Duration::ZERO,
        };
        let nif_state = Weak::clone(&self.nif_state);
        let handle = self.tokio_handle.spawn(async move {
            tokio::time::sleep(delay).await;
            let bridge = nif_state
                .upgrade()
                .ok_or_else(|| "engine NIF state has been dropped".to_owned())
                .and_then(|state| timer_bridge(&state).map_err(|error| error.to_string()));
            let service = match &bridge {
                Ok(bridge) => bridge.service(),
                Err(error) => {
                    tracing::warn!(error = %error, "timer wheel could not resolve timer service");
                    return;
                }
            };
            if let Err(error) = service.fire_timer(workflow_id, timer_id, fire_at).await {
                tracing::warn!(error = %error, "timer wheel fire callback failed");
            }
            if let Ok(bridge) = bridge {
                if bridge
                    .pending_timers
                    .get(&task_key)
                    .is_some_and(|pending| pending.generation == generation)
                {
                    bridge.pending_timers.remove(&task_key);
                }
            }
        });
        self.pending_timers
            .insert(key, PendingTimerTask { generation, handle });
        Ok(())
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &TimerId,
    ) -> Result<(), EngineSeamError> {
        if let Some((_, pending)) = self.pending_timers.remove(&(process, timer_id.clone())) {
            pending.handle.abort();
        }
        Ok(())
    }

    fn record_workflow_event(
        &self,
        workflow_id: &WorkflowId,
        event: Event,
    ) -> Result<(), EngineSeamError> {
        let recorded_at = *event.recorded_at();
        let outcome = match event {
            Event::TimerFired { timer_id, .. } => TimerOutcome::Fired(timer_id),
            Event::TimerCancelled { timer_id, .. } => TimerOutcome::Cancelled(timer_id),
            other => {
                return Err(EngineSeamError::Recorder {
                    reason: format!("timer NIF bridge cannot record {}", event_kind(&other)),
                });
            }
        };
        let handle = self
            .registry
            .list()
            .map_err(|error| EngineSeamError::Recorder {
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|handle| handle.workflow_id() == workflow_id)
            .ok_or_else(|| EngineSeamError::UnknownWorkflow {
                workflow_id: workflow_id.clone(),
            })?;
        let recorder = handle.recorder();
        run_blocking(&self.tokio_handle, async {
            let mut recorder = recorder.lock().await;
            match outcome {
                TimerOutcome::Fired(timer_id) => {
                    recorder.record_timer_fired(recorded_at, timer_id).await
                }
                TimerOutcome::Cancelled(timer_id) => {
                    recorder.record_timer_cancelled(recorded_at, timer_id).await
                }
            }
        })
        .map_err(|error| EngineSeamError::Recorder {
            reason: error.to_string(),
        })
    }
}

/// Install the engine-scoped timer bridge used by raw NIF function pointers.
pub(crate) fn install_timer_nif_bridge(
    state: &Arc<EngineNifState>,
    registry: Arc<Registry>,
    store: Arc<dyn EventStore>,
    tokio_handle: Handle,
) {
    let store: Arc<dyn ReadableEventStore> = Arc::new(ReadableEventStoreAdapter { store });
    let bridge = Arc::new(TimerNifBridge {
        registry,
        store,
        tokio_handle,
        pending_timers: DashMap::new(),
        next_timer_generation: AtomicU64::new(0),
        nif_state: Arc::downgrade(state),
    });
    match state.timer_bridge.lock() {
        Ok(mut installed) => *installed = Some(bridge),
        Err(poisoned) => *poisoned.into_inner() = Some(bridge),
    }
}

pub(crate) fn installed_timer_service(state: &EngineNifState) -> Result<Arc<TimerService>, String> {
    timer_bridge(state)
        .map(|bridge| Arc::new(bridge.service()))
        .map_err(|error| error.to_string())
}

pub(super) fn timer_bridge(state: &EngineNifState) -> Result<Arc<TimerNifBridge>, NifTimerError> {
    state
        .timer_bridge
        .lock()
        .map_err(|_| NifTimerError::Context("timer bridge lock is poisoned".to_owned()))?
        .clone()
        .ok_or_else(|| NifTimerError::Context("timer bridge is not configured".to_owned()))
}

/// Drive a future to completion from synchronous bridge code.
///
/// Bridge methods are called both from dirty NIF threads (no ambient tokio
/// runtime — `block_on` directly) and from tasks spawned on the engine
/// runtime itself (the armed-timer fire path), where `Handle::block_on`
/// panics with "Cannot start a runtime from within a runtime". In that case
/// the wait moves to a scoped helper thread so the runtime stays free to
/// drive the future.
pub(super) fn run_blocking<T, F>(handle: &Handle, future: F) -> T
where
    T: Send,
    F: std::future::Future<Output = T> + Send,
{
    if Handle::try_current().is_err() {
        return handle.block_on(future);
    }
    std::thread::scope(
        |scope| match scope.spawn(|| handle.block_on(future)).join() {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        },
    )
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::TimerFired { .. } => "TimerFired",
        Event::TimerCancelled { .. } => "TimerCancelled",
        Event::WithTimeoutCompleted { .. } => "WithTimeoutCompleted",
        _ => "non-timer",
    }
}
