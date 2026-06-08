//! Graceful shutdown and single-node activity drain coordination.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::ServerState;
use crate::error::ServerError;
use crate::worker::LostWorkerReport;

/// Process exit selected by the shutdown coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    /// Drain completed before the configured timeout.
    Clean,
    /// At least one in-flight activity exceeded the drain timeout.
    TimedOut,
    /// A second termination signal requested immediate process exit.
    Forced,
}

impl ShutdownOutcome {
    /// Convert the outcome to the process exit code required by the operations contract.
    #[must_use]
    pub fn exit_code(self) -> ExitCode {
        match self {
            Self::Clean => ExitCode::SUCCESS,
            Self::TimedOut => ExitCode::FAILURE,
            Self::Forced => ExitCode::from(130),
        }
    }
}

/// Cloneable gate shared by transports, dispatchers, worker streams, and the
/// shutdown coordinator.
#[derive(Clone, Debug, Default)]
pub struct DrainState {
    inner: Arc<DrainStateInner>,
}

#[derive(Debug, Default)]
struct DrainStateInner {
    draining: AtomicBool,
    empty: Notify,
}

impl DrainState {
    /// Return whether drain has begun and new workflow/activity starts must be rejected.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    /// Mark the server draining. Returns true for the first caller that changed the state.
    #[must_use]
    pub fn begin(&self) -> bool {
        !self.inner.draining.swap(true, Ordering::AcqRel)
    }

    /// Reject a new unit of work if drain has already begun.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::WorkerDispatch`] with a stable drain message when work is closed.
    pub fn ensure_accepting(
        &self,
        namespace: &str,
        activity_type: &str,
    ) -> Result<(), ServerError> {
        if self.is_draining() {
            Err(ServerError::worker_dispatch(
                namespace.to_owned(),
                activity_type.to_owned(),
                "server is draining and not accepting new activity tasks",
            ))
        } else {
            Ok(())
        }
    }

    /// Wake waiters after in-flight accounting may have reached zero.
    pub fn notify_activity_drained(&self) {
        self.inner.empty.notify_waiters();
    }

    async fn wait_for_empty(&self, state: &ServerState) -> Result<(), ServerError> {
        loop {
            let in_flight = state.heartbeat_tracker().in_flight_count()?;
            if in_flight == 0 {
                return Ok(());
            }
            let notified = self.inner.empty.notified();
            if state.heartbeat_tracker().in_flight_count()? == 0 {
                return Ok(());
            }
            notified.await;
        }
    }
}

/// Run the graceful drain after the first termination signal.
///
/// The caller is responsible for stopping transports as soon as drain begins.
pub async fn drain_after_first_signal(
    state: ServerState,
    second_signal: impl std::future::Future<Output = ()>,
) -> Result<ShutdownOutcome, ServerError> {
    let drain = state.drain_state().clone();
    let first = drain.begin();
    if first {
        info!("shutdown signal received; beginning graceful drain");
    }

    let delivered_workers = state.worker_registry().broadcast_drain()?;
    info!(delivered_workers, "sent drain request to connected workers");

    let timeout = state.runtime_config().drain_timeout;
    tokio::pin!(second_signal);

    let outcome = tokio::select! {
        () = &mut second_signal => {
            warn!("second shutdown signal received; forcing immediate exit");
            ShutdownOutcome::Forced
        }
        result = wait_for_drain_or_timeout(&state, &drain, timeout) => result?,
    };

    if matches!(outcome, ShutdownOutcome::Forced) {
        return Ok(outcome);
    }

    state.shutdown()?;
    Ok(outcome)
}

async fn wait_for_drain_or_timeout(
    state: &ServerState,
    drain: &DrainState,
    timeout: Duration,
) -> Result<ShutdownOutcome, ServerError> {
    match tokio::time::timeout(timeout, drain.wait_for_empty(state)).await {
        Ok(result) => {
            result?;
            info!("activity drain completed cleanly");
            Ok(ShutdownOutcome::Clean)
        }
        Err(_elapsed) => {
            let reports = state
                .heartbeat_tracker()
                .fail_all_in_flight_workers(state.worker_registry(), state.pending_activities())?;
            log_lost_workers(&reports);
            Ok(ShutdownOutcome::TimedOut)
        }
    }
}

fn log_lost_workers(reports: &[LostWorkerReport]) {
    let failed_tasks: usize = reports.iter().map(|report| report.tasks.len()).sum();
    if failed_tasks == 0 {
        info!("activity drain timed out with no tracked in-flight activity failures");
    } else {
        error!(
            failed_workers = reports.len(),
            failed_tasks,
            "activity drain timed out; remaining activities surfaced as retryable lost-worker failures"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::DrainState;

    #[test]
    fn begin_is_idempotent_and_sets_draining() {
        let drain = DrainState::default();

        assert!(!drain.is_draining());
        assert!(drain.begin());
        assert!(drain.is_draining());
        assert!(!drain.begin());
    }
}
