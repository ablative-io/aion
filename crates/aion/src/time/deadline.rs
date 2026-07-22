//! Reserved workflow-deadline timer identity and the deadline-handler seam.
//!
//! A declared workflow timeout arms a single reserved timer named
//! `deadline:{run_id}`. The reserved prefix is engine-minted only — the author
//! NIF choke point (`decode_timer_id_arg`) refuses any author-supplied name that
//! starts with it — so a `deadline:` timer in history is always the engine's
//! per-run deadline and never a workflow-authored timer.
//!
//! When such a timer elapses, [`TimerService`](crate::time::TimerService)
//! demuxes it out of the generic `TimerFired` path and hands it to the
//! registered [`DeadlineHandler`], which records `WorkflowTimedOut` and tears
//! the run down engine-side. Routing is inversion of control precisely so the
//! timer machinery never needs a strong handle back to the engine (the
//! documented `RuntimeHandle`/`EngineNifState` cycle-avoidance): the engine
//! registers a handler holding whatever weak references it needs at construction
//! time.

use aion_core::{RunId, TimerId};
use uuid::Uuid;

/// Reserved name prefix for a workflow's declared-timeout deadline timer.
///
/// Engine-minted only: `decode_timer_id_arg` refuses author-supplied names under
/// this prefix, so a `deadline:` timer is always the per-run workflow deadline.
pub const DEADLINE_TIMER_PREFIX: &str = "deadline:";

/// Closed-set descriptor token recorded on the `WorkflowTimedOut` terminal for a
/// declared workflow timeout, consistent with the `deadline:` timer id family.
///
/// User-visible in `one_motion` output and replay terminals; the sole value a
/// declared-workflow-timeout deadline records.
pub const WORKFLOW_TIMEOUT_DESCRIPTOR: &str = "workflow";

/// The reserved deadline timer id for `run_id`: `deadline:{run_id}`.
///
/// # Errors
///
/// Returns [`aion_core::IdError`] only if timer-name construction rejects the
/// composed name; the name is always non-empty, so this never fails in practice
/// and the `Result` exists solely to keep the helper total without an `unwrap`.
pub fn deadline_timer_id(run_id: &RunId) -> Result<TimerId, aion_core::IdError> {
    TimerId::named(format!("{DEADLINE_TIMER_PREFIX}{run_id}"))
}

/// Whether `timer_id` is a reserved workflow-deadline timer.
#[must_use]
pub fn is_deadline_timer(timer_id: &TimerId) -> bool {
    timer_id
        .name()
        .is_some_and(|name| name.starts_with(DEADLINE_TIMER_PREFIX))
}

/// The run id encoded in a `deadline:{run_id}` timer, if `timer_id` is a
/// well-formed reserved deadline timer.
///
/// Returns `None` for a non-deadline timer or a deadline-prefixed name whose
/// suffix is not a valid run identifier.
#[must_use]
pub fn deadline_run_id(timer_id: &TimerId) -> Option<RunId> {
    let suffix = timer_id.name()?.strip_prefix(DEADLINE_TIMER_PREFIX)?;
    Uuid::parse_str(suffix).ok().map(RunId::new)
}

/// Errors surfaced by a [`DeadlineHandler`] to the timer service.
///
/// Deliberately message-only: the handler lives engine-side and produces the
/// engine's own typed errors, which the timer service (in a lower crate layer)
/// only needs to observe and propagate as a fire failure.
#[derive(thiserror::Error, Debug)]
#[error("deadline handler failed: {0}")]
pub struct DeadlineHandlerError(pub String);

/// Engine-side handler invoked when a workflow's declared-timeout deadline
/// elapses.
///
/// The timer service demuxes a reserved `deadline:{run_id}` fire to this seam
/// instead of recording a generic `TimerFired`. The implementation records
/// `WorkflowTimedOut` for the run under the per-handle recorder lock (losing
/// cleanly to any concurrent terminal) and tears the run down.
#[async_trait::async_trait]
pub trait DeadlineHandler: Send + Sync {
    /// Drive `run_id` of `workflow_id` to a `WorkflowTimedOut` terminal.
    ///
    /// # Errors
    ///
    /// Returns [`DeadlineHandlerError`] when the terminal cannot be recorded or
    /// the run cannot be torn down; the timer service surfaces it as a fire
    /// failure.
    async fn on_deadline_elapsed(
        &self,
        workflow_id: aion_core::WorkflowId,
        run_id: RunId,
    ) -> Result<(), DeadlineHandlerError>;
}

#[cfg(test)]
mod tests {
    use aion_core::{RunId, TimerId};
    use uuid::Uuid;

    use super::{deadline_run_id, deadline_timer_id, is_deadline_timer};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn deadline_timer_id_round_trips_to_its_run() -> TestResult {
        let run_id = RunId::new(Uuid::from_u128(42));
        let timer_id = deadline_timer_id(&run_id)?;
        assert!(is_deadline_timer(&timer_id));
        assert_eq!(deadline_run_id(&timer_id), Some(run_id));
        Ok(())
    }

    #[test]
    fn author_named_timer_is_not_a_deadline() -> TestResult {
        let timer_id = TimerId::named("review-deadline")?;
        assert!(!is_deadline_timer(&timer_id));
        assert_eq!(deadline_run_id(&timer_id), None);
        Ok(())
    }

    #[test]
    fn anonymous_timer_is_not_a_deadline() {
        let timer_id = TimerId::anonymous(7);
        assert!(!is_deadline_timer(&timer_id));
        assert_eq!(deadline_run_id(&timer_id), None);
    }

    #[test]
    fn deadline_prefixed_but_malformed_suffix_has_no_run() -> TestResult {
        // A `deadline:`-prefixed name whose suffix is not a UUID is detected as
        // a deadline timer (prefix match) but yields no run id — the timer
        // service turns this into a typed refusal, never a silent generic fire.
        let timer_id = TimerId::named("deadline:not-a-uuid")?;
        assert!(is_deadline_timer(&timer_id));
        assert_eq!(deadline_run_id(&timer_id), None);
        Ok(())
    }
}
