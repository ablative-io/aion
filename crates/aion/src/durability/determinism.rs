//! Deterministic workflow-visible time.
//!
//! `DeterminismContext` is replay state, not a clock. Workflow-visible `now`
//! is always the timestamp recorded on the history event currently being
//! applied. Recovery wall time, when supplied elsewhere as an `as_of` value for
//! expired-timer decisions, is intentionally outside this context and is not
//! workflow-visible.
//!
//! Workflow-visible **random** is deliberately *not* served here. The single
//! production random path is the determinism NIF
//! ([`crate::runtime::nif_determinism`]): `workflow.random()` /
//! `workflow.random_int()` draw `deterministic_float` / `deterministic_i64`
//! keyed by a per-call sequence ordinal the workflow handle hands out, once per
//! `random()` call the workflow code actually makes. There is no parallel
//! random stream in this context — keeping one would be a second, divergent
//! source that no production code consumes (ADR-002).

use chrono::{DateTime, Utc};

/// Per-execution deterministic state for workflow-visible time.
///
/// The current timestamp is advanced only from recorded event timestamps as
/// replay consumes history; no wall clock participates. Random is served by the
/// determinism NIF, not this context (see the module docs).
pub struct DeterminismContext {
    current_recorded_at: DateTime<Utc>,
}

impl DeterminismContext {
    /// Creates deterministic state for a workflow run.
    ///
    /// `workflow_started_recorded_at` must be the `recorded_at` timestamp from
    /// the run's first recorded `WorkflowStarted` event. Before any later event
    /// is applied, [`Self::now`] returns this timestamp.
    #[must_use]
    pub const fn new(workflow_started_recorded_at: DateTime<Utc>) -> Self {
        Self {
            current_recorded_at: workflow_started_recorded_at,
        }
    }

    /// Returns the currently applied recorded timestamp for workflow-visible
    /// `now`.
    #[must_use]
    pub const fn now(&self) -> DateTime<Utc> {
        self.current_recorded_at
    }

    /// Advances workflow-visible `now` to the timestamp of a newly applied
    /// recorded event.
    pub const fn advance_to_recorded_at(&mut self, recorded_at: DateTime<Utc>) {
        self.current_recorded_at = recorded_at;
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};

    use super::DeterminismContext;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn timestamp(seconds: i64) -> TestResult<DateTime<Utc>> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| format!("invalid fixed timestamp {seconds}").into())
    }

    #[test]
    fn now_starts_at_workflow_started_and_advances_with_recorded_events() -> TestResult {
        let started_at = timestamp(1_700_000_000)?;
        let first_event_at = timestamp(1_700_000_010)?;
        let second_event_at = timestamp(1_700_000_020)?;
        let mut context = DeterminismContext::new(started_at);

        assert_eq!(context.now(), started_at);
        context.advance_to_recorded_at(first_event_at);
        assert_eq!(context.now(), first_event_at);
        context.advance_to_recorded_at(second_event_at);
        assert_eq!(context.now(), second_event_at);

        Ok(())
    }

    #[test]
    fn identical_recorded_sequences_have_identical_now_values() -> TestResult {
        let started_at = timestamp(1_700_100_000)?;
        let events = [
            timestamp(1_700_100_001)?,
            timestamp(1_700_100_005)?,
            timestamp(1_700_100_030)?,
        ];
        let mut first = DeterminismContext::new(started_at);
        let mut second = DeterminismContext::new(started_at);

        assert_eq!(first.now(), second.now());
        for recorded_at in events {
            first.advance_to_recorded_at(recorded_at);
            second.advance_to_recorded_at(recorded_at);
            assert_eq!(first.now(), second.now());
        }

        Ok(())
    }
}
