//! Replay/live splice for per-workflow subscription resumption.
//!
//! A resume cursor (`resume_from_seq` = R, "first seq wanted") is honored by
//! replaying the recorded history slice `[R ..= head]` and then tailing the
//! live broadcast filtered to `seq > head`:
//!
//! - **Gap-free**: the caller attaches the live subscription *before* taking
//!   the history snapshot, and the engine's `PublishingEventStore` broadcasts
//!   strictly after durable commit, so every event with `seq > head` was
//!   committed — and therefore broadcast — after the live receiver attached.
//! - **Duplicate-free**: the replay slice delivers exactly `[R ..= head]` from
//!   the snapshot, and the live filter drops every `seq <= head`, so an event
//!   that both landed in the snapshot and arrived on the broadcast is emitted
//!   exactly once, from the snapshot.
//!
//! Anti-leak contract: callers run the namespace guard verdict before reading
//! history or validating the cursor, so an unauthorized probe always receives
//! the guard's `not_found` and never a cursor error that would disclose a
//! foreign workflow's existence or history length.

use aion::EventStreamLagged;
use aion_core::Event;
use aion_proto::{WireError, WireErrorCode};
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::error::ServerError;

/// `error_type` discriminator for a cursor beyond the recorded history head.
pub const RESUME_CURSOR_AHEAD_OF_HISTORY: &str = "ResumeCursorAheadOfHistory";

/// Live event stream item type shared with the engine subscription seam.
pub type LiveEventStream = BoxStream<'static, Result<Event, EventStreamLagged>>;

/// Validate a resume cursor against a history snapshot and build the splice.
///
/// `live` must be attached before `history` was read (subscribe-then-snapshot)
/// and `history` must be the full per-workflow history sorted by `seq` — both
/// halves of the gap-free argument documented on this module.
///
/// Returns the replay slice (`seq >= resume_from_seq`) and the live tail
/// filtered to `seq > head`; lag items pass through unfiltered so a lagging
/// consumer is always told, never silently gapped.
///
/// # Errors
///
/// Returns [`ServerError::Wire`] `invalid_input` when `resume_from_seq` is `0`,
/// or `invalid_input` with `error_type` [`RESUME_CURSOR_AHEAD_OF_HISTORY`] when
/// `resume_from_seq > head + 1`.
pub fn splice(
    live: LiveEventStream,
    history: Vec<Event>,
    resume_from_seq: u64,
) -> Result<(Vec<Event>, LiveEventStream), ServerError> {
    if resume_from_seq == 0 {
        return Err(WireError::invalid_input("resume_from_seq must be >= 1").into());
    }
    let head = history.last().map_or(0, Event::seq);
    if resume_from_seq > head.saturating_add(1) {
        return Err(WireError::new_with_type(
            WireErrorCode::InvalidInput,
            RESUME_CURSOR_AHEAD_OF_HISTORY,
            format!(
                "resume_from_seq {resume_from_seq} is ahead of recorded history \
                 (head seq {head}); the largest valid cursor is {}",
                head.saturating_add(1)
            ),
        )
        .into());
    }

    let mut history = history;
    let replay_start = history.partition_point(|event| event.seq() < resume_from_seq);
    let replay = history.split_off(replay_start);

    let tail = live
        .filter(move |item| {
            let keep = match item {
                Ok(event) => event.seq() > head,
                // Lag is information, never filtered away.
                Err(EventStreamLagged { .. }) => true,
            };
            futures::future::ready(keep)
        })
        .boxed();

    Ok((replay, tail))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aion::EventStreamLagged;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use aion_proto::WireErrorCode;
    use futures::{StreamExt, stream};

    use super::{RESUME_CURSOR_AHEAD_OF_HISTORY, splice};
    use crate::namespace::{NamespaceResolver, StaticWorkflowNamespaces};
    use crate::stream::namespace_filter::NamespaceEventGate;
    use crate::stream::socket::spawn_encoded_event_stream;

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }

    fn signal(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::SignalReceived {
            envelope: EventEnvelope {
                seq,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id(),
            },
            name: format!("signal-{seq}"),
            payload: Payload::from_json(&serde_json::json!({ "seq": seq }))?,
        })
    }

    fn completed(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowCompleted {
            envelope: EventEnvelope {
                seq,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id(),
            },
            result: Payload::from_json(&serde_json::json!({ "seq": seq }))?,
        })
    }

    fn history(seqs: std::ops::RangeInclusive<u64>) -> Result<Vec<Event>, aion_core::PayloadError> {
        seqs.map(signal).collect()
    }

    fn live(
        items: Vec<Result<Event, EventStreamLagged>>,
    ) -> futures::stream::BoxStream<'static, Result<Event, EventStreamLagged>> {
        stream::iter(items).boxed()
    }

    fn gate() -> Result<NamespaceEventGate, Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), "tenant-a")?;
        let resolver = NamespaceResolver::authorization_only(
            crate::config::NamespaceMode::SharedEngine,
            ownership,
            crate::namespace::StaticScheduleNamespaces::default(),
        );
        let capacity = std::num::NonZeroUsize::new(8).ok_or("verdict capacity must be non-zero")?;
        Ok(NamespaceEventGate::new(
            resolver,
            "tenant-a".to_owned(),
            capacity,
        ))
    }

    fn delivered_seqs(events: &[Event]) -> Vec<u64> {
        events.iter().map(Event::seq).collect()
    }

    #[tokio::test]
    async fn cursor_zero_is_invalid_input() -> Result<(), Box<dyn std::error::Error>> {
        let error = splice(live(Vec::new()), history(1..=3)?, 0)
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("cursor 0 must be rejected")?;

        assert_eq!(error.code, WireErrorCode::InvalidInput);
        assert!(error.message.contains("resume_from_seq must be >= 1"));
        Ok(())
    }

    #[tokio::test]
    async fn cursor_ahead_of_history_is_invalid_input_with_discriminator()
    -> Result<(), Box<dyn std::error::Error>> {
        let error = splice(live(Vec::new()), history(1..=5)?, 7)
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("cursor head+2 must be rejected")?;

        assert_eq!(error.code, WireErrorCode::InvalidInput);
        assert_eq!(
            error.error_type.as_deref(),
            Some(RESUME_CURSOR_AHEAD_OF_HISTORY)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cursor_ahead_of_empty_history_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let error = splice(live(Vec::new()), Vec::new(), 2)
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("cursor 2 over empty history must be rejected")?;

        assert_eq!(
            error.error_type.as_deref(),
            Some(RESUME_CURSOR_AHEAD_OF_HISTORY)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cursor_at_head_plus_one_yields_empty_replay_and_live_tail_only()
    -> Result<(), Box<dyn std::error::Error>> {
        let (replay, tail) = splice(
            live(vec![Ok(signal(6)?), Ok(signal(7)?)]),
            history(1..=5)?,
            6,
        )?;

        assert!(replay.is_empty(), "head+1 cursor must replay nothing");
        let tail: Vec<u64> = tail
            .map(|item| item.map(|event| event.seq()).unwrap_or_default())
            .collect()
            .await;
        assert_eq!(tail, vec![6, 7]);
        Ok(())
    }

    #[tokio::test]
    async fn overlap_between_snapshot_and_live_is_deduplicated_contiguous_unique()
    -> Result<(), Box<dyn std::error::Error>> {
        // Snapshot holds 1..=5; the live broadcast re-emits 4 and 5 (arrived
        // between attach and snapshot) before the genuinely new 6.
        let (replay, tail) = splice(
            live(vec![Ok(signal(4)?), Ok(signal(5)?), Ok(signal(6)?)]),
            history(1..=5)?,
            1,
        )?;

        let mut delivered = delivered_seqs(&replay);
        let tail: Vec<u64> = tail
            .map(|item| item.map(|event| event.seq()).unwrap_or_default())
            .collect()
            .await;
        delivered.extend(tail);
        assert_eq!(
            delivered,
            vec![1, 2, 3, 4, 5, 6],
            "delivery must be contiguous and duplicate-free"
        );
        Ok(())
    }

    #[tokio::test]
    async fn mid_history_cursor_replays_suffix_only() -> Result<(), Box<dyn std::error::Error>> {
        let (replay, _tail) = splice(live(Vec::new()), history(1..=5)?, 3)?;

        assert_eq!(delivered_seqs(&replay), vec![3, 4, 5]);
        Ok(())
    }

    #[tokio::test]
    async fn replay_containing_terminal_event_closes_after_it()
    -> Result<(), Box<dyn std::error::Error>> {
        // Terminal at seq 3 mid-replay: the socket must deliver 1..=3 and then
        // close without draining the live tail (CAN/terminal run boundary).
        let mut history = history(1..=2)?;
        history.push(completed(3)?);
        history.push(signal(4)?);
        let (replay, tail) = splice(live(vec![Ok(signal(5)?)]), history, 1)?;

        let subscription = crate::stream::EventSubscription {
            namespace: "tenant-a".to_owned(),
            filter: aion::EventFilter::default(),
            selector: crate::stream::selector::SubscriptionSelector::unrestricted(),
            workflow_target: Some(workflow_id()),
            replay,
            events: tail,
        };
        let mut encoded = spawn_encoded_event_stream(subscription, gate()?, 8)?;

        let mut frames = 0_usize;
        while let Some(frame) =
            tokio::time::timeout(Duration::from_secs(1), encoded.frames.recv()).await?
        {
            drop(frame);
            frames += 1;
        }
        assert_eq!(
            frames, 3,
            "stream must close after the terminal replay frame"
        );
        Ok(())
    }

    #[tokio::test]
    async fn lag_mid_splice_surfaces_terminal_lagged_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let (replay, tail) = splice(
            live(vec![Err(EventStreamLagged { skipped: 3 })]),
            history(1..=2)?,
            1,
        )?;

        let subscription = crate::stream::EventSubscription {
            namespace: "tenant-a".to_owned(),
            filter: aion::EventFilter::default(),
            selector: crate::stream::selector::SubscriptionSelector::unrestricted(),
            workflow_target: Some(workflow_id()),
            replay,
            events: tail,
        };
        let mut encoded = spawn_encoded_event_stream(subscription, gate()?, 8)?;

        let mut frames = 0_usize;
        while let Some(frame) =
            tokio::time::timeout(Duration::from_secs(1), encoded.frames.recv()).await?
        {
            drop(frame);
            frames += 1;
        }
        assert_eq!(frames, 2, "both replay frames must be delivered before lag");
        let lag = tokio::time::timeout(Duration::from_secs(1), encoded.lagged).await??;
        assert_eq!(lag.code, WireErrorCode::Lagged);
        Ok(())
    }
}
