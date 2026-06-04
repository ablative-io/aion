//! Per-workflow sequence-head tracking.

use crate::durability::DurabilityError;

/// Tracks the current persisted sequence head for one workflow history.
///
/// The head is the store's `expected_seq` value: empty histories start at `0`, and after appending
/// `N` events the head advances by exactly `N`. Callers should advance this tracker only after the
/// backing store has accepted the append.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SequenceHead {
    head: u64,
}

impl SequenceHead {
    /// Creates a tracker for a fresh workflow history at head `0`.
    #[must_use]
    pub const fn new() -> Self {
        Self::from_head(0)
    }

    /// Creates a tracker from an explicitly derived persisted head.
    #[must_use]
    pub const fn from_head(head: u64) -> Self {
        Self { head }
    }

    /// Returns the current store head to use as `expected_seq`.
    #[must_use]
    pub const fn current(&self) -> u64 {
        self.head
    }

    /// Returns the sequence number for the first event in the next append batch.
    #[must_use]
    pub const fn next_seq(&self) -> Option<u64> {
        self.head.checked_add(1)
    }

    /// Marks a successful append and advances the head by the number of appended events.
    ///
    /// Advancing by zero is accepted as a no-op because stores may accept empty append batches, but
    /// the Recorder only records non-empty single-event batches.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError::HistoryShape`] if the event count cannot fit into `u64` or if the
    /// resulting sequence head would overflow.
    pub fn mark_append_success(&mut self, event_count: usize) -> Result<(), DurabilityError> {
        let count = u64::try_from(event_count).map_err(|error| DurabilityError::HistoryShape {
            reason: format!("append batch length does not fit in u64: {error}"),
        })?;
        self.head = self
            .head
            .checked_add(count)
            .ok_or_else(|| DurabilityError::HistoryShape {
                reason: format!("sequence head overflow advancing {} by {count}", self.head),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SequenceHead;

    #[test]
    fn starts_at_zero_by_default() {
        let sequence = SequenceHead::new();

        assert_eq!(sequence.current(), 0);
        assert_eq!(sequence.next_seq(), Some(1));
    }

    #[test]
    fn starts_from_explicit_head() {
        let sequence = SequenceHead::from_head(7);

        assert_eq!(sequence.current(), 7);
        assert_eq!(sequence.next_seq(), Some(8));
    }

    #[test]
    fn next_sequence_overflow_is_detectable_by_advance() {
        let mut sequence = SequenceHead::from_head(u64::MAX);

        assert_eq!(sequence.next_seq(), None);
        assert!(sequence.mark_append_success(1).is_err());
        assert_eq!(sequence.current(), u64::MAX);
    }

    #[test]
    fn advances_only_after_success_is_marked() -> Result<(), Box<dyn std::error::Error>> {
        let mut sequence = SequenceHead::from_head(3);

        let simulated_failure: Result<(), ()> = Err(());
        if simulated_failure.is_ok() {
            sequence.mark_append_success(2)?;
        }
        assert_eq!(sequence.current(), 3);

        sequence.mark_append_success(2)?;
        assert_eq!(sequence.current(), 5);
        assert_eq!(sequence.next_seq(), Some(6));
        Ok(())
    }
}
