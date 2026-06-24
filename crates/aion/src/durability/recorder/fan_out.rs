//! Durable fan-out dispatch: the additive atomic write of the
//! `N×(ActivityScheduled + ActivityStarted)` event batch together with the matching `N` outbox
//! rows, in one store transaction.
//!
//! This is the Phase 1 Recorder capability behind the interim durable outbox. It is **additive**:
//! the live dispatch path (`nif_collect.rs::dispatch_unscheduled`) still records its events and
//! spawns completion tasks exactly as before. A later flag-gated cutover will route fresh fan-out
//! batches through [`Recorder::record_fan_out_dispatch`] instead.

use aion_core::{ActivityId, Event, EventEnvelope, Payload};
use aion_store::OutboxRow;
use chrono::{DateTime, Utc};

use super::Recorder;
use crate::durability::DurabilityError;

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
}
