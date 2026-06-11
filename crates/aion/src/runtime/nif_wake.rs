//! Shared wake-marker consumption for suspending awaits.
//!
//! Every asynchronous arrival (activity completion or failure, signal,
//! fired timer) wakes a suspended workflow process by enqueueing one atom
//! marker into its mailbox. Markers are pure wakes: the arrival's state
//! lives in recorded history or the runtime's completion maps, never in the
//! marker itself, so any aion await may consume any marker.

use beamr::native::ProcessContext;
use beamr::term::Term;

use crate::RuntimeHandle;

/// Remove one queued aion wake marker from the calling process mailbox.
///
/// Suspending awaits (`sleep`, `receive_signal`, `await_activity_result`)
/// must call this exactly once per invocation before deciding to suspend
/// again. A marker left queued would defeat the suspend — beamr's parked
/// wait re-checks `mailbox().is_empty()` and immediately re-wakes — turning
/// the await into a busy spin. Consuming more than one is not possible: the
/// native select facility honours a single removal per native call.
///
/// Consuming a marker destined for a different await is safe: that await
/// re-checks its own recorded or runtime state on entry and completes
/// without needing the wake, and any surplus marker drains through the next
/// suspend/wake cycle. That includes the `aion_query` marker: a query marker
/// consumed by an await that then resolves without ever suspending again is
/// only safe because every suspending await runs the query-pump entry check
/// on every invocation — not just wakes — so the queued query is still
/// drained at the next yield point regardless of which await ate its marker.
pub(super) fn consume_wake_marker(process_context: &mut ProcessContext, runtime: &RuntimeHandle) {
    let markers = [
        runtime.activity_complete_atom(),
        runtime.activity_failed_atom(),
        runtime.activity_result_atom(),
        runtime.signal_received_atom(),
        runtime.timer_fired_atom(),
        runtime.query_marker_atom(),
    ];
    let Some(select) = process_context.select_facility() else {
        // No facility means an empty mailbox: a subsequent suspend parks
        // cleanly and the next marker arrival wakes it.
        return;
    };
    let message_count = select.message_count();
    for index in 0..message_count {
        let Some(message) = select.peek_message(index) else {
            continue;
        };
        if markers.iter().any(|marker| message == Term::atom(*marker)) {
            select.remove_message(index);
            return;
        }
    }
    // A non-empty mailbox with no aion marker would make a subsequent
    // suspend insta-rewake into a busy spin. The engine is the only producer
    // of workflow-process messages and only enqueues the marker atoms, so
    // this is unreachable today — but if the message surface ever widens,
    // this trace is the observable symptom.
    tracing::warn!(
        pid = ?process_context.pid(),
        queued_messages = message_count,
        "suspending await found no consumable aion wake marker in a non-empty mailbox"
    );
}
