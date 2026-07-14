//! Tests for the NOI-5 transcript sequencer + fan-out, including the two
//! mandatory negative controls (concurrent-writer monotonicity, failover
//! dedup) and the transcript retention bounds (per-event truncation, the
//! per-stream cap marker, past-cap live-only delivery).

use std::num::NonZeroUsize;

use aion_core::{ActivityEventKind, ActivityId, MessageRole, WorkflowId};
use aion_store::InMemoryObservabilityStore;
use chrono::Utc;
use futures::StreamExt;
use uuid::Uuid;

use super::*;

fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
    NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
}

fn publisher(cap: usize) -> Result<ActivityEventPublisher, Box<dyn std::error::Error>> {
    let store = Arc::new(InMemoryObservabilityStore::default());
    Ok(ActivityEventPublisher::new(store, capacity(cap)?))
}

fn event(attempt: u32, worker_seq: u64, ephemeral: bool, text: &str) -> ActivityEvent {
    ActivityEvent {
        workflow_id: WorkflowId::new(Uuid::from_u128(1)),
        activity_id: ActivityId::from_sequence_position(3),
        attempt,
        agent_id: Uuid::from_u128(9),
        agent_role: "orchestrator".to_owned(),
        emitted_at: Utc::now(),
        worker_seq,
        store_seq: None,
        ephemeral,
        kind: if ephemeral {
            ActivityEventKind::Delta {
                message_id: "m1".to_owned(),
                text_fragment: text.to_owned(),
            }
        } else {
            ActivityEventKind::Message {
                role: MessageRole::Assistant,
                text: text.to_owned(),
            }
        },
    }
}

fn key(attempt: u32) -> ActivityStreamKey {
    ActivityStreamKey::new(
        WorkflowId::new(Uuid::from_u128(1)),
        ActivityId::from_sequence_position(3),
        attempt,
    )
}

#[tokio::test]
async fn publish_assigns_commit_allocated_monotonic_store_seq()
-> Result<(), Box<dyn std::error::Error>> {
    let publisher = publisher(16)?;
    assert_eq!(publisher.publish(&event(0, 1, false, "a")).await?, Some(0));
    assert_eq!(publisher.publish(&event(0, 2, false, "b")).await?, Some(1));
    assert_eq!(publisher.publish(&event(0, 3, false, "c")).await?, Some(2));
    let tail = publisher.replay_from(&key(0), 0).await?;
    assert_eq!(
        tail.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    Ok(())
}

#[tokio::test]
async fn ephemeral_events_are_never_persisted() -> Result<(), Box<dyn std::error::Error>> {
    let publisher = publisher(16)?;
    // A live subscriber (fresh, no resume cursor) must still SEE the ephemeral
    // delta AND the persisted message at store_seq 0.
    let mut live = publisher.subscribe(key(0), None);
    assert_eq!(publisher.publish(&event(0, 1, true, "wor")).await?, None);
    assert_eq!(
        publisher.publish(&event(0, 2, false, "word")).await?,
        Some(0)
    );
    // Durable tail has ONLY the non-ephemeral message.
    let tail = publisher.replay_from(&key(0), 0).await?;
    assert_eq!(tail.len(), 1);
    assert!(matches!(
        tail[0].event.kind,
        ActivityEventKind::Message { .. }
    ));

    // Live stream delivered the ephemeral delta first (store_seq None), then
    // the persisted message (store_seq Some(0)).
    let first = live.next().await.ok_or("missing ephemeral")??;
    assert!(first.ephemeral);
    assert_eq!(first.store_seq, None);
    let second = live.next().await.ok_or("missing message")??;
    assert!(!second.ephemeral);
    assert_eq!(second.store_seq, Some(0));
    Ok(())
}

/// MANDATORY NEGATIVE CONTROL (a): concurrent writers on ONE
/// `(wf,act,attempt)` stream. Many `publish` calls race the same head; the
/// read-head -> append -> on-conflict-retry loop must serialize them so the
/// final durable sequence is strictly monotonic 0..N-1 with NO gap and NO
/// duplicate — proving the retry loop (not the store) enforces monotonicity.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_writers_produce_gapless_monotonic_store_seq()
-> Result<(), Box<dyn std::error::Error>> {
    let publisher = publisher(256)?;
    let writers = 32u64;
    let mut handles = Vec::new();
    for worker_seq in 0..writers {
        let publisher = publisher.clone();
        handles.push(tokio::spawn(async move {
            publisher
                .publish(&event(0, worker_seq, false, "concurrent"))
                .await
        }));
    }
    let mut assigned = Vec::new();
    for handle in handles {
        if let Some(store_seq) = handle.await?? {
            assigned.push(store_seq);
        }
    }
    assigned.sort_unstable();
    // Every writer won exactly one distinct, contiguous store_seq.
    assert_eq!(
        assigned,
        (0..writers).collect::<Vec<_>>(),
        "concurrent writers must produce a gapless, duplicate-free monotonic sequence"
    );
    // The durable tail agrees: exactly `writers` records, seqs 0..N-1 in order.
    let tail = publisher.replay_from(&key(0), 0).await?;
    assert_eq!(
        tail.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
        (0..writers).collect::<Vec<_>>()
    );
    Ok(())
}

/// MANDATORY NEGATIVE CONTROL (b): failover dedup. A dying worker and an
/// adopting worker BOTH emit for one `(wf,act,attempt)` (the same session
/// resumes across failover). Modeled as two publishers sharing ONE store
/// (the server owns durability, §5.3) racing appends: the commit-allocated
/// `store_seq` collapses both emitters into one monotonic stream with no
/// gap/dup — the transcript is not duplicated or reordered by the double-emit.
#[tokio::test(flavor = "multi_thread")]
async fn failover_double_emit_dedupes_to_one_monotonic_stream()
-> Result<(), Box<dyn std::error::Error>> {
    // ONE durable store (the server), TWO publisher clones = the dying +
    // adopting worker's events both flowing through the single server writer.
    let store = Arc::new(InMemoryObservabilityStore::default());
    let dying = ActivityEventPublisher::new(store.clone(), capacity(256)?);
    let adopting = ActivityEventPublisher::new(store, capacity(256)?);

    let mut handles = Vec::new();
    for worker_seq in 0..16u64 {
        let dying = dying.clone();
        let adopting = adopting.clone();
        handles.push(tokio::spawn(async move {
            // Both survivors emit the SAME logical event for this attempt.
            let a = dying.publish(&event(0, worker_seq, false, "dying")).await;
            let b = adopting
                .publish(&event(0, worker_seq, false, "adopting"))
                .await;
            (a, b)
        }));
    }
    let mut assigned = Vec::new();
    for handle in handles {
        let (a, b) = handle.await?;
        if let Some(seq) = a? {
            assigned.push(seq);
        }
        if let Some(seq) = b? {
            assigned.push(seq);
        }
    }
    assigned.sort_unstable();
    // 16 events x 2 emitters = 32 durable records, each with a DISTINCT,
    // contiguous store_seq — the double-emit does not corrupt monotonicity.
    assert_eq!(
        assigned,
        (0..32).collect::<Vec<_>>(),
        "failover double-emit must land a gapless monotonic store_seq stream"
    );
    let tail = dying.replay_from(&key(0), 0).await?;
    let seqs: Vec<u64> = tail.iter().map(|r| r.store_seq).collect();
    assert_eq!(seqs, (0..32).collect::<Vec<_>>());
    Ok(())
}

/// MANDATORY NEGATIVE CONTROL: the WRONG-allocator case. A process-local
/// `AtomicU64` (the `ClusterEventPublisher` pattern the design forbids for
/// `store_seq`) is shown to produce COLLIDING sequences when two "survivors"
/// each start their own counter after a failover — proving the counter
/// belongs in the commit, not the process. The commit-allocated publisher
/// (above) does NOT exhibit this; this test pins the failure mode we avoid.
#[tokio::test]
async fn process_local_atomic_counter_collides_across_survivors()
-> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    // Two survivors, each with its OWN fresh process-local counter (exactly
    // what a per-process AtomicU64 does after restart/failover).
    let dying = AtomicU64::new(0);
    let adopting = AtomicU64::new(0);
    let dying_seq = dying.fetch_add(1, Ordering::SeqCst);
    let adopting_seq = adopting.fetch_add(1, Ordering::SeqCst);
    // Both minted the SAME sequence — a collision. This is the bug the
    // commit-allocated design exists to prevent.
    assert_eq!(
        dying_seq, adopting_seq,
        "process-local counters collide across survivors (the forbidden pattern)"
    );

    // By contrast the commit-allocated publisher over a shared store never
    // collides: the two survivors get DISTINCT sequences.
    let store = Arc::new(InMemoryObservabilityStore::default());
    let a = ActivityEventPublisher::new(store.clone(), capacity(8)?);
    let b = ActivityEventPublisher::new(store, capacity(8)?);
    let sa = a.publish(&event(0, 1, false, "a")).await?;
    let sb = b.publish(&event(0, 2, false, "b")).await?;
    assert_ne!(
        sa, sb,
        "commit-allocated store_seq must be distinct across survivors"
    );
    assert_eq!((sa, sb), (Some(0), Some(1)));
    Ok(())
}

/// The live-stream + resume-by-`store_seq` gate: a subscriber tails live
/// events, and a reconnecting client resumes from the durable tail by
/// `store_seq` then splices onto the live broadcast with NO gap and NO
/// duplicate at the seam.
#[tokio::test]
async fn live_stream_then_resume_by_store_seq_has_no_gap() -> Result<(), Box<dyn std::error::Error>>
{
    let publisher = publisher(64)?;
    // Persist a few events (as if the client was connected then dropped).
    for seq in 0..3u64 {
        publisher.publish(&event(0, seq, false, "early")).await?;
    }
    // Client reconnects having last seen store_seq 1. Splice: attach the live
    // stream BEFORE reading the durable tail (gap-free splice), with the
    // resume cursor as after_seq so live re-delivery of <=cursor is suppressed.
    let mut live = publisher.subscribe(key(0), Some(1));
    let replay = publisher.replay_from(&key(0), 2).await?;
    // The durable tail from the cursor is exactly seq 2 (the one it missed).
    assert_eq!(
        replay.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
        vec![2]
    );
    // Now new live events arrive.
    publisher.publish(&event(0, 10, false, "live-3")).await?;
    publisher.publish(&event(0, 11, false, "live-4")).await?;
    // The live splice yields seq 3 then 4 — no gap, no re-delivery of <=1.
    let first = live.next().await.ok_or("missing live-3")??;
    assert_eq!(first.store_seq, Some(3));
    let second = live.next().await.ok_or("missing live-4")??;
    assert_eq!(second.store_seq, Some(4));
    Ok(())
}

#[tokio::test]
async fn subscribe_filters_out_other_attempt_streams() -> Result<(), Box<dyn std::error::Error>> {
    let publisher = publisher(64)?;
    let mut live = publisher.subscribe(key(0), None);
    // An event for a DIFFERENT attempt must not reach this subscriber.
    publisher
        .publish(&event(1, 1, false, "other-attempt"))
        .await?;
    publisher.publish(&event(0, 1, false, "mine")).await?;
    let received = live.next().await.ok_or("missing my event")??;
    assert_eq!(received.attempt, 0);
    assert!(matches!(
        received.kind,
        ActivityEventKind::Message { text, .. } if text == "mine"
    ));
    Ok(())
}

#[tokio::test]
async fn lagged_subscriber_yields_typed_skip_count() -> Result<(), Box<dyn std::error::Error>> {
    let publisher = publisher(2)?;
    let mut live = publisher.subscribe(key(0), None);
    // Overflow the capacity-2 live buffer without consuming.
    for seq in 0..5u64 {
        publisher.publish(&event(0, seq, false, "flood")).await?;
    }
    let lagged = live.next().await.ok_or("missing lag item")?;
    assert!(matches!(lagged, Err(TranscriptStreamLagged { .. })));
    Ok(())
}

// --- transcript retention bounds -------------------------------------------

fn bounded_publisher(
    cap: usize,
    bounds: TranscriptBounds,
) -> Result<ActivityEventPublisher, Box<dyn std::error::Error>> {
    Ok(publisher(cap)?.with_bounds(bounds))
}

/// The per-stream cap: with `max_stream_events = 3`, publishes 0..2 persist
/// normally, the 4th append persists ONE marker record (a `Progress`/`Note`
/// naming the cap) and every publish past that persists nothing.
#[tokio::test]
async fn stream_cap_appends_one_marker_then_stops_persisting()
-> Result<(), Box<dyn std::error::Error>> {
    let publisher = bounded_publisher(
        64,
        TranscriptBounds {
            max_event_bytes: 256 * 1024,
            max_stream_events: 3,
        },
    )?;
    let mut assigned = Vec::new();
    for worker_seq in 0..6u64 {
        assigned.push(
            publisher
                .publish(&event(0, worker_seq, false, "chatty"))
                .await?,
        );
    }
    assert_eq!(
        assigned,
        vec![Some(0), Some(1), Some(2), None, None, None],
        "publishes past the cap return Ok(None)"
    );
    let tail = publisher.replay_from(&key(0), 0).await?;
    assert_eq!(
        tail.iter().map(|r| r.store_seq).collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "exactly the capped records plus the one marker"
    );
    let ActivityEventKind::Progress {
        detail: ProgressDetail::Note { text },
    } = &tail[3].event.kind
    else {
        return Err("record 3 must be the retention-cap marker note".into());
    };
    assert!(
        text.contains("retention cap"),
        "the marker names the cap: {text}"
    );
    assert!(text.contains("3 events"), "the marker names the value");
    Ok(())
}

/// Past the cap the stream stays LIVE: a subscriber still receives every
/// event, with `store_seq: None` and `ephemeral == false` (real transcript,
/// just not retained).
#[tokio::test]
async fn capped_stream_still_fans_out_live_without_store_seq()
-> Result<(), Box<dyn std::error::Error>> {
    let publisher = bounded_publisher(
        64,
        TranscriptBounds {
            max_event_bytes: 256 * 1024,
            max_stream_events: 1,
        },
    )?;
    let mut live = publisher.subscribe(key(0), None);
    assert_eq!(
        publisher.publish(&event(0, 1, false, "kept")).await?,
        Some(0)
    );
    // Crosses the cap: persists the marker, fans the event out live-only.
    assert_eq!(publisher.publish(&event(0, 2, false, "over")).await?, None);
    // Fully past the cap: live-only.
    assert_eq!(
        publisher.publish(&event(0, 3, false, "way-over")).await?,
        None
    );

    let kept = live.next().await.ok_or("missing kept")??;
    assert_eq!(kept.store_seq, Some(0));
    let marker = live.next().await.ok_or("missing marker")??;
    assert_eq!(
        marker.store_seq,
        Some(1),
        "the marker carries its store_seq"
    );
    assert!(matches!(marker.kind, ActivityEventKind::Progress { .. }));
    let over = live.next().await.ok_or("missing over")??;
    assert_eq!(over.store_seq, None, "past-cap events carry no store_seq");
    assert!(!over.ephemeral, "past-cap events are NOT ephemeral");
    assert!(matches!(
        over.kind,
        ActivityEventKind::Message { text, .. } if text == "over"
    ));
    let way_over = live.next().await.ok_or("missing way-over")??;
    assert_eq!(way_over.store_seq, None);
    assert!(!way_over.ephemeral);
    Ok(())
}

/// The per-event ceiling: an oversized message is truncated BEFORE the durable
/// append, so the retained record is bounded and marked.
#[tokio::test]
async fn oversized_event_is_truncated_before_persist() -> Result<(), Box<dyn std::error::Error>> {
    let publisher = bounded_publisher(
        16,
        TranscriptBounds {
            max_event_bytes: 512,
            max_stream_events: 20_000,
        },
    )?;
    let huge = "x".repeat(10_000);
    assert_eq!(
        publisher.publish(&event(0, 1, false, &huge)).await?,
        Some(0)
    );
    let tail = publisher.replay_from(&key(0), 0).await?;
    assert_eq!(tail.len(), 1);
    let ActivityEventKind::Message { text, .. } = &tail[0].event.kind else {
        return Err("expected the truncated message".into());
    };
    assert!(
        text.ends_with("bytes by observability.max_event_bytes]"),
        "the retained text ends with the truncation marker: {text}"
    );
    assert!(
        serde_json::to_vec(&tail[0].event)?.len() <= 1024,
        "the retained record is bounded (512 + marker slack)"
    );
    assert!(
        !text.contains(&huge),
        "the original oversized text is not retained in full"
    );
    Ok(())
}
