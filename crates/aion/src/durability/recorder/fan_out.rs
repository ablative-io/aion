//! Durable fan-out dispatch: the additive atomic write of the
//! `N×(ActivityScheduled + ActivityStarted)` event batch together with the matching `N` outbox
//! rows, in one store transaction.
//!
//! This is the Phase 1 Recorder capability behind the interim durable outbox. It is **additive**:
//! the live dispatch path (`nif_collect.rs::dispatch_unscheduled`) still records its events and
//! spawns completion tasks exactly as before. A later flag-gated cutover will route fresh fan-out
//! batches through [`Recorder::record_fan_out_dispatch`] instead.

use aion_core::{ActivityError, ActivityId, Event, EventEnvelope, Payload};
use aion_store::OutboxRow;
use chrono::{DateTime, Utc};

use super::Recorder;
use crate::durability::DurabilityError;

/// Terminal outcome of a single fan-out activity, carrying exactly the fields the matching terminal
/// event records.
///
/// [`FanOutOutcome::Completed`] maps to [`Event::ActivityCompleted`] (its `result` payload);
/// [`FanOutOutcome::Failed`] maps to [`Event::ActivityFailed`] (its classified `error` and one-based
/// `attempt`). The shapes mirror the events the live completion path records today through
/// [`Recorder::record_activity_completed`] / [`Recorder::record_activity_failed`].
#[derive(Clone, Debug)]
pub enum FanOutOutcome {
    /// The activity succeeded with this result payload.
    Completed(Payload),
    /// The activity attempt failed with this classified error on the given one-based attempt.
    Failed {
        /// Classified activity failure.
        error: ActivityError,
        /// One-based activity attempt number that produced this failure.
        attempt: u32,
    },
}

/// Outcome of [`Recorder::record_fan_out_completion`]: whether the terminal was newly recorded or
/// dropped as a duplicate of an already-resolved ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanOutCompletionResult {
    /// The terminal event was appended; the head advanced by exactly one.
    Recorded,
    /// The ordinal already had a terminal in history; nothing was appended and the head is unchanged.
    Dropped,
}

/// One member of a durable fan-out dispatch staged through [`Recorder::record_fan_out_dispatch`].
///
/// Each item carries the pinned `ordinal` within the workflow's contiguous fan-out range (the same
/// ordinal `nif_collect.rs` allocates), and the activity type and input the worker must execute. The
/// recorder derives the per-item `ActivityScheduled`/`ActivityStarted` events and the matching
/// [`OutboxRow`] (with `dispatch_key = "{workflow_id}:{ordinal}"`) from this.
#[derive(Clone, Debug)]
pub struct FanOutItem {
    /// Pinned ordinal of this activity within the workflow's fan-out range.
    pub ordinal: u64,
    /// Activity type the worker must execute.
    pub activity_type: String,
    /// Opaque activity input payload.
    pub input: Payload,
}

impl Recorder {
    /// Atomically records a durable fan-out dispatch: the `N×(ActivityScheduled + ActivityStarted)`
    /// event batch AND the matching `N` outbox rows in one store transaction.
    ///
    /// The event batch is the same shape the live dispatch path
    /// (`nif_collect.rs::dispatch_unscheduled`) records today — for each item, an
    /// [`Event::ActivityScheduled`] immediately followed by an [`Event::ActivityStarted`], both
    /// keyed by `ActivityId::from_sequence_position(ordinal)`, in `items` order. Each outbox row is a
    /// fresh [`OutboxRow::pending`] with the canonical `dispatch_key = "{workflow_id}:{ordinal}"`
    /// idempotency guard.
    ///
    /// On success the sequence head advances by exactly `2 * items.len()`. The store applies the
    /// events and outbox rows in a single transaction, so a failure (including
    /// [`StoreError::SequenceConflict`](aion_store::StoreError::SequenceConflict)) leaves the head
    /// unadvanced AND writes neither events nor outbox rows — matching the single-writer discipline
    /// where a conflict is a hard error, never a retry. An empty `items` slice is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if the event store rejects the atomic append (a sequence conflict
    /// surfaces without advancing the head), if an envelope sequence would overflow `u64`, or if the
    /// sequence tracker cannot advance after a successful append.
    pub async fn record_fan_out_dispatch(
        &mut self,
        recorded_at: DateTime<Utc>,
        items: &[FanOutItem],
    ) -> Result<(), DurabilityError> {
        if items.is_empty() {
            return Ok(());
        }

        let mut events = Vec::with_capacity(items.len() * 2);
        let mut outbox_rows = Vec::with_capacity(items.len());
        let mut previous: Option<EventEnvelope> = None;
        for item in items {
            let scheduled_envelope = match &previous {
                Some(previous) => self.envelope_after(previous, recorded_at)?,
                None => self.next_envelope(recorded_at)?,
            };
            let started_envelope = self.envelope_after(&scheduled_envelope, recorded_at)?;
            previous = Some(started_envelope.clone());

            let activity_id = ActivityId::from_sequence_position(item.ordinal);
            events.push(Event::ActivityScheduled {
                envelope: scheduled_envelope,
                activity_id: activity_id.clone(),
                activity_type: item.activity_type.clone(),
                input: item.input.clone(),
            });
            events.push(Event::ActivityStarted {
                envelope: started_envelope,
                activity_id,
            });
            outbox_rows.push(OutboxRow::pending(
                self.workflow_id.clone(),
                item.ordinal,
                item.activity_type.clone(),
                item.input.clone(),
                recorded_at,
            ));
        }

        let expected_seq = self.sequence.current();
        self.store
            .append_with_outbox(
                self.write_token,
                &self.workflow_id,
                &events,
                expected_seq,
                &outbox_rows,
            )
            .await?;
        self.sequence.mark_append_success(events.len())
    }

    /// Store-backed completion dedup for one fan-out ordinal: the cross-node completion chokepoint.
    ///
    /// Determines whether [`ActivityId::from_sequence_position(ordinal)`](ActivityId::from_sequence_position)
    /// already has a terminal ([`Event::ActivityCompleted`] or [`Event::ActivityFailed`]) in this
    /// workflow's recorded history. This is the same "resolved" predicate the live dispatch path uses
    /// (`nif_collect.rs::recorded_terminal`): an ordinal is resolved once either terminal is present.
    ///
    /// - If the ordinal is **already resolved**, returns [`FanOutCompletionResult::Dropped`] WITHOUT
    ///   writing anything — no store append, the sequence head is unchanged. This is the core dedup
    ///   invariant: a duplicate completion (e.g. a redelivered cross-node send) never appends a
    ///   second terminal.
    /// - If the ordinal is **not yet resolved**, appends the single terminal event derived from
    ///   `outcome` ([`Event::ActivityCompleted`] for [`FanOutOutcome::Completed`],
    ///   [`Event::ActivityFailed`] for [`FanOutOutcome::Failed`]) through the normal terminal-append
    ///   path; the head advances by exactly one and it returns [`FanOutCompletionResult::Recorded`].
    ///
    /// # Boundary
    ///
    /// This method is **additive** and does NOT touch the runtime completion sink: it does not wake
    /// any workflow PID, does not signal `WorkerActivityDispatcher`, and does not interact with
    /// `nif_collect.rs`. Routing a recorded completion to a waiting collect is the later flag-gated
    /// cutover's responsibility; this method only owns the durable dedup-and-append.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] if reading history fails, if the terminal append is rejected (a
    /// [`StoreError::SequenceConflict`](aion_store::StoreError::SequenceConflict) surfaces as a hard
    /// error with the head unadvanced, mirroring the single-writer discipline), or if the sequence
    /// tracker cannot advance after a successful append.
    pub async fn record_fan_out_completion(
        &mut self,
        recorded_at: DateTime<Utc>,
        ordinal: u64,
        outcome: FanOutOutcome,
    ) -> Result<FanOutCompletionResult, DurabilityError> {
        let activity_id = ActivityId::from_sequence_position(ordinal);
        let history = self.store.read_history(&self.workflow_id).await?;
        if ordinal_is_resolved(&history, &activity_id) {
            return Ok(FanOutCompletionResult::Dropped);
        }

        match outcome {
            FanOutOutcome::Completed(result) => {
                self.append_with(recorded_at, |envelope| Event::ActivityCompleted {
                    envelope,
                    activity_id,
                    result,
                })
                .await?;
            }
            FanOutOutcome::Failed { error, attempt } => {
                self.append_with(recorded_at, |envelope| Event::ActivityFailed {
                    envelope,
                    activity_id,
                    error,
                    attempt,
                })
                .await?;
            }
        }
        Ok(FanOutCompletionResult::Recorded)
    }
}

/// Whether `activity_id`'s ordinal already has a terminal ([`Event::ActivityCompleted`],
/// [`Event::ActivityFailed`], or [`Event::ActivityCancelled`]) recorded in `history`.
///
/// Mirrors the full "resolved" terminal set of `nif_collect.rs::recorded_terminal` — including
/// `ActivityCancelled`. A cancelled ordinal IS terminally resolved, so a late worker completion
/// arriving for it must be dropped, not recorded over the cancellation.
fn ordinal_is_resolved(history: &[Event], activity_id: &ActivityId) -> bool {
    history.iter().any(|event| match event {
        Event::ActivityCompleted {
            activity_id: recorded,
            ..
        }
        | Event::ActivityFailed {
            activity_id: recorded,
            ..
        }
        | Event::ActivityCancelled {
            activity_id: recorded,
            ..
        } => recorded == activity_id,
        _ => false,
    })
}
