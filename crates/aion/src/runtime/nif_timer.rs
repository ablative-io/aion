//! Timer NIF implementations for the `aion_flow_ffi` namespace.

use std::time::Duration;

use aion_core::{Event, TimerId};
use beamr::native::ProcessContext;
use beamr::term::Term;
use chrono::{DateTime, Utc};

use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::runtime::engine_nifs::{decode_string_arg, error_result_term, ok_result_term};
use crate::runtime::nif_context::{NifContext, NifContextError};
use crate::runtime::nif_state::{EngineNifState, PendingAwait};
use crate::runtime::nif_timer_bridge::{run_blocking, timer_bridge};
use crate::time;

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
    // Queries first (Q6): the entry check runs on every invocation — fresh
    // and wake re-entry — before this await's own resolution. The servicing
    // refusal precedes the marker consumption so a refused handler call
    // never eats a wake; the recording guard itself re-fires inside
    // `timer_call` for the non-sleep timer NIFs.
    if let Ok(state) = super::nif_state::engine_nif_state(ctx)
        && let Some(pid) = ctx.pid()
    {
        if let Err(error) = super::nif_query_pump::ensure_not_servicing_query(&state, pid, "sleep")
        {
            return Ok(error_result_term(&error).unwrap_or(Term::NIL));
        }
        consume_one_wake_marker(ctx);
        if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(&state, pid) {
            return Ok(error_result_term(&sentinel).unwrap_or(Term::NIL));
        }
    } else {
        consume_one_wake_marker(ctx);
    }
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
            Some(PendingAwait::Child { .. }) => {
                return Err(NifTimerError::Context(
                    "sleep: process is pinned to a pending child await".to_owned(),
                ));
            }
            Some(PendingAwait::Collect { .. }) => {
                return Err(NifTimerError::Context(
                    "sleep: process is pinned to a pending collect await".to_owned(),
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
    // Every timer NIF records durable timer events; refuse while the caller
    // is servicing a query so handler misuse never writes history.
    if let Some(pid) = process_context.pid()
        && let Err(error) = super::nif_query_pump::ensure_not_servicing_query(&state, pid, name)
    {
        return Ok(error_result_term(&error).unwrap_or(Term::NIL));
    }
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
