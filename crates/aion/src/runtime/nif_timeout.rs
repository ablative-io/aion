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

/// Canonical workflow-visible message for an expired deadline scope.
pub(crate) const SCOPE_EXPIRED_MESSAGE: &str = "timeout:deadline expired";

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

#[cfg(test)]
impl TimeoutScope {
    /// Build a replay-outcome scope so suspending-await tests can exercise
    /// the expired-deadline abort path without arming a live timer.
    pub(super) fn replayed_for_test(pid: u64, timed_out: bool) -> Self {
        Self {
            pid,
            timer_id: TimerId::anonymous(0),
            replay_timed_out: Some(timed_out),
        }
    }

    /// Build a live scope (no replay outcome) whose expiry is decided by the
    /// recorded `TimerFired` of `timer_id`, so determinism tests can model
    /// the live-vs-replay snapshot races of N-1/N-2/N-3 without arming a
    /// real timer.
    pub(super) fn live_for_test(pid: u64, timer_id: TimerId) -> Self {
        Self {
            pid,
            timer_id,
            replay_timed_out: None,
        }
    }

    /// Build a replay-expired scope carrying its real deadline timer id —
    /// what `arm_scope` derives on replay — so F1b ordering tests read the
    /// recorded `TimerFired` position exactly as production replay does.
    pub(super) fn replayed_expired_with_deadline_for_test(pid: u64, timer_id: TimerId) -> Self {
        Self {
            pid,
            timer_id,
            replay_timed_out: Some(true),
        }
    }
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
                Some(true) => return Some(SCOPE_EXPIRED_MESSAGE.to_owned()),
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
        .then(|| SCOPE_EXPIRED_MESSAGE.to_owned())
}

/// History position of an expired enclosing `with_timeout` deadline.
///
/// Used by awaits that resolve recorded *arrivals* (a child terminal
/// recorded by the watcher) to order the scope's expiry against the arrival:
/// an arrival recorded after the deadline's `TimerFired` was never observed
/// by the live run (it took the timeout branch), so the replayed await must
/// take the timeout branch too (F1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpiredScopeDeadline {
    /// The deadline's `TimerFired` is recorded at this envelope sequence.
    RecordedAt(u64),
    /// The scope is known expired but its `TimerFired` is not in the run
    /// segment (replay-derived scope state); the expiry is treated as
    /// preceding every recorded arrival.
    Unordered,
}

/// Earliest recorded deadline position among the pid's expired scopes.
///
/// Returns `None` when no enclosing scope is expired. `history` is the
/// caller's run-segment snapshot — the same events its own resolution read,
/// so the ordering decision and the resolution agree on one snapshot.
pub(crate) fn expired_scope_deadline(
    state: &EngineNifState,
    pid: u64,
    history: &[aion_core::Event],
) -> Option<ExpiredScopeDeadline> {
    let mut earliest: Option<u64> = None;
    let mut expired_without_position = false;
    {
        let stack = state.timeout_scope_stacks.get(&pid)?;
        for state_id in stack.iter() {
            let Some(scope) = state.timeout_scopes.get(state_id) else {
                continue;
            };
            let fired_seq = timer_fired_seq(history, &scope.timer_id);
            let expired = match scope.replay_timed_out {
                Some(true) => true,
                Some(false) => false,
                None => fired_seq.is_some(),
            };
            if !expired {
                continue;
            }
            match fired_seq {
                Some(seq) => {
                    earliest = Some(earliest.map_or(seq, |current| current.min(seq)));
                }
                None => expired_without_position = true,
            }
        }
    }
    if expired_without_position {
        // Conservative and deterministic: an expired scope whose deadline
        // position is unknown orders before every arrival, on live and on
        // replay alike.
        return Some(ExpiredScopeDeadline::Unordered);
    }
    earliest.map(ExpiredScopeDeadline::RecordedAt)
}

/// Envelope sequence of the recorded `TimerFired` for `timer_id`, if any.
fn timer_fired_seq(history: &[aion_core::Event], timer_id: &TimerId) -> Option<u64> {
    history.iter().find_map(|event| match event {
        aion_core::Event::TimerFired {
            envelope,
            timer_id: fired,
            ..
        } if fired == timer_id => Some(envelope.seq),
        _ => None,
    })
}

/// NIF backing `aion_flow_ffi:with_timeout/2`.
pub(super) fn with_timeout_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Ok(error_result_term(
            ctx,
            &format!("with_timeout: expected 2 arguments, got {}", args.len()),
        )
        .unwrap_or(Term::NIL));
    }
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "with_timeout: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // with_timeout records its durable deadline timer; a query handler must
    // stay read-only.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "with_timeout")
    {
        return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
    }
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
        Err(message) => Ok(error_result_term(ctx, &message).unwrap_or(Term::NIL)),
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
        ResolveOutcome::Recorded(Resolution::TimerStarted) => {
            // The scope's start is recorded but its terminal is not the
            // immediately following event — the wrapped operation's recorded
            // events (a collect fan-out, an activity, a signal arrival)
            // interleave between them. Read the terminal from the run
            // segment, exactly as `sleep` does. `None` (started, no terminal:
            // crash mid-scope, durable timer re-armed by recovery) replays as
            // a live scope so the race settles durably on resume.
            timer_terminal_recorded(&context, &timer_id)
        }
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
                error_result_term(ctx, &error).unwrap_or(Term::NIL),
            ));
        }
    };
    let Some((_, scope)) = engine_state.timeout_scopes.remove(&state_id) else {
        return Ok(ContinuationStep::Done(
            error_result_term(ctx, "with_timeout: scope state missing").unwrap_or(Term::NIL),
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
                    error_result_term(ctx, &message).unwrap_or(Term::NIL),
                ));
            }
        },
    };
    if timed_out {
        Ok(ContinuationStep::Done(
            error_result_term(ctx, SCOPE_EXPIRED_MESSAGE).unwrap_or(Term::NIL),
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
