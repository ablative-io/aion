//! Settlement and recorded-history helpers for fan-out collection.

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, Event, Payload};
use chrono::Utc;

use crate::durability::{FanOutCompletionResult, FanOutOutcome};
use crate::runtime::nif_activity_dispatch::FIRST_DELIVERY_ATTEMPT;
use crate::runtime::nif_collect::{
    CollectDeps, CollectError, CollectStep, OrdinalState, RaceSettlement,
};
use crate::runtime::nif_context::NifContext;
use crate::runtime::nif_state::EngineNifState;
use crate::runtime::nif_timeout::SCOPE_EXPIRED_MESSAGE;

/// `collect_all`/`collect_map` settlement over one sweep of the range.
pub(super) fn settle_all(
    state: &EngineNifState,
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    base_ordinal: u64,
    count: u64,
) -> Result<CollectStep, CollectError> {
    let mut states = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for ordinal in base_ordinal..base_ordinal + count {
        let recorded = match recorded_terminal(context.history(), ordinal)? {
            Some(recorded) => recorded,
            None => take_and_record(deps, context, pid, ordinal)?,
        };
        states.push(recorded);
    }
    // Fail fast: the lowest-ordinal recorded failure. The rule is a function
    // of the recorded terminal *set*, so replay derives the same value.
    let lowest_failure = states.iter().find_map(|recorded| match recorded {
        OrdinalState::Failed(message) => Some(message.clone()),
        _ => None,
    });
    if let Some(message) = lowest_failure {
        cancel_pending(deps, context, pid, base_ordinal, &states)?;
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::FailFast(message));
    }
    if states
        .iter()
        .all(|recorded| matches!(recorded, OrdinalState::Completed(_)))
    {
        let results = states
            .into_iter()
            .filter_map(|recorded| match recorded {
                OrdinalState::Completed(payload) => Some(payload),
                _ => None,
            })
            .collect();
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::AllCompleted(results));
    }
    // An expired enclosing with_timeout deadline aborts the await: every
    // unresolved member is cancelled durably so replay derives the abort.
    //
    // The expiry decision is a pure function of the RESOLUTION snapshot
    // (`context.history()`), never a fresh store read: deciding the abort
    // from events newer than the snapshot this sweep settled on is the N-1
    // defect family. A deadline whose `TimerFired` landed after the
    // snapshot is settled by the wake it triggers, whose fresh snapshot
    // re-enters this sweep. No deadline-vs-terminal seq ordering is needed
    // on the recorded path (unlike await_child/receive_signal): member
    // terminals are recorded synchronously by this collect itself, and the
    // abort is recorded as the cancellation set, so replay reads the
    // decision instead of re-deriving the race.
    if super::nif_timeout::expired_scope_deadline(state, pid, context.history()).is_some() {
        cancel_pending(deps, context, pid, base_ordinal, &states)?;
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    // No failure, not all completed, nothing pending: a replayed batch whose
    // live run was aborted by scope expiry (cancelled-without-failure).
    if !states
        .iter()
        .any(|recorded| matches!(recorded, OrdinalState::Pending))
    {
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    Ok(CollectStep::Suspend)
}

/// `collect_race` settlement: first recorded terminal wins, batch ties
/// break to the lowest ordinal, losers are cancelled durably.
pub(super) fn settle_race(
    state: &EngineNifState,
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    base_ordinal: u64,
    count: u64,
) -> Result<CollectStep, CollectError> {
    let history = context.history();
    // The earliest-seq recorded non-cancelled terminal is the settled winner
    // (live: recorded by an earlier re-entry; replay: the only one).
    let mut winner = recorded_race_winner(history, base_ordinal, count)?;
    if winner.is_none() {
        // Take in ordinal order: of a batch sitting in the maps on one
        // wake, the lowest ordinal becomes the recorded winner.
        for ordinal in base_ordinal..base_ordinal + count {
            if recorded_terminal(history, ordinal)?.is_some() {
                // A recorded Cancelled never revives into a winner.
                continue;
            }
            match take_and_record(deps, context, pid, ordinal)? {
                OrdinalState::Completed(payload) => {
                    winner = Some((ordinal, Ok(payload)));
                    break;
                }
                OrdinalState::Failed(message) => {
                    winner = Some((ordinal, Err(message)));
                    break;
                }
                OrdinalState::Cancelled | OrdinalState::Pending => {}
            }
        }
    }
    if let Some((winner_ordinal, outcome)) = winner {
        for ordinal in base_ordinal..base_ordinal + count {
            if ordinal == winner_ordinal {
                continue;
            }
            drop_runtime_entries(deps, pid, ordinal)?;
            if recorded_terminal(history, ordinal)?.is_none() {
                record_cancelled(context, ordinal)?;
            }
        }
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::RaceWon(outcome));
    }
    // Snapshot-pure expiry, exactly as in `settle_all`: the abort is decided
    // from this resolution's history snapshot and recorded as the durable
    // cancellation set, so live and replay read the same decision. A
    // deadline firing after the snapshot re-enters via its wake. The
    // winner-first check order above is itself deterministic: a winner is a
    // recorded terminal, so replay settles it identically before consulting
    // the scope.
    if super::nif_timeout::expired_scope_deadline(state, pid, history).is_some() {
        for ordinal in base_ordinal..base_ordinal + count {
            drop_runtime_entries(deps, pid, ordinal)?;
            if recorded_terminal(history, ordinal)?.is_none() {
                record_cancelled(context, ordinal)?;
            }
        }
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    // Every member cancelled with no winner: a replayed batch whose live
    // run was aborted by scope expiry before anything settled.
    let mut all_cancelled = true;
    for ordinal in base_ordinal..base_ordinal + count {
        if recorded_terminal(history, ordinal)? != Some(OrdinalState::Cancelled) {
            all_cancelled = false;
            break;
        }
    }
    if all_cancelled {
        state.pending_awaits.remove(&pid);
        return Ok(CollectStep::ScopeExpired(SCOPE_EXPIRED_MESSAGE.to_owned()));
    }
    Ok(CollectStep::Suspend)
}

/// Record `ActivityCancelled` for every pending member and drop any runtime
/// completion that raced in after the sweep.
fn cancel_pending(
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    base_ordinal: u64,
    states: &[OrdinalState],
) -> Result<(), CollectError> {
    for (offset, recorded) in states.iter().enumerate() {
        if matches!(recorded, OrdinalState::Pending) {
            let ordinal = base_ordinal + offset_to_u64(offset)?;
            record_cancelled(context, ordinal)?;
            drop_runtime_entries(deps, pid, ordinal)?;
        }
    }
    Ok(())
}

/// Take this ordinal's runtime-map completion and attempt atomically, if delivered, and record it.
fn take_and_record(
    deps: &CollectDeps,
    context: &NifContext,
    pid: u64,
    ordinal: u64,
) -> Result<OrdinalState, CollectError> {
    let activity_id = ActivityId::from_sequence_position(ordinal);
    // Flag ON records terminals through the store-backed dedup primitive; the
    // returned OrdinalState is the same either way — the terminal for this
    // ordinal is in history whether this call Recorded it or found it Dropped.
    let outbox_enabled = deps.runtime.outbox_enabled();
    if let Some((payload, attempt)) = deps.runtime.take_activity_result(pid, ordinal)? {
        let attempt = attempt.unwrap_or(FIRST_DELIVERY_ATTEMPT);
        if outbox_enabled {
            let result = context
                .record_fan_out_completion(
                    Utc::now(),
                    ordinal,
                    FanOutOutcome::Completed {
                        result: payload.clone(),
                        attempt,
                    },
                )
                .map_err(|error| error.error_reason())?;
            log_unexpected_drop(result, ordinal);
        } else {
            context
                .record_activity_completed(Utc::now(), activity_id, payload.clone(), attempt)
                .map_err(|error| error.error_reason())?;
        }
        return Ok(OrdinalState::Completed(payload_text(&payload)?));
    }
    if let Some((error, attempt)) = deps.runtime.take_activity_error(pid, ordinal)? {
        let attempt = attempt.unwrap_or(FIRST_DELIVERY_ATTEMPT);
        if outbox_enabled {
            let result = context
                .record_fan_out_completion(
                    Utc::now(),
                    ordinal,
                    FanOutOutcome::Failed {
                        error: terminal_error(&error.message),
                        attempt,
                    },
                )
                .map_err(|inner| inner.error_reason())?;
            log_unexpected_drop(result, ordinal);
        } else {
            context
                .record_activity_failed(
                    Utc::now(),
                    activity_id,
                    terminal_error(&error.message),
                    attempt,
                )
                .map_err(|inner| inner.error_reason())?;
        }
        return Ok(OrdinalState::Failed(error.message));
    }
    Ok(OrdinalState::Pending)
}

/// Log a fan-out completion that the dedup primitive Dropped.
///
/// `settle_all`/`settle_race` short-circuit via `recorded_terminal` before
/// reaching `take_and_record`, so within one single-writer turn the result is
/// always `Recorded`. A `Dropped` here is unexpected on a single node — log it,
/// but the caller still maps to the correct terminal `OrdinalState` regardless.
fn log_unexpected_drop(result: FanOutCompletionResult, ordinal: u64) {
    if result == FanOutCompletionResult::Dropped {
        tracing::warn!(
            ordinal,
            "fan-out completion dropped as duplicate within a single-writer turn (unexpected single-node)"
        );
    }
}

fn record_cancelled(context: &NifContext, ordinal: u64) -> Result<(), String> {
    context
        // NOI-0: the cancelled fan-out ordinal was dispatched once at `FIRST_DELIVERY_ATTEMPT`.
        .record_activity_cancelled_and_settle_outbox(Utc::now(), ordinal, FIRST_DELIVERY_ATTEMPT)
        .map_err(|error| error.error_reason())
}

/// Drop both retained runtime-map entries for an ordinal (D5 hygiene at
/// settle time; the monitor drain covers post-exit stragglers).
fn drop_runtime_entries(deps: &CollectDeps, pid: u64, ordinal: u64) -> Result<(), CollectError> {
    drop(deps.runtime.take_activity_result(pid, ordinal)?);
    drop(deps.runtime.take_activity_error(pid, ordinal)?);
    Ok(())
}

/// The recorded terminal for `ordinal` in this run's segment, if any.
pub(super) fn recorded_terminal(
    history: &[Event],
    ordinal: u64,
) -> Result<Option<OrdinalState>, String> {
    let target = ActivityId::from_sequence_position(ordinal);
    for event in history {
        match event {
            Event::ActivityCompleted {
                activity_id,
                result,
                ..
            } if *activity_id == target => {
                return Ok(Some(OrdinalState::Completed(payload_text(result)?)));
            }
            Event::ActivityFailed {
                activity_id, error, ..
            } if *activity_id == target => {
                return Ok(Some(OrdinalState::Failed(error.message.clone())));
            }
            Event::ActivityCancelled { activity_id, .. } if *activity_id == target => {
                return Ok(Some(OrdinalState::Cancelled));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The earliest-seq recorded non-cancelled terminal in the fan-out range.
fn recorded_race_winner(
    history: &[Event],
    base_ordinal: u64,
    count: u64,
) -> Result<Option<RaceSettlement>, String> {
    let in_range = |activity_id: &ActivityId| {
        let position = activity_id.sequence_position();
        position >= base_ordinal && position < base_ordinal + count
    };
    for event in history {
        match event {
            Event::ActivityCompleted {
                activity_id,
                result,
                ..
            } if in_range(activity_id) => {
                return Ok(Some((
                    activity_id.sequence_position(),
                    Ok(payload_text(result)?),
                )));
            }
            Event::ActivityFailed {
                activity_id, error, ..
            } if in_range(activity_id) => {
                return Ok(Some((
                    activity_id.sequence_position(),
                    Err(error.message.clone()),
                )));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The recorded `ActivityScheduled` type for `ordinal`, if any.
pub(super) fn scheduled_activity_type(history: &[Event], ordinal: u64) -> Option<String> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().find_map(|event| match event {
        Event::ActivityScheduled {
            activity_id,
            activity_type,
            ..
        } if *activity_id == target => Some(activity_type.clone()),
        _ => None,
    })
}

/// The recorded `ActivityScheduled` task queue for `ordinal`, if any (NSTQ-3 recovery).
///
/// The durable source of truth for re-targeting the same pool on reopen/recovery. A history
/// recorded before the `task_queue` field existed decodes the field to the named default
/// (`aion_core::DEFAULT_TASK_QUEUE`) via the event's serde default, so this returns `"default"`
/// for such ordinals — never absent for a recorded `ActivityScheduled`.
pub(super) fn scheduled_task_queue(history: &[Event], ordinal: u64) -> Option<String> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().find_map(|event| match event {
        Event::ActivityScheduled {
            activity_id,
            task_queue,
            ..
        } if *activity_id == target => Some(task_queue.clone()),
        _ => None,
    })
}

/// The recorded `ActivityScheduled` OPTIONAL node affinity for `ordinal` (NODE-3 recovery).
///
/// The durable source of truth for re-targeting the same node on reopen/recovery. Returns the
/// recorded `node` (`Some`/`None`) for a recorded `ActivityScheduled`, or `None` if no
/// `ActivityScheduled` exists for the ordinal. A history recorded before the `node` field existed
/// decodes the field to `None` via the event's serde default, so this is `None` (no affinity) for
/// such ordinals — never a sentinel, deterministically replay-safe.
pub(super) fn scheduled_node(history: &[Event], ordinal: u64) -> Option<String> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().find_map(|event| match event {
        Event::ActivityScheduled {
            activity_id, node, ..
        } if *activity_id == target => node.clone(),
        _ => None,
    })
}

/// The recorded `ActivityStarted` one-based attempt for `ordinal` (NOI-0 recovery).
///
/// The durable source of truth for re-stamping the SAME attempt on a crash-recovery re-dispatch, so
/// the re-armed dispatch keeps the identity the original `ActivityStarted` recorded. Returns the
/// recorded `attempt` of the LATEST `ActivityStarted` for the ordinal — with an engine-driven retry
/// trail (#197) the last start IS the in-flight delivery — or `None` if no `ActivityStarted` exists
/// for the ordinal. A history recorded before the `attempt` field existed decodes it to the legacy
/// sentinel (`0`) via the event's serde default — deterministically replay-safe, never a panic.
pub(super) fn started_attempt(history: &[Event], ordinal: u64) -> Option<u32> {
    let target = ActivityId::from_sequence_position(ordinal);
    history.iter().rev().find_map(|event| match event {
        Event::ActivityStarted {
            activity_id,
            attempt,
            ..
        } if *activity_id == target => Some(*attempt),
        _ => None,
    })
}

pub(super) fn payload_from_json_text(text: &str, label: &str) -> Result<Payload, String> {
    let value = serde_json::from_str(text)
        .map_err(|error| format!("{label}: invalid JSON payload: {error}"))?;
    Payload::from_json(&value).map_err(|error| format!("{label}: {error}"))
}

fn payload_text(payload: &Payload) -> Result<String, String> {
    String::from_utf8(payload.bytes().to_vec())
        .map_err(|_| "recorded activity payload is not valid UTF-8".to_owned())
}

fn terminal_error(message: &str) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: message.to_owned(),
        details: None,
    }
}

pub(super) fn offset_to_u64(offset: usize) -> Result<u64, String> {
    u64::try_from(offset).map_err(|_| "activity offset overflows u64".to_owned())
}
