//! Correlation keys + matching rules.

use std::collections::HashMap;

use aion_core::{Event, TimerId};

/// Deterministic identity for one world-touching workflow call.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CorrelationKey {
    /// Activity scheduled at the deterministic ordinal carried by its [`aion_core::ActivityId`].
    Activity(
        /// Deterministic scheduling ordinal.
        u64,
    ),
    /// Child workflow scheduled at the deterministic spawn ordinal.
    ///
    /// The ordinal is positional: the n-th `spawn_child` call a run makes
    /// correlates with the n-th recorded `ChildWorkflowStarted` in that run's
    /// history segment. Like activity ordinals it restarts at zero for every
    /// run, so replay re-derives the same identity regardless of how many
    /// asynchronous-arrival events (signals, timer fires) interleave.
    Child(
        /// Zero-based spawn ordinal within the run segment.
        u64,
    ),
    /// Timer selected by workflow code or assigned by the engine.
    Timer(
        /// Timer identifier from the recorded timer-start event.
        TimerId,
    ),
    /// Signal delivery by name and zero-based occurrence for that name in history order.
    Signal {
        /// Signal name selected by workflow code.
        name: String,
        /// Zero-based occurrence of this signal name in recorded history order.
        index: usize,
    },
}

/// Derives correlation keys for every event in an ordered history.
///
/// Non-world-touching events and activity/timer/child outcome events do not introduce a new call
/// key, so their slots contain `None`. Signal keys carry the per-name occurrence index and child
/// workflow start keys carry the positional spawn ordinal, both derived from event order within
/// the supplied history slice.
#[must_use]
pub fn correlation_keys_for_history(events: &[Event]) -> Vec<Option<CorrelationKey>> {
    let mut counters = OccurrenceCounters::default();
    events
        .iter()
        .map(|event| key_for_event_with_counters(event, &mut counters))
        .collect()
}

/// Derives the correlation key for one event in an ordered history.
///
/// `index` is the event's index inside `events`; it is used to count prior signals with the same
/// name and prior child workflow starts, so signal occurrence indices and child spawn ordinals are
/// positional within the slice.
#[must_use]
pub fn key_for_event(events: &[Event], index: usize) -> Option<CorrelationKey> {
    let event = events.get(index)?;
    match event {
        Event::SignalReceived { name, .. } | Event::SignalSent { name, .. } => {
            let prior_same_name = events
                .iter()
                .take(index)
                .filter(|prior| matches!(prior, Event::SignalReceived { name: prior_name, .. } | Event::SignalSent { name: prior_name, .. } if prior_name == name))
                .count();
            Some(CorrelationKey::Signal {
                name: name.clone(),
                index: prior_same_name,
            })
        }
        Event::ChildWorkflowStarted { .. } => {
            let prior_starts = events
                .iter()
                .take(index)
                .filter(|prior| matches!(prior, Event::ChildWorkflowStarted { .. }))
                .count();
            // On the 64-bit targets this engine ships on, usize -> u64 never
            // fails; the fallible conversion exists only to satisfy the type
            // signature on hypothetical wider-usize platforms. There, an
            // overflowing count would leave this start event keyless: strict
            // replay reports a mismatch when resolution reaches it, but the
            // live fast-forward path skips keyless events silently — a
            // missing key is not guaranteed to surface as an error.
            u64::try_from(prior_starts).ok().map(CorrelationKey::Child)
        }
        _ => key_for_positionless_event(event),
    }
}

/// Running occurrence counts used to derive positional keys in one history pass.
#[derive(Default)]
struct OccurrenceCounters {
    signal_counts: HashMap<String, usize>,
    child_spawns: u64,
}

fn key_for_event_with_counters(
    event: &Event,
    counters: &mut OccurrenceCounters,
) -> Option<CorrelationKey> {
    match event {
        Event::SignalReceived { name, .. } | Event::SignalSent { name, .. } => {
            let index = counters
                .signal_counts
                .get(name)
                .copied()
                .unwrap_or_default();
            counters.signal_counts.insert(name.clone(), index + 1);
            Some(CorrelationKey::Signal {
                name: name.clone(),
                index,
            })
        }
        Event::ChildWorkflowStarted { .. } => {
            let ordinal = counters.child_spawns;
            counters.child_spawns += 1;
            Some(CorrelationKey::Child(ordinal))
        }
        _ => key_for_positionless_event(event),
    }
}

fn key_for_positionless_event(event: &Event) -> Option<CorrelationKey> {
    match event {
        Event::ActivityScheduled { activity_id, .. } => {
            Some(CorrelationKey::Activity(activity_id.sequence_position()))
        }
        Event::TimerStarted { timer_id, .. } => Some(CorrelationKey::Timer(timer_id.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{CorrelationKey, correlation_keys_for_history, key_for_event};

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: WorkflowId::new(Uuid::nil()),
        }
    }

    fn payload() -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(&json!(null))?)
    }

    #[test]
    fn derives_signal_occurrence_indices_by_name() -> Result<(), Box<dyn std::error::Error>> {
        let history = vec![
            Event::SignalReceived {
                envelope: envelope(1),
                name: "ready".to_owned(),
                payload: payload()?,
            },
            Event::SignalReceived {
                envelope: envelope(2),
                name: "other".to_owned(),
                payload: payload()?,
            },
            Event::SignalReceived {
                envelope: envelope(3),
                name: "ready".to_owned(),
                payload: payload()?,
            },
        ];

        let keys = correlation_keys_for_history(&history);

        assert_eq!(
            keys,
            vec![
                Some(CorrelationKey::Signal {
                    name: "ready".to_owned(),
                    index: 0,
                }),
                Some(CorrelationKey::Signal {
                    name: "other".to_owned(),
                    index: 0,
                }),
                Some(CorrelationKey::Signal {
                    name: "ready".to_owned(),
                    index: 1,
                }),
            ]
        );
        Ok(())
    }

    fn child_started(seq: u64, child: u128) -> Result<Event, Box<dyn std::error::Error>> {
        Ok(Event::ChildWorkflowStarted {
            envelope: envelope(seq),
            child_workflow_id: WorkflowId::new(Uuid::from_u128(child)),
            workflow_type: "child".to_owned(),
            input: payload()?,
        })
    }

    #[test]
    fn derives_positional_child_ordinals_independent_of_sequence_numbers()
    -> Result<(), Box<dyn std::error::Error>> {
        // Deliberately sparse, late sequence numbers: positional ordinals
        // must not be derived from event sequence values.
        let history = vec![child_started(41, 1)?, child_started(97, 2)?];

        let keys = correlation_keys_for_history(&history);

        assert_eq!(
            keys,
            vec![
                Some(CorrelationKey::Child(0)),
                Some(CorrelationKey::Child(1)),
            ]
        );
        assert_eq!(key_for_event(&history, 0), Some(CorrelationKey::Child(0)));
        assert_eq!(key_for_event(&history, 1), Some(CorrelationKey::Child(1)));
        Ok(())
    }

    #[test]
    fn interleaved_async_arrivals_do_not_shift_child_ordinals()
    -> Result<(), Box<dyn std::error::Error>> {
        let history = vec![
            child_started(1, 1)?,
            Event::SignalReceived {
                envelope: envelope(2),
                name: "mid".to_owned(),
                payload: payload()?,
            },
            Event::ChildWorkflowCompleted {
                envelope: envelope(3),
                child_workflow_id: WorkflowId::new(Uuid::from_u128(1)),
                result: payload()?,
            },
            child_started(4, 2)?,
        ];

        let keys = correlation_keys_for_history(&history);

        assert_eq!(
            keys,
            vec![
                Some(CorrelationKey::Child(0)),
                Some(CorrelationKey::Signal {
                    name: "mid".to_owned(),
                    index: 0,
                }),
                None,
                Some(CorrelationKey::Child(1)),
            ]
        );
        assert_eq!(key_for_event(&history, 3), Some(CorrelationKey::Child(1)));
        Ok(())
    }
}
