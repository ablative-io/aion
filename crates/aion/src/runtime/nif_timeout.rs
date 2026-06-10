//! `aion_flow_ffi:with_timeout/2`: durable deadline scopes racing a closure.
//!
//! The NIF arms a durable deadline timer, then runs the wrapped zero-arity
//! closure on the workflow process via beamr's native-continuation
//! trampoline. The race is settled by the scope timer's recorded terminal
//! event — `TimerService` arbitrates fire-versus-cancel with
//! first-recorded-wins — so `TimerCancelled` means the operation won and
//! `TimerFired` means the deadline won. Replay re-runs the closure (its
//! inner commands replay from history) and derives the same outcome from
//! the same recorded event, so live and replay agree by construction.
//!
//! While a live scope is armed, blocking awaits inside the closure observe
//! the deadline: the timer bridge wakes the workflow process when a scope
//! deadline fires, sleeping timers return cancelled, and activity awaits
//! record a durable timeout failure instead of re-suspending.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use aion_core::{Event, TimerId};
use beamr::atom::Atom;
use beamr::native::stdlib_stubs::maps_bifs::ContinuationStep;
use beamr::native::{AionTimeoutContinuation, NativeContinuation, ProcessContext};
use beamr::term::Term;
use dashmap::DashMap;

use crate::durability::{Resolution, ResolveOutcome};
use crate::runtime::engine_nifs::error_result_term;
use crate::runtime::nif_context::NifContext;
use crate::runtime::nif_timer::{
    add_duration, build_context_for_pid, cancel_live_timer, current_head, decode_duration_arg,
    drain_timer_deliveries, record_started, recorded_now, schedule_sleep_timer, timer_command,
};

/// One armed `with_timeout` deadline scope.
struct TimeoutScope {
    /// Workflow process the scope belongs to.
    pid: u64,
    /// Durable deadline timer racing the wrapped operation.
    timer_id: TimerId,
    /// `Some(timed_out)` when this run replays a recorded outcome and no
    /// live timer was armed; `None` on the live path.
    replay_timed_out: Option<bool>,
}

static SCOPES: OnceLock<DashMap<u64, TimeoutScope>> = OnceLock::new();
static SCOPE_STACKS: OnceLock<DashMap<u64, Vec<u64>>> = OnceLock::new();
static NEXT_SCOPE_ID: AtomicU64 = AtomicU64::new(1);

fn scopes() -> &'static DashMap<u64, TimeoutScope> {
    SCOPES.get_or_init(DashMap::new)
}

fn scope_stacks() -> &'static DashMap<u64, Vec<u64>> {
    SCOPE_STACKS.get_or_init(DashMap::new)
}

/// Deadline timer ids of every live scope currently armed for `pid`,
/// innermost last. Replay scopes are excluded — their outcome is recorded.
pub(crate) fn active_scope_timers(pid: u64) -> Vec<TimerId> {
    let Some(stack) = scope_stacks().get(&pid) else {
        return Vec::new();
    };
    stack
        .iter()
        .filter_map(|state_id| {
            scopes().get(state_id).and_then(|scope| {
                scope
                    .replay_timed_out
                    .is_none()
                    .then(|| scope.timer_id.clone())
            })
        })
        .collect()
}

/// True when `timer_id` is the deadline of a live scope armed for `pid`.
pub(crate) fn is_scope_deadline(pid: u64, timer_id: &TimerId) -> bool {
    active_scope_timers(pid)
        .iter()
        .any(|scope_timer| scope_timer == timer_id)
}

/// Return the message recorded for an expired enclosing scope, if any.
///
/// Blocking awaits call this after waking: a `TimerFired` recorded for any
/// live enclosing scope means the deadline won and the await must abort.
pub(crate) fn expired_scope_message(pid: u64) -> Option<String> {
    let scope_timers = active_scope_timers(pid);
    if scope_timers.is_empty() {
        return None;
    }
    let context = build_context_for_pid(pid).ok()?;
    scope_timers
        .iter()
        .any(|timer_id| timer_fired_recorded(&context, timer_id).unwrap_or(false))
        .then(|| "timeout:deadline expired".to_owned())
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
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term("with_timeout: missing calling process pid").unwrap_or(Term::NIL),
        );
    };
    match arm_scope(args, pid) {
        Ok((fun, state_id)) => {
            ctx.set_continuation_trampoline(
                fun,
                Vec::new(),
                NativeContinuation::AionTimeout(AionTimeoutContinuation {
                    state_id,
                    // Non-capturing closure coercing to the required fn
                    // pointer; the implementation borrows the state.
                    resume: |state, closure_result, ctx| {
                        resume_with_timeout(&state, closure_result, ctx)
                    },
                }),
            );
            Ok(Term::NIL)
        }
        Err(message) => Ok(error_result_term(&message).unwrap_or(Term::NIL)),
    }
}

/// Resolve the scope's durable timer command and register the scope.
fn arm_scope(args: &[Term], pid: u64) -> Result<(Term, u64), String> {
    let duration =
        decode_duration_arg("with_timeout deadline", args[0]).map_err(|error| error.to_string())?;
    let fun = args[1];
    let mut context = build_context_for_pid(pid).map_err(|error| error.to_string())?;
    let now = recorded_now(&context).map_err(|error| error.to_string())?;
    let timer_id = TimerId::anonymous(current_head(&context).map_err(|error| error.to_string())?);
    let fire_at = add_duration(now, duration).map_err(|error| error.to_string())?;
    let replay_timed_out = match context
        .resolve_command(timer_command(timer_id.clone(), fire_at))
        .map_err(|error| error.to_string())?
    {
        ResolveOutcome::Recorded(Resolution::TimerFired) => Some(true),
        ResolveOutcome::Recorded(Resolution::TimerCancelled) => Some(false),
        ResolveOutcome::Recorded(_) => return Err("with_timeout history mismatch".to_owned()),
        ResolveOutcome::ResumeLive => {
            record_started(&context, now, timer_id.clone(), fire_at)
                .map_err(|error| error.to_string())?;
            schedule_sleep_timer(&context, duration, now, &timer_id, fire_at)
                .map_err(|error| error.to_string())?;
            None
        }
    };
    let state_id = NEXT_SCOPE_ID.fetch_add(1, Ordering::Relaxed);
    scopes().insert(
        state_id,
        TimeoutScope {
            pid,
            timer_id,
            replay_timed_out,
        },
    );
    scope_stacks().entry(pid).or_default().push(state_id);
    Ok((fun, state_id))
}

/// Continuation resume: the closure returned in `closure_result`.
fn resume_with_timeout(
    state: &AionTimeoutContinuation,
    closure_result: Term,
    ctx: &mut ProcessContext<'_>,
) -> Result<ContinuationStep, Term> {
    let state_id = state.state_id;
    let Some((_, scope)) = scopes().remove(&state_id) else {
        return Ok(ContinuationStep::Done(
            error_result_term("with_timeout: scope state missing").unwrap_or(Term::NIL),
        ));
    };
    if let Some(mut stack) = scope_stacks().get_mut(&scope.pid) {
        stack.retain(|entry| *entry != state_id);
    }
    let timed_out = match scope.replay_timed_out {
        Some(timed_out) => timed_out,
        None => match settle_live_scope(&scope) {
            Ok(timed_out) => timed_out,
            Err(message) => {
                return Ok(ContinuationStep::Done(
                    error_result_term(&message).unwrap_or(Term::NIL),
                ));
            }
        },
    };
    if timed_out {
        Ok(ContinuationStep::Done(
            error_result_term("timeout:deadline expired").unwrap_or(Term::NIL),
        ))
    } else {
        let wrapped = ctx.alloc_tuple(&[Term::atom(Atom::OK), closure_result])?;
        Ok(ContinuationStep::Done(wrapped))
    }
}

/// Settle a live scope's race durably and report whether the deadline won.
///
/// The cancel is a no-op when `TimerFired` is already recorded (the timer
/// service's terminal guard), so exactly one terminal event exists after
/// this call and both live and replay read the same outcome from it.
fn settle_live_scope(scope: &TimeoutScope) -> Result<bool, String> {
    let context = build_context_for_pid(scope.pid).map_err(|error| error.to_string())?;
    cancel_live_timer(&context, scope.timer_id.clone()).map_err(|error| error.to_string())?;
    drain_timer_deliveries(scope.pid, &scope.timer_id).map_err(|error| error.to_string())?;
    timer_fired_recorded(&context, &scope.timer_id)
}

/// Whether `TimerFired` is the recorded terminal event for `timer_id`.
fn timer_fired_recorded(context: &NifContext, timer_id: &TimerId) -> Result<bool, String> {
    let needle = timer_id.clone();
    context
        .block_on_recorder(move |recorder| {
            let needle = needle.clone();
            Box::pin(async move {
                let history = recorder.read_history().await?;
                Ok(history.iter().rev().find_map(|event| match event {
                    Event::TimerFired { timer_id, .. } if *timer_id == needle => Some(true),
                    Event::TimerCancelled { timer_id, .. } if *timer_id == needle => Some(false),
                    _ => None,
                }))
            })
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "with_timeout: deadline timer has no recorded terminal event".to_owned())
}
