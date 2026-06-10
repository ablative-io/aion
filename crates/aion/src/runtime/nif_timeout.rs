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

use std::sync::atomic::Ordering;

use aion_core::TimerId;
use beamr::atom::Atom;
use beamr::native::stdlib_stubs::maps_bifs::ContinuationStep;
use beamr::native::{AionTimeoutContinuation, NativeContinuation, ProcessContext};
use beamr::term::Term;

use crate::durability::{Resolution, ResolveOutcome};
use crate::runtime::engine_nifs::error_result_term;
use crate::runtime::nif_context::NifContext;
use crate::runtime::nif_state::EngineNifState;
use crate::runtime::nif_timer::{
    add_duration, build_context_for_pid, cancel_live_timer, decode_duration_arg, record_started,
    recorded_now, schedule_sleep_timer, timer_command, timer_terminal_recorded,
};

/// One armed `with_timeout` deadline scope.
pub(super) struct TimeoutScope {
    /// Workflow process the scope belongs to.
    pub(super) pid: u64,
    /// Durable deadline timer racing the wrapped operation.
    timer_id: TimerId,
    /// `Some(timed_out)` when this run replays a recorded outcome and no
    /// live timer was armed; `None` on the live path.
    replay_timed_out: Option<bool>,
}

/// Return the message for an expired enclosing `with_timeout` scope, if any.
///
/// Suspending awaits call this before parking and after every wake: an
/// expired enclosing deadline means the await must abort instead of parking
/// again. Live scopes expire when their deadline timer's `TimerFired` is
/// recorded; replay scopes carry the recorded outcome directly, so a
/// replayed timed-out await aborts identically instead of parking forever.
pub(crate) fn expired_scope_message(state: &EngineNifState, pid: u64) -> Option<String> {
    let mut live_deadlines: Vec<TimerId> = Vec::new();
    {
        let stack = state.timeout_scope_stacks.get(&pid)?;
        for state_id in stack.iter() {
            let Some(scope) = state.timeout_scopes.get(state_id) else {
                continue;
            };
            match scope.replay_timed_out {
                Some(true) => return Some("timeout:deadline expired".to_owned()),
                Some(false) => {}
                None => live_deadlines.push(scope.timer_id.clone()),
            }
        }
    }
    if live_deadlines.is_empty() {
        return None;
    }
    let context = build_context_for_pid(state, pid).ok()?;
    live_deadlines
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
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    match arm_scope(&state, args, pid) {
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
fn arm_scope(state: &EngineNifState, args: &[Term], pid: u64) -> Result<(Term, u64), String> {
    let duration =
        decode_duration_arg("with_timeout deadline", args[0]).map_err(|error| error.to_string())?;
    let fun = args[1];
    let mut context = build_context_for_pid(state, pid).map_err(|error| error.to_string())?;
    let now = recorded_now(&context);
    let timer_id = TimerId::anonymous(context.next_timer_ordinal());
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
            schedule_sleep_timer(state, &context, duration, now, &timer_id, fire_at)
                .map_err(|error| error.to_string())?;
            None
        }
    };
    let state_id = state.next_timeout_scope_id.fetch_add(1, Ordering::Relaxed);
    state.timeout_scopes.insert(
        state_id,
        TimeoutScope {
            pid,
            timer_id,
            replay_timed_out,
        },
    );
    state
        .timeout_scope_stacks
        .entry(pid)
        .or_default()
        .push(state_id);
    Ok((fun, state_id))
}

/// Continuation resume: the closure returned in `closure_result`.
fn resume_with_timeout(
    continuation: &AionTimeoutContinuation,
    closure_result: Term,
    ctx: &mut ProcessContext<'_>,
) -> Result<ContinuationStep, Term> {
    let state_id = continuation.state_id;
    let engine_state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => {
            return Ok(ContinuationStep::Done(
                error_result_term(&error).unwrap_or(Term::NIL),
            ));
        }
    };
    let Some((_, scope)) = engine_state.timeout_scopes.remove(&state_id) else {
        return Ok(ContinuationStep::Done(
            error_result_term("with_timeout: scope state missing").unwrap_or(Term::NIL),
        ));
    };
    if let Some(mut stack) = engine_state.timeout_scope_stacks.get_mut(&scope.pid) {
        stack.retain(|entry| *entry != state_id);
    }
    let timed_out = match scope.replay_timed_out {
        Some(timed_out) => timed_out,
        None => match settle_live_scope(&engine_state, &scope) {
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
fn settle_live_scope(state: &EngineNifState, scope: &TimeoutScope) -> Result<bool, String> {
    let context = build_context_for_pid(state, scope.pid).map_err(|error| error.to_string())?;
    cancel_live_timer(state, &context, scope.timer_id.clone())
        .map_err(|error| error.to_string())?;
    // Rebuild: the cancel may have just recorded the terminal, and the
    // first context's history snapshot predates it.
    let context = build_context_for_pid(state, scope.pid).map_err(|error| error.to_string())?;
    timer_fired_recorded(&context, &scope.timer_id)
}

/// Whether `TimerFired` is the recorded terminal event for `timer_id`.
fn timer_fired_recorded(context: &NifContext, timer_id: &TimerId) -> Result<bool, String> {
    timer_terminal_recorded(context, timer_id)
        .ok_or_else(|| "with_timeout: deadline timer has no recorded terminal event".to_owned())
}
