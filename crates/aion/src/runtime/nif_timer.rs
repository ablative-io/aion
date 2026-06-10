//! Timer NIF implementations for the `aion_flow_ffi` namespace.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use aion_core::{Event, TimerId, WorkflowFilter, WorkflowId, WorkflowSummary};
use aion_store::{EventStore, ReadableEventStore, RunSummary, StoreError, TimerEntry};
use beamr::native::ProcessContext;
use beamr::term::Term;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::registry::Registry;
use crate::runtime::engine_nifs::{decode_string_arg, error_result_term, ok_result_term};
use crate::runtime::nif_context::{NifContext, NifContextError};
use crate::runtime::nif_state::{EngineNifState, PendingAwait};
use crate::time::{self, TimerService};

pub(super) struct TimerNifBridge {
    registry: Arc<Registry>,
    store: Arc<dyn ReadableEventStore>,
    tokio_handle: Handle,
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
    fn service(self: &Arc<Self>) -> TimerService {
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

/// NIF backing `aion_flow_ffi:sleep/1`.
///
/// Two-phase suspend: the native never blocks a scheduler thread. On first
/// live arrival it records `TimerStarted`, schedules the durable timer, pins
/// the await identity, and parks the process via `request_suspend`. Every
/// mailbox wake re-enters this native, which consumes one wake marker and
/// re-runs the resolution: a recorded `TimerFired`/`TimerCancelled` settles
/// the sleep; an expired enclosing `with_timeout` deadline cancels it
/// durably; anything else parks again.
pub(super) fn sleep_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    consume_one_wake_marker(ctx);
    timer_call("sleep", 1, args, ctx, |state, mut context, args| {
        let pid = context.pid();
        let duration = decode_duration_arg("sleep duration", args[0])?;
        let pinned = match state.pending_awaits.get(&pid).map(|entry| entry.clone()) {
            Some(PendingAwait::Sleep { timer_id, fire_at }) => Some((timer_id, fire_at)),
            Some(PendingAwait::Signal { .. }) => {
                return Err(NifTimerError::Context(
                    "sleep: process is pinned to a pending signal await".to_owned(),
                ));
            }
            None => None,
        };
        let first_arrival = pinned.is_none();
        let (timer_id, fire_at) = if let Some(parts) = pinned {
            parts
        } else {
            let now = recorded_now(&context);
            let timer_id = TimerId::anonymous(context.next_timer_ordinal());
            (timer_id, add_duration(now, duration)?)
        };
        match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
            ResolveOutcome::Recorded(Resolution::TimerFired) => {
                state.pending_awaits.remove(&pid);
                Ok(ok_result("fired"))
            }
            ResolveOutcome::Recorded(Resolution::TimerCancelled) => {
                state.pending_awaits.remove(&pid);
                Ok(error_result("cancelled"))
            }
            ResolveOutcome::Recorded(Resolution::TimerStarted) => {
                // The start is recorded but its terminal is not the
                // immediately following event (asynchronous arrivals
                // interleave); read the terminal from the run segment.
                match timer_terminal_recorded(&context, &timer_id) {
                    Some(true) => {
                        state.pending_awaits.remove(&pid);
                        Ok(ok_result("fired"))
                    }
                    Some(false) => {
                        state.pending_awaits.remove(&pid);
                        Ok(error_result("cancelled"))
                    }
                    // Mid-await: a woken re-entry of a live sleep, or
                    // recovery replay of a sleep whose durable timer the
                    // recovery pass re-armed. Park again.
                    None => park_sleep(state, &context, pid, timer_id, fire_at),
                }
            }
            ResolveOutcome::Recorded(_) => Ok(error_result("sleep history mismatch")),
            ResolveOutcome::ResumeLive => {
                if first_arrival {
                    let now = recorded_now(&context);
                    record_started(&context, now, timer_id.clone(), fire_at)?;
                    schedule_sleep_timer(state, &context, duration, now, &timer_id, fire_at)?;
                }
                park_sleep(state, &context, pid, timer_id, fire_at)
            }
        }
    })
}

/// Park the calling process on its pinned sleep, unless an enclosing
/// `with_timeout` deadline already expired — then the sleep is cancelled
/// durably so replay returns the same error.
fn park_sleep(
    state: &EngineNifState,
    context: &NifContext,
    pid: u64,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
) -> Result<NifResult, NifTimerError> {
    if super::nif_timeout::expired_scope_message(state, pid).is_some() {
        cancel_live_timer(state, context, timer_id)?;
        state.pending_awaits.remove(&pid);
        return Ok(error_result("cancelled"));
    }
    state
        .pending_awaits
        .insert(pid, PendingAwait::Sleep { timer_id, fire_at });
    Ok(NifResult::Suspend)
}

/// Consume one queued aion wake marker before a suspending await re-runs.
///
/// State recovery errors are deliberately not surfaced here: the await body
/// recovers the same state next and reports the failure as its result.
fn consume_one_wake_marker(ctx: &mut ProcessContext) {
    if let Ok(state) = super::nif_state::engine_nif_state(ctx)
        && let Ok(runtime) = super::nif_activity::runtime_context(&state)
    {
        super::nif_wake::consume_wake_marker(ctx, &runtime.runtime);
    }
}

/// NIF backing `aion_flow_ffi:start_timer/2`.
pub(super) fn start_timer_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("start_timer", 2, args, ctx, |state, mut context, args| {
        let timer_id = decode_timer_id_arg("start_timer timer_id", args[0])?;
        let duration = decode_duration_arg("start_timer duration", args[1])?;
        let recorded_now = recorded_now(&context);
        let fire_at = add_duration(recorded_now, duration)?;
        match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
            ResolveOutcome::Recorded(Resolution::TimerStarted | Resolution::TimerFired) => {
                Ok(ok_result(timer_ref(&timer_id)))
            }
            ResolveOutcome::Recorded(Resolution::TimerCancelled) => Ok(error_result("cancelled")),
            ResolveOutcome::Recorded(_) => Ok(error_result("start_timer history mismatch")),
            ResolveOutcome::ResumeLive => {
                record_started(&context, recorded_now, timer_id.clone(), fire_at)?;
                schedule_timer(state, &context, timer_id.clone(), fire_at, TimerKind::Named)?;
                Ok(ok_result(timer_ref(&timer_id)))
            }
        }
    })
}

/// NIF backing `aion_flow_ffi:cancel_timer/1`.
pub(super) fn cancel_timer_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("cancel_timer", 1, args, ctx, |state, mut context, args| {
        let timer_id = decode_timer_id_arg("cancel_timer timer_id", args[0])?;
        let recorded_now = recorded_now(&context);
        match context.resolve_command(timer_command(timer_id.clone(), recorded_now))? {
            ResolveOutcome::Recorded(Resolution::TimerCancelled | Resolution::TimerFired) => {
                Ok(ok_result("cancelled"))
            }
            ResolveOutcome::Recorded(_) => Ok(error_result("cancel_timer history mismatch")),
            ResolveOutcome::ResumeLive => {
                cancel_live_timer(state, &context, timer_id)?;
                Ok(ok_result("cancelled"))
            }
        }
    })
}

fn timer_call<F>(
    name: &str,
    arity: usize,
    args: &[Term],
    process_context: &mut ProcessContext,
    f: F,
) -> Result<Term, Term>
where
    F: FnOnce(&EngineNifState, NifContext, &[Term]) -> Result<NifResult, NifTimerError>,
{
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != arity {
        return Ok(error_result_term(&format!(
            "{name}: expected {arity} arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }
    let state = match super::nif_state::engine_nif_state(process_context) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match build_context(&state, process_context).and_then(|context| f(&state, context, args)) {
        Ok(NifResult::Ok(value)) => Ok(ok_result_term(&value).unwrap_or(Term::NIL)),
        Ok(NifResult::Error(message)) => Ok(error_result_term(&message).unwrap_or(Term::NIL)),
        Ok(NifResult::Suspend) => {
            // Park the process; the next mailbox wake re-invokes this
            // native from the top with the await identity pinned in
            // `EngineNifState::pending_awaits`. The NIL return is never
            // observed by workflow code.
            process_context.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(error) => Ok(error_result_term(&error.to_string()).unwrap_or(Term::NIL)),
    }
}

fn build_context(
    state: &EngineNifState,
    process_context: &ProcessContext,
) -> Result<NifContext, NifTimerError> {
    let pid = process_context
        .pid()
        .ok_or_else(|| NifTimerError::Context("missing calling pid".to_owned()))?;
    build_context_for_pid(state, pid)
}

/// Build a [`NifContext`] for a workflow process by pid.
pub(super) fn build_context_for_pid(
    state: &EngineNifState,
    pid: u64,
) -> Result<NifContext, NifTimerError> {
    let bridge = timer_bridge(state)?;
    NifContext::new(pid, bridge.registry.as_ref(), bridge.tokio_handle.clone()).map_err(Into::into)
}

pub(super) fn decode_duration_arg(label: &str, term: Term) -> Result<Duration, NifTimerError> {
    let text = decode_string_arg(term).map_err(NifTimerError::Argument)?;
    let millis = text
        .parse::<u64>()
        .map_err(|error| NifTimerError::Argument(format!("{label}: {error}")))?;
    Ok(Duration::from_millis(millis))
}

fn decode_timer_id_arg(label: &str, term: Term) -> Result<TimerId, NifTimerError> {
    let raw = decode_string_arg(term).map_err(NifTimerError::Argument)?;
    let name = raw.strip_prefix("timer:named:").unwrap_or(raw.as_str());
    TimerId::named(name).map_err(|error| NifTimerError::Argument(format!("{label}: {error}")))
}

fn timer_ref(timer_id: &TimerId) -> &str {
    timer_id.name().unwrap_or("anonymous")
}

pub(super) fn timer_command(timer_id: TimerId, fire_at: DateTime<Utc>) -> Command {
    Command::StartTimer {
        key: CorrelationKey::Timer(timer_id),
        fire_at,
    }
}

/// Deterministic workflow-visible "now": the timestamp of the last event in
/// the run segment the calling context resolved against.
pub(super) fn recorded_now(context: &NifContext) -> DateTime<Utc> {
    context
        .last_recorded_at()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// The recorded terminal for `timer_id` in this run's segment, if any.
///
/// `Some(true)` means `TimerFired`, `Some(false)` means `TimerCancelled`,
/// `None` means the timer is still pending.
pub(super) fn timer_terminal_recorded(context: &NifContext, timer_id: &TimerId) -> Option<bool> {
    context
        .history()
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::TimerFired {
                timer_id: recorded, ..
            } if recorded == timer_id => Some(true),
            Event::TimerCancelled {
                timer_id: recorded, ..
            } if recorded == timer_id => Some(false),
            _ => None,
        })
}

pub(super) fn add_duration(
    recorded_now: DateTime<Utc>,
    duration: Duration,
) -> Result<DateTime<Utc>, NifTimerError> {
    let chrono_duration = chrono::Duration::from_std(duration)
        .map_err(|_| NifTimerError::Argument("duration is out of range".to_owned()))?;
    recorded_now
        .checked_add_signed(chrono_duration)
        .ok_or_else(|| NifTimerError::Argument("timer fire_at overflowed".to_owned()))
}

pub(super) fn record_started(
    context: &NifContext,
    recorded_at: DateTime<Utc>,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
) -> Result<(), NifTimerError> {
    context
        .block_on_recorder(|recorder| {
            Box::pin(async move {
                recorder
                    .record_timer_started(recorded_at, timer_id, fire_at)
                    .await
            })
        })
        .map_err(Into::into)
}

#[derive(Clone, Copy)]
enum TimerKind {
    Anonymous {
        duration: Duration,
        recorded_now: DateTime<Utc>,
    },
    Named,
}

pub(super) fn schedule_sleep_timer(
    state: &EngineNifState,
    context: &NifContext,
    duration: Duration,
    recorded_now: DateTime<Utc>,
    timer_id: &TimerId,
    fire_at: DateTime<Utc>,
) -> Result<(), NifTimerError> {
    let kind = TimerKind::Anonymous {
        duration,
        recorded_now,
    };
    let scheduled = schedule_timer(state, context, timer_id.clone(), fire_at, kind)?;
    if scheduled == *timer_id {
        Ok(())
    } else {
        Err(NifTimerError::Context(
            "AT sleep service returned a mismatched timer id".to_owned(),
        ))
    }
}

fn schedule_timer(
    state: &EngineNifState,
    context: &NifContext,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
    kind: TimerKind,
) -> Result<TimerId, NifTimerError> {
    let workflow_id = context.workflow_id().clone();
    let bridge = timer_bridge(state)?;
    let service = bridge.service();
    let scheduled = run_blocking(&bridge.tokio_handle, async {
        match kind {
            TimerKind::Anonymous {
                duration,
                recorded_now,
            } => time::sleep(
                &service,
                workflow_id,
                duration,
                recorded_now,
                timer_id.sequence_position().ok_or_else(|| {
                    NifTimerError::Context("sleep timer id is not anonymous".to_owned())
                })?,
            )
            .await
            .map(|sleep| sleep.timer_id)
            .map_err(NifTimerError::from),
            TimerKind::Named => time::start_timer(&service, workflow_id, timer_id.clone(), fire_at)
                .await
                .map(|()| timer_id)
                .map_err(NifTimerError::from),
        }
    })?;
    Ok(scheduled)
}

pub(super) fn cancel_live_timer(
    state: &EngineNifState,
    context: &NifContext,
    timer_id: TimerId,
) -> Result<(), NifTimerError> {
    let workflow_id = context.workflow_id().clone();
    let bridge = timer_bridge(state)?;
    let service = bridge.service();
    run_blocking(&bridge.tokio_handle, async {
        time::cancel_timer(&service, workflow_id, timer_id).await
    })?;
    Ok(())
}

/// Drive a future to completion from synchronous bridge code.
///
/// Bridge methods are called both from dirty NIF threads (no ambient tokio
/// runtime — `block_on` directly) and from tasks spawned on the engine
/// runtime itself (the armed-timer fire path), where `Handle::block_on`
/// panics with "Cannot start a runtime from within a runtime". In that case
/// the wait moves to a scoped helper thread so the runtime stays free to
/// drive the future.
fn run_blocking<T, F>(handle: &Handle, future: F) -> T
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

fn timer_bridge(state: &EngineNifState) -> Result<Arc<TimerNifBridge>, NifTimerError> {
    state
        .timer_bridge
        .lock()
        .map_err(|_| NifTimerError::Context("timer bridge lock is poisoned".to_owned()))?
        .clone()
        .ok_or_else(|| NifTimerError::Context("timer bridge is not configured".to_owned()))
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::TimerFired { .. } => "TimerFired",
        Event::TimerCancelled { .. } => "TimerCancelled",
        Event::WithTimeoutCompleted { .. } => "WithTimeoutCompleted",
        _ => "non-timer",
    }
}

fn ok_result(value: impl Into<String>) -> NifResult {
    NifResult::Ok(value.into())
}

fn error_result(value: impl Into<String>) -> NifResult {
    NifResult::Error(value.into())
}

enum NifResult {
    Ok(String),
    Error(String),
    /// Park the calling process via `request_suspend`; a mailbox wake
    /// re-invokes the native to re-run its two-phase resolution.
    Suspend,
}

#[derive(thiserror::Error, Debug)]
pub(super) enum NifTimerError {
    #[error("argument:{0}")]
    Argument(String),
    #[error("context:{0}")]
    Context(String),
    #[error("durability:{0}")]
    Durability(#[from] DurabilityError),
    #[error("timer:{0}")]
    Timer(#[from] crate::time::TimerServiceError),
    #[error("sleep:{0}")]
    Sleep(#[from] crate::time::SleepTimerError),
}

impl From<NifContextError> for NifTimerError {
    fn from(error: NifContextError) -> Self {
        match error {
            NifContextError::Durability(error) => Self::Durability(error),
            other => Self::Context(other.to_string()),
        }
    }
}
