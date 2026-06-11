//! Query-pump entry check shared by every suspending await native.
//!
//! Delivered queries wait in `EngineNifState::pending_queries` until the
//! workflow reaches a yield point. Each suspending await — `receive_signal`,
//! `sleep`, `await_activity_result`, and any future two-phase suspend such as
//! the converted `await_child`/`collect_*` natives — calls
//! [`take_pending_query_sentinel`] at the top of every invocation (fresh
//! entry and wake re-entry alike), *before* its own resolution, including any
//! recorded-resolution fast path. When a query is pending, the await returns
//! the sentinel error `{error, <<"aion_query:", Json/binary>>}` to the SDK
//! pump instead of resolving; the pump runs the registered handler from the
//! process dictionary, replies through `reply_query`/`reply_query_error`,
//! and re-enters the same await, which re-resolves identically (its
//! `pending_awaits` pin is untouched and replay resolution reads history).
//!
//! Between the sentinel return and the reply, the pid carries a servicing
//! flag; recording NIFs consult [`ensure_not_servicing_query`] and refuse
//! typed, so a query handler that tries to record (or reach another
//! nondeterministic read) surfaces as `HandlerFailed` instead of writing
//! history.

use aion_core::Event;

use super::nif_context::NifContext;
use super::nif_query::{pending_reply_is_live, take_pending_reply};
use super::nif_state::{EngineNifState, PendingQuery};

/// Sentinel prefix consumed by the SDK query pump.
pub(super) const QUERY_SENTINEL_PREFIX: &str = "aion_query:";

/// Pop one pending query for `pid` and build its sentinel error payload.
///
/// Returns `None` when no serviceable query is queued, or when the pid is
/// already servicing one (a handler re-entering an await must not be handed
/// a second query). Queries whose caller already stopped waiting (timed-out
/// reply sender dropped or removed) are discarded instead of serviced, so a
/// woken workflow never wastes a handler run on a dead reply channel.
///
/// On `Some`, the pid's servicing flag is set to the popped query id; the
/// reply NIFs clear it. The caller returns the sentinel string as its
/// standard `{error, <<...>>}` result without consuming a wake marker for
/// it — the marker that woke the process drains on the pump's re-entry.
pub(super) fn take_pending_query_sentinel(state: &EngineNifState, pid: u64) -> Option<String> {
    if state.servicing_queries.contains_key(&pid) {
        return None;
    }
    loop {
        let pending = {
            let mut queue = state.pending_queries.get_mut(&pid)?;
            queue.pop_front()?
        };
        match pending_reply_is_live(state, &pending.query_id) {
            Ok(true) => {
                state
                    .servicing_queries
                    .insert(pid, pending.query_id.clone());
                return Some(sentinel_payload(&pending));
            }
            Ok(false) => {
                // Caller gave up (timeout) or the reply was already taken:
                // drop the stale sender, if any, and try the next query.
                if let Ok(Some(stale)) = take_pending_reply(state, &pending.query_id) {
                    drop(stale);
                }
            }
            Err(error) => {
                tracing::warn!(
                    pid,
                    query_id = %pending.query_id,
                    error = %error,
                    "query pump could not inspect the pending reply registry; skipping query"
                );
            }
        }
    }
}

/// Refuse an operation while the calling pid is servicing a query.
///
/// Recording NIFs (`resolve_command` / recorder paths) and `dispatch_query`
/// call this first; a query handler reaching them gets a typed error the SDK
/// pump converts into `HandlerFailed`, never a silent history write.
///
/// # Errors
///
/// Returns the typed `query_servicing:` reason when the pid has an unanswered
/// query sentinel outstanding.
pub(super) fn ensure_not_servicing_query(
    state: &EngineNifState,
    pid: u64,
    operation: &str,
) -> Result<(), String> {
    match state.servicing_queries.get(&pid) {
        Some(entry) => Err(format!(
            "query_servicing:{operation} is forbidden while query {} is being serviced; \
             query handlers are read-only",
            entry.value()
        )),
        None => Ok(()),
    }
}

/// Clear the pid's servicing flag if it matches `query_id`.
///
/// Called by both reply NIFs before resolving the pending sender, so even a
/// reply for an already-cleaned-up query (late reply after caller timeout)
/// releases the guard instead of wedging the workflow.
pub(super) fn clear_servicing_query(state: &EngineNifState, pid: u64, query_id: &str) {
    state
        .servicing_queries
        .remove_if(&pid, |_, servicing| servicing == query_id);
}

/// Whether the calling workflow process is still replaying recorded history.
///
/// A run is mid-replay while its recorded history contains command-issued
/// events the live re-execution has not yet re-issued. Command issuance is
/// observable through the handle's run-scoped ordinal counters, which replay
/// re-allocates deterministically: while any counter lags its recorded
/// count, replay has not reached the recorded frontier. Asynchronous-arrival
/// events (signals, completions) are deliberately not counted — they are
/// recorded ahead of consumption in live execution and would false-positive.
/// `SignalSent` and named timers carry no run-scoped counter, so a replay
/// suffix containing only those is not detected; every counted command kind
/// is exact.
pub(super) fn is_mid_replay(context: &NifContext) -> bool {
    let handle = context.workflow_handle();
    let recorded = recorded_command_counts(context.history());
    handle.activity_ordinals_allocated() < recorded.activities
        || handle.timer_ordinals_allocated() < recorded.anonymous_timers
        || handle.child_ordinals_allocated() < recorded.children
}

/// Command-issued event counts in a run-segment history.
struct RecordedCommandCounts {
    activities: u64,
    anonymous_timers: u64,
    children: u64,
}

fn recorded_command_counts(history: &[Event]) -> RecordedCommandCounts {
    let mut counts = RecordedCommandCounts {
        activities: 0,
        anonymous_timers: 0,
        children: 0,
    };
    for event in history {
        match event {
            Event::ActivityScheduled { .. } => counts.activities += 1,
            Event::TimerStarted { timer_id, .. } if timer_id.sequence_position().is_some() => {
                counts.anonymous_timers += 1;
            }
            Event::ChildWorkflowStarted { .. } => counts.children += 1,
            _ => {}
        }
    }
    counts
}

fn sentinel_payload(pending: &PendingQuery) -> String {
    let json = serde_json::json!({
        "query_id": pending.query_id,
        "name": pending.name,
    });
    format!("{QUERY_SENTINEL_PREFIX}{json}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::oneshot;

    use super::super::nif_state::{EngineNifState, PendingAwait, PendingQuery};
    use super::{clear_servicing_query, ensure_not_servicing_query, take_pending_query_sentinel};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn queue_query(
        state: &EngineNifState,
        pid: u64,
        query_id: &str,
        name: &str,
    ) -> Result<oneshot::Receiver<crate::query::QueryResult>, String> {
        let (sender, receiver) = oneshot::channel();
        crate::runtime::nif_query::insert_pending_reply(state, query_id.to_owned(), pid, sender)?;
        state
            .pending_queries
            .entry(pid)
            .or_default()
            .push_back(PendingQuery {
                query_id: query_id.to_owned(),
                name: name.to_owned(),
            });
        Ok(receiver)
    }

    #[test]
    fn sentinel_pops_one_query_sets_servicing_and_preserves_await_pin() -> TestResult {
        let state = Arc::new(EngineNifState::default());
        state
            .pending_awaits
            .insert(7, PendingAwait::Signal { index: 3 });
        let _receiver = queue_query(&state, 7, "q-1", "state")?;

        let sentinel =
            take_pending_query_sentinel(&state, 7).ok_or("expected a sentinel for the query")?;

        assert!(sentinel.starts_with("aion_query:"));
        let json: serde_json::Value = serde_json::from_str(
            sentinel
                .strip_prefix("aion_query:")
                .ok_or("missing prefix")?,
        )?;
        assert_eq!(json["query_id"], "q-1");
        assert_eq!(json["name"], "state");
        assert_eq!(
            state.servicing_queries.get(&7).map(|e| e.clone()),
            Some("q-1".to_owned())
        );
        // The await's pinned identity is untouched: the pump re-enters the
        // same logical await after replying.
        assert!(matches!(
            state.pending_awaits.get(&7).map(|e| e.clone()),
            Some(PendingAwait::Signal { index: 3 })
        ));
        // While servicing, no second query is handed out.
        let _second_receiver = queue_query(&state, 7, "q-2", "state")?;
        assert!(take_pending_query_sentinel(&state, 7).is_none());
        Ok(())
    }

    #[test]
    fn sentinel_skips_queries_whose_caller_stopped_waiting() -> TestResult {
        let state = Arc::new(EngineNifState::default());
        let dead_receiver = queue_query(&state, 9, "dead", "state")?;
        drop(dead_receiver);
        let _live_receiver = queue_query(&state, 9, "live", "state")?;

        let sentinel =
            take_pending_query_sentinel(&state, 9).ok_or("expected the live query's sentinel")?;

        assert!(sentinel.contains("\"query_id\":\"live\""));
        // The dead query's sender was discarded along the way.
        assert!(!crate::runtime::nif_query::pending_reply_is_live(
            &state, "dead"
        )?);
        Ok(())
    }

    #[test]
    fn empty_queue_returns_none() {
        let state = EngineNifState::default();
        assert!(take_pending_query_sentinel(&state, 1).is_none());
    }

    #[test]
    fn servicing_guard_refuses_then_clears() -> TestResult {
        let state = Arc::new(EngineNifState::default());
        let _receiver = queue_query(&state, 5, "q-9", "state")?;
        let _sentinel =
            take_pending_query_sentinel(&state, 5).ok_or("expected sentinel before guard")?;

        let refused = ensure_not_servicing_query(&state, 5, "dispatch_activity")
            .err()
            .ok_or("recording during servicing was not refused")?;
        assert!(refused.starts_with("query_servicing:dispatch_activity"));
        assert!(refused.contains("q-9"));

        // A mismatched id does not lift the guard; the matching one does.
        clear_servicing_query(&state, 5, "other");
        assert!(ensure_not_servicing_query(&state, 5, "sleep").is_err());
        clear_servicing_query(&state, 5, "q-9");
        ensure_not_servicing_query(&state, 5, "sleep").map_err(Box::<dyn std::error::Error>::from)
    }
}
