//! Reconnect-aware observability drain for the liminal worker transport (#254).
//!
//! # Why this exists
//!
//! The worker streams an activity's transcript live to the server by publishing
//! each [`ActivityEvent`] on the [`OBSERVABILITY_CHANNEL`] over its push
//! connection (a [`PushWriter`] leg of the socket). The dispatch path already
//! SURVIVES a server loss: [`serve_with_redial`](crate::serve_with_redial) drives
//! [`run_redial_loop`](crate::runtime::liminal_redial::run_redial_loop), which on
//! a connection drop dials the next candidate and rebuilds a fresh
//! [`PushClient`](liminal_sdk::PushClient) on the survivor. The observability
//! drain used to capture ONE [`PushWriter`] snapshot at dispatch time, pinned to
//! the connection that received the dispatch — so when that connection died the
//! drain published to a dead socket forever, emitting one `Broken pipe` warning
//! PER event (the transcript silently lost) with no path back to the redialed
//! connection.
//!
//! # What this module gives the drain
//!
//! The drain now writes through a [`LiveWriter`]: a shared slot the redial driver
//! refreshes with the CURRENT connection's [`PushWriter`] on every (re)connection.
//! The drain re-resolves that slot per publish, so once the dispatch path redials
//! to a survivor the drain resumes on the SAME live connection — it survives a
//! server loss by riding the dispatch path's reconnect rather than opening a
//! second connection of its own.
//!
//! Because the transcript is best-effort LIVE streaming (durability is the
//! server's O-keyspace commit, not this transport), an event that cannot be
//! published during an outage is DROPPED, not buffered — inventing an unbounded
//! buffer here would trade a bounded, honest loss for an unbounded memory hazard.
//! The loss is made visible and counted: exactly ONE log line marks the outage
//! transition, subsequent failed events are dropped-and-counted WITHOUT
//! re-logging, and a single line reports the dropped total when the connection is
//! restored (or at end of activity if it never is). Between failed publishes the
//! drain backs off (the same bounded schedule the dispatch redial uses) so a dead
//! socket is not hammered once per event.
//!
//! # Testability
//!
//! The outage/recovery bookkeeping is a pure [`DrainState`] over an injected
//! `now` and an [`EventSink`] result, with no socket and no async, so the exact
//! coalescing + backoff + drop-count behaviour is unit-tested with in-memory
//! fakes exactly as the redial driver is. [`LiveWriter`] is the production
//! [`EventSink`]; the async [`run_drain`] loop wires the two together.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aion_core::ActivityEvent;
use liminal_sdk::{OBSERVABILITY_CHANNEL, PushWriter};
use tokio::sync::mpsc;

use crate::runtime::liminal_redial::RedialBackoff;

/// A shared slot holding the CURRENT live push-connection writer.
///
/// The redial driver refreshes it (via [`LiveWriter::set`]) with the survivor's
/// [`PushWriter`] on every (re)connection, and the observability drain reads it
/// (via the [`EventSink`] impl) per publish. Cloning is an `Arc` bump: every
/// clone — the one each drain holds and the one the connect closure updates —
/// refers to the SAME slot, so a writer installed by a reconnect is immediately
/// visible to a drain spawned against an earlier connection.
#[derive(Clone, Debug, Default)]
pub struct LiveWriter {
    slot: Arc<Mutex<Option<PushWriter>>>,
}

impl LiveWriter {
    /// Builds a slot pre-seeded with `writer` — the current connection's write
    /// leg, so a drain resolves a live writer immediately without waiting for a
    /// reconnect.
    #[must_use]
    pub fn seeded(writer: PushWriter) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Some(writer))),
        }
    }

    /// Installs `writer` as the current live connection, replacing any prior one.
    ///
    /// Called by the redial driver's connect closure on every (re)connection so a
    /// drain re-resolving the slot lands on the survivor. A poisoned lock is
    /// recovered (the guarded value is a plain `Option`, never left inconsistent
    /// by a panicking writer), so a poisoned slot never wedges the drain.
    pub fn set(&self, writer: PushWriter) {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(writer);
    }

    /// The current live writer, or `None` when no connection is up.
    fn current(&self) -> Option<PushWriter> {
        self.slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The result of attempting to publish one encoded event to the current
/// connection.
#[derive(Debug)]
enum DrainStep {
    /// The event reached the (current) live connection.
    Published,
    /// No live connection accepted the event; it is lost. `reason` is the
    /// transport error (or absence), logged ONCE on the outage transition.
    Broken {
        /// Human-readable cause, surfaced only on the first failure of an outage.
        reason: String,
    },
}

/// A publish sink the observability drain writes each encoded transcript event
/// to.
///
/// Abstracted over the concrete [`LiveWriter`] so the drain's outage/recovery
/// state machine is exercised with an in-memory fake that fails then recovers —
/// exactly as a redial swaps a dead connection for a survivor — without a socket.
trait EventSink: Send {
    /// Publish one already-encoded event to the current live connection.
    fn publish(&self, payload: Vec<u8>) -> DrainStep;
}

impl EventSink for LiveWriter {
    fn publish(&self, payload: Vec<u8>) -> DrainStep {
        match self.current() {
            Some(writer) => match writer.publish(OBSERVABILITY_CHANNEL, payload) {
                Ok(()) => DrainStep::Published,
                Err(error) => DrainStep::Broken {
                    reason: error.to_string(),
                },
            },
            None => DrainStep::Broken {
                reason: "no live connection is currently available".to_owned(),
            },
        }
    }
}

/// Pure outage/recovery bookkeeping for the observability drain, separated from
/// the async receive loop and the transport so it is exhaustively unit-testable.
///
/// It records whether the connection is currently believed broken and how many
/// events were dropped during the CURRENT outage, and — per event — decides
/// whether to probe the sink now or defer (backoff), and which single log
/// transition (if any) the outcome crosses. It never touches a socket or logs:
/// the caller drives it and emits exactly the one line each transition reports.
#[derive(Debug)]
struct DrainState {
    /// Whether the last probe failed and the connection is believed down.
    broken: bool,
    /// Events dropped since the current outage began (reset on recovery).
    dropped: u64,
    /// The bounded reconnect backoff (mirrors the dispatch redial schedule).
    backoff: RedialBackoff,
    /// Earliest instant a re-probe is allowed while broken; between probes,
    /// events are dropped-and-counted WITHOUT touching the dead socket.
    next_probe: Instant,
}

impl DrainState {
    /// Builds a healthy state over the reconnect `backoff` schedule.
    fn new(backoff: RedialBackoff, now: Instant) -> Self {
        Self {
            broken: false,
            dropped: 0,
            backoff,
            next_probe: now,
        }
    }

    /// Whether the sink should be probed for the next event: always while
    /// healthy, and while broken only once the backoff interval has elapsed.
    fn should_probe(&self, now: Instant) -> bool {
        !self.broken || now >= self.next_probe
    }

    /// Records an event dropped without probing (broken, still within backoff).
    fn on_deferred(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    /// Records a successful publish. Returns `Some(dropped)` when this crossed
    /// broken -> healthy (the single recovery transition, carrying the outage's
    /// dropped total), resetting the outage state and backoff; `None` when the
    /// connection was already healthy.
    fn on_published(&mut self) -> Option<u64> {
        if !self.broken {
            return None;
        }
        let dropped = self.dropped;
        self.broken = false;
        self.dropped = 0;
        self.backoff.reset();
        Some(dropped)
    }

    /// Records a failed publish: the event is dropped-and-counted and the next
    /// probe is pushed out by the current backoff (then widened). Returns `true`
    /// when this crossed healthy -> broken (the single outage-start transition),
    /// `false` on a subsequent failure within the same outage.
    fn on_broken(&mut self, now: Instant) -> bool {
        let started = !self.broken;
        self.broken = true;
        self.dropped = self.dropped.saturating_add(1);
        self.next_probe = now + self.backoff.current();
        self.backoff.increase();
        started
    }

    /// The dropped total to report at end of activity when the outage never
    /// recovered, or `None` when the connection ended healthy.
    fn on_close(&self) -> Option<u64> {
        (self.broken && self.dropped > 0).then_some(self.dropped)
    }
}

/// Drives the drain: publishes each drained [`ActivityEvent`] through `sink`
/// until the sender is dropped, coalescing outage logging and dropping (never
/// buffering) events while the connection is down.
async fn run_drain<S: EventSink>(
    sink: S,
    mut receiver: mpsc::UnboundedReceiver<ActivityEvent>,
    backoff: RedialBackoff,
) {
    let mut state = DrainState::new(backoff, Instant::now());
    while let Some(event) = receiver.recv().await {
        let payload = match serde_json::to_vec(&event) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "observability drain: failed to encode ActivityEvent");
                continue;
            }
        };
        let now = Instant::now();
        // While broken, do not hammer the dead socket once per event: between
        // backoff-spaced probes an event is dropped-and-counted in silence.
        if !state.should_probe(now) {
            state.on_deferred();
            continue;
        }
        match sink.publish(payload) {
            DrainStep::Published => {
                if let Some(dropped) = state.on_published() {
                    tracing::warn!(
                        dropped,
                        "observability drain: connection restored; resumed publishing after \
                         dropping transcript events during the outage"
                    );
                }
            }
            DrainStep::Broken { reason } => {
                if state.on_broken(now) {
                    tracing::warn!(
                        error = %reason,
                        "observability drain: connection lost; dropping transcript events until \
                         it is restored (best-effort live stream, no buffering)"
                    );
                }
            }
        }
    }
    if let Some(dropped) = state.on_close() {
        tracing::warn!(
            dropped,
            "observability drain: connection never restored before the activity ended; \
             transcript events were dropped"
        );
    }
}

/// A running observability-drain task tied to one activity attempt.
///
/// Holding it keeps the drain alive; [`EventDrain::finish`] drops the event
/// sender (ending the drain's receive loop) and awaits the task, so no
/// still-publishable event is lost and no task is leaked across dispatches.
pub(crate) struct EventDrain {
    handle: tokio::task::JoinHandle<()>,
}

impl EventDrain {
    /// Await the drain task to completion. The caller has already dropped its
    /// event sender (it was moved into the context/driver), so the drain's
    /// receiver closes and the task finishes after handling every queued event.
    pub(crate) async fn finish(self) {
        drop(self.handle.await);
    }
}

/// The observability drain's connection binding: the shared live-writer slot the
/// drain re-resolves per publish, and the bounded reconnect backoff (seeded from
/// the worker's reconnect config, coherent with the dispatch redial) it paces its
/// re-probes with during an outage.
///
/// Bundled so the dispatch path threads the drain's binding as one value — in
/// particular the detached agent-dispatch task, whose argument budget it keeps in
/// bounds.
#[derive(Clone, Debug)]
pub(crate) struct DrainBinding {
    writer: LiveWriter,
    backoff: RedialBackoff,
}

impl DrainBinding {
    /// Binds a drain to the shared live-writer `slot` paced by `backoff`.
    #[must_use]
    pub(crate) fn new(slot: LiveWriter, backoff: RedialBackoff) -> Self {
        Self {
            writer: slot,
            backoff,
        }
    }
}

/// Builds the LIVE observability event drain: an [`ActivityEvent`] sender handed
/// to the [`ActivityContext`](crate::context::ActivityContext)/agent driver, and
/// a background task that publishes every event to the server through the bound
/// live-writer slot — re-resolving the current live connection per publish so it
/// survives a redial, coalescing outage logging and dropping (never buffering)
/// events while the connection is down.
pub(crate) fn spawn_event_drain(
    binding: DrainBinding,
) -> (mpsc::UnboundedSender<ActivityEvent>, EventDrain) {
    let (event_sender, event_receiver) = mpsc::unbounded_channel::<ActivityEvent>();
    let handle = tokio::spawn(run_drain(binding.writer, event_receiver, binding.backoff));
    (event_sender, EventDrain { handle })
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use aion_core::{ActivityEvent, ActivityEventKind, ActivityId, MessageRole, WorkflowId};
    use uuid::Uuid;

    use super::{DrainState, DrainStep, EventSink, RedialBackoff, run_drain};

    fn backoff() -> RedialBackoff {
        RedialBackoff::new(Duration::from_millis(10), Duration::from_millis(40))
    }

    /// A healthy publish crosses no transition and drops nothing.
    #[test]
    fn healthy_publish_reports_no_transition() {
        let now = std::time::Instant::now();
        let mut state = DrainState::new(backoff(), now);
        assert!(state.should_probe(now));
        assert_eq!(
            state.on_published(),
            None,
            "no recovery while already healthy"
        );
        assert_eq!(state.on_close(), None, "a healthy close reports nothing");
    }

    /// The FIRST failure of an outage is the single logged transition; every
    /// subsequent failure in the same outage reports `false`, so the log fires
    /// once per outage, never once per event — the core coalescing guarantee.
    #[test]
    fn outage_start_is_reported_exactly_once() {
        let now = std::time::Instant::now();
        let mut state = DrainState::new(backoff(), now);

        assert!(state.on_broken(now), "the first failure starts the outage");
        for _ in 0..50 {
            // Probe time has been pushed out by backoff; a failure that DOES
            // re-probe still never re-reports the transition.
            let probe_at = state.next_probe;
            assert!(
                !state.on_broken(probe_at),
                "a subsequent failure never re-reports the outage transition"
            );
        }
        assert_eq!(state.on_close(), Some(51), "every failed event is counted");
    }

    /// While broken and within the backoff interval the drain defers (drops
    /// without probing); once the interval elapses it probes again. Deferred and
    /// probed drops are both counted.
    #[test]
    fn broken_defers_within_backoff_then_reprobes() {
        let start = std::time::Instant::now();
        let mut state = DrainState::new(backoff(), start);
        assert!(state.on_broken(start));
        let probe_at = state.next_probe;

        // Within the backoff window (the outage instant precedes the next probe):
        // defer, do not probe.
        assert!(
            !state.should_probe(start),
            "within backoff the drain defers"
        );
        state.on_deferred();

        // At/after the window: probing is allowed again.
        assert!(
            state.should_probe(probe_at),
            "backoff elapsed re-enables probing"
        );
        assert_eq!(
            state.on_close(),
            Some(2),
            "the deferred event is still counted"
        );
    }

    /// A successful publish after an outage is the single recovery transition,
    /// carrying the outage's dropped total, and it resets the state so a later
    /// outage is reported afresh.
    #[test]
    fn recovery_reports_dropped_total_once_and_resets() {
        let now = std::time::Instant::now();
        let mut state = DrainState::new(backoff(), now);
        assert!(state.on_broken(now));
        state.on_deferred();
        state.on_deferred();

        assert_eq!(
            state.on_published(),
            Some(3),
            "recovery reports the outage's dropped total"
        );
        assert_eq!(
            state.on_published(),
            None,
            "a second success is not a fresh recovery"
        );
        assert_eq!(state.on_close(), None, "a recovered drain closes clean");

        // A fresh outage after recovery is reported again (state was reset).
        assert!(
            state.on_broken(now),
            "a new outage re-reports its transition"
        );
    }

    /// The backoff widens across failures (so a dead socket is not hammered) and
    /// resets on recovery (a transient blip does not inflate the next outage's
    /// pause).
    #[test]
    fn backoff_widens_across_failures_and_resets_on_recovery() {
        let now = std::time::Instant::now();
        let mut state = DrainState::new(backoff(), now);

        state.on_broken(now);
        let first_gap = state.next_probe.saturating_duration_since(now);
        let second_base = state.next_probe;
        state.on_broken(second_base);
        let second_gap = state.next_probe.saturating_duration_since(second_base);
        assert!(
            second_gap > first_gap,
            "the re-probe interval widens across consecutive failures"
        );

        assert!(state.on_published().is_some());
        // After recovery the schedule is back to the initial pause.
        let fresh = std::time::Instant::now();
        state.on_broken(fresh);
        assert_eq!(
            state.next_probe.saturating_duration_since(fresh),
            Duration::from_millis(10),
            "recovery reset the backoff to its initial pause"
        );
    }

    /// An in-memory sink that fails while `up` is false (an outage) and publishes
    /// while it is true (a redial installed a live writer), recording every
    /// accepted payload and counting how many publish attempts reached it.
    struct FakeSink {
        up: Mutex<bool>,
        accepted: Mutex<Vec<Vec<u8>>>,
        attempts: Mutex<u32>,
    }

    impl FakeSink {
        fn new(up: bool) -> Self {
            Self {
                up: Mutex::new(up),
                accepted: Mutex::new(Vec::new()),
                attempts: Mutex::new(0),
            }
        }

        fn set_up(&self, up: bool) {
            *self.up.lock().unwrap_or_else(PoisonError::into_inner) = up;
        }

        fn accepted(&self) -> Vec<Vec<u8>> {
            self.accepted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn attempts(&self) -> u32 {
            *self.attempts.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn do_publish(&self, payload: Vec<u8>) -> DrainStep {
            *self.attempts.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            if *self.up.lock().unwrap_or_else(PoisonError::into_inner) {
                self.accepted
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(payload);
                DrainStep::Published
            } else {
                DrainStep::Broken {
                    reason: "fake outage".to_owned(),
                }
            }
        }
    }

    impl EventSink for std::sync::Arc<FakeSink> {
        fn publish(&self, payload: Vec<u8>) -> DrainStep {
            self.do_publish(payload)
        }
    }

    fn event(worker_seq: u64) -> ActivityEvent {
        ActivityEvent {
            workflow_id: WorkflowId::new(Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(1),
            attempt: 1,
            agent_id: Uuid::nil(),
            agent_role: "orchestrator".to_owned(),
            emitted_at: chrono::Utc::now(),
            worker_seq,
            store_seq: None,
            ephemeral: false,
            kind: ActivityEventKind::Message {
                role: MessageRole::Assistant,
                text: format!("event-{worker_seq}"),
            },
        }
    }

    /// End to end over the async loop: a publish failure triggers the drain to
    /// keep re-probing the sink (its reconnect attempt), events sent during the
    /// outage are DROPPED (never reach the sink's accepted set), and once the
    /// sink recovers — as a redial would install a live writer — later events
    /// RESUME flowing to it. A zero backoff makes every broken event re-probe,
    /// modelling the per-event reconnect attempt without wall-clock timing.
    #[tokio::test]
    async fn events_resume_flowing_after_the_sink_recovers()
    -> Result<(), Box<dyn std::error::Error>> {
        let sink = std::sync::Arc::new(FakeSink::new(false));
        let zero = RedialBackoff::new(Duration::ZERO, Duration::ZERO);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        let outage = event(1);
        let recovered_a = event(2);
        let recovered_b = event(3);
        let expected_a = serde_json::to_vec(&recovered_a)?;
        let expected_b = serde_json::to_vec(&recovered_b)?;

        let drain = tokio::spawn(run_drain(std::sync::Arc::clone(&sink), receiver, zero));

        // The first event lands during the outage: it must be dropped, and the
        // drain must have probed the sink at least once (its reconnect attempt).
        sender.send(outage)?;
        while sink.attempts() == 0 {
            tokio::task::yield_now().await;
        }
        assert!(
            sink.accepted().is_empty(),
            "an event sent during the outage is dropped, never published"
        );

        // A redial installs a live writer: flip the sink up, then send the
        // recovery events. The drain's next probe (zero backoff) resumes.
        sink.set_up(true);
        sender.send(recovered_a)?;
        sender.send(recovered_b)?;
        drop(sender);
        drain.await?;

        assert_eq!(
            sink.accepted(),
            vec![expected_a, expected_b],
            "post-recovery events resume flowing; the outage event is not among them"
        );
        Ok(())
    }
}
