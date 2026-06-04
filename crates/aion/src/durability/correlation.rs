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
    /// Child workflow scheduled at the deterministic ordinal currently represented by its start sequence.
    Child(
        /// Deterministic child scheduling ordinal.
        u64,
    ),
    /// Timer selected by workflow code or assigned by the engine.
    Timer(
        /// Timer identifier from the recorded timer-start event.
        TimerId,
    ),
    /// Signal delivery by name and zero-based occurrence for that name in history order.
    Signal {
        /// Signal name selected by the sender and requested by workflow code.
        name: String,
        /// Zero-based occurrence of this signal name in recorded history order.
        index: usize,
    },
}

/// Derives correlation keys for every event in an ordered history.
///
/// Non-world-touching events and activity/timer/child outcome events do not introduce a new call key,
/// so their slots contain `None`. Child workflow start events currently use the event sequence as the
/// child ordinal because `aion_core::WorkflowId` does not expose a child scheduling ordinal.
#[must_use]
pub fn correlation_keys_for_history(events: &[Event]) -> Vec<Option<CorrelationKey>> {
    let mut signal_counts = HashMap::<String, usize>::new();
    events
        .iter()
        .map(|event| key_for_event_with_signal_counts(event, &mut signal_counts))
        .collect()
}

/// Derives the correlation key for one event in an ordered history.
///
/// `index` is the event's index inside `events`; it is used to count prior signals with the same name.
#[must_use]
pub fn key_for_event(events: &[Event], index: usize) -> Option<CorrelationKey> {
    let event = events.get(index)?;
    match event {
        Event::SignalReceived { name, .. } => {
            let prior_same_name = events
                .iter()
                .take(index)
                .filter(|prior| matches!(prior, Event::SignalReceived { name: prior_name, .. } if prior_name == name))
                .count();
            Some(CorrelationKey::Signal {
                name: name.clone(),
                index: prior_same_name,
            })
        }
        _ => key_for_matchable_non_signal_event(event),
    }
}

fn key_for_event_with_signal_counts(
    event: &Event,
    signal_counts: &mut HashMap<String, usize>,
) -> Option<CorrelationKey> {
    match event {
        Event::SignalReceived { name, .. } => {
            let index = match signal_counts.get(name).copied() {
                Some(count) => count,
                None => 0,
            };
            signal_counts.insert(name.clone(), index + 1);
            Some(CorrelationKey::Signal {
                name: name.clone(),
                index,
            })
        }
        _ => key_for_matchable_non_signal_event(event),
    }
}

fn key_for_matchable_non_signal_event(event: &Event) -> Option<CorrelationKey> {
    match event {
        Event::ActivityScheduled { activity_id, .. } => {
            Some(CorrelationKey::Activity(activity_id.sequence_position()))
        }
        Event::TimerStarted { timer_id, .. } => Some(CorrelationKey::Timer(timer_id.clone())),
        Event::ChildWorkflowStarted { .. } => Some(CorrelationKey::Child(event.seq())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{CorrelationKey, correlation_keys_for_history};

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
}
