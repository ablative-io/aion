//! Remote activity completion delivery and durable retry execution.

use std::sync::Arc;

use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::Recorder;

/// Spawn the completion task for one dispatched activity.
///
/// The task drives the dispatch to its FINAL outcome before waking the
/// workflow: a retryable-class failure (`retryable:` reason prefix — the
/// string form of the wire's structured `ActivityErrorKind`, see
/// [`super::nif_activity_retry`]) with budget left under the SDK-declared
/// retry policy is recorded durably as a non-terminal `ActivityFailed`
/// (kind `Retryable`), backed off, and re-dispatched with the SAME ordinal
/// and routing at the incremented attempt. Non-retryable failures, absent
/// policies (`"retry": null` — the SDK's run-exactly-once contract), and an
/// exhausted budget deliver to the workflow exactly as before, with the last
/// reason verbatim.
///
/// Every durable retry record is guarded against the settle races the
/// workflow thread can win mid-loop (a `with_timeout` expiry recording the
/// ordinal's terminal, a workflow terminal): the guard re-reads history under
/// the recorder lock and aborts the loop once the decision was made elsewhere.
/// The backoff sleep itself is task-local, not a durable timer: an engine
/// crash mid-backoff recovers through replay, whose dangling retryable
/// failure re-dispatches the activity live at the next attempt.
pub(super) fn spawn_completion_task(
    tokio_handle: &tokio::runtime::Handle,
    runtime: Arc<crate::RuntimeHandle>,
    dispatcher: Arc<dyn ActivityDispatcher>,
    seam: RetryRecorderSeam,
    workflow_pid: u64,
    correlation_id: String,
    request: ActivityDispatch,
) {
    let future = async move {
        let outcome = dispatch_with_retries(&dispatcher, &seam, &request).await;
        let attempt = outcome.attempt;
        match outcome.terminal {
            RetryLoopTerminal::Completed(payload) => {
                if let Err(error) = runtime.deliver_activity_completion_message_with_attempt(
                    workflow_pid,
                    &correlation_id,
                    payload,
                    Some(attempt),
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity completion delivery failed");
                }
            }
            RetryLoopTerminal::Failed(reason) => {
                if let Err(error) = runtime.deliver_activity_failure_message_with_attempt(
                    workflow_pid,
                    &correlation_id,
                    reason,
                    Some(attempt),
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity failure delivery failed");
                }
            }
            RetryLoopTerminal::SettledElsewhere => {
                // The awaited ordinal (or the whole workflow) reached a
                // recorded terminal while the loop ran — deliver nothing; the
                // workflow already took that branch.
                tracing::debug!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    "activity retry loop stopped: the activity settled through another path"
                );
            }
            RetryLoopTerminal::Parked => {
                // The server parked this dispatch for restart recovery
                // (graceful drain, #207): record nothing, deliver nothing. The
                // durable log ends at the dangling scheduled/started trail —
                // byte-equivalent to a kill -9 — so post-restart replay
                // re-dispatches the activity live (cursor Exhausted →
                // ResumeLive), exactly the SettledElsewhere stand-down shape.
                tracing::debug!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    "activity dispatch parked for restart recovery; retry loop stood down"
                );
            }
        }
    };
    tokio_handle.spawn(future);
}

/// The durable seam one completion task records retry attempts through: the
/// workflow's single-writer recorder plus the run the dispatch belongs to
/// (settlement is a per-run question — see [`record_retry_event`]).
pub(super) struct RetryRecorderSeam {
    /// The workflow's single-writer recorder, shared with the NIF contexts.
    pub(super) recorder: Arc<tokio::sync::Mutex<Recorder>>,
    /// The run this dispatch was issued by.
    pub(super) run_id: aion_core::RunId,
}

/// The retry loop's final disposition, carrying the attempt that produced it.
pub(super) struct RetryLoopOutcome {
    pub(super) attempt: u32,
    pub(super) terminal: RetryLoopTerminal,
}

pub(super) enum RetryLoopTerminal {
    /// The encoded output of the successful attempt.
    Completed(String),
    /// The last failure reason, verbatim (prefix included).
    Failed(String),
    /// A terminal for this ordinal/workflow was recorded by another path
    /// mid-loop; nothing may be delivered or recorded for it anymore.
    SettledElsewhere,
    /// The server parked the dispatch for restart recovery during a graceful
    /// drain (#207): nothing may be delivered or recorded — the workflow stays
    /// suspended and post-restart replay re-dispatches the dangling ordinal.
    Parked,
}

/// Classify a failed dispatch BEFORE any durable retry record: the parked
/// sentinel stands the loop down (park beats retry, #207 — nothing recorded,
/// nothing delivered, no budget consumed; restart recovery re-dispatches the
/// dangling ordinal); a non-retryable class, an absent policy (`"retry":
/// null`), or an exhausted budget fails with the reason verbatim. `None`
/// means the loop retries under the policy.
fn failure_stand_down(
    policy: Option<&super::nif_activity_retry::RetryPolicy>,
    reason: &str,
    attempt: u32,
) -> Option<RetryLoopTerminal> {
    use super::nif_activity_retry::{is_parked_reason, is_retryable_reason};

    if is_parked_reason(reason) {
        return Some(RetryLoopTerminal::Parked);
    }
    match policy {
        Some(policy) if is_retryable_reason(reason) && attempt < policy.max_attempts => None,
        _ => Some(RetryLoopTerminal::Failed(reason.to_owned())),
    }
}

/// Drive one activity dispatch to its final outcome under the SDK-declared
/// retry policy carried in the dispatch config (#197).
pub(super) async fn dispatch_with_retries(
    dispatcher: &Arc<dyn ActivityDispatcher>,
    seam: &RetryRecorderSeam,
    request: &ActivityDispatch,
) -> RetryLoopOutcome {
    use super::nif_activity_retry::retry_policy_from_config;

    let policy = retry_policy_from_config(&request.config);
    let mut attempt = request.attempt;
    loop {
        let mut delivery = request.clone();
        delivery.attempt = attempt;
        let reason = match Arc::clone(dispatcher).dispatch_async(delivery).await {
            Ok(payload) => {
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::Completed(payload),
                };
            }
            Err(reason) => reason,
        };
        if let Some(terminal) = failure_stand_down(policy.as_ref(), &reason, attempt) {
            return RetryLoopOutcome { attempt, terminal };
        }
        // The stand-down above returns `Failed` whenever no policy is present,
        // so a `None` here is structurally unreachable — kept as the honest
        // failure terminal rather than an unwrap.
        let Some(policy) = policy.as_ref() else {
            return RetryLoopOutcome {
                attempt,
                terminal: RetryLoopTerminal::Failed(reason),
            };
        };
        // Record the failed attempt durably as a NON-terminal (Retryable)
        // `ActivityFailed` — the observable retry record the history cursor
        // walks past to the eventual terminal. Recording failures abort the
        // loop into an honest terminal failure: an unrecorded retry is a
        // silent retry.
        match record_retry_event(
            seam,
            request,
            RetryRecord::AttemptFailed {
                attempt,
                reason: reason.clone(),
            },
        )
        .await
        {
            RetryRecordOutcome::Recorded => {}
            RetryRecordOutcome::Settled => {
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::SettledElsewhere,
                };
            }
            RetryRecordOutcome::RecordFailed(record_error) => {
                tracing::warn!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    error = %record_error,
                    "failed to record a retryable activity failure; failing the activity instead \
                     of retrying unrecorded"
                );
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::Failed(reason),
                };
            }
        }
        let delay = policy.backoff.delay_after(attempt);
        tracing::warn!(
            workflow_id = %request.workflow_id,
            activity_id = %request.activity_id,
            activity_type = %request.name,
            attempt,
            max_attempts = policy.max_attempts,
            retry_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            reason = %reason,
            "activity attempt failed with a retryable error; re-dispatching"
        );
        tokio::time::sleep(delay).await;
        attempt += 1;
        // Record the retry delivery's `ActivityStarted` before it goes on the
        // wire, so history and the worker wire agree on the attempt (NOI-0) —
        // re-guarded, because the backoff sleep is a settle-race window.
        match record_retry_event(seam, request, RetryRecord::AttemptStarted { attempt }).await {
            RetryRecordOutcome::Recorded => {}
            RetryRecordOutcome::Settled => {
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::SettledElsewhere,
                };
            }
            RetryRecordOutcome::RecordFailed(record_error) => {
                tracing::warn!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    error = %record_error,
                    "failed to record a retry attempt start; failing the activity instead of \
                     dispatching unrecorded"
                );
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::Failed(reason),
                };
            }
        }
    }
}

/// One durable retry record the loop appends between attempts.
enum RetryRecord {
    /// The just-failed attempt's non-terminal `ActivityFailed`.
    AttemptFailed { attempt: u32, reason: String },
    /// The next delivery's `ActivityStarted`.
    AttemptStarted { attempt: u32 },
}

enum RetryRecordOutcome {
    Recorded,
    /// The ordinal (or workflow) already has a recorded terminal; the loop
    /// must stop without recording or delivering anything further.
    Settled,
    RecordFailed(crate::durability::DurabilityError),
}

/// Append one retry record under the recorder lock, re-checking settlement
/// first so the append can never land after a terminal recorded by the
/// workflow thread (`with_timeout` expiry, workflow terminal).
async fn record_retry_event(
    seam: &RetryRecorderSeam,
    request: &ActivityDispatch,
    record: RetryRecord,
) -> RetryRecordOutcome {
    let mut recorder = seam.recorder.lock().await;
    let history = match recorder.read_history().await {
        Ok(history) => history,
        Err(error) => return RetryRecordOutcome::RecordFailed(error),
    };
    // Settlement is a per-run question: scope to the current run's segment so
    // a prior run's terminal (continue-as-new) never aborts this run's loop.
    let history = match crate::durability::current_run_segment(history, &seam.run_id) {
        Ok(history) => history,
        Err(error) => return RetryRecordOutcome::RecordFailed(error),
    };
    if super::nif_activity_retry::activity_settled(&history, &request.activity_id) {
        return RetryRecordOutcome::Settled;
    }
    let append_result = match record {
        RetryRecord::AttemptFailed { attempt, reason } => {
            recorder
                .record_activity_failed(
                    chrono::Utc::now(),
                    request.activity_id.clone(),
                    aion_core::ActivityError {
                        kind: aion_core::ActivityErrorKind::Retryable,
                        message: reason,
                        details: None,
                    },
                    attempt,
                )
                .await
        }
        RetryRecord::AttemptStarted { attempt } => {
            recorder
                .record_activity_started(chrono::Utc::now(), request.activity_id.clone(), attempt)
                .await
        }
    };
    match append_result {
        Ok(()) => RetryRecordOutcome::Recorded,
        Err(error) => RetryRecordOutcome::RecordFailed(error),
    }
}
