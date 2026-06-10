//! Timer NIF implementations for the `aion_flow_ffi` namespace.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aion_core::{
    Event, Payload, TimerId, WithTimeoutOutcome, WorkflowFilter, WorkflowId, WorkflowSummary,
};
use aion_store::{EventStore, ReadableEventStore, RunSummary, StoreError, TimerEntry};
use beamr::native::stdlib_stubs::maps_bifs::ContinuationStep;
use beamr::native::{AionTimeoutContinuation, NativeContinuation, ProcessContext};
use beamr::term::Term;
use beamr::term::boxed::Closure;
use chrono::{DateTime, Utc};
use tokio::runtime::Handle;

use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::engine_seam::{
    ChildWorkflowSpawnRequest, ChildWorkflowSpawnResult, EngineHandle, EngineSeamError,
    TimerWheelEntry, WorkflowMailboxMessage, WorkflowProcessHandle, WorkflowResidency,
};
use crate::registry::Registry;
use crate::runtime::engine_nifs::{decode_string_arg, error_result_term, ok_result_term};
use crate::runtime::nif_context::{NifContext, NifContextError};
use crate::time::{self, TimerService};

static TIMER_BRIDGE: OnceLock<Mutex<Option<Arc<TimerNifBridge>>>> = OnceLock::new();
static TIMEOUT_CONTINUATIONS: OnceLock<Mutex<HashMap<u64, WithTimeoutState>>> = OnceLock::new();
static NEXT_TIMEOUT_CONTINUATION_ID: AtomicU64 = AtomicU64::new(1);

struct TimerNifBridge {
    registry: Arc<Registry>,
    store: Arc<dyn ReadableEventStore>,
    tokio_handle: Handle,
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
        TimerService::with_recorded_at(engine, store, deterministic_epoch)
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
        let _ = (process, message);
        Ok(())
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
        let _ = entry;
        Ok(())
    }

    fn disarm_timer(
        &self,
        process: WorkflowProcessHandle,
        timer_id: &TimerId,
    ) -> Result<(), EngineSeamError> {
        let _ = (process, timer_id);
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
        self.tokio_handle
            .block_on(async {
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

/// Install the process-wide timer bridge used by raw NIF function pointers.
pub(crate) fn install_timer_nif_bridge(
    registry: Arc<Registry>,
    store: Arc<dyn EventStore>,
    tokio_handle: Handle,
) {
    let store: Arc<dyn ReadableEventStore> = Arc::new(ReadableEventStoreAdapter { store });
    let bridge = Arc::new(TimerNifBridge {
        registry,
        store,
        tokio_handle,
    });
    let bridge_slot = TIMER_BRIDGE.get_or_init(|| Mutex::new(None));
    match bridge_slot.lock() {
        Ok(mut installed) => *installed = Some(bridge),
        Err(poisoned) => *poisoned.into_inner() = Some(bridge),
    }
}

/// NIF backing `aion_flow_ffi:sleep/1`.
pub(super) fn sleep_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("sleep", 1, args, ctx, |mut context, args| {
        let duration = decode_duration_arg("sleep duration", args[0])?;
        let recorded_now = recorded_now(&context)?;
        let timer_id = TimerId::anonymous(current_head(&context)?);
        let fire_at = add_duration(recorded_now, duration)?;
        match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
            ResolveOutcome::Recorded(Resolution::TimerFired) => Ok(ok_result("fired")),
            ResolveOutcome::Recorded(Resolution::TimerCancelled) => Ok(error_result("cancelled")),
            ResolveOutcome::Recorded(_) => Ok(error_result("sleep history mismatch")),
            ResolveOutcome::ResumeLive => {
                record_started(&context, recorded_now, timer_id.clone(), fire_at)?;
                schedule_sleep_timer(&context, duration, recorded_now, &timer_id, fire_at)?;
                wait_duration(duration)?;
                record_fired(&context, fire_at, timer_id)?;
                Ok(ok_result("fired"))
            }
        }
    })
}

/// NIF backing `aion_flow_ffi:start_timer/2`.
pub(super) fn start_timer_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("start_timer", 2, args, ctx, |mut context, args| {
        let timer_id = decode_timer_id_arg("start_timer timer_id", args[0])?;
        let duration = decode_duration_arg("start_timer duration", args[1])?;
        let recorded_now = recorded_now(&context)?;
        let fire_at = add_duration(recorded_now, duration)?;
        match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
            ResolveOutcome::Recorded(Resolution::TimerStarted | Resolution::TimerFired) => {
                Ok(ok_result(timer_ref(&timer_id)))
            }
            ResolveOutcome::Recorded(Resolution::TimerCancelled) => Ok(error_result("cancelled")),
            ResolveOutcome::Recorded(_) => Ok(error_result("start_timer history mismatch")),
            ResolveOutcome::ResumeLive => {
                record_started(&context, recorded_now, timer_id.clone(), fire_at)?;
                schedule_timer(&context, timer_id.clone(), fire_at, TimerKind::Named)?;
                Ok(ok_result(timer_ref(&timer_id)))
            }
        }
    })
}

/// NIF backing `aion_flow_ffi:cancel_timer/1`.
pub(super) fn cancel_timer_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("cancel_timer", 1, args, ctx, |mut context, args| {
        let timer_id = decode_timer_id_arg("cancel_timer timer_id", args[0])?;
        let recorded_now = recorded_now(&context)?;
        match context.resolve_command(timer_command(timer_id.clone(), recorded_now))? {
            ResolveOutcome::Recorded(Resolution::TimerCancelled | Resolution::TimerFired) => {
                Ok(ok_result("cancelled"))
            }
            ResolveOutcome::Recorded(_) => Ok(error_result("cancel_timer history mismatch")),
            ResolveOutcome::ResumeLive => {
                cancel_live_timer(&context, timer_id)?;
                Ok(ok_result("cancelled"))
            }
        }
    })
}

/// NIF backing `aion_flow_ffi:with_timeout/2`.
pub(super) fn with_timeout_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Ok(error_result_term(&format!(
            "with_timeout: expected 2 arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }

    match with_timeout_prepare(args, ctx) {
        Ok(WithTimeoutPrepare::Replay(term)) => Ok(term),
        Ok(WithTimeoutPrepare::Call { operation, state }) => {
            let state_id = store_timeout_state(state)
                .map_err(|error| error_result_term(&error.to_string()).unwrap_or(Term::NIL))?;
            ctx.set_continuation_trampoline(
                operation,
                Vec::new(),
                NativeContinuation::AionTimeout(AionTimeoutContinuation {
                    state_id,
                    resume: resume_with_timeout,
                }),
            );
            Ok(Term::NIL)
        }
        Err(error) => Ok(error_result_term(&error.to_string()).unwrap_or(Term::NIL)),
    }
}

enum WithTimeoutPrepare {
    Replay(Term),
    Call {
        operation: Term,
        state: WithTimeoutState,
    },
}

#[derive(Clone, Debug)]
struct WithTimeoutState {
    timer_id: TimerId,
    recorded_now: DateTime<Utc>,
    fire_at: DateTime<Utc>,
    duration: Duration,
}

fn with_timeout_prepare(
    args: &[Term],
    process_context: &mut ProcessContext,
) -> Result<WithTimeoutPrepare, NifTimerError> {
    let duration = decode_duration_arg("with_timeout duration", args[0])?;
    ensure_zero_arity_callable(args[1])?;
    let context = build_context(process_context)?;
    let recorded_now = recorded_now(&context)?;
    let timer_id = TimerId::anonymous(current_head(&context)?);
    let fire_at = add_duration(recorded_now, duration)?;
    match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
        ResolveOutcome::Recorded(Resolution::WithTimeout { outcome, result }) => {
            Ok(WithTimeoutPrepare::Replay(replay_with_timeout_result(
                outcome,
                result,
                process_context,
            )?))
        }
        ResolveOutcome::Recorded(Resolution::TimerFired) => Ok(WithTimeoutPrepare::Replay(
            error_result_term("timeout").unwrap_or(Term::NIL),
        )),
        ResolveOutcome::Recorded(_) => Ok(WithTimeoutPrepare::Replay(
            error_result_term("with_timeout history mismatch").unwrap_or(Term::NIL),
        )),
        ResolveOutcome::ResumeLive => {
            record_started(&context, recorded_now, timer_id.clone(), fire_at)?;
            schedule_sleep_timer(&context, duration, recorded_now, &timer_id, fire_at)?;
            Ok(WithTimeoutPrepare::Call {
                operation: args[1],
                state: WithTimeoutState {
                    timer_id,
                    recorded_now,
                    fire_at,
                    duration,
                },
            })
        }
    }
}

fn timer_call<F>(
    name: &str,
    arity: usize,
    args: &[Term],
    process_context: &mut ProcessContext,
    f: F,
) -> Result<Term, Term>
where
    F: FnOnce(NifContext, &[Term]) -> Result<NifResult, NifTimerError>,
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
    match build_context(process_context).and_then(|context| f(context, args)) {
        Ok(NifResult::Ok(value)) => Ok(ok_result_term(&value).unwrap_or(Term::NIL)),
        Ok(NifResult::Error(message)) => Ok(error_result_term(&message).unwrap_or(Term::NIL)),
        Err(error) => Ok(error_result_term(&error.to_string()).unwrap_or(Term::NIL)),
    }
}

fn ensure_zero_arity_callable(term: Term) -> Result<(), NifTimerError> {
    let closure = Closure::new(term).ok_or_else(|| {
        NifTimerError::Argument("with_timeout operation: expected a callable function".to_owned())
    })?;
    if closure.arity() == 0 {
        Ok(())
    } else {
        Err(NifTimerError::Argument(format!(
            "with_timeout operation: expected arity 0 function, got arity {}",
            closure.arity()
        )))
    }
}

fn store_timeout_state(state: WithTimeoutState) -> Result<u64, NifTimerError> {
    let id = NEXT_TIMEOUT_CONTINUATION_ID.fetch_add(1, Ordering::Relaxed);
    timeout_states()?
        .lock()
        .map_err(|_| {
            NifTimerError::Context("with_timeout continuation lock is poisoned".to_owned())
        })?
        .insert(id, state);
    Ok(id)
}

fn take_timeout_state(id: u64) -> Result<WithTimeoutState, NifTimerError> {
    timeout_states()?
        .lock()
        .map_err(|_| {
            NifTimerError::Context("with_timeout continuation lock is poisoned".to_owned())
        })?
        .remove(&id)
        .ok_or_else(|| {
            NifTimerError::Context("with_timeout continuation state is missing".to_owned())
        })
}

fn timeout_states() -> Result<&'static Mutex<HashMap<u64, WithTimeoutState>>, NifTimerError> {
    Ok(TIMEOUT_CONTINUATIONS.get_or_init(|| Mutex::new(HashMap::new())))
}

fn build_context(process_context: &ProcessContext) -> Result<NifContext, NifTimerError> {
    let pid = process_context
        .pid()
        .ok_or_else(|| NifTimerError::Context("missing calling pid".to_owned()))?;
    let bridge = timer_bridge()?;
    NifContext::new(pid, bridge.registry.as_ref(), bridge.tokio_handle.clone()).map_err(Into::into)
}

fn decode_duration_arg(label: &str, term: Term) -> Result<Duration, NifTimerError> {
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

fn timer_command(timer_id: TimerId, fire_at: DateTime<Utc>) -> Command {
    Command::StartTimer {
        key: CorrelationKey::Timer(timer_id),
        fire_at,
    }
}

fn recorded_now(context: &NifContext) -> Result<DateTime<Utc>, NifTimerError> {
    context
        .block_on_recorder(|recorder| {
            Box::pin(async move {
                let history = recorder.read_history().await?;
                Ok(history
                    .last()
                    .map(Event::recorded_at)
                    .copied()
                    .unwrap_or(DateTime::<Utc>::UNIX_EPOCH))
            })
        })
        .map_err(Into::into)
}

fn current_head(context: &NifContext) -> Result<u64, NifTimerError> {
    context
        .block_on_recorder(|recorder| Box::pin(async move { Ok(recorder.current_head()) }))
        .map_err(Into::into)
}

fn add_duration(
    recorded_now: DateTime<Utc>,
    duration: Duration,
) -> Result<DateTime<Utc>, NifTimerError> {
    let chrono_duration = chrono::Duration::from_std(duration)
        .map_err(|_| NifTimerError::Argument("duration is out of range".to_owned()))?;
    recorded_now
        .checked_add_signed(chrono_duration)
        .ok_or_else(|| NifTimerError::Argument("timer fire_at overflowed".to_owned()))
}

fn resume_with_timeout(
    continuation: AionTimeoutContinuation,
    closure_result: Term,
    context: &mut ProcessContext,
) -> Result<ContinuationStep, Term> {
    let result = resume_with_timeout_inner(continuation, closure_result, context)
        .unwrap_or_else(|error| error_result_term(&error.to_string()).unwrap_or(Term::NIL));
    Ok(ContinuationStep::Done(result))
}

fn resume_with_timeout_inner(
    continuation: AionTimeoutContinuation,
    closure_result: Term,
    context: &mut ProcessContext,
) -> Result<Term, NifTimerError> {
    let state = take_timeout_state(continuation.state_id)?;
    let nif_context = build_context(context)?;
    if state.duration.is_zero() {
        record_with_timeout_completed(
            &nif_context,
            state.fire_at,
            state.timer_id,
            WithTimeoutOutcome::TimedOut,
            None,
        )?;
        Ok(error_result_term("timeout").unwrap_or(Term::NIL))
    } else {
        let payload = encode_term_payload(closure_result, context)?;
        record_with_timeout_completed(
            &nif_context,
            state.recorded_now,
            state.timer_id,
            WithTimeoutOutcome::OperationCompleted,
            Some(payload),
        )?;
        Ok(ok_term(closure_result, context))
    }
}

fn encode_term_payload(term: Term, context: &ProcessContext) -> Result<Payload, NifTimerError> {
    let atom_table = context.atom_table().ok_or_else(|| {
        NifTimerError::Context("with_timeout replay requires an atom table".to_owned())
    })?;
    let value = beamr::term::json::term_to_value(term, atom_table).map_err(|error| {
        NifTimerError::Context(format!("with_timeout result encoding: {error}"))
    })?;
    Payload::from_json(&value)
        .map_err(|error| NifTimerError::Context(format!("with_timeout result payload: {error}")))
}

fn decode_term_payload(
    payload: Payload,
    context: &mut ProcessContext,
) -> Result<Term, NifTimerError> {
    let value = payload
        .to_json()
        .map_err(|error| NifTimerError::Context(format!("with_timeout replay payload: {error}")))?;
    beamr::term::json::value_to_term(&value, context).map_err(|error| {
        NifTimerError::Context(format!("with_timeout replay term decoding: {error}"))
    })
}

fn replay_with_timeout_result(
    outcome: WithTimeoutOutcome,
    result: Option<Payload>,
    context: &mut ProcessContext,
) -> Result<Term, NifTimerError> {
    match outcome {
        WithTimeoutOutcome::TimedOut => Ok(error_result_term("timeout").unwrap_or(Term::NIL)),
        WithTimeoutOutcome::OperationCompleted => {
            let payload = result.ok_or_else(|| {
                NifTimerError::Context(
                    "with_timeout operation outcome missing result payload".to_owned(),
                )
            })?;
            let value = decode_term_payload(payload, context)?;
            Ok(ok_term(value, context))
        }
    }
}

fn ok_term(value: Term, context: &mut ProcessContext) -> Term {
    context
        .alloc_tuple(&[Term::atom(beamr::atom::Atom::OK), value])
        .unwrap_or(Term::NIL)
}

fn record_started(
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

fn record_with_timeout_completed(
    context: &NifContext,
    recorded_at: DateTime<Utc>,
    timer_id: TimerId,
    outcome: WithTimeoutOutcome,
    result: Option<Payload>,
) -> Result<(), NifTimerError> {
    context
        .block_on_recorder(|recorder| {
            Box::pin(async move {
                recorder
                    .record_with_timeout_completed(recorded_at, timer_id, outcome, result)
                    .await
            })
        })
        .map_err(Into::into)
}

fn record_fired(
    context: &NifContext,
    recorded_at: DateTime<Utc>,
    timer_id: TimerId,
) -> Result<(), NifTimerError> {
    context
        .block_on_recorder(|recorder| {
            Box::pin(async move { recorder.record_timer_fired(recorded_at, timer_id).await })
        })
        .map_err(Into::into)
}

fn record_cancelled(
    context: &NifContext,
    recorded_at: DateTime<Utc>,
    timer_id: TimerId,
) -> Result<(), NifTimerError> {
    context
        .block_on_recorder(|recorder| {
            Box::pin(async move { recorder.record_timer_cancelled(recorded_at, timer_id).await })
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

fn schedule_sleep_timer(
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
    let scheduled = schedule_timer(context, timer_id.clone(), fire_at, kind)?;
    if scheduled == *timer_id {
        Ok(())
    } else {
        Err(NifTimerError::Context(
            "AT sleep service returned a mismatched timer id".to_owned(),
        ))
    }
}

fn schedule_timer(
    context: &NifContext,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
    kind: TimerKind,
) -> Result<TimerId, NifTimerError> {
    let workflow_id = context.workflow_id().clone();
    let bridge = timer_bridge()?;
    let service = bridge.service();
    let scheduled = bridge.tokio_handle.block_on(async {
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

fn cancel_live_timer(context: &NifContext, timer_id: TimerId) -> Result<(), NifTimerError> {
    let workflow_id = context.workflow_id().clone();
    let bridge = timer_bridge()?;
    let service = bridge.service();
    bridge
        .tokio_handle
        .block_on(async { time::cancel_timer(&service, workflow_id, timer_id).await })?;
    Ok(())
}

fn wait_duration(duration: Duration) -> Result<(), NifTimerError> {
    let bridge = timer_bridge()?;
    bridge.tokio_handle.block_on(tokio::time::sleep(duration));
    Ok(())
}

fn timer_bridge() -> Result<Arc<TimerNifBridge>, NifTimerError> {
    let bridge_slot = TIMER_BRIDGE
        .get()
        .ok_or_else(|| NifTimerError::Context("timer bridge is not configured".to_owned()))?;
    bridge_slot
        .lock()
        .map_err(|_| NifTimerError::Context("timer bridge lock is poisoned".to_owned()))?
        .clone()
        .ok_or_else(|| NifTimerError::Context("timer bridge is not configured".to_owned()))
}

fn deterministic_epoch() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
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
}

#[derive(thiserror::Error, Debug)]
enum NifTimerError {
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
