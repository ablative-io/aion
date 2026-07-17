//! Activity await resolution and atomic runtime-completion collection.

use std::sync::Arc;

use aion_core::{ActivityId, Payload};
use beamr::native::ProcessContext;
use beamr::term::Term;

use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome};
use crate::error::EngineError;
use crate::runtime::nif_activity::{
    activity_error, activity_id_from_correlation, error_result_term, ok_result_term,
};
use crate::runtime::nif_activity_dispatch::FIRST_DELIVERY_ATTEMPT;
use crate::runtime::nif_context::NifContext;

pub(super) fn await_activity_result_with_context(
    state: &crate::runtime::EngineNifState,
    mut context: NifContext,
    runtime: &Arc<crate::RuntimeHandle>,
    process_context: &mut ProcessContext,
    correlation: &str,
) -> Result<Term, Term> {
    // A query handler must not nest into another await; refuse before any
    // marker is consumed or resolution attempted.
    if let Err(error) = crate::runtime::nif_query_pump::ensure_not_servicing_query(
        state,
        context.pid(),
        "await_activity_result",
    ) {
        return Ok(error_result_term(process_context, &error).unwrap_or(Term::NIL));
    }
    // Queries first (Q6), and deliberately BEFORE the recorded-resolution
    // fast path below: a tight replay loop whose awaits all resolve from
    // history instantly must still drain queued queries at each yield point.
    if let Some(sentinel) =
        crate::runtime::nif_query_pump::take_pending_query_sentinel(state, context.pid())
    {
        return Ok(error_result_term(process_context, &sentinel).unwrap_or(Term::NIL));
    }
    let activity_id = activity_id_from_correlation(process_context, correlation)?;
    let step = await_activity_step(state, &mut context, runtime, &activity_id, || {
        super::nif_wake::consume_wake_marker(process_context, runtime);
    });
    match step {
        Ok(ActivityAwaitStep::Completed(bytes)) => {
            Ok(ok_result_term(process_context, &bytes).unwrap_or(Term::NIL))
        }
        Ok(ActivityAwaitStep::Failed(message)) => {
            Ok(error_result_term(process_context, &message).unwrap_or(Term::NIL))
        }
        Ok(ActivityAwaitStep::Suspend) => {
            process_context.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(error) => {
            Err(error_result_term(process_context, &error.to_string()).unwrap_or(Term::NIL))
        }
    }
}

/// Outcome of one ProcessContext-free `await_activity_result` resolution
/// step, invoked fresh on every mailbox wake by the NIF shell above.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ActivityAwaitStep {
    /// The recorded (or just-recorded) completion payload bytes.
    Completed(Vec<u8>),
    /// A workflow-visible `{error, _}`: the recorded (or just-recorded)
    /// terminal failure message, or a recorded non-activity resolution.
    Failed(String),
    /// Park the calling process; a mailbox wake re-invokes the native.
    Suspend,
}

/// One activity-await resolution pass.
///
/// Recorded terminal first — it IS the live run's decision (this await
/// records completions, failures, and the durable timeout failure itself,
/// synchronously, before workflow code observes the branch), so replay reads
/// the decision and no deadline-vs-terminal seq ordering is needed on the
/// recorded path (unlike `await_child`/`receive_signal`, whose arrivals are
/// recorded by racing third parties). Then a takeable runtime-map completion
/// (recorded now), then the enclosing-scope expiry, then suspend.
///
/// `consume_wake_marker` runs once per live pass, only when no recorded
/// terminal resolves the await: markers are pure wakes and the completion
/// state lives in the runtime's keyed maps, so any marker (even one destined
/// for another await) is safe to take — leaving it queued would insta-rewake
/// the suspend into a busy spin.
pub(super) fn await_activity_step(
    state: &crate::runtime::EngineNifState,
    context: &mut NifContext,
    runtime: &crate::RuntimeHandle,
    activity_id: &ActivityId,
    consume_wake_marker: impl FnOnce(),
) -> Result<ActivityAwaitStep, EngineError> {
    if let Some(recorded) = recorded_resolution_for(context, activity_id)? {
        return Ok(recorded_step(recorded));
    }
    consume_wake_marker();
    if let Some(step) = take_runtime_completion(context, runtime, activity_id.clone())? {
        return Ok(step);
    }
    // An expired enclosing with_timeout deadline aborts the await instead of
    // re-suspending; the failure is recorded durably so replay returns it
    // verbatim. The expiry decision is a pure function of the RESOLUTION
    // snapshot (`context.history()`), never a fresh store read: this
    // resolution observed neither the activity terminal nor the deadline's
    // `TimerFired`, and deciding the branch from a newer snapshot than the
    // one the resolution settled on is the N-1 defect family. A deadline
    // firing after the snapshot is settled by the wake it triggers, whose
    // fresh snapshot re-enters this step.
    if crate::runtime::nif_timeout::expired_scope_deadline(state, context.pid(), context.history())
        .is_some()
    {
        let message = crate::runtime::nif_timeout::SCOPE_EXPIRED_MESSAGE.to_owned();
        // #197: stamp the terminal with the LATEST attempt the resolution
        // snapshot recorded for this ordinal (a retry loop may have moved it
        // past the first delivery); a snapshot with no attempt trail keeps
        // the first delivery, exactly as before.
        let attempt =
            super::nif_activity_retry::latest_recorded_attempt(context.history(), activity_id)
                .unwrap_or(FIRST_DELIVERY_ATTEMPT)
                .max(FIRST_DELIVERY_ATTEMPT);
        context
            .record_activity_failed(
                chrono::Utc::now(),
                activity_id.clone(),
                activity_error(message.clone()),
                attempt,
            )
            .map_err(|error| EngineError::Runtime {
                reason: error.error_reason(),
            })?;
        return Ok(ActivityAwaitStep::Failed(message));
    }
    Ok(ActivityAwaitStep::Suspend)
}

fn recorded_resolution_for(
    context: &mut NifContext,
    activity_id: &ActivityId,
) -> Result<Option<Resolution>, EngineError> {
    let ordinal = activity_id.sequence_position();
    let input =
        Payload::from_json(&serde_json::Value::Null).map_err(|error| EngineError::Runtime {
            reason: format!("await_activity_result replay input: {error}"),
        })?;
    match context
        .resolve_command(Command::RunActivity {
            key: CorrelationKey::Activity(ordinal),
            activity_type: "await_activity_result".to_owned(),
            input,
        })
        .map_err(|error| EngineError::Runtime {
            reason: error.error_reason(),
        })? {
        ResolveOutcome::Recorded(resolution) => Ok(Some(resolution)),
        ResolveOutcome::ResumeLive => Ok(None),
    }
}

fn recorded_step(resolution: Resolution) -> ActivityAwaitStep {
    match resolution {
        Resolution::ActivityCompleted(payload) => {
            ActivityAwaitStep::Completed(payload.bytes().to_vec())
        }
        Resolution::ActivityFailedTerminal(error) => ActivityAwaitStep::Failed(error.message),
        other => ActivityAwaitStep::Failed(format!(
            "await_activity_result: recorded non-activity resolution {other:?}"
        )),
    }
}

fn take_runtime_completion(
    context: &NifContext,
    runtime: &crate::RuntimeHandle,
    activity_id: ActivityId,
) -> Result<Option<ActivityAwaitStep>, EngineError> {
    let ordinal = activity_id.sequence_position();
    if let Some((payload, attempt)) = runtime.take_activity_result(context.pid(), ordinal)? {
        // NOI-0/#197: the completion task retains the attempt that produced the
        // delivered outcome (a retry loop can move it past the first delivery);
        // paths that never retry (outbox re-delivery, in-VM) retain nothing and
        // resolve to the first delivery exactly as before.
        let attempt = attempt.unwrap_or(FIRST_DELIVERY_ATTEMPT);
        context
            .record_activity_completed(chrono::Utc::now(), activity_id, payload.clone(), attempt)
            .map_err(|error| EngineError::Runtime {
                reason: error.error_reason(),
            })?;
        return Ok(Some(ActivityAwaitStep::Completed(payload.bytes().to_vec())));
    }
    if let Some((error, attempt)) = runtime.take_activity_error(context.pid(), ordinal)? {
        // #197: an exhausted retry budget fails the workflow with the LAST
        // reason verbatim and the final attempt count on the recorded
        // terminal `ActivityFailed`.
        let attempt = attempt.unwrap_or(FIRST_DELIVERY_ATTEMPT);
        context
            .record_activity_failed(
                chrono::Utc::now(),
                activity_id,
                activity_error(error.message.clone()),
                attempt,
            )
            .map_err(|record_error| EngineError::Runtime {
                reason: record_error.error_reason(),
            })?;
        return Ok(Some(ActivityAwaitStep::Failed(error.message)));
    }
    Ok(None)
}
